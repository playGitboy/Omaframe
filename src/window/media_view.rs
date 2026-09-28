//! 媒体显示控件：自定义 GtkWidget，精确控制尺寸与绘制。
//!
//! 一个 widget 负责全部绘制（媒体纹理 / PNG 相框 / 悬停控制层），
//! 好处是坐标系统一、命中检测与绘制永远一致。
//!
//! 不用 GtkPicture 的原因：相框 PNG 需要精确叠在媒体之上、控制层要固定在
//! 特定位置、视频需要每帧重绘，而且 GtkPicture 会被图片自然尺寸撑爆窗口。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::controls::{ControlLayout, Controls, HitZone};
use gtk::prelude::*;
use gtk::subclass::prelude::*;

type ClickHandler = std::rc::Rc<dyn Fn(HitZone)>;
type DragHandler = std::rc::Rc<dyn Fn(crate::controls::DragPhase)>;

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
        pub on_drag: RefCell<Option<DragHandler>>,
        /// 按下位置（区分点击与拖动）
        pub press_x: Cell<f64>,
        pub press_y: Cell<f64>,
        pub press_valid: Cell<bool>,
        /// 尺寸变化回调（FrameWindow 用它同步 surface 输入区域）
        pub size_hook: RefCell<Option<Rc<dyn Fn()>>>,
        /// 拖动时的视觉偏移（只改绘制，widget 几何不动）
        pub offset_x: Cell<f64>,
        pub offset_y: Cell<f64>,
        /// 改大小拖动中的目标尺寸（0,0 = 无预览）
        pub preview_w: Cell<i32>,
        pub preview_h: Cell<i32>,
        /// 媒体相对组件的内缩比例（0.96 = 四周留 2% 细边）
        pub media_scale: Cell<f64>,
        /// 最近一次指针在控件内的位置（拖动位移用它算，不依赖 GTK 的 delta）
        pub last_x: Cell<f64>,
        pub last_y: Cell<f64>,
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
            let (w, h) = (
                self.content_w.get().max(1),
                self.content_h.get().max(1),
            );
            // 返回 (minimum, natural, minimum_baseline, natural_baseline)
            match orientation {
                gtk::Orientation::Horizontal => (0, w, -1, -1),
                _ => (0, h, -1, -1),
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            // 此刻分配已完成：通知外部同步 surface 输入区域 / 边距
            let hook = self.size_hook.borrow().clone();
            if let Some(h) = hook {
                h();
            }

            // 拖动视觉偏移：把内容整体平移绘制（控件尺寸不变 → surface 不重建）
            let (ox, oy) = (self.offset_x.get(), self.offset_y.get());
            if ox != 0.0 || oy != 0.0 {
                snapshot.translate(&gtk::graphene::Point::new(ox as f32, oy as f32));
            }

            // 改大小预览：媒体画在目标尺寸处（不超过控件本体，surface 尺寸不变）
            let (pw, ph) = if self.preview_w.get() > 0 {
                (self.preview_w.get(), self.preview_h.get())
            } else {
                (self.content_w.get(), self.content_h.get())
            };
            let rect = gtk::graphene::Rect::new(0.0, 0.0, pw as f32, ph as f32);
            // 组件尺寸是**固定的最大框**，媒体在框内按自身比例缩放并居中 ——
            // 这样切换媒体时 surface 尺寸永远不变（不再重建、不再闪黑/残影）
            let texture0 = self.texture.borrow().clone();
            let (mx, my, mw, mh) = match texture0.as_ref() {
                Some(t) => crate::geometry::fit_rect(
                    t.width(),
                    t.height(),
                    self.content_w.get(),
                    self.content_h.get(),
                    self.media_scale.get(),
                ),
                None => crate::geometry::inset(
                    self.content_w.get(),
                    self.content_h.get(),
                    self.media_scale.get(),
                ),
            };
            let media_rect = gtk::graphene::Rect::new(
                mx as f32,
                my as f32,
                mw as f32,
                mh as f32,
            );

            let texture = texture0;
            match texture {
                Some(tex) => {
                    if !self.logged.replace(true) {
                        crate::debug!("绘制纹理 {}x{}", tex.width(), tex.height());
                    }
                    snapshot.append_texture(&tex, &media_rect);
                }
                None => {
                    // 没有媒体时**不画任何底色**：layer surface 若画出深色底，
                    // 合成器会把它当成不透明黑块（用户看到的"外层黑色"）。
                    if self.placeholder.get() {
                        let cr = snapshot.append_cairo(&media_rect);
                        // 只画一圈极细的提示描边，不填充
                        cr.set_source_rgba(0.55, 0.75, 1.0, 0.35);
                        cr.set_line_width(1.0);
                        cr.rectangle(0.5, 0.5, mw as f64 - 1.0, mh as f64 - 1.0);
                        let _ = cr.stroke();
                    }
                }
            }

            // 预览时用虚线框出目标范围（"还能再拖多大"一目了然）
            if self.preview_w.get() > 0 {
                let cr = snapshot.append_cairo(&rect);
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.6);
                cr.set_line_width(1.5);
                cr.set_dash(&[5.0, 4.0], 0.0);
                cr.rectangle(0.75, 0.75, (pw - 1) as f64, (ph - 1) as f64);
                let _ = cr.stroke();
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
        view.add_css_class("photo-frame-view");
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

        // 拖动：移动组件（body）/ 右下角改大小（Resize 热区）
        // 注：坐标要用 gtk_gesture_drag_get_start_point/delta 拿，信号本身不带坐标。
        let drag = gtk::GestureDrag::new();
        drag.set_button(0);
        let wd = weak.clone();
        drag.connect_drag_begin(move |_, x, y| {
            let Some(v) = wd.upgrade() else { return };
            let layout = ControlLayout::new(v.imp().content_w.get(), v.imp().content_h.get());
            // 播放/暂停按钮上不启动拖动（留给点击）；其余都可以拖
            let Some(mode) = layout.drag_mode_at(x, y) else {
                return;
            };
            let handler = v.imp().on_drag.borrow().clone();
            if let Some(cb) = handler {
                cb(crate::controls::DragPhase::Begin(mode, x, y));
            }
        });
        let wu = weak.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            if let Some(v) = wu.upgrade() {
                let handler = v.imp().on_drag.borrow().clone();
                if let Some(cb) = handler {
                    cb(crate::controls::DragPhase::Update(dx, dy));
                }
            }
        });
        let we = weak.clone();
        drag.connect_drag_end(move |_, _, _| {
            if let Some(v) = we.upgrade() {
                let handler = v.imp().on_drag.borrow().clone();
                if let Some(cb) = handler {
                    cb(crate::controls::DragPhase::End);
                }
            }
        });
        self.add_controller(drag);

        // 点击：按下记位置，松开时位移很小才算"点击"（拖动不会误触发切图）
        let click = gtk::GestureClick::new();
        click.set_button(0);
        let wp = weak.clone();
        click.connect_pressed(move |_, _, x, y| {
            if let Some(v) = wp.upgrade() {
                let imp = v.imp();
                imp.press_x.set(x);
                imp.press_y.set(y);
                imp.press_valid.set(true);
            }
        });
        let wr = weak.clone();
        click.connect_released(move |_, n_press, x, y| {
            if n_press != 1 {
                return;
            }
            let Some(v) = wr.upgrade() else { return };
            let imp = v.imp();
            if !imp.press_valid.replace(false) {
                return;
            }
            if (x - imp.press_x.get()).abs() > 6.0 || (y - imp.press_y.get()).abs() > 6.0 {
                return; // 按下→松开位移大 = 拖动，不是点击
            }
            let layout = ControlLayout::new(imp.content_w.get(), imp.content_h.get());
            let zone = layout.hit(x, y);
            if zone == HitZone::None {
                return;
            }
            crate::debug!("点击命中 {:?}", zone);
            let handler = imp.on_click.borrow().clone();
            if let Some(cb) = handler {
                cb(zone);
            }
        });
        self.add_controller(click);
    }

    /// 最近一次指针位置（控件坐标系）
    pub fn last_pointer(&self) -> (f64, f64) {
        (self.imp().last_x.get(), self.imp().last_y.get())
    }

    fn update_zone(&self, x: f64, y: f64) {
        let imp = self.imp();
        imp.last_x.set(x);
        imp.last_y.set(y);
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

    /// 拖动回调（主线程）：移动组件 / 右下角改大小
    pub fn set_drag_handler(&self, cb: impl Fn(crate::controls::DragPhase) + 'static) {
        *self.imp().on_drag.borrow_mut() = Some(std::rc::Rc::new(cb));
    }

    /// 媒体内缩比例（0.96 = 四周留 2% 细边）
    pub fn set_media_scale(&self, scale: f64) {
        let s = if scale.is_finite() {
            scale.clamp(0.2, 1.0)
        } else {
            1.0
        };
        if (self.imp().media_scale.get() - s).abs() < f64::EPSILON {
            return;
        }
        self.imp().media_scale.set(s);
        self.queue_draw();
    }

    /// 改大小拖动中的目标尺寸（只影响绘制，不改变控件/窗口尺寸）
    pub fn set_preview_size(&self, w: i32, h: i32) {
        let (w, h) = (w.max(0), h.max(0));
        let imp = self.imp();
        if imp.preview_w.get() == w && imp.preview_h.get() == h {
            return;
        }
        imp.preview_w.set(w);
        imp.preview_h.set(h);
        self.queue_draw();
    }

    /// 拖动时的视觉偏移（不改变 widget 几何）
    pub fn set_visual_offset(&self, dx: f64, dy: f64) {
        let imp = self.imp();
        if imp.offset_x.get() == dx && imp.offset_y.get() == dy {
            return;
        }
        imp.offset_x.set(dx);
        imp.offset_y.set(dy);
        self.queue_draw();
    }

    /// 绘制/分配完成后回调（主线程）：用于同步 surface 输入区域、边距
    pub fn set_size_hook(&self, cb: impl Fn() + 'static) {
        *self.imp().size_hook.borrow_mut() = Some(std::rc::Rc::new(cb));
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
