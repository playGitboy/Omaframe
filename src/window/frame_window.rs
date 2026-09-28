//! 桌面相框窗口。
//!
//! 后端 A（Hyprland/Sway 等 wlroots 系）：wlr-layer-shell bottom 层。
//!   - 不参与平铺、不出现在窗口列表、永不获得键盘焦点
//!   - 永远位于普通窗口之下
//!   - 指针事件只在其未被覆盖时到达 → 被覆盖时天然不响应
//!
//! **surface 铺满整个显示器且永不改变尺寸**：相框的位置/大小全部是"绘制"出来的。
//! 这样拖动/缩放时指针永远不会离开控件（否则 GTK 会停止派发事件，
//! 表现为"拖到目标却只移动一段、还慢半拍"），而且全程零合成器 configure。
//!
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
    /// 最近一次已生效的输入区域（用于去重，避免"设区域→重绘→再设"死循环）
    applied_region: std::rc::Rc<std::cell::Cell<(i32, i32, i32, i32)>>,
}

impl FrameWindow {
    pub fn new(state: Rc<AppState>, app: &adw::Application) -> Self {
        let window = gtk::Window::new();
        // 必须挂到 Application 上，否则 GApplication 看不到任何窗口会立即退出
        window.set_application(Some(app));
        window.set_title(Some("photo-frame"));

        // 窗口底色必须显式透明：layer surface 一旦被 GTK 标记为不透明，
        // 相框 PNG 的透明处（内孔、圆角外）就会露出主题背景色 —— 用户看到的是"黑色"。
        let css = gtk::CssProvider::new();
        css.connect_parsing_error(|_, section, err| {
            crate::warn!("CSS 解析错误 @{:?}: {err}", section.start_location());
        });
        css.load_from_data(
            r#"
            window, window.background, .background {
                background-color: transparent;
                background-image: none;
                box-shadow: none;
                border-style: none;
            }
            .photo-frame-view {
                background-color: transparent;
                background-image: none;
            }
            "#,
        );
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        window.set_resizable(false);
        window.set_decorated(false);

        let view = MediaView::new();
        {
            let cfg = state.config.borrow();
            view.set_box(cfg.display.max_width, cfg.display.max_height);
            view.set_frame_pos(cfg.window.x, cfg.window.y);
            view.set_media_scale(cfg.display.media_scale);
        }

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
            applied_region: std::rc::Rc::new(std::cell::Cell::new((-1, -1, -1, -1))),
        };
        fw.setup_backend(&state);
        fw.setup_input_region_hook();
        fw
    }

    /// 后端初始化：surface 铺满监视器（四边锚定、边距 0），尺寸固定不变。
    fn setup_backend(&self, state: &Rc<AppState>) {
        let connector = state.config.borrow().window.monitor.clone();
        let monitor = target_monitor(&connector);
        let bounds = monitor
            .as_ref()
            .map(monitor_bounds)
            .unwrap_or(crate::geometry::Bounds {
                width: 1920,
                height: 1080,
            });
        self.view.set_surface_size(bounds.width, bounds.height);

        match state.backend {
            Backend::LayerShell => {
                self.window.init_layer_shell();
                self.window.set_layer(Layer::Bottom);
                self.window.set_namespace(Some("photo-frame"));
                // 关键：不要任何键盘交互 → 桌面快捷键永不受影响
                self.window.set_keyboard_mode(KeyboardMode::None);
                self.window.set_exclusive_zone(0);
                // 四边锚定 + 边距 0 → surface 正好等于整块显示器
                for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
                    self.window.set_anchor(edge, true);
                    self.window.set_margin(edge, 0);
                }
                if let Some(m) = monitor.as_ref() {
                    self.window.set_monitor(Some(m));
                }
            }
            Backend::Toplevel => {
                crate::warn!("降级模式：窗口可能被平铺/抢焦点，建议使用支持 layer-shell 的合成器");
            }
        }
    }

    /// 每次绘制后把输入区域同步为「当前可见相框矩形」：
    /// 相框以外（整屏的其余部分）点击穿透，不会挡住桌面。
    fn setup_input_region_hook(&self) {
        let weak: glib::WeakRef<gtk::Window> = glib::WeakRef::new();
        weak.set(Some(&self.window));
        let view_weak: glib::WeakRef<MediaView> = glib::WeakRef::new();
        view_weak.set(Some(&self.view));
        let applied = self.applied_region.clone();
        self.view.set_size_hook(move || {
            let (Some(w), Some(v)) = (weak.upgrade(), view_weak.upgrade()) else {
                return;
            };
            let Some(n) = w.native() else { return };
            let Some(surface) = n.surface() else { return };
            let (sw, sh) = (n.width().max(1), n.height().max(1));
            // 拖动中：整屏都可接收（指针一定会移出相框，否则手势会被中断）
            let (x, y, cw, ch) = if v.is_dragging() {
                (0, 0, sw, sh)
            } else {
                let (fx, fy, fw, fh) = v.hit_rect_now();
                let x = (fx.round() as i32).clamp(0, sw - 1);
                let y = (fy.round() as i32).clamp(0, sh - 1);
                (
                    x,
                    y,
                    (fw.round() as i32).max(1).min(sw - x),
                    (fh.round() as i32).max(1).min(sh - y),
                )
            };
            // 去重：只有真的变了才重新设置并请求一帧
            // （输入区域要在下一次 commit 才生效，所以设置后必须再画一帧）
            if applied.get() == (x, y, cw, ch) {
                return;
            }
            applied.set((x, y, cw, ch));
            let rect = cairo::RectangleInt::new(x, y, cw, ch);
            surface.set_input_region(Some(&cairo::Region::create_rectangle(&rect)));
            crate::debug!("输入区域 → {}x{}+{}+{}", cw, ch, x, y);
            v.queue_draw();
        });
    }

    pub fn present(&self) {
        self.window.present();
    }

    /// 相框位置：直接改绘制坐标（不动 layer 边距 → 零 configure）
    pub fn set_position(&self, x: i32, y: i32) {
        self.view.set_frame_pos(x, y);
    }

    /// 媒体上限（配置 max_width/max_height）
    pub fn set_box(&self, w: i32, h: i32) {
        self.view.set_box(w, h);
    }

    pub fn sync_input_region(&self) {
        // 输入区域由每次绘制的 hook 同步，这里只需触发一次重绘
        self.view.queue_draw();
    }

    pub fn has_debug(&self) -> bool {
        self.hud.borrow().is_some()
    }

    /// 调试浮层开关（运行时生效）
    pub fn set_debug(&self, on: bool) {
        crate::debug!("调试浮层 → {on}");
        let mut hud = self.hud.borrow_mut();
        match (on, hud.is_some()) {
            (true, false) => {
                let label = gtk::Label::new(Some("photo-frame"));
                label.set_halign(gtk::Align::Start);
                label.set_valign(gtk::Align::Start);
                label.set_margin_start(8);
                label.set_margin_top(6);
                label.set_opacity(0.75);
                self.root.add_overlay(&label);
                *hud = Some(label);
            }
            (false, true) => {
                if let Some(l) = hud.take() {
                    self.root.remove_overlay(&l);
                }
            }
            _ => {}
        }
    }

    pub fn update_hud(&self, state: &AppState, extra: &str) {
        if let Some(hud) = self.hud.borrow().as_ref() {
            let cfg = state.config.borrow();
            let (fx, fy) = self.view.frame_pos();
            let (fw, fh) = self.view.frame_size();
            let (mw, mh) = self.view.media_size();
            hud.set_text(&format!(
                "photo-frame\nbackend: {}\nmonitor: {}\nframe: {fx},{fy} {fw}x{fh}\nmedia: {mw}x{mh} (max {}x{})\n{}",
                state.backend.as_str(),
                cfg.window.monitor,
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
}
