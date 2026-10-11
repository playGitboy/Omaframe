//! 设置**面板**（弹出式，Omarchy 插件那种手感）：libadwaita 原生风格，改动即时生效并自动保存。
//!
//! 行为：
//! - 点状态栏图标 → 面板出现在顶栏右下方（layer-shell overlay 层，不参与平铺、不抢平铺位置）
//! - 再点一次图标 / 按 Esc / **别的窗口或工作区拿到焦点** → 隐藏
//!   （焦点变化靠 Hyprland IPC 事件判定：见 `hypr::on_focus_change`）
//! - 隐藏即**销毁**窗口：下次打开看到的一定是最新配置（拖动改过的宽高会立刻反映出来）
//!
//! - `omaframe settings` → 若已有实例在跑，通过 `$XDG_RUNTIME_DIR` 下的
//!   Unix socket 通知它开面板；否则直接以设置模式启动。

use crate::app::AppState;
use adw::prelude::*;
use layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::RefCell;
use std::rc::Rc;

/// 面板宽度 / 目标高度上限（逻辑像素）；高度会按可用区自适应
const PANEL_W: i32 = 360;
const PANEL_H_MAX: i32 = 720;
/// 与顶栏 / 屏幕边缘的间隙
const PANEL_GAP: i32 = 6;

/// 显示器逻辑尺寸（取不到时给个保守值）
fn monitor_size(state: &Rc<AppState>) -> (i32, i32) {
    let connector = state.config.borrow().window.monitor.clone();
    crate::window::target_monitor(&connector)
        .as_ref()
        .map(crate::window::monitor_bounds)
        .map(|b| (b.width, b.height))
        .unwrap_or((1920, 1080))
}

/// 弹出面板。
///
/// 形态：**一个**铺满"顶栏以下"的 overlay 窗口（= 遮罩 + 面板，不再用第二个 surface）。
/// 面板卡片对齐到右上角，用坐标判断"点在卡片外"就收起。
/// 为什么不用单独的透明遮罩表面：四边锚定被合成器拉伸的 surface 实测**收不到鼠标事件**
/// （面板这种有正常尺寸协商的窗口能收到），见 docs/KEY-FINDINGS.md 3.8。
pub struct Panel {
    win: adw::ApplicationWindow,
}

impl Panel {
    /// 挂到 Application 上（否则 GApplication 会看不到窗口而退出）
    pub fn set_application(&self, app: &adw::Application) {
        self.win.set_application(Some(app));
    }

    pub fn is_visible(&self) -> bool {
        self.win.is_visible()
    }
}

/// 刚显示面板的时刻：用于"宽限期"，避免刚弹出就被焦点事件又关掉
static LAST_SHOW: std::sync::OnceLock<std::sync::Mutex<Option<std::time::Instant>>> =
    std::sync::OnceLock::new();

fn mark_shown() {
    let slot = LAST_SHOW.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(mut g) = slot.lock() {
        *g = Some(std::time::Instant::now());
    }
}

/// 面板刚显示不足 `GRACE_MS` 毫秒 → 忽略"焦点变化"（例如从目录对话框关掉后回到面板）
fn within_show_grace() -> bool {
    const GRACE_MS: u128 = 500;
    LAST_SHOW
        .get()
        .and_then(|s| s.lock().ok().and_then(|g| *g))
        .map(|t| t.elapsed().as_millis() < GRACE_MS)
        .unwrap_or(false)
}

/// 显示面板（已经开着就只把它抬到前面，不重建）
/// 同步"用户可能在外部改过"的状态：媒体目录重扫 + 相框库索引重建。
/// 幂等且很便宜（相框库用指纹比对，没变就跳过）。
fn sync_external_state(state: &Rc<AppState>) {
    if let Some(player) = state.player.borrow().clone() {
        player.rescan();                 // 媒体目录：用户可能新加了素材
        player.refresh_frame_index();    // 相框库：用户可能新加/删/换了相框图
    }
}

pub fn show(state: &Rc<AppState>) {
    // 面板已打开也要同步：否则重复点托盘图标/再次 `omaframe settings`
    // 会走下面的 early-return，把重扫整个跳过（表现为"删了相框还在"）。
    sync_external_state(state);
    if let Some(p) = state.settings_window.borrow().as_ref() {
        if p.is_visible() {
            p.win.present();
            mark_shown();
            return;
        }
    }
    // 打开前**重新读取配置文件**：外部改过 config.toml 时，面板里的数值/下拉项也要跟着变
    if state.reload_from_disk() {
        apply_reloaded(state);
    }
    // 每次打开面板都**重新扫描媒体目录**：用户很可能刚手动加了新素材，
    // 桌面组件默认只在启动时扫一次，不重扫就看不到新文件。
    // 扫描在后台线程（rescan 是异步的），不阻塞面板显示；
    // 扫完由 after_scan() 刷新当前素材/列表。
    hide(state, "重建");
    let panel = build(state);
    // **必须在 present() 之前挂到 Application**：否则窗口已经 realize 才关联，
    // GtkApplication 的窗口跟踪会错位。原来是在 app.rs 里 present 之后才补挂的。
    // 用 gio::Application::default() 取默认实例 → 任何调用方（托盘/控制 socket/
    // 命令行）打开面板都能正确挂上，不依赖调用点是否持有 app。
    if let Some(app) = gio::Application::default() {
        if let Ok(a) = app.downcast::<adw::Application>() {
            panel.set_application(&a);
        }
    }
    panel.win.present();
    panel.win.grab_focus();
    *state.settings_window.borrow_mut() = Some(panel);
    mark_shown();
    crate::debug!("设置面板 → 显示");
}

