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
    /// 本次启动做过"首次自动摆放" → 首帧就绪后按真实相框尺寸补贴靠
    pub auto_placed: std::cell::Cell<bool>,
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

        // 贴靠/夹取都基于**可见可用区**（已扣掉合成器 reserved，例如顶栏），
        // 否则 bottom/right 方向的边距会平白少掉顶栏高度（实测：选左下角+边距30，
        // 底部几乎贴边、左侧正常）。
        // 注意用 config.window.monitor（此时已由上面的 monitor.connector() 填好），
        // 不能再用 connector —— 它已被 unwrap_or(connector) 移动掉了。
        let mon_name = config.window.monitor.clone();
        let usable = match crate::hypr::monitor_reserved(&mon_name) {
            Some([l, t, r, b]) => geometry::Bounds {
                width: (bounds.width - l - r).max(1),
                height: (bounds.height - t - b).max(1),
            },
            None => bounds,
        };

        let mut auto_placed_now = false;
        if !config.window.placed {
            let (x, y) = geometry::anchor_pos(
                &config.window.default_anchor,
                usable,
                config.window.width,
                config.window.height,
                config.window.margin,
            );
            config.window.x = x;
            config.window.y = y;
            config.window.placed = true;
            auto_placed_now = true;
            needs_save = true;
        } else {
            // 显示器分辨率/布局变化后把位置夹回可见范围
            let (x, y) = geometry::clamp_to_screen(
                config.window.x,
                config.window.y,
                config.window.width,
                config.window.height,
                usable,
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
            auto_placed: std::cell::Cell::new(auto_placed_now),
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

    /// **从磁盘重新读一遍配置**（外部改了 config.toml 时用）。
    ///
    /// 设置页每次打开都调用它：这样即使配置是被外部修改的（手改文件 / 另一个实例），
    /// 面板里的数值与下拉项也一定是最新的，而不是内存里的旧值。
    /// 磁盘上没有可解析的配置时保留内存值，不破坏运行。
    pub fn reload_from_disk(&self) -> bool {
        let loaded = self.manager.load();
        let fresh = loaded.config; // ConfigManager::load 总是给出默认配置
        let mut cur = self.config.borrow_mut();
        if *cur == fresh {
            return false;
        }
        crate::debug!("设置页打开：从磁盘重读配置（{}）", fresh.source.path);
        *cur = fresh;
        true
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
Omaframe — Omarchy 智能自适应 PNG 相框

用法：
  omaframe                启动（桌面相框）
  omaframe settings   打开设置窗口（已有实例则通知它打开）
  omaframe quit       退出运行中的实例
  omaframe --help     显示帮助
  omaframe --version  显示版本

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
            println!("omaframe {}", env!("CARGO_PKG_VERSION"));
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
    crate::info!("omaframe {} 启动中…", env!("CARGO_PKG_VERSION"));

    // 已有实例在跑 → 通知它开设置窗口，然后本进程退出
    let want_settings = args.iter().any(|a| a == "settings" || a == "--settings");
    if want_settings && crate::settings::request_open() {
        crate::info!("已通知运行中的实例打开设置窗口");
        return Ok(0);
    }
    // `quit`：通过控制通道让**运行中的实例**退出。
    // 必须在 adw::init()/boot 之前返回 —— 否则本进程会白白启动一整套 GTK+托盘，
    // 什么也没退掉（旧行为：第二个实例自己开了个窗口就结束，主实例还在跑），
    // 而且 GIO 会把 "quit" 当成要打开的文件 → "This application can not open files"。
    let quit_only = args.iter().any(|a| a == "quit");
    if quit_only {
        if crate::settings::request_quit() {
            crate::info!("已通知运行中的实例退出");
            return Ok(0);
        }
        // 没有运行中的实例：没有可退的，直接正常结束（不要报 GIO 错误）
        crate::info!("没有运行中的实例，无需退出");
        return Ok(0);
    }

    adw::init().map_err(|e| format!("GTK 初始化失败：{e}"))?;

    // 默认 8px 的拖拽判定会让"起手"感觉迟钝；调到 2px 更跟手
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_dnd_drag_threshold(2);
    }

    let state = AppState::boot();
    // 开机启动对账：配置为 true 但 autostart 项缺失（被删/换机器）时补回来；
    // 配置为 false 则确保项已移除。这样"默认开启"对新装和已有用户都成立。
    {
        let want = state.config.borrow().autostart;
        if let Err(e) = crate::config::sync_autostart(want) {
            crate::warn!("同步开机启动失败：{e}");
        }
    }
    crate::settings::serve_control(state.clone());

    // 应用标志：**只保留 NON_UNIQUE**。
    // GtkApplication/AdwApplication 默认带 HANDLES_OPEN | HANDLES_COMMAND_LINE，
    // GIO 会把命令行参数当成"要打开的文件" → 传 `quit` 时报
    // “This application can not open files”。参数我们自己解析（见上），所以
    // 明确不要这两个标志。
    let mut app_flags = gio::ApplicationFlags::empty();
    app_flags.insert(gio::ApplicationFlags::NON_UNIQUE);

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(app_flags)
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
                player.set_autoplaced(app_state.auto_placed.get());
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
                crate::hypr::on_focus_change(move |payload| {
                    crate::settings::hide_if_open(&st_panel, payload)
                });
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
