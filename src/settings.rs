//! 设置窗口：libadwaita 原生风格，改动即时生效并自动保存。
//!
//! - `photo-frame settings` → 若已有实例在跑，通过 `$XDG_RUNTIME_DIR` 下的
//!   Unix socket 通知它开设置窗；否则直接以设置窗模式启动。

use crate::app::AppState;
use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// 通知运行中的实例打开设置窗口；成功返回 true
pub fn request_open() -> bool {
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
            let _ = s.write_all(b"settings\n");
            true
        }
        Err(_) => false,
    }
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
        .name("photo-frame-ctl".into())
        .spawn(move || {
            let ctx = glib::MainContext::default();
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                use std::io::Read;
                let mut buf = [0u8; 64];
                let n = s.read(&mut buf).unwrap_or(0);
                let cmd = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                if cmd == "settings" {
                    let ctx = ctx.clone();
                    ctx.invoke(|| {
                        STATE.with(|slot| {
                            let st = slot.borrow().clone();
                            let Some(st) = st.as_ref() else {
                                return;
                            };
                            if st.settings_window.borrow().is_none() {
                                let w = build(st);
                                w.present();
                                *st.settings_window.borrow_mut() = Some(w);
                            } else if let Some(w) = st.settings_window.borrow().as_ref() {
                                w.present();
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

/// 紧凑样式：Adw 默认行高偏大，这里整体压缩
const COMPACT_CSS: &str = "\
window.photo-frame-settings { background-color: @theme_bg_color; }\
preferences-page { background-color: transparent; }\
preferences-page > scrolledwindow > viewport { margin: 0; padding: 0; }\
preferences-group { margin-top: 6px; margin-bottom: 6px; }\
preferences-group > box { margin-top: 0; margin-bottom: 0; }\
preferences-group label.heading { font-size: 0.86em; font-weight: bold; margin-top: 2px; margin-bottom: 1px; }\
preferences-group label.description { font-size: 0.76em; margin-top: 0; margin-bottom: 2px; }\
row, row.entry, row.spin, row.switch, row.combo { min-height: 30px; padding-top: 0; padding-bottom: 0; }\
row label.title, row label.subtitle { margin-top: 0; margin-bottom: 0; }\
row label.title { font-size: 0.88em; }\
row label.subtitle { font-size: 0.76em; }\
entry, spinbutton, spinbutton button { min-height: 24px; font-size: 0.85em; }\
entry { padding-left: 6px; padding-right: 6px; }\
switch { min-height: 24px; min-width: 42px; }\
button.flat { min-height: 24px; min-width: 24px; padding: 0; }\
";

fn apply_compact(win: &adw::ApplicationWindow) {
    win.add_css_class("photo-frame-settings");
    let provider = gtk::CssProvider::new();
    provider.load_from_data(COMPACT_CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

pub fn build(state: &Rc<AppState>) -> adw::ApplicationWindow {
    let win = adw::ApplicationWindow::builder()
        .title("桌面相框设置")
        .default_width(400)
        .default_height(560)
        .build();
    // 固定尺寸 + 不可缩放：既保证紧凑，也让合成器把它当对话框浮动
    // （否则会被当普通窗口平铺，看起来又大又难用）
    win.set_resizable(false);
    win.set_size_request(400, 560);
    apply_compact(&win);

    let page = adw::PreferencesPage::new();
    page.set_title("相框");
    page.set_icon_name(Some("preferences-desktop-display-symbolic"));
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
        let ww = win_weak.clone();
        dir_browse.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::builder()
                .title("选择媒体目录")
                .build();
            let st = st.clone();
            let row2 = row_in.clone();
            let parent = ww.upgrade();
            dialog.open(
                parent.as_ref(),
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
                },
            );
        });
    }
    dir_row.set_text(&state.config.borrow().source.path);
    dir_row.add_suffix(&dir_browse);
    g_media.add(&dir_row);
    page.add(&g_media);

    // ---------------- 显示 ----------------
    let g_display = adw::PreferencesGroup::builder()
        .title("显示")
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
    page.add(&g_display);

    // ---------------- 轮换 ----------------
    let g_slide = adw::PreferencesGroup::builder()
        .title("自动轮换")
        .description("图片按间隔轮换；视频默认播完整段再切换")
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
    page.add(&g_slide);

    // ---------------- 视频 ----------------
    let g_video = adw::PreferencesGroup::builder()
        .title("视频")
        .description("由系统 ffmpeg 解码（与系统动态壁纸同一套解码器）")
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
    page.add(&g_video);

    // ---------------- 相框 ----------------
    let g_frame = adw::PreferencesGroup::builder()
        .title("相框")
        .description("透明 PNG 叠在媒体之上")
        .build();
    g_frame.add(&switch_row(
        "启用",
        state.clone(),
        |c| c.frame.enabled,
        |c, v| c.frame.enabled = v,
    ));
    let frame_row = adw::EntryRow::builder().title("PNG 路径").build();
    frame_row.set_text(&state.config.borrow().frame.path);
    let frame_btn = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .valign(gtk::Align::Center)
        .build();
    {
        let st = state.clone();
        let row_in = frame_row.clone();
        let ww = win_weak.clone();
        frame_btn.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::builder()
                .title("选择相框 PNG")
                .build();
            let st = st.clone();
            let row2 = row_in.clone();
            let parent = ww.upgrade();
            dialog.open(
                parent.as_ref(),
                gio::Cancellable::NONE,
                move |res| {
                    if let Ok(f) = res {
                        if let Some(p) = f.path() {
                            let p = p.to_string_lossy().into_owned();
                            st.update(|c| c.frame.path = p.clone());
                            row2.set_text(&p);
                            if let Some(pl) = st.player.borrow().as_ref() {
                                pl.load_frame();
                            }
                        }
                    }
                },
            );
        });
    }
    frame_row.add_suffix(&frame_btn);
    g_frame.add(&frame_row);
    page.add(&g_frame);

    // ---------------- 位置 ----------------
    let g_pos = adw::PreferencesGroup::builder().title("位置与外观").build();
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
    g_pos.add(&spin_row(
        "边距",
        0,
        512,
        4,
        state.clone(),
        |c| c.window.margin,
        |c, v| c.window.margin = v,
    ));
    // 调试浮层：运行时即时显隐（不再依赖环境变量）
    {
        let row = adw::SwitchRow::new();
        row.set_title("调试浮层");
        row.set_subtitle("在相框左上角显示后端/尺寸/位置");
        row.set_active(
            state
                .window()
                .map(|w| w.has_debug())
                .unwrap_or(false),
        );
        let st = state.clone();
        row.connect_active_notify(move |r| {
            if let Some(w) = st.window() {
                w.set_debug(r.is_active());
                w.update_hud(&st, "");
            }
        });
        g_pos.add(&row);
    }
    page.add(&g_pos);

    win.set_content(Some(&page));
    win
}

fn refresh_media(state: &Rc<AppState>) {
    if let Some(p) = state.player.borrow().as_ref() {
        p.rescan();
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