/// 隐藏并**释放**面板（`why` 只用于排查"到底谁把它关了"）
/// 退出整个应用（`omaframe quit` / 控制通道 quit 命令）。
///
/// 先把配置落盘（`AppState::dirty` 为真时），再退出 GTK 主循环，
/// `app.run()` 就会返回、进程正常退出（比 kill 干净，配置不会丢）。
pub fn quit(state: &Rc<AppState>) {
    crate::info!("收到退出命令，正在退出…");
    if state.dirty.get() {
        state.commit();
    }
    if let Some(app) = gtk::gio::Application::default() {
        app.quit();
    }
}

pub fn hide(state: &Rc<AppState>, why: &str) {
    let taken = state.settings_window.borrow_mut().take();
    if let Some(p) = taken {
        p.win.set_visible(false);
        crate::debug!("设置面板 → 隐藏并销毁（{why}）");
        // 面板**每次打开都会重建**（有意为之：重读配置 + 重扫素材目录），
        // 所以旧窗口必须真正销毁。只 set_visible(false) 是不够的：
        // 窗口已挂到 GApplication（GtkApplication 持有其引用）→ 它会一直活着，
        // 反复开关设置页就会持续累积控件树（内存只涨不降）。
        //
        // 放到 idle 里销毁：hide() 可能正从 close_request / 焦点回调里被调用，
        // 在信号回调中直接销毁窗口时序敏感 —— 而且 close_request 里我们返回
        // Propagation::Stop，GTK 不会自己销毁，必须由这里收尾。
        let win = p.win.clone();
        glib::idle_add_local_once(move || win.destroy());
    }
}

/// 焦点事件处理：**别的窗口**（或工作区）拿到焦点时收起面板。
///
/// 按负载去重：Hyprland 在打开 GTK 弹窗（下拉列表）时也会补发 `activewindow`，
/// 但负载里的窗口其实没变（同一个 class,title）→ 这种情况不该收面板。
pub fn hide_if_open(state: &Rc<AppState>, payload: &str) {
    LAST_FOCUS.with(|last| {
        let changed = last.borrow().as_deref() != Some(payload);
        *last.borrow_mut() = Some(payload.to_string());
        if !changed {
            return;
        }
        let open = state
            .settings_window
            .borrow()
            .as_ref()
            .map(|p| p.is_visible())
            .unwrap_or(false);
        // 刚弹出的一小段时间内忽略焦点事件：比如从目录对话框关掉后面板刚回来，
        // 焦点正好切回原窗口，不该被当成"失去焦点"立刻关掉。
        if open && !payload.is_empty() && !within_show_grace() {
            hide(state, "失去焦点（其它窗口/工作区）");
        }
    });
}

thread_local! {
    /// 上一次焦点事件负载（用于去重 Hyprland 补发的同值事件）
    static LAST_FOCUS: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// 配置从磁盘重读后，把"会立刻影响观感"的几项重新应用到运行中的窗口
fn apply_reloaded(state: &Rc<AppState>) {
    let player = state.player.borrow().clone();
    let Some(p) = player else { return };
    p.load_frame();
    p.apply_zoom();
    if let Some(w) = state.window() {
        w.update_hud(state, "");
    }
    p.refresh_visibility_rect();
}

/// 状态栏图标点击 = 开关
pub fn toggle(state: &Rc<AppState>) {
    let visible = state
        .settings_window
        .borrow()
        .as_ref()
        .map(|p| p.is_visible())
        .unwrap_or(false);
    if visible {
        hide(state, "toggle");
    } else {
        show(state);
    }
}

/// 通知运行中的实例执行某个控制命令（settings/quit 等）；成功返回 true。
/// 内部函数：把命令写进控制 socket。
fn send_control(cmd: &str) -> bool {
    let Some(dir) = crate::config::runtime_dir() else {
        return false;
    };
    let path = dir.join("control.sock");
    if !path.exists() {
        return false;
    }
    match std::os::unix::net::UnixStream::connect(&path) {
        Ok(mut s) => {
            use std::io::Write;
            let _ = s.write_all(format!("{cmd}\n").as_bytes());
            true
        }
        Err(_) => false,
    }
}

/// 通知运行中的实例打开设置窗口；成功返回 true
pub fn request_open() -> bool {
    send_control("settings")
}

/// 通知运行中的实例退出；成功返回 true
pub fn request_quit() -> bool {
    send_control("quit")
}

// 监听控制 socket（只有主实例做）
thread_local! {
    /// 主线程侧的 AppState（控制线程通过 invoke 回调取用；invoke 保证在主线程执行）
    static STATE: RefCell<Option<Rc<AppState>>> = const { RefCell::new(None) };
}

pub fn serve_control(state: Rc<AppState>) {
    STATE.with(|s| *s.borrow_mut() = Some(state.clone()));
    let Some(dir) = crate::config::runtime_dir() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("control.sock");
    let _ = std::fs::remove_file(&path);
    let Ok(listener) = std::os::unix::net::UnixListener::bind(&path) else {
        crate::warn!("无法创建控制 socket：{}", path.display());
        return;
    };
    std::thread::Builder::new()
        .name("omaframe-ctl".into())
        .spawn(move || {
            let ctx = glib::MainContext::default();
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                use std::io::Read;
                let mut buf = [0u8; 64];
                let n = s.read(&mut buf).unwrap_or(0);
                let cmd = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                // 控制命令（供 `omaframe settings`、快捷键绑定、测试脚本用）
                // 语义要分清：show/hide 幂等，toggle 才是开关
                // （`omaframe settings` 发的是 "settings"，用开关语义）
                let action: Option<fn(&Rc<AppState>)> = match cmd.as_str() {
                    "settings" | "toggle" => Some(toggle),
                    "show" | "open" => Some(show),
                    "hide" | "close" => Some(|st| hide(st, "控制命令")),
                    "quit" | "exit" => Some(quit),
                    _ => None,
                };
                if let Some(action) = action {
                    let ctx = ctx.clone();
                    ctx.invoke(move || {
                        STATE.with(|slot| {
                            let st = slot.borrow().clone();
                            if let Some(st) = st.as_ref() {
                                action(st);
                            }
                        });
                    });
                } else if cmd == "quit" {
                    let ctx = ctx.clone();
                    ctx.invoke(|| {
                        adw::Application::default().quit();
                    });
                }
            }
        })
        .ok();
}

