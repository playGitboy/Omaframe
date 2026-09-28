//! 媒体显示控件：自定义 GtkWidget，精确控制尺寸与绘制。
//!
//! 一个 widget 负责全部绘制（媒体纹理 / PNG 相框 / 悬停控制层），
//! 好处是坐标系统一、命中检测与绘制永远一致。
//!
//! 不用 GtkPicture 的原因：相框 PNG 需要精确叠在媒体之上、控制层要固定在
//! 特定位置、视频需要每帧重绘，而且 GtkPicture 会被图片自然尺寸撑爆窗口。

use std::cell::{Cell, RefCell};

use crate::controls::{ControlLayout, Controls, HitZone};
use gtk::prelude::*;
use gtk::subclass::prelude::*;

type ClickHandler = std::rc::Rc<dyn Fn(HitZone)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MediaView {
        pub content_w: Cell<i32>,
        pub content_h: Cell<i32>,
        pub placeholder: Cell<bool>,
        pub texture: RefCell<Option<gdk::Texture>>,
        /// PNG 相框叠加层（Step 5）
        pub frame: RefCell<Option<gdk::Texture>>,
        /// 悬停控制层
        pub controls: Controls,
        pub caption: RefCell<String>,
        pub logged: Cell<bool>,
        pub on_click: RefCell<Option<ClickHandler>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MediaView {
        const NAME: &'static str = "PhotoFrameMediaView";
        type Type = super::MediaView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for MediaView {}

    impl WidgetImpl for MediaView {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let (w, h) = (self.content_w.get().max(1), self.content_h.get().max(1));
            // 返回 (minimum, natural, minimum_baseline, natural_baseline)
            match orientation {
                gtk::Orientation::Horizontal => (0, w, -1, -1),
                _ => (0, h, -1, -1),
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let w = self.content_w.get().max(1) as f32;
            let h = self.content_h.get().max(1) as f32;
            let rect = gtk::graphene::Rect::new(0.0, 0.0, w, h);

            let texture = self.texture.borrow().clone();
            match texture {
                Some(tex) => {
                    if !self.logged.replace(true) {
                        crate::debug!("绘制纹理 {}x{}", tex.width(), tex.height());
                    }
                    // 控件尺寸 == 图片显示尺寸，直接 1:1 上屏
                    snapshot.append_texture(&tex, &rect);
                }
                None => {
                    if !self.placeholder.get() {
                        return;
                    }
                    // 占位底板，方便肉眼确认位置与实际尺寸
                    let cr = snapshot.append_cairo(&rect);
                    cr.set_source_rgba(0.13, 0.13, 0.15, 0.92);
                    cr.rectangle(0.0, 0.0, w as f64, h as f64);
                    let _ = cr.fill();
                    cr.set_source_rgba(0.55, 0.75, 1.0, 0.9);
                    cr.set_line_width(2.0);
                    cr.rectangle(1.0, 1.0, w as f64 - 2.0, h as f64 - 2.0);
                    let _ = cr.stroke();
                }
            }

            // PNG 相框覆盖在媒体之上
            if let Some(frame) = self.frame.borrow().as_ref() {
                snapshot.append_texture(frame, &rect);
            }

            // 悬停控制层（最上层）
            let layout = ControlLayout::new(self.content_w.get(), self.content_h.get());
            let cr = snapshot.append_cairo(&rect);
            crate::controls::paint(&cr, &layout, &self.controls);
        }
    }
}

glib::wrapper! {
    pub struct MediaView(ObjectSubclass<imp::MediaView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MediaView {
    pub fn new() -> Self {
        let view: Self = glib::Object::builder().build();
        view.setup_gestures();
        view
    }

    fn setup_gestures(&self) {
        // glib 0.22 移除了 clone! 宏，手工用 WeakRef 捕获（不产生引用环）
        let weak: glib::WeakRef<MediaView> = glib::WeakRef::new();
        weak.set(Some(self));

        // 动画每一步都需要重绘
        let weak_draw = weak.clone();
        self.imp()
            .controls
            .set_redraw_hook(move || {
                if let Some(v) = weak_draw.upgrade() {
                    v.queue_draw();
                }
            });

        // 悬停：进入/离开控制层显隐
        let motion = gtk::EventControllerMotion::new();
        let w1 = weak.clone();
        motion.connect_enter(move |_, x, y| {
            crate::debug!("pointer enter ({x:.0},{y:.0})");
            if let Some(v) = w1.upgrade() {
                v.imp().controls.set_hover(true);
                v.update_zone(x, y);
            }
        });
        let w2 = weak.clone();
        motion.connect_leave(move |_| {
            crate::debug!("pointer leave");
            if let Some(v) = w2.upgrade() {
                v.imp().controls.set_hover(false);
                v.set_cursor_name(None);
                v.queue_draw();
            }
        });
        let w3 = weak.clone();
        motion.connect_motion(move |_, x, y| {
            crate::debug!("pointer motion ({x:.0},{y:.0})");
            if let Some(v) = w3.upgrade() {
                v.update_zone(x, y);
            }
        });
        self.add_controller(motion);

        // 点击：命中区域交给外部（播放器）处理
        let click = gtk::GestureClick::new();
        click.set_button(0);
        let w4 = weak.clone();
        click.connect_pressed(move |_, n_press, x, y| {
            if n_press != 1 {
                return;
            }
            let Some(v) = w4.upgrade() else { return };
            let layout = ControlLayout::new(v.imp().content_w.get(), v.imp().content_h.get());
            let zone = layout.hit(x, y);
            if zone == HitZone::None {
                return;
            }
            crate::debug!("点击命中 {:?}", zone);
            let handler = v.imp().on_click.borrow().clone();
            if let Some(cb) = handler {
                cb(zone);
            }
        });
        self.add_controller(click);
    }

    fn update_zone(&self, x: f64, y: f64) {
        let imp = self.imp();
        let layout = ControlLayout::new(imp.content_w.get(), imp.content_h.get());
        let zone = layout.hit(x, y);
        imp.controls.set_zone(zone);
        self.set_cursor_name(match zone {
            HitZone::Resize => Some("nwse-resize"),
            HitZone::None => None,
            _ => Some("pointer"),
        });
        self.queue_draw();
    }

    fn set_cursor_name(&self, name: Option<&str>) {
        let cursor = name
            .and_then(|n| gdk::Cursor::from_name(n, None))
            .or_else(|| gdk::Cursor::from_name("default", None));
        if let Some(c) = cursor {
            WidgetExt::set_cursor(self, Some(&c));
        }
    }

    /// 设置组件逻辑尺寸（立即触发重新测量）
    pub fn set_content_size(&self, w: i32, h: i32) {
        let w = w.max(1);
        let h = h.max(1);
        let imp = self.imp();
        if imp.content_w.get() != w || imp.content_h.get() != h {
            imp.content_w.set(w);
            imp.content_h.set(h);
            imp.controls.on_resize();
            self.queue_resize();
        }
    }

    pub fn content_size(&self) -> (i32, i32) {
        (self.imp().content_w.get(), self.imp().content_h.get())
    }

    /// 是否画占位底板
    pub fn set_placeholder(&self, on: bool) {
        if self.imp().placeholder.replace(on) != on {
            self.queue_draw();
        }
    }

    /// 设置要显示的图片纹理（控件尺寸会跟随图片尺寸）
    pub fn set_image(&self, texture: Option<gdk::Texture>, size: (i32, i32), caption: &str) {
        {
            let imp = self.imp();
            *imp.texture.borrow_mut() = texture;
            *imp.caption.borrow_mut() = caption.to_string();
            imp.placeholder.set(false);
            imp.logged.set(false);
        }
        self.set_content_size(size.0, size.1);
        self.queue_draw();
    }

    /// 视频帧纹理（不重置 caption/占位状态）
    pub fn set_video_frame(&self, texture: Option<gdk::Texture>, size: (i32, i32)) {
        {
            let imp = self.imp();
            *imp.texture.borrow_mut() = texture;
            imp.placeholder.set(false);
        }
        self.set_content_size(size.0, size.1);
        self.queue_draw();
    }

    /// PNG 相框叠加层
    pub fn set_frame_texture(&self, texture: Option<gdk::Texture>) {
        *self.imp().frame.borrow_mut() = texture;
        self.queue_draw();
    }

    /// 控制层图标状态：true = 正在运行（显示暂停图标）
    pub fn set_running(&self, running: bool) {
        self.imp().controls.set_running(running);
        self.queue_draw();
    }

    /// 点击回调（主线程）
    pub fn set_click_handler(&self, cb: impl Fn(HitZone) + 'static) {
        *self.imp().on_click.borrow_mut() = Some(std::rc::Rc::new(cb));
    }

    pub fn caption(&self) -> String {
        self.imp().caption.borrow().clone()
    }

    pub fn has_image(&self) -> bool {
        self.imp().texture.borrow().is_some()
    }
}

impl Default for MediaView {
    fn default() -> Self {
        Self::new()
    }
}
