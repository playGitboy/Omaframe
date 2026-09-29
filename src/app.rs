//! 应用装配：加载配置 → 选后端 → 建窗口 → 跑 GTK 主循环。

use crate::config::{Config, ConfigManager, APP_ID};
use crate::geometry;
use crate::window::frame_window::FrameWindow;
use crate::window::{detect_backend, monitor_bounds, target_monitor, Backend};
use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

pub struct AppState {
    pub manager: ConfigManager,
    pub config: RefCell<Config>,
    pub backend: Backend,
    pub window: RefCell<Option<Rc<FrameWindow>>>,
    pub player: RefCell<Option<Rc<crate::player::MediaPlayer>>>,
    /// Hyprland 覆盖检测（不可用时为 None）
    pub visibility: RefCell<Option<Rc<crate::hypr::VisibilityMonitor>>>,
    /// 设置窗口
    /// 设置**面板**（弹出式：点状态栏图标开、Esc/点外面关）
    pub settings_window: RefCell<Option<crate::settings::Panel>>,
    /// 托盘图标（无托盘服务时为 None）
    pub tray: RefCell<Option<Rc<crate::tray::Tray>>>,
    /// 配置是否刚刚被程序修改过（退出时需要再存一次）
    pub dirty: std::cell::Cell<bool>,
}

impl AppState {
    pub fn boot() -> Rc<Self> {
        let manager = ConfigManager::new();
        let loaded = manager.load();
        let mut config = loaded.config;
        let mut needs_save = loaded.fresh;

        // 1) 媒体目录：首次运行给一个能用的默认值
        if loaded.fresh || config.source.path.trim().is_empty() {
            let dir = crate::config::default_media_dir();
            if dir.is_dir() {
                config.source.path = dir.to_string_lossy().into_owned();
            }
        }

        // 2) 显示器 + 位置：首次运行按 default_anchor（默认右上角）计算
        let connector = if config.window.monitor.trim().is_empty() {
            target_monitor("")
                .and_then(|m| m.connector().map(|c| c.to_string()))
                .unwrap_or_default()
        } else {
            config.window.monitor.clone()
        };
        let monitor = target_monitor(&connector);
        let bounds = monitor
            .as_ref()
            .map(monitor_bounds)
            .unwrap_or(geometry::Bounds {
                width: 1920,
                height: 1080,
            });
        if let Some(m) = monitor.as_ref() {
            config.window.monitor = m.connector().map(|c| c.to_string()).unwrap_or(connector);
            crate::debug!(
                "显示器 {} scale_factor={} 解码倍数={:.1}",
                config.window.monitor,
                m.scale_factor(),
                crate::window::monitor_scale(m)
            );
        }

        if !config.window.placed {
            let (x, y) = geometry::anchor_pos(
                &config.window.default_anchor,
                bounds,
                config.window.width,
                config.window.height,
                config.window.margin,
            );
            config.window.x = x;
            config.window.y = y;
            config.window.placed = true;
            needs_save = true;
        } else {
            // 显示器分辨率/布局变化后把位置夹回可见范围
            let (x, y) = geometry::clamp_to_screen(
                config.window.x,
                config.window.y,
                config.window.width,
                config.window.height,
                bounds,
            );
            if (x, y) != (config.window.x, config.window.y) {
                config.window.x = x;
                config.window.y = y;
                needs_save = true;
            }
        }

        config.sanitize();
        // load() 内部已 sanitize 过；这里再兜一次（播放/窗口相关字段）
        let migrated = loaded.migrated;
        if migrated {
            needs_save = true;
        }
        let backend = detect_backend();

        if migrated {
            // 立即落盘（不等退出，避免被 kill 时丢失迁移结果）
            if let Err(e) = manager.save(&config) {
                crate::warn!("迁移后保存配置失败：{e}");
            } else {
                crate::info!("配置已自动迁移并保存（旧 path → 内置相框库）");
            }
        }

        let state = Rc::new(Self {
            manager,
            config: RefCell::new(config),
            backend,
            window: RefCell::new(None),
            player: RefCell::new(None),
            visibility: RefCell::new(None),
            settings_window: RefCell::new(None),
            tray: RefCell::new(None),
            dirty: std::cell::Cell::new(needs_save),
        });

        crate::info!(
            "启动：后端={} 显示器={} 位置=({},{}) 尺寸={}x{} 媒体目录={}",
            state.backend.as_str(),
            state.config.borrow().window.monitor,
            state.config.borrow().window.x,
            state.config.borrow().window.y,
            state.config.borrow().window.width,
            state.config.borrow().window.height,
            state.config.borrow().source.path
        );
        state
    }

    /// 改配置并落盘。拖拽过程中请用 `edit` + `commit`，避免频繁写盘。
    pub fn update<F: FnOnce(&mut Config)>(&self, f: F) {
        {
            let mut cfg = self.config.borrow_mut();
            f(&mut cfg);
            cfg.sanitize();
        }
        self.commit();
    }