/// 紧凑样式 + 弹出面板外观（Adw 默认行高偏大，这里整体压缩）。
/// 根容器兼作模态压暗：既保证 surface 不是全透明（合成器才不会跳过输入），也有遮罩观感。
const PANEL_CSS: &str = "\
.panel-backdrop { background-color: rgba(0, 0, 0, 0.08); background-image: none; }\
window.omaframe-panel, window.omaframe-panel.background { background-color: transparent; background-image: none; box-shadow: none; border-style: none; }\
.panel-shell { background-color: @popover_bg_color; border: 1px solid @borders; border-radius: 12px; box-shadow: 0 6px 18px alpha(black, 0.35); margin: 8px; }\
preferences-page { background-color: transparent; }\
preferences-page > scrolledwindow > viewport { margin: 0; padding: 0; }\
preferences-group { margin-top: 4px; margin-bottom: 4px; }\
preferences-group > box { margin-top: 0; margin-bottom: 0; }\
preferences-group label.heading { font-size: 0.82em; font-weight: bold; margin-top: 0; margin-bottom: 0; }\
preferences-group label.description { font-size: 0.76em; margin-top: 0; margin-bottom: 2px; }\
row, row.entry, row.spin, row.switch, row.combo { min-height: 26px; padding-top: 0; padding-bottom: 0; }\
row label.title, row label.subtitle { margin-top: 0; margin-bottom: 0; }\
row label.title { font-size: 0.84em; }\
row label.subtitle { font-size: 0.72em; }\
entry, spinbutton, spinbutton button { min-height: 22px; font-size: 0.82em; }\
entry { padding-left: 6px; padding-right: 6px; }\
switch { min-height: 22px; min-width: 38px; }\
button.flat { min-height: 24px; min-width: 24px; padding: 0; }\
";

