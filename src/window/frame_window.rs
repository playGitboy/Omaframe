//! 桌面相框窗口。
//!
//! 后端 A（Hyprland/Sway 等 wlroots 系）：wlr-layer-shell bottom 层。
//!   - 不参与平铺、不出现在窗口列表、永不获得键盘焦点
//!   - 永远位于普通窗口之下
//!   - 指针事件只在其未被覆盖时到达 → 被覆盖时天然不响应
//! 后端 B（降级）：普通 toplevel，位置/焦点由合成器决定（不写用户配置文件）。

use crate::app::AppState;
use crate::window::media_view::MediaView;
use crate::window::{monitor_bounds, target_monitor, Backend};
use gtk::prelude::*;
use layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::RefCell;
use std::rc::Rc;

pub struct FrameWindow {
    pub window: gtk::Window,
    pub view: MediaView,
    root: gtk::Overlay,
    hud: RefCell<Option<gtk::Label>>,
}

impl FrameWindow {
    pub fn new(state: Rc<AppState>, app: &adw::Application) -> Self {
        let window = gtk::Window::new();
        // 必须挂到 Application 上，否则 GApplication 看不到任何窗口会立即退出
        window.set_application(Some(app));
        window.set_title(Some("photo-frame"));
        window.set_resizable(false);
        window.set_decorated(false);

        let (w, h) = {
            let cfg = state.config.borrow();
            (cfg.window.width, cfg.window.height)
        };

        let view = MediaView::new();
        view.set_content_size(w, h);
        view.set_placeholder(true);

        let root = gtk::Overlay::new();
        root.set_child(Some(&view));

        let hud = if std::env::var_os("PHOTO_FRAME_DEBUG").is_some() {
            let label = gtk::Label::new(Some("photo-frame"));
            label.set_halign(gtk::Align::Start);
            label.set_valign(gtk::Align::Start);
            label.set_margin_start(8);
            label.set_margin_top(6);
            label.set_opacity(0.75);
            root.add_overlay(&label);
            Some(label)
        } else {
            None
        };

        window.set_child(Some(&root));

        let fw = Self {
            window,
            view,
            root,
            hud: RefCell::new(hud),
        };
        fw.setup_backend(&state);
        fw
    }

    /// 后端相关初始化：必须在窗口 realize 之前完成。
    fn setup_backend(&self, state: &Rc<AppState>) {
        let (x, y, connector) = {
            let cfg = state.config.borrow();
            (cfg.window.x, cfg.window.y, cfg.window.monitor.clone())
        };
        let monitor = target_monitor(&connector);

        match state.backend {
            Backend::LayerShell => {
                self.window.init_layer_shell();
                self.window.set_layer(Layer::Bottom);
                self.window.set_namespace(Some("photo-frame"));
                // 关键：不要任何键盘交互 → 桌面快捷键永不受影响
                self.window.set_keyboard_mode(KeyboardMode::None);
                self.window.set_exclusive_zone(0);
                self.window.set_anchor(Edge::Left, true);
                self.window.set_anchor(Edge::Top, true);
                if let Some(m) = monitor.as_ref() {
                    self.window.set_monitor(Some(m));
                }
                self.window.set_margin(Edge::Left, x);
                self.window.set_margin(Edge::Top, y);
            }
            Backend::Toplevel => {
                self.window.set_decorated(false);
                crate::warn!("降级模式：窗口可能被平铺/抢焦点，建议使用支持 layer-shell 的合成器");
            }
        }
    }

    pub fn present(&self) {
        self.window.present();
        // 自定义 widget + layer-shell 组合下，GTK 可能不会给 surface 设置输入区域，
        // 导致收不到指针事件；这里显式把整个组件矩形设为可输入。
        let weak = glib::WeakRef::<gtk::Window>::new();
        weak.set(Some(&self.window));
        self.window.connect_map(move |_| {
            if let Some(w) = weak.upgrade() {
                if let Some(native) = w.native() {
                    if let Some(surface) = native.surface() {
                        let (width, height) = (
                            native.width() as f64,
                            native.height() as f64,
                        );
                        let rect = cairo::RectangleInt::new(0, 0, width.max(1.0) as i32, height.max(1.0) as i32);
                        let region = cairo::Region::create_rectangle(&rect);
                        surface.set_input_region(Some(&region));
                    }
                }
            }
        });
    }

    pub fn set_position(&self, x: i32, y: i32) {
        if self.window.is_layer_window() {
            self.window.set_margin(Edge::Left, x);
            self.window.set_margin(Edge::Top, y);
        } else {
            // 降级模式：Wayland 下应用无法自行移动 toplevel，由合成器决定位置
            crate::debug!("降级模式忽略位置 ({}, {})", x, y);
        }
    }

    pub fn set_size(&self, w: i32, h: i32) {
        self.view.set_content_size(w, h);
        self.sync_input_region();
    }

    /// 尺寸变化后同步 surface 输入区域（layer-shell 必需，否则鼠标事件收不到）
    pub fn sync_input_region(&self) {
        if let Some(native) = self.window.native() {
            if let Some(surface) = native.surface() {
                let (w, h) = self.view.content_size();
                let rect = cairo::RectangleInt::new(0, 0, w.max(1), h.max(1));
                let region = cairo::Region::create_rectangle(&rect);
                surface.set_input_region(Some(&region));
            }
        }
    }

    pub fn update_hud(&self, state: &AppState, extra: &str) {
        if let Some(hud) = self.hud.borrow().as_ref() {
            let cfg = state.config.borrow();
            let (w, h) = self.view.content_size();
            hud.set_text(&format!(
                "photo-frame\nbackend: {}\nmonitor: {}\npos: {},{}\nsize: {}x{} (max {}x{})\n{}",
                state.backend.as_str(),
                cfg.window.monitor,
                cfg.window.x,
                cfg.window.y,
                w,
                h,
                cfg.display.max_width,
                cfg.display.max_height,
                extra
            ));
        }
    }

    /// 当前屏幕可用区域（逻辑像素）
    pub fn screen_bounds(&self, state: &AppState) -> crate::geometry::Bounds {
        let connector = state.config.borrow().window.monitor.clone();
        target_monitor(&connector)
            .as_ref()
            .map(monitor_bounds)
            .unwrap_or(crate::geometry::Bounds {
                width: 1920,
                height: 1080,
            })
    }

    /// 供后续拖动/resize 使用：窗口根控件
    pub fn root(&self) -> &gtk::Overlay {
        &self.root
    }
}