    /// 只改内存（拖拽中每帧调用）
    pub fn edit<F: FnOnce(&mut Config)>(&self, f: F) {
        let mut cfg = self.config.borrow_mut();
        f(&mut cfg);
        cfg.sanitize();
        self.dirty.set(true);
    }

    /// 把内存中的配置写盘
    pub fn commit(&self) {
        let cfg = self.config.borrow();
        match self.manager.save(&cfg) {
            Ok(()) => {
                self.dirty.set(false);
                crate::debug!("配置已保存到 {}", self.manager.path.display());
            }
            Err(e) => crate::error!("配置保存失败：{}", e),
        }
    }

    pub fn window(&self) -> Option<Rc<FrameWindow>> {
        self.window.borrow().clone()
    }
}

const USAGE: &str = "\
photo-frame — Omarchy 桌面电子相框

用法：
  photo-frame            启动桌面相框
  photo-frame settings   打开设置窗口（已有实例则通知它打开）
  photo-frame quit       退出运行中的实例
  photo-frame --help     显示帮助
  photo-frame --version  显示版本

环境变量：
  PHOTO_FRAME_LOG=debug|info|warn|error   日志级别（默认 info）
  PHOTO_FRAME_DEBUG=1                    显示调试信息浮层
";

pub fn run(args: &[String]) -> Result<u8, String> {
    match args.first().map(|s| s.as_str()) {
        Some("-h") | Some("--help") | Some("help") => {
            print!("{USAGE}");
            return Ok(0);
        }
        Some("-v") | Some("--version") => {
            println!("photo-frame {}", env!("CARGO_PKG_VERSION"));
            return Ok(0);
        }
        Some(other) if other.starts_with('-') => {
            eprintln!("未知参数：{other}\n\n{USAGE}");
            return Ok(2);
        }
        _ => {}
    }

    let level = std::env::var("PHOTO_FRAME_LOG").unwrap_or_else(|_| "info".into());
    crate::log::init(&crate::config::state_dir(), crate::log::Level::parse(&level));
    crate::info!("photo-frame {} 启动中…", env!("CARGO_PKG_VERSION"));

    // 已有实例在跑 → 通知它开设置窗口，然后本进程退出
    let want_settings = args.iter().any(|a| a == "settings" || a == "--settings");
    if want_settings && crate::settings::request_open() {
        crate::info!("已通知运行中的实例打开设置窗口");
        return Ok(0);
    }
    let quit_only = args.iter().any(|a| a == "quit");

    adw::init().map_err(|e| format!("GTK 初始化失败：{e}"))?;

    // 默认 8px 的拖拽判定会让"起手"感觉迟钝；调到 2px 更跟手
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_dnd_drag_threshold(2);
    }

    let state = AppState::boot();
    crate::settings::serve_control(state.clone());

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    let app_state = state.clone();
    app.connect_activate(move |app| {
        if want_settings {
            crate::settings::show(&app_state);
            if let Some(p) = app_state.settings_window.borrow().as_ref() {
                p.set_application(app);
            }
        }
        if quit_only {
            return;
        }
        if app_state.window.borrow().is_none() {
            let fw = Rc::new(FrameWindow::new(app_state.clone(), app));
            fw.present();
            fw.update_hud(&app_state, "");
            *app_state.window.borrow_mut() = Some(fw.clone());
            crate::info!("相框窗口已显示");

            if let Some(player) = crate::player::MediaPlayer::new(app_state.clone()) {
                player.start();
                player.apply_desktop_visible();
                // 重启后自动恢复"调试浮层"开关
                if app_state.config.borrow().frame.debug_hud {
                    fw.set_debug(true);
                    fw.update_hud(&app_state, "");
                }
                *app_state.player.borrow_mut() = Some(player.clone());

                // 「被窗口覆盖则暂停」：Hyprland IPC 事件驱动
                let (mx, my) = {
                    let cfg = app_state.config.borrow();
                    (cfg.window.x, cfg.window.y)
                };
                let (fw_, fh_) = fw.view.frame_size();
                let monitor = crate::hypr::VisibilityMonitor::start(crate::hypr::Rect {
                    x: mx,
                    y: my,
                    w: fw_,
                    h: fh_,
                });
                let p2 = player.clone();
                monitor.on_change(move |visible| p2.set_active(visible));
                *app_state.visibility.borrow_mut() = Some(monitor);

                // 弹出面板：别的窗口拿到焦点（或换工作区）就自动收起
                let st_panel = app_state.clone();
                crate::hypr::on_focus_change(move || crate::settings::hide_if_open(&st_panel));
            }
        }
    });

    // 状态栏图标：左键开关设置面板
    {
        let st = state.clone();
        if let Some(tray) = crate::tray::Tray::new(
            "emblem-photos-symbolic",
            "桌面相框 · 点击开关设置面板",
            move || crate::settings::toggle(&st),
        ) {
            *state.tray.borrow_mut() = Some(tray);
        }
    }

    let code = app.run();
    if state.dirty.get() {
        state.commit();
    }
    Ok(code.get())
}