/// 全局样式只注册一次
/// （面板是"用完即销毁、再开重建"的，每次重建都注册会在显示服务上累积 provider）
fn apply_theme() {
    thread_local! { static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    let provider = gtk::CssProvider::new();
    provider.connect_parsing_error(|_, section, err| {
        crate::warn!("设置面板 CSS 解析错误 @{:?}: {err}", section.start_location());
    });
    provider.load_from_data(PANEL_CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn build(state: &Rc<AppState>) -> Panel {
    apply_theme();

    // ---------- 面板：一个铺满"顶栏以下"的 overlay 窗口 ----------
    let (mw, mh) = monitor_size(state);
    // 面板卡片高度自适应：内容更高就滚动，屏幕小就跟着变小（不写死，兼容各种分辨率）
    let panel_h = (mh - 22 - PANEL_GAP * 2).clamp(320, PANEL_H_MAX);
    let win = adw::ApplicationWindow::builder()
        .title("桌面相框设置")
        .build();
    win.set_decorated(false);
    win.set_resizable(false);
    win.add_css_class("omaframe-panel");
    win.set_default_size(mw, mh);
    // 窗口本身也要有最小尺寸：GTK 会按内容的自然尺寸缩小窗口，只有 default_size 不够
    win.set_size_request(mw, mh);
    win.set_visible(false);
    win.init_layer_shell();
    win.set_layer(Layer::Overlay);
    win.set_namespace(Some("omaframe-settings"));
    // 独占键盘：Esc 与输入框都能用；隐藏后 surface 不映射，不会抢键盘
    win.set_keyboard_mode(KeyboardMode::Exclusive);
    win.set_exclusive_zone(0);
    // 左上锚定 + 显式尺寸（不做四边拉伸，否则 GTK 收不到 configure → 收不到鼠标事件）
    win.set_anchor(Edge::Top, true);
    win.set_anchor(Edge::Left, true);

    // 根容器 = 整片区域（很淡的模态压暗）；面板卡片对齐右上角
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("panel-backdrop");
    root.set_size_request(mw, mh);
    root.set_hexpand(true);
    root.set_vexpand(true);

    let page = adw::PreferencesPage::new();
    page.set_title("相框");
    page.set_icon_name(Some("preferences-desktop-display-symbolic"));

    // ---------------- 媒体 ----------------
    let g_media = adw::PreferencesGroup::builder()
        .title("媒体")
        .description("本地媒体目录（递归扫描子目录）")
        .build();

    // 闭包里只用弱引用拿窗口（否则 win 会被 move 进去）
    let win_weak: glib::WeakRef<adw::ApplicationWindow> = glib::WeakRef::new();
    win_weak.set(Some(&win));

    // ---------------- 素材目录（方案 A：一行 + 折叠列表）----------------
    // 收起时只占 1 行（副标题显示摘要），展开后逐目录列出 + 添加 / 重新扫描。
    // 这样目录再多也不会把面板撑高 —— 这是选方案 A 的原因。
    let dirs: Vec<String> = state.config.borrow().source.paths.clone();
    let dir_exp = adw::ExpanderRow::builder().title("素材目录").build();
    dir_exp.set_subtitle(&dir_summary(&dirs));
    for d in &dirs {
        let row = adw::ActionRow::builder().title(d.as_str()).build();
        // 目录不存在/未挂载 → 置灰并标注，但**配置保留**（NAS 掉线不该丢配置）
        if !std::path::Path::new(d).is_dir() {
            row.add_css_class("dim-label");
            row.set_subtitle("不可用（未挂载或已删除？）");
        }
        let btn = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .valign(gtk::Align::Center)
            .tooltip_text("移除该目录")
            .build();
        {
            let st = state.clone();
            let p = d.clone();
            btn.connect_clicked(move |_| {
                st.update(|c| c.source.paths.retain(|x| *x != p));
                refresh_media(&st);
                // 面板每次打开都重建（有意设计）→ 这里直接重建以刷新列表
                show(&st);
            });
        }
        row.add_suffix(&btn);
        dir_exp.add_row(&row);
    }
    {
        let add = adw::ActionRow::builder()
            .title("添加目录…")
            .subtitle("选择后会先统计素材数量，过多会先提示")
            .build();
        add.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
        add.set_activatable(true);
        let st = state.clone();
        add.connect_activated(move |_| pick_dir(&st));
        dir_exp.add_row(&add);
    }
    {
        // 手动输入路径（保留原来 EntryRow 的能力）
        let entry = adw::EntryRow::builder().title("或手动输入路径后回车").build();
        let st = state.clone();
        entry.connect_apply(move |r| {
            let p = r.text().trim().to_string();
            if !p.is_empty() {
                add_dir(&st, p);
            }
        });
        dir_exp.add_row(&entry);
    }
    {
        // 手动加过文件、或目录 mtime 没变时用
        let rescan = adw::ActionRow::builder().title("重新扫描素材").build();
        rescan.add_prefix(&gtk::Image::from_icon_name("view-refresh-symbolic"));
        rescan.set_activatable(true);
        let st = state.clone();
        rescan.connect_activated(move |_| {
            // **先删缓存**再扫：否则 TTL 内会命中缓存，用户会以为"点了没刷新"
            crate::media::source::local::invalidate_scan_cache();
            refresh_media(&st);
        });
        dir_exp.add_row(&rescan);
    }
    g_media.add(&dir_exp);

    // ---------------- 显示 ----------------
    let g_display = adw::PreferencesGroup::builder()
        .title("尺寸与位置")
        .description("宽高是“保持比例的最大允许尺寸”，不会拉伸变形")
        .build();

    let max_w = spin_row(
        "最大宽度",
        160,
        4000,
        10,
        state.clone(),
        |c| c.display.max_width,
        |c, v| c.display.max_width = v,
    );
    let max_h = spin_row(
        "最大高度",
        120,
        4000,
        10,
        state.clone(),
        |c| c.display.max_height,
        |c, v| c.display.max_height = v,
    );
    g_display.add(&max_w);
    g_display.add(&max_h);

    // ---------------- 轮换 ----------------
    let g_slide = adw::PreferencesGroup::builder()
        .title("轮换与视频")
        .build();
    g_slide.add(&switch_row(
        "启用",
        state.clone(),
        |c| c.slideshow.enabled,
        |c, v| c.slideshow.enabled = v,
    ));
    g_slide.add(&spin_row(
        "间隔（秒）",
        1,
        86400,
        1,
        state.clone(),
        |c| c.slideshow.interval as i32,
        |c, v| c.slideshow.interval = v as u32,
    ));
    g_slide.add(&switch_row(
        "随机",
        state.clone(),
        |c| c.slideshow.random,
        |c, v| c.slideshow.random = v,
    ));

    // ---------------- 视频 ----------------
    // 「视频」与「轮换」同属"怎么播" → **复用同一个 GObject** 合成一组
    // （clone 是引用计数而非深拷贝；行仍按创建顺序追加：轮换的行在前，视频的行在后）
    let g_video = g_slide.clone();
    g_video.add(&switch_row(
        "自动播放",
        state.clone(),
        |c| c.video.autoplay,
        |c, v| c.video.autoplay = v,
    ));
    g_video.add(&switch_row(
        "静音",
        state.clone(),
        |c| c.video.muted,
        |c, v| c.video.muted = v,
    ));
    g_video.add(&spin_row(
        "帧率上限",
        1,
        60,
        1,
        state.clone(),
        |c| c.video.max_fps,
        |c, v| c.video.max_fps = v,
    ));

    // ---------------- 相框 ----------------
    let g_frame = adw::PreferencesGroup::builder()
        .title("相框")
        .build();
    // 「启用」放本组第一行：它是相框总开关，应最先看到
    g_frame.add(&switch_row(
        "启用",
        state.clone(),
        |c| c.frame.enabled,
        |c, v| c.frame.enabled = v,
    ));
    // 自适应随机推荐：开启后按素材方向在「横/竖/方」相框里随机挑，
    // 此时"相框样式"由程序决定 → 该行置灰
    let auto_row = switch_row(
        "自适应随机推荐",
        state.clone(),
        |c| c.frame.auto_style,
        |c, v| c.frame.auto_style = v,
    );
    g_frame.add(&auto_row);
    // 相框样式：读取程序目录 frame/ 下的所有 PNG
    {
        let styles = crate::config::list_frame_styles();
        let cur = state.config.borrow().frame.style.trim().to_string();
        let labels: Vec<String> = if styles.is_empty() {
            vec!["（未找到相框库）".to_string()]
        } else {
            styles
                .iter()
                .map(|n| n.trim_end_matches(".png").to_string())
                .collect()
        };
        let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
        let combo = adw::ComboRow::builder()
            .title("相框样式")
            .subtitle({
                if styles.is_empty() {
                    "程序目录 frame/ 下没有 PNG".to_string()
                } else {
                    // 不显示相框库路径：那是实现细节（frame/ 可能来自源码目录或
                    // 可执行文件同级），对用户没有决策价值，只占地方。
                    format!("内置相框库 {} 个", styles.len())
                }
            })
            .model(&gtk::StringList::new(&label_refs))
            .build();
        if let Some(i) = styles.iter().position(|n| *n == cur) {
            combo.set_selected(i as u32);
        } else if !styles.is_empty() {
            combo.set_selected(0);
            // 配置里的样式已失效 → 落到第一个可用样式
            let first = styles[0].clone();
            state.update(|c| c.frame.style = first);
        }
        let st = state.clone();
        let names = styles.clone();
        combo.connect_selected_notify(move |row| {
            let i = row.selected() as usize;
            if let Some(name) = names.get(i) {
                let name = name.clone();
                st.update(|c| c.frame.style = name);
                if let Some(p) = st.player.borrow().as_ref() {
                    p.load_frame();
                    p.apply_zoom();
                }
            }
        });
        // auto_style 开 → 样式由程序决定，禁用下拉
        let sync_combo = {
            let combo = combo.clone();
            let st = state.clone();
            move || {
                let auto = st.config.borrow().frame.auto_style;
                combo.set_sensitive(!auto);
                let sub = if auto {
                    "已开启「自适应随机推荐」，由程序按素材方向自动挑选".to_string()
                } else if styles.is_empty() {
                    "程序目录 frame/ 下没有 PNG".to_string()
                } else {
                    format!("内置相框库 {} 个", styles.len())
                };
                combo.set_subtitle(&sub);
            }
        };
        sync_combo();
        auto_row.connect_active_notify({
            let sync_combo = sync_combo.clone();
            move |_| {
                sync_combo();
            }
        });
        g_frame.add(&combo);
    }

    // ---------------- 常规 ----------------
    let g_general = adw::PreferencesGroup::builder().title("常规").build();
    let st_auto = state.clone();
    let auto_start_row = adw::SwitchRow::builder()
        .title("开机启动")
        .subtitle("登录后自动运行（写入 ~/.config/autostart）")
        .active(state.config.borrow().autostart)
        .build();
    auto_start_row.connect_active_notify(move |r| {
        let v = r.is_active();
        st_auto.update(|c| c.autostart = v);
        // 立即落盘/删除 autostart 项（不必等重启）
        if let Err(e) = crate::config::sync_autostart(v) {
            crate::warn!("同步开机启动失败：{e}");
        }
        crate::info!("开机启动 → {v}");
    });
    g_general.add(&auto_start_row);
    // 顺序：媒体 → 显示 → 自动轮换 → 视频 → 相框 → 常规
    // （「相框」是用户最常调的，放前面；「常规」放最后）
    // ---------------- 转场 ----------------
    // 放在「自动轮换」之后：转场是切换时的呈现效果，与轮换相邻最直观。
    {
        let g_tr = adw::PreferencesGroup::builder()
            .title("转场")
            .description("切换素材时的过渡效果")
            .build();

        // 总开关：关掉后不转场（效果/随机/时长三行一并置灰）
        let enable_row = switch_row(
            "启用转场",
            state.clone(),
            |c| c.transition.enabled,
            |c, v| c.transition.enabled = v,
        );
        g_tr.add(&enable_row);

        // 转场效果下拉
        let labels: Vec<&str> = crate::config::TRANSITION_EFFECTS.iter().map(|(_, n)| *n).collect();
        let keys: Vec<&str> = crate::config::TRANSITION_EFFECTS.iter().map(|(k, _)| *k).collect();
        let cur = state.config.borrow().transition.effect.clone();
        let combo = adw::ComboRow::builder()
            .title("转场效果")
            .model(&gtk::StringList::new(&labels))
            .build();
        if let Some(i) = keys.iter().position(|k| *k == cur) {
            combo.set_selected(i as u32);
        }
        g_tr.add(&combo);

        // 随机转场：开启则每次切换随机挑一种 → 效果下拉置灰
        let random_row = switch_row(
            "随机转场",
            state.clone(),
            |c| c.transition.random,
            |c, v| c.transition.random = v,
        );
        g_tr.add(&random_row);

        // 时长（不叠加到轮换间隔：转场只在切换瞬间播放）
        // 上限与 sanitize 共用 config::transition_max_ms → 上限只有一个来源，
        // 不会出现"设置页能调到 3000、但保存时被 sanitize 砍回 1000"的错位。
        let dur_cap = crate::config::transition_max_ms(&state.config.borrow());
        let dur_row = spin_row(
            "时长（毫秒）",
            200,
            dur_cap,
            100,
            state.clone(),
            |c| c.transition.duration_ms as i32,
            |c, v| c.transition.duration_ms = v.max(0) as u32,
        );
        dur_row.set_subtitle("不超过轮换间隔");
        g_tr.add(&dur_row);

        // 视频是否也应用转场（关掉即回到旧行为：视频不转场）
        let vid_row = switch_row(
            "视频也应用转场",
            state.clone(),
            |c| c.transition.apply_to_video,
            |c, v| c.transition.apply_to_video = v,
        );
        vid_row.set_subtitle("图片↔视频、视频↔视频 切换时同样播放转场；关闭则视频直接切换");
        g_tr.add(&vid_row);

        // 联动：
        //   总开关关 → 效果/随机/时长 三行全部置灰
        //   随机开   → 效果下拉置灰并提示（时长仍可调）
        let sync = {
            let combo = combo.clone();
            let random_row = random_row.clone();
            let dur_row = dur_row.clone();
            let st = state.clone();
            move || {
                let (en, random) = {
                    let c = st.config.borrow();
                    (c.transition.enabled, c.transition.random)
                };
                random_row.set_sensitive(en);
                dur_row.set_sensitive(en);
                combo.set_sensitive(en && !random);
                let sub = if !en {
                    "已关闭转场".to_string()
                } else if random {
                    "已开启「随机转场」，每次切换由程序随机挑选".to_string()
                } else {
                    String::new()
                };
                combo.set_subtitle(&sub);
            }
        };
        sync();
        random_row.connect_active_notify({
            let sync = sync.clone();
            move |_| sync()
        });
        enable_row.connect_active_notify({
            let sync = sync.clone();
            move |_| sync()
        });

        // 选中即写配置。转场参数是**切换那一刻**才读取的，所以改完不需要
        // apply_settings()（那会连带 show_current + load_frame，属于过重的副作用）。
        let st = state.clone();
        let names = keys.iter().map(|k| k.to_string()).collect::<Vec<_>>();
        combo.connect_selected_notify(move |row| {
            let i = row.selected() as usize;
            if let Some(k) = names.get(i) {
                let k = k.clone();
                st.update(|c| c.transition.effect = k);
            }
        });

        page.add(&g_media);
        page.add(&g_slide);
        page.add(&g_tr);
    }

    // ---------------- 分组顺序 ----------------
    // 媒体 → 轮换 → 视频 → 转场 → 相框 → 尺寸 → 桌面位置 → 常规 →(链接 / 退出)
    //
    // 三条原则：
    //  1) **同主题相邻**：尺寸上限(g_display) 与 桌面位置(g_pos) 都属于"相框怎么摆"，
    //     原来中间隔着"常规"，现在贴在一起；
    //  2) **流程自上而下**：先选素材 → 定怎么换（轮换/视频/转场）→ 定外框 → 定摆放；
    //  3) **应用级项沉底**：开机启动 / 项目主页 / 退出 与内容无关，统一放最下面。
    // 转场(g_tr) 在它自己的作用域里 add（见上），顺序天然落在"视频"之后。
    page.add(&g_frame);
    page.add(&g_display);

    // ---------------- 位置 ----------------
    // 「尺寸」与「桌面位置」同属"相框怎么摆" → **复用同一个 GObject** 合成一组
    // （clone 是 GObject 引用计数，不是深拷贝），这样只需改动两行、不用搬任何行的创建代码。
    let g_pos = g_display.clone();
    // 桌面显示：显示/隐藏桌面上的相框（媒体与设置照常工作）
    {
        let row = adw::SwitchRow::new();
        row.set_title("在桌面显示相框");
        row.set_subtitle("关闭后相框从桌面隐藏，媒体与设置不受影响");
        row.set_active(state.config.borrow().frame.desktop_enabled);
        let st = state.clone();
        row.connect_active_notify(move |r| {
            let v = r.is_active();
            st.update(|c| c.frame.desktop_enabled = v);
            if let Some(p) = st.player.borrow().as_ref() {
                p.apply_desktop_visible();
            }
        });
        g_pos.add(&row);
    }
    page.add(&g_general);

    // ---------------- 项目链接 ----------------
    // 放在设置页最下方，点击用系统默认浏览器打开项目仓库
    // 底部两项（项目主页 / 退出）**合成一组**：原来是无标题的两个独立卡片，
    // 视觉上很碎；合起来也更符合"关于与退出"这一主题。
    let g_bottom = adw::PreferencesGroup::builder().build();
    {
        let row = adw::ActionRow::builder()
            .title("项目主页")
            .subtitle(env!("CARGO_PKG_REPOSITORY"))
            .activatable(true)
            .build();
        let url = env!("CARGO_PKG_REPOSITORY").to_string();
        row.add_suffix(&gtk::Image::from_icon_name("emblem-system-symbolic"));
        row.connect_activated(move |_| {
            crate::open_url(&url);
        });
        g_bottom.add(&row);
    }

    // ---------------- 退出 ----------------
    // 放在设置页最下方：破坏性操作放最后、最不显眼，避免误点。
    // 用 ActionRow + Button（不用 adw::ButtonRow，那个要 libadwaita 1.5 特性，
    // 会把构建门槛从 1.4 抬到 1.5）。
    {
        let row = adw::ActionRow::builder()
            .title("退出")
            .subtitle("关闭桌面相框（当前配置会先保存）")
            .build();
        let btn = gtk::Button::with_label("退出");
        btn.add_css_class("destructive-action");
        // 整行可点（等价于点按钮）
        row.set_activatable_widget(Some(&btn));
        row.add_suffix(&btn);
        let st = state.clone();
        btn.connect_clicked(move |_| {
            crate::info!("设置页点击「退出」");
            quit(&st);
        });
        g_bottom.add(&row);
        page.add(&g_bottom);
    }
    let anchor = adw::ComboRow::builder()
        .title("默认位置")
        .model(&gtk::StringList::new(&[
            "左上角", "右上角", "左下角", "右下角", "居中",
        ]))
        .build();
    {
        let st = state.clone();
        let row = anchor.clone();
        let cur = match state.config.borrow().window.default_anchor.as_str() {
            "top-right" => 1,
            "bottom-left" => 2,
            "bottom-right" => 3,
            "center" => 4,
            _ => 0,
        };
        row.set_selected(cur);
        row.connect_selected_notify(move |row| {
            let v = match row.selected() {
                1 => "top-right",
                2 => "bottom-left",
                3 => "bottom-right",
                4 => "center",
                _ => "top-left",
            };
            st.update(|c| c.window.default_anchor = v.into());
            // 立即生效：把相框挪到该停靠位置（而不是"下次启动才生效"）
            if let Some(pl) = st.player.borrow().as_ref() {
                pl.apply_anchor();
            }
        });
    }
    g_pos.add(&anchor);
    // 边距：改了要立刻重新贴靠（只写配置是看不出效果的）
    {
        let adj = gtk::Adjustment::new(
            state.config.borrow().window.margin as f64,
            0.0,
            512.0,
            4.0,
            16.0,
            0.0,
        );
        let row = adw::SpinRow::builder().title("边距").adjustment(&adj).build();
        let st = state.clone();
        adj.connect_value_changed(move |a| {
            let v = a.value() as i32;
            st.update(|c| c.window.margin = v);
            if let Some(p) = st.player.borrow().as_ref() {
                p.apply_anchor();
            }
        });
        g_pos.add(&row);
    }
    // 调试浮层：运行时即时显隐（不再依赖环境变量）
    {
        let row = adw::SwitchRow::new();
        row.set_title("调试浮层");
        row.set_subtitle("在相框左上角显示后端/尺寸/位置");
        row.set_active(state.config.borrow().frame.debug_hud);
        let st = state.clone();
        row.connect_active_notify(move |r| {
            let v = r.is_active();
            // 即时保存：重启后自动恢复
            st.update(|c| c.frame.debug_hud = v);
            if let Some(w) = st.window() {
                w.set_debug(v);
                w.update_hud(&st, "");
            }
        });
        g_pos.add(&row);
    }

    // 面板卡片：圆角 + 背景 + 滚动
    let shell = gtk::Box::new(gtk::Orientation::Vertical, 0);
    shell.add_css_class("panel-shell");
    shell.set_size_request(PANEL_W, panel_h);
    shell.set_halign(gtk::Align::End);
    shell.set_valign(gtk::Align::Start);
    shell.set_margin_end(PANEL_GAP);
    shell.set_margin_top(PANEL_GAP);
    shell.set_vexpand(false);
    // 关键：**必须自己包一层 ScrolledWindow**。AdwPreferencesPage 单用时会把内容的
    // 自然高度（≈950px）报给窗口，窗口只有 720 → 直接被裁掉且滚不动，
    // "相框样式/默认位置"这些在下面的行根本够不到。
    // propagate_natural_height(false) 让 scrolledwindow 不把内容高度传上去，
    // 窗口才能保持 720 并由它自己滚动。
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(false)
        .vexpand(true)
        .child(&page)
        .build();
    scroller.set_size_request(PANEL_W, -1);
    shell.append(&scroller);
    root.append(&shell);
    win.set_content(Some(&root));
    // 点卡片外 → 收起（坐标判断，避免"点卡片内空白处也关"）。
    // 用 capture 阶段：先做"外面就关"的判断，再让卡片里的控件正常处理自己的点击。
    {
        let st = state.clone();
        let shell_ref = shell.clone();
        let press = gtk::GestureClick::new();
        press.set_propagation_phase(gtk::PropagationPhase::Capture);
        press.connect_pressed(move |gesture, _, x, y| {
            let Some(widget) = gesture.widget() else {
                return;
            };
            let inside = shell_ref
                .compute_bounds(&widget)
                .map(|b| {
                    x >= b.x() as f64
                        && y >= b.y() as f64
                        && x <= (b.x() + b.width()) as f64
                        && y <= (b.y() + b.height()) as f64
                })
                .unwrap_or(false);
            if !inside {
                hide(&st, "点面板外");
            }
        });
        root.add_controller(press);
    }

    // 关闭请求（Esc 走 close()）→ 隐藏并释放
    {
        let st = state.clone();
        win.connect_close_request(move |_| {
            hide(&st, "close（Esc/外部关闭请求）");
            glib::Propagation::Stop
        });
    }

    // 注意：这里**不要**用 `is_active_notify` 做"失去焦点就隐藏"。
    // 面板里的下拉框（AdwComboRow）打开时是 GTK 弹窗，会让 toplevel 的 is_active 变 false，
    // 于是"点下拉 → 面板立刻消失"（用户报的 bug）。
    // 焦点变化统一走 Hyprland IPC 事件（见 hypr::on_focus_change），并按负载去重。

    // Esc 关闭设置页。
    // 用 **capture 阶段**的 EventControllerKey：按键会先送到窗口，再到焦点控件，
    // 因此焦点落在 Entry（目录 / PNG 路径输入框）里时，Esc 也不会被输入框吞掉。
    {
        let weak: glib::WeakRef<adw::ApplicationWindow> = glib::WeakRef::new();
        weak.set(Some(&win));
        let ctrl = gtk::EventControllerKey::new();
        ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
        win.add_controller(ctrl.clone());
        ctrl.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(w) = weak.upgrade() {
                    w.close();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });

        // 兜底：bubble 阶段再来一次（个别控件仍可能吃掉 key 事件）
        let ctrl2 = gtk::EventControllerKey::new();
        win.add_controller(ctrl2.clone());
        let weak2: glib::WeakRef<adw::ApplicationWindow> = glib::WeakRef::new();
        weak2.set(Some(&win));
        ctrl2.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(w) = weak2.upgrade() {
                    w.close();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }

    Panel { win }
}

/// 媒体目录变化后刷新：重建媒体源 → 重新扫描 → 展示窗立刻显示新目录的第一项
fn refresh_media(state: &Rc<AppState>) {
    if let Some(p) = state.player.borrow().as_ref() {
        p.reload_source();
    }
}

fn switch_row(
    title: &str,
    state: Rc<AppState>,
    get: fn(&crate::config::Config) -> bool,
    set: fn(&mut crate::config::Config, bool),
) -> adw::SwitchRow {
    let row = adw::SwitchRow::new();
    row.set_title(title);
    row.set_active(get(&state.config.borrow()));
    let st = state.clone();
    row.connect_active_notify(move |r| {
        let v = r.is_active();
        st.update(|c| set(c, v));
        if let Some(p) = st.player.borrow().as_ref() {
            p.apply_settings();
        }
    });
    row
}

fn spin_row(
    title: &str,
    min: i32,
    max: i32,
    step: i32,
    state: Rc<AppState>,
    get: fn(&crate::config::Config) -> i32,
    set: fn(&mut crate::config::Config, i32),
) -> adw::SpinRow {
    let init = get(&state.config.borrow()).clamp(min, max) as f64;
    let adj = gtk::Adjustment::new(
        init,
        min as f64,
        max as f64,
        step as f64,
        10.0,
        0.0,
    );
    // 用 builder 设置标题（SpinRow::new 不带标题）
    let row = adw::SpinRow::builder()
        .title(title)
        .adjustment(&adj)
        .build();
    let st = state.clone();
    // 监听 adjustment 的值变化（SpinRow 的同名信号与 gtk 侧方法会撞名，直接用 Adjustment 更稳）
    adj.connect_value_changed(move |a| {
        let v = a.value() as i32;
        st.update(|c| set(c, v));
        if let Some(p) = st.player.borrow().as_ref() {
            p.apply_settings();
        }
    });
    row
}

/// 超过这个数量就先提示用户"扫描需要等待"（避免大目录像卡死）
const BIG_DIR_WARN: usize = 500;

thread_local! {
    /// 「添加目录」后台统计完成后的回调需要回到主线程取 AppState。
    /// `MainContext::invoke` 的闭包必须 `Send`，而 `Rc<AppState>` 不是 Send，
    /// 所以不捕获它 —— 在主线程的 thread_local 里取（与 library.rs 同一手法）。
    static ADD_STATE: std::cell::RefCell<Option<Rc<AppState>>> =
        const { std::cell::RefCell::new(None) };
}

/// 「素材目录」副标题摘要：1 个显示路径，多个显示个数（收起时也只占一行）
fn dir_summary(dirs: &[String]) -> String {
    match dirs.len() {
        0 => "未设置".to_string(),
        1 => dirs[0].clone(),
        n => format!("{n} 个目录"),
    }
}

/// 选目录 → 交给 add_dir。
/// 对话框必须**独立开**（不能把设置面板当父窗口）：面板是 layer-shell surface，
/// 设为 transient parent 会触发协议错误被合成器踢掉；而且面板在 Overlay 层会挡住
/// 普通 toplevel 对话框。所以先收起面板 → 无父窗口打开 → 结束后把面板放回来。
fn pick_dir(state: &Rc<AppState>) {
    hide(state, "打开目录对话框");
    let st = state.clone();
    let dialog = gtk::FileDialog::builder().title("选择媒体目录").build();
    // 必须 select_folder（选目录）；open() 是选文件 → 会选到文件导致扫描失败
    dialog.select_folder(
        None::<&gtk::Window>,
        gio::Cancellable::NONE,
        move |res| {
            if let Ok(folder) = res {
                if let Some(p) = folder.path() {
                    add_dir(&st, p.to_string_lossy().into_owned());
                    return; // add_dir 内部会重建面板
                }
            }
            show(&st);
        },
    );
}

/// 添加一个目录：先去重，再**后台统计数量**（大目录的 walk 绝不能放主线程，
/// 否则面板会卡死几秒）→ 超过阈值先弹提示给用户预期，否则直接加入。
fn add_dir(state: &Rc<AppState>, path: String) {
    let p = crate::config::expand_user(path.trim())
        .to_string_lossy()
        .into_owned();
    if p.is_empty() {
        show(state);
        return;
    }
    if state.config.borrow().source.paths.iter().any(|d| *d == p) {
        crate::info!("目录已在列表中，忽略重复添加：{p}");
        show(state);
        return;
    }
    let recursive = state.config.borrow().source.recursive;
    ADD_STATE.with(|s| *s.borrow_mut() = Some(state.clone()));
    let ctx = glib::MainContext::default();
    let p2 = p.clone();
    std::thread::Builder::new()
        .name("omaframe-count".into())
        .spawn(move || {
            let n = crate::media::source::local::count_media(std::path::Path::new(&p2), recursive);
            ctx.invoke(move || {
                let st = ADD_STATE.with(|s| s.borrow().clone());
                let Some(st) = st else {
                    crate::warn!("添加目录：主线程状态缺失，跳过");
                    return;
                };
                if n > BIG_DIR_WARN {
                    crate::info!("目录 {p2} 含 {n} 个素材，先提示用户");
                    hide(&st, "素材过多提示");
                    let dlg = adw::MessageDialog::builder()
                        .heading("素材较多")
                        .body(format!(
                            "该目录包含 {n} 个素材。\n\n首次扫描需要一些时间（之后会缓存，重启不再全量重扫）。\n仍要添加吗？"
                        ))
                        .build();
                    dlg.add_response("cancel", "取消");
                    dlg.add_response("add", "添加");
                    dlg.set_response_appearance("add", adw::ResponseAppearance::Suggested);
                    let st2 = st.clone();
                    let p3 = p2.clone();
                    // 第一个参数是"只监听某个 detail"的过滤器，None = 监听全部
                    dlg.connect_response(None, move |d: &adw::MessageDialog, r: &str| {
                        d.close();
                        if r == "add" {
                            commit_dir(&st2, &p3);
                        }
                        show(&st2);
                    });
                    dlg.present();
                } else {
                    commit_dir(&st, &p2);
                }
            });
        })
        .ok();
}

/// 真正写入配置 + 重扫 + 重建面板（列表随之刷新）
fn commit_dir(state: &Rc<AppState>, p: &str) {
    let p = p.to_string();
    state.update(|c| {
        if !c.source.paths.iter().any(|d| *d == p) {
            c.source.paths.push(p.clone());
        }
    });
    refresh_media(state);
    show(state);
}
