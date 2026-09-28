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
    /// 当前的拖动余量（像素）
    pad: std::cell::Cell<i32>,
    /// 余量是否四边对称（true = 需要负边距向左上扩张）
    pad_sym: std::cell::Cell<bool>,
    /// 组件左上角在屏幕上的位置
    pos: std::cell::Cell<(i32, i32)>,
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

        // view 直接作为 Overlay 的子控件：拖动时给它加 margin，
        // 窗口自然尺寸 = 组件 + 2*margin（内容视觉位置不变）
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
            pad: std::cell::Cell::new(0),
            pad_sym: std::cell::Cell::new(false),
            pos: std::cell::Cell::new((0, 0)),
        };
        fw.setup_backend(&state);

        // 每次绘制后同步 surface 输入区域 + 边距
        // （layer-shell 的输入区域必须显式设置，否则收不到指针事件）
        fw.set_size_hook_sync();
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
                self.pos.set((x, y));
                self.window.set_margin(Edge::Left, x);
                self.window.set_margin(Edge::Top, y);
            }
            Backend::Toplevel => {
                self.window.set_decorated(false);
                crate::warn!("降级模式：窗口可能被平铺/抢焦点，建议使用支持 layer-shell 的合成器");
            }
        }
    }

    /// 注册"每次绘制后"的同步钩子
    fn set_size_hook_sync(&self) {
        let weak: glib::WeakRef<gtk::Window> = glib::WeakRef::new();
        weak.set(Some(&self.window));
        // 上面的闭包只借用 window；last_region 用 WeakRef 之外的办法拿不到，
        // 所以改成只读日志（不依赖 self）
        self.view.set_size_hook(move || {
            let Some(w) = weak.upgrade() else { return };
            if let Some(n) = w.native() {
                if let Some(surface) = n.surface() {
                    let rect = cairo::RectangleInt::new(0, 0, n.width().max(1), n.height().max(1));
                    surface.set_input_region(Some(&cairo::Region::create_rectangle(&rect)));
                    crate::debug!("输入区域 → {}x{}", rect.width(), rect.height());
                }
            }
        });
    }

    /// 拖动期间给 surface 加余量，让指针能拖到组件外面。
    /// 内容位置与视觉完全不变，只是 surface 变大（透明区域不绘制）。
    /// `symmetric=true`：四边都加余量（surface 需要负边距向左上扩张），
    ///                 用于"移动"——内容用绘制偏移跟随指针，widget 几何不动，
    ///                 因此 GTK 的 drag-delta 始终等于真实屏幕位移。
    /// `symmetric=false`：只在右/下加余量（不需要负边距），
    ///                 用于"改大小"——内容真实变大，widget 原点固定，delta 同样准确。
    /// 拖动时给 widget 增加活动区域。
    /// 只放大控件本身（**不**改 layer 边距），控件原点始终不动 ——
    /// 这样"控件坐标 = 屏幕坐标 - 组件位置"恒成立，位移计算不受任何重配置影响。
    pub fn set_drag_padding_full(&self, px: i32, _symmetric: bool) {
        self.view.set_drag_pad(px);
        crate::debug!("拖动活动区 → {px}px");
    }

    fn apply_margins(&self) {
        if !self.window.is_layer_window() {
            return;
        }
        let (x, y) = self.pos.get();
        // 值没变就不要动边距：改边距会让 layer shell 重新配置 surface，
        // 那一瞬间合成器可能把它画成不透明黑块
        if self.window.margin(Edge::Left) != x {
            self.window.set_margin(Edge::Left, x);
        }
        if self.window.margin(Edge::Top) != y {
            self.window.set_margin(Edge::Top, y);
        }
    }

    pub fn present(&self) {
        self.window.present();
        // （保留注释）自定义 widget + layer-shell 组合下，GTK 可能不会给 surface 设置输入区域，
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
        self.pos.set((x, y));
        if self.window.is_layer_window() {
            self.apply_margins();
        } else {
            // 降级模式：Wayland 下应用无法自行移动 toplevel，由合成器决定位置
            crate::debug!("降级模式忽略位置 ({}, {})", x, y);
        }
    }

    pub fn set_size(&self, w: i32, h: i32) {
        self.view.set_content_size(w, h);
        self.sync_input_region();
    }

    /// 同步 surface 输入区域（layer-shell 必需）。
    /// 用**窗口实际尺寸**（含拖动余量）而不是组件尺寸，否则余量区收不到事件。
    pub fn sync_input_region(&self) {
        if let Some(native) = self.window.native() {
            if let Some(surface) = native.surface() {
                let w = native.width().max(1);
                let h = native.height().max(1);
                let rect = cairo::RectangleInt::new(0, 0, w, h);
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
