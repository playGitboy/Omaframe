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
        crate::debug!("设置面板 → 隐藏（{why}）");
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

    let dir_row = adw::EntryRow::builder().title("目录").build();
    let dir_browse = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .valign(gtk::Align::Center)
        .build();
    {
        let st = state.clone();
        let row_in = dir_row.clone();
        dir_browse.connect_clicked(move |_| {
            let st = st.clone();
            let row2 = row_in.clone();
            // 选目录对话框必须**独立开**，不能把设置面板当父窗口：
            // ① 面板是 layer-shell surface（不是 xdg_toplevel），把它设成对话框的
            //    transient parent 会触发协议错误 → 合成器直接踢掉我们的客户端
            //    （现象：点"打开"就闪退，Hyprland 日志报 "error in client communication"）。
            // ② 面板在 Overlay 层、盖满屏幕，会把普通 toplevel 的对话框整个挡住。
            // 所以：先收起面板 → 以无父窗口方式打开对话框 → 结束后再把面板放回来。
            hide(&st, "打开目录对话框");
            let dialog = gtk::FileDialog::builder()
                .title("选择媒体目录")
                .build();
            // 必须用 select_folder（**选目录**）；open() 是"选文件"，
            // 选到文件后 source.path 变成文件路径 → 扫描失败、目录内容不刷新
            dialog.select_folder(
                None::<&gtk::Window>,
                gio::Cancellable::NONE,
                move |res| {
                    if let Ok(folder) = res {
                        if let Some(p) = folder.path() {
                            let p = p.to_string_lossy().into_owned();
                            st.update(|c| c.source.path = p.clone());
                            row2.set_text(&p);
                            refresh_media(&st);
                        }
                    }
                    // 对话框关了就回到设置面板（配置已即时保存 ✓）
                    show(&st);
                },
            );
        });
    }
    dir_row.set_text(&state.config.borrow().source.path);
    dir_row.add_suffix(&dir_browse);
    {
        // 手动输入路径：回车（EntryRow 的 apply 信号）同样生效
        let st = state.clone();
        let row2 = dir_row.clone();
        dir_row.connect_apply(move |_| {
            let p = row2.text().trim().to_string();
            if p.is_empty() {
                return;
            }
            let p = crate::config::expand_user(&p).to_string_lossy().into_owned();
            st.update(|c| c.source.path = p.clone());
            row2.set_text(&p);
            refresh_media(&st);
        });
    }
    g_media.add(&dir_row);

    // ---------------- 显示 ----------------
    let g_display = adw::PreferencesGroup::builder()
        .title("尺寸")
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
        .title("轮换")
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
    let g_video = adw::PreferencesGroup::builder()
        .title("视频")
        .build();
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
        let dur_row = spin_row(
            "时长（毫秒）",
            200,
            3000,
            100,
            state.clone(),
            |c| c.transition.duration_ms as i32,
            |c, v| c.transition.duration_ms = v.max(0) as u32,
        );
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
        page.add(&g_video);
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
    let g_pos = adw::PreferencesGroup::builder().title("桌面位置").build();
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
    page.add(&g_pos);
    page.add(&g_general);

    // ---------------- 项目链接 ----------------
    // 放在设置页最下方，点击用系统默认浏览器打开项目仓库
    {
        let g_link = adw::PreferencesGroup::builder().build();
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
        g_link.add(&row);
        page.add(&g_link);
    }

    // ---------------- 退出 ----------------
    // 放在设置页最下方：破坏性操作放最后、最不显眼，避免误点。
    // 用 ActionRow + Button（不用 adw::ButtonRow，那个要 libadwaita 1.5 特性，
    // 会把构建门槛从 1.4 抬到 1.5）。
    {
        let g_quit = adw::PreferencesGroup::builder().build();
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
        g_quit.add(&row);
        page.add(&g_quit);
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
