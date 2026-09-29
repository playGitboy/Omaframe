//! 媒体显示控件：自定义 GtkWidget，精确控制尺寸与绘制。
//!
//! **坐标模型（关键，别再改回去）**
//! - 控件 = 整块 layer surface = **整个显示器**，尺寸恒定、永不重建
//! - 相框左上角 `frame_x/frame_y` 直接来自配置（就是屏幕坐标）
//! - 媒体尺寸 = `fit(素材比例, max_width×media_scale, max_height×media_scale)`
//! - 相框 = 媒体 × 1.03，居中于 `frame_x/frame_y`
//!
//! 为什么 surface 要铺满整屏：拖动/缩放时指针必须**始终在控件内**。
//! 之前 surface 只有"最大框"大小，指针一移出去 GTK 就停止派发事件，
//! 于是"拖到目标位置却只移动了一部分、还慢半拍"。
//! 铺满整屏后：指针永不离开 → 事件连续 → 位移精确；且拖动/缩放只改**绘制**，
//! 没有任何合成器 configure 往返 → 顺滑。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::controls::{ControlLayout, Controls, HitZone};
use gtk::prelude::*;
use gtk::subclass::prelude::*;

/// 媒体绘制区域相对内孔**内缩**的像素数。
/// 不能外扩：相框边缘常带半透明羽化，外扩会让图片从羽化带透出来
/// （用户报的"图片上边缘露出相框"）。内缩一点让硬边藏在不透明环内。
const MASK_INSET: f64 = 1.0;

type ClickHandler = Rc<dyn Fn(HitZone)>;
type DragHandler = Rc<dyn Fn(crate::controls::DragPhase)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MediaView {
        /// 控件（=layer surface）尺寸：整块显示器，固定不变
        pub surf_w: Cell<i32>,
        pub surf_h: Cell<i32>,
        /// 媒体显示上限（配置 max_width / max_height）
        pub box_w: Cell<i32>,
        pub box_h: Cell<i32>,
        /// 相框左上角的屏幕坐标（配置 window.x / window.y）
        pub frame_x: Cell<i32>,
        pub frame_y: Cell<i32>,
        /// 媒体内缩比例（已废弃：改用 PNG 内孔遮罩让位，此处恒为 1.0，仅为兼容旧配置）
        pub media_scale: Cell<f64>,
        /// 素材显示比（0.0~1.0，1.0 = 铺满相框），**以相框中心为基准缩放**
        pub media_zoom: Cell<f64>,
        /// 相框 PNG 自身宽高比（>0 时相框不拉伸，按比例居中）
        pub frame_aspect: Cell<f64>,
        /// 相框相对素材的外扩比例（0.05 = 大 5%）
        pub frame_grow: Cell<f64>,
        pub texture: RefCell<Option<gdk::Texture>>,
        pub frame: RefCell<Option<gdk::Texture>>,
        pub controls: Controls,
        pub caption: RefCell<String>,
        pub logged: Cell<bool>,
        /// 缩放预览：目标**媒体**尺寸（0,0 = 无预览）
        pub preview_w: Cell<i32>,
        pub preview_h: Cell<i32>,
        /// 拖动绘制偏移（只影响绘制，控件几何不动 → 事件坐标始终有效）
        pub offset_x: Cell<f64>,
        pub offset_y: Cell<f64>,
        /// 最近一次绘制的相框矩形（含偏移）：命中检测与输入区域用它
        pub hit_rect: Cell<(f64, f64, f64, f64)>,
        /// 相框内孔（透明区）比例（None = 无内孔，退回叠图）
        pub inner_hole: Cell<Option<crate::frame::InnerHole>>,
        /// 内孔遮罩（cairo surface：alpha=255 处允许显示媒体），把媒体擦成内孔形状
        pub frame_mask: RefCell<Option<Rc<cairo::ImageSurface>>>,
        pub press_x: Cell<f64>,
        pub press_y: Cell<f64>,
        pub press_valid: Cell<bool>,
        /// 是否正在拖动：拖动中把输入区域放宽到整屏，否则指针移出相框后事件会中断
        pub dragging: Cell<bool>,
        pub on_click: RefCell<Option<ClickHandler>>,
        pub on_drag: RefCell<Option<DragHandler>>,
        pub size_hook: RefCell<Option<Rc<dyn Fn()>>>,
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
            let (w, h) = (self.surf_w.get().max(1), self.surf_h.get().max(1));
            match orientation {
                gtk::Orientation::Horizontal => (0, w, -1, -1),
                _ => (0, h, -1, -1),
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let (fx, fy, fw, fh, mx, my, mw, mh) = self.geometry();
            let (ox, oy) = (self.offset_x.get(), self.offset_y.get());
            // 记录当前可见相框矩形（含偏移）
            self.hit_rect
                .set((fx as f64 + ox, fy as f64 + oy, fw as f64, fh as f64));
            if let Some(h) = self.size_hook.borrow().clone() {
                h();
            }

            snapshot.save();
            if ox != 0.0 || oy != 0.0 {
                snapshot.translate(&gtk::graphene::Point::new(ox as f32, oy as f32));
            }

            let media_rect = gtk::graphene::Rect::new(mx as f32, my as f32, mw as f32, mh as f32);
            let frame_rect = gtk::graphene::Rect::new(fx as f32, fy as f32, fw as f32, fh as f32);
            // 媒体绘制：若相框检测到内孔，裁剪到内孔（并外扩 MASK_FEATHER 藏边）
            let hole = self.inner_hole.get();
            snapshot.save();
            if let Some(h) = hole {
                let ins = MASK_INSET as f32;
                let ix = (fx as f64 + fw as f64 * h.x0) as f32;
                let iy = (fy as f64 + fh as f64 * h.y0) as f32;
                let iw = (fw as f64 * h.width()) as f32;
                let ih = (fh as f64 * h.height()) as f32;
                // 严格裁剪到内孔，并轻微内缩：硬边落在不透明环内，
                // 而相框的半透明羽化带后面是桌面，不会透出图片。
                let clip = gtk::graphene::Rect::new(
                    ix + ins,
                    iy + ins,
                    (iw - ins * 2.0).max(1.0),
                    (ih - ins * 2.0).max(1.0),
                );
                snapshot.push_clip(&clip);
            }
            if let Some(tex) = self.texture.borrow().clone() {
                if !self.logged.replace(true) {
                    crate::debug!("绘制纹理 {}x{}", tex.width(), tex.height());
                }
                // 媒体按 **cover** 填满可视区：内孔比例与媒体比例不同时，
                // 轻微裁切而不是留边（照片满了更好看）
                let (mw_, mh_) = (media_rect.width(), media_rect.height());
                let (tw_, th_) = (tex.width() as f32, tex.height() as f32);
                let src_aspect = tw_ / th_;
                let dst_aspect = mw_ / mh_;
                let draw = if src_aspect > dst_aspect {
                    // 源更宽：按高度铺满，左右超出被裁
                    let h2 = mh_;
                    let w2 = h2 * src_aspect;
                    gtk::graphene::Rect::new(
                        media_rect.x() - (w2 - mw_) / 2.0,
                        media_rect.y(),
                        w2,
                        h2,
                    )
                } else {
                    let w2 = mw_;
                    let h2 = w2 / src_aspect;
                    gtk::graphene::Rect::new(
                        media_rect.x(),
                        media_rect.y() - (h2 - mh_) / 2.0,
                        w2,
                        h2,
                    )
                };
                snapshot.append_texture(&tex, &draw);
            }
            if hole.is_some() {
                snapshot.pop();
            }
            snapshot.restore();

            // 用遮罩把媒体**擦成内孔形状**：外部透明缺口（异形轮廓）与相框不透明处都不显示媒体。
            // 遮罩边缘做过 1px 羽化 → 与相框自然衔接（半透明渐变观感）。
            let mask_guard = self.frame_mask.borrow();
            if let Some(mask) = mask_guard.as_deref() {
                let cr = snapshot.append_cairo(&frame_rect);
                let _ = cr.save();
                cr.set_operator(cairo::Operator::DestOut);
                // 关键：遮罩是按"相框矩形尺寸"生成的，但 cairo 原点在 surface (0,0)。
                // 必须先平移到相框位置再贴，否则会擦错区域，媒体从相框边缘漏出。
                cr.translate(frame_rect.x() as f64, frame_rect.y() as f64);
                let _ = cr.set_source_surface(mask, 0.0, 0.0);
                cr.rectangle(
                    0.0,
                    0.0,
                    frame_rect.width() as f64,
                    frame_rect.height() as f64,
                );
                let _ = cr.fill();
                let _ = cr.restore();
            }
            // 没有媒体时不画任何底色：layer surface 一旦被当成不透明就会变黑块

            // PNG 相框叠在最上层：它的透明区正好透出被裁剪的媒体
            if let Some(frame) = self.frame.borrow().as_ref() {
                snapshot.append_texture(frame, &frame_rect);
            }

            // 缩放预览虚线框
            if self.preview_w.get() > 0 {
                let cr = snapshot.append_cairo(&frame_rect);
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.6);
                cr.set_line_width(1.5);
                cr.set_dash(&[5.0, 4.0], 0.0);
                cr.rectangle(0.75, 0.75, fw as f64 - 1.5, fh as f64 - 1.5);
                let _ = cr.stroke();
            }

            // 悬停控制层（以相框矩形为基准）
            let layout = ControlLayout::new(fw, fh);
            snapshot.save();
            snapshot.translate(&gtk::graphene::Point::new(fx as f32, fy as f32));
            let cr = snapshot.append_cairo(&frame_rect);
            crate::controls::paint(&cr, &layout, &self.controls);
            snapshot.restore();

            snapshot.restore();
        }
    }
}

impl imp::MediaView {
    /// (相框 x,y,w,h, 媒体 x,y,w,h)，单位 px，控件（=屏幕）坐标
    #[allow(clippy::type_complexity)]
    pub fn geometry(&self) -> (i32, i32, i32, i32, i32, i32, i32, i32) {
        let (bw, bh) = (self.box_w.get().max(16), self.box_h.get().max(16));

        // ① 素材矩形：按素材比例在上限盒内取最大（换素材时尺寸随之变化）
        let (sw, sh) = if self.preview_w.get() > 0 {
            (self.preview_w.get().max(1), self.preview_h.get().max(1))
        } else if let Some(t) = self.texture.borrow().as_ref() {
            crate::geometry::fit(t.width().max(1), t.height().max(1), bw, bh)
        } else {
            (bw, bh)
        };

        // ② 相框目标盒：以素材为中心外扩 grow（默认 5%）
        let grow = 1.0 + self.frame_grow.get().clamp(0.0, 0.5);
        let box_fw = ((sw as f64) * grow).round().max(1.0) as i32;
        let box_fh = ((sh as f64) * grow).round().max(1.0) as i32;
        let cx = self.frame_x.get() as f64 + sw as f64 / 2.0;
        let cy = self.frame_y.get() as f64 + sh as f64 / 2.0;

        // ③ 相框矩形：按 **PNG 自身比例** fit 进目标盒
        //    → 宽高随素材自适应，且保持 PNG 比例不被拉伸
        let aspect = self.frame_aspect.get();
        let (fw, fh) = if aspect > 0.01 {
            crate::geometry::fit((aspect * 10_000.0).round() as i32, 10_000, box_fw, box_fh)
        } else {
            (box_fw, box_fh)
        };
        let fx = (cx - fw as f64 / 2.0).round() as i32;
        let fy = (cy - fh as f64 / 2.0).round() as i32;

        // ④ 素材：在相框内居中放置（含显示比）→ 与相框中心恒等
        let (mx, my, mw, mh) = crate::geometry::place_media(fx, fy, fw, fh, sw, sh, self.media_zoom.get());
        (fx, fy, fw, fh, mx, my, mw, mh)
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
        view.imp().media_zoom.set(1.0);
        view.imp().frame_grow.set(0.05); // 相框默认比素材大 5%
        view.add_css_class("photo-frame-view");
        view.setup_gestures();
        view
    }

    fn setup_gestures(&self) {
        // glib 0.22 无 clone! 宏，手工 WeakRef
        let weak: glib::WeakRef<MediaView> = glib::WeakRef::new();
        weak.set(Some(self));

        // 悬停
        let motion = gtk::EventControllerMotion::new();
        let w1 = weak.clone();
        motion.connect_enter(move |_, x, y| {
            if let Some(v) = w1.upgrade() {
                v.imp().controls.set_hover(true);
                v.update_zone(x, y);
            }
        });
        let w2 = weak.clone();
        motion.connect_leave(move |_| {
            if let Some(v) = w2.upgrade() {
                v.imp().controls.set_hover(false);
                v.queue_draw();
            }
        });
        let w3 = weak.clone();
        motion.connect_motion(move |_, x, y| {
            if let Some(v) = w3.upgrade() {
                v.update_zone(x, y);
            }
        });
        self.add_controller(motion);

        // 拖动：移动 / 右下角改大小。delta 是**相对按下点**的绝对位移，
        // 因为控件（整屏）全程不动，所以它精确等于屏幕位移。
        let drag = gtk::GestureDrag::new();
        drag.set_button(0);
        let wd = weak.clone();
        drag.connect_drag_begin(move |d, x, y| {
            let Some(v) = wd.upgrade() else { return };
            let _ = d;
            let (fx, fy, fw, fh) = v.hit_rect_now();
            let layout = ControlLayout::new(fw as i32, fh as i32);
            let Some(mode) = layout.drag_mode_at(x - fx, y - fy) else {
                return;
            };
            let handler = v.imp().on_drag.borrow().clone();
            if let Some(cb) = handler {
                cb(crate::controls::DragPhase::Begin(
                    mode,
                    x - fx,
                    y - fy,
                ));
            }
        });
        let wu = weak.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            let Some(v) = wu.upgrade() else { return };
            let handler = v.imp().on_drag.borrow().clone();
            if let Some(cb) = handler {
                cb(crate::controls::DragPhase::Update(dx, dy));
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

        // 点击（与拖动区分：位移 ≤6px 才算点击）
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
                return;
            }
            let (fx, fy, fw, fh) = v.hit_rect_now();
            let layout = ControlLayout::new(fw as i32, fh as i32);
            let zone = layout.hit(x - fx, y - fy);
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

    fn update_zone(&self, x: f64, y: f64) {
        let (fx, fy, fw, fh) = self.hit_rect_now();
        let layout = ControlLayout::new(fw as i32, fh as i32);
        let zone = layout.hit(x - fx, y - fy);
        self.imp().controls.set_zone(zone);
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

    pub fn is_dragging(&self) -> bool {
        self.imp().dragging.get()
    }

    pub fn set_dragging(&self, on: bool) {
        if self.imp().dragging.replace(on) == on {
            return;
        }
        if !on {
            self.queue_draw(); // 让输入区域恢复成相框矩形
        }
    }

    /// 当前可见相框矩形（含拖动偏移）
    pub fn hit_rect_now(&self) -> (f64, f64, f64, f64) {
        let r = self.imp().hit_rect.get();
        if r.2 > 0.0 {
            return r;
        }
        let (fx, fy, fw, fh, ..) = self.imp().geometry();
        (fx as f64, fy as f64, fw as f64, fh as f64)
    }

    /// 控件（=surface）尺寸：整块显示器，固定
    pub fn set_surface_size(&self, w: i32, h: i32) {
        let imp = self.imp();
        let (w, h) = (w.max(1), h.max(1));
        if imp.surf_w.get() == w && imp.surf_h.get() == h {
            return;
        }
        imp.surf_w.set(w);
        imp.surf_h.set(h);
        self.queue_resize();
        self.queue_draw();
    }

    pub fn surface_size(&self) -> (i32, i32) {
        (self.imp().surf_w.get(), self.imp().surf_h.get())
    }

    /// 媒体显示上限（配置 max_width / max_height）
    pub fn set_box(&self, w: i32, h: i32) {
        let imp = self.imp();
        let (w, h) = (w.max(16), h.max(16));
        if imp.box_w.get() == w && imp.box_h.get() == h {
            return;
        }
        imp.box_w.set(w);
        imp.box_h.set(h);
        self.queue_draw();
    }

    pub fn box_size(&self) -> (i32, i32) {
        (self.imp().box_w.get(), self.imp().box_h.get())
    }

    /// 相框左上角（屏幕坐标）
    pub fn set_frame_pos(&self, x: i32, y: i32) {
        let imp = self.imp();
        if imp.frame_x.get() == x && imp.frame_y.get() == y {
            return;
        }
        imp.frame_x.set(x);
        imp.frame_y.set(y);
        self.queue_draw();
    }

    pub fn frame_pos(&self) -> (i32, i32) {
        (self.imp().frame_x.get(), self.imp().frame_y.get())
    }

    /// 相框尺寸（由素材比例与上限决定）
    pub fn frame_size(&self) -> (i32, i32) {
        let (_, _, fw, fh, ..) = self.imp().geometry();
        (fw, fh)
    }

    /// 媒体尺寸（相框内实际显示的照片大小）
    pub fn media_size(&self) -> (i32, i32) {
        let (.., mw, mh) = self.imp().geometry();
        (mw, mh)
    }

    pub fn media_scale(&self) -> f64 {
        self.imp().media_scale.get()
    }

    /// 相框外扩比例（1.05 = 比素材大 5%，以素材中心为基准）
    pub fn set_frame_grow(&self, grow: f64) {
        let g = if grow.is_finite() {
            grow.clamp(0.0, 0.5)
        } else {
            0.05
        };
        if (self.imp().frame_grow.get() - g).abs() < f64::EPSILON {
            return;
        }
        self.imp().frame_grow.set(g);
        self.queue_draw();
    }

    /// 相框 PNG 自身宽高比（>0 时按比例居中，不拉伸）
    pub fn set_frame_aspect(&self, aspect: f64) {
        let a = if aspect.is_finite() && aspect > 0.0 {
            aspect.clamp(0.1, 10.0)
        } else {
            0.0
        };
        if (self.imp().frame_aspect.get() - a).abs() < f64::EPSILON {
            return;
        }
        self.imp().frame_aspect.set(a);
        self.queue_resize();
        self.queue_draw();
    }

    /// 素材显示比（0.0~1.0；<1 时以相框中心为基准缩小）
    pub fn set_media_zoom(&self, zoom: f64) {
        let z = if zoom.is_finite() { zoom.clamp(0.0, 1.0) } else { 1.0 };
        if (self.imp().media_zoom.get() - z).abs() < f64::EPSILON {
            return;
        }
        self.imp().media_zoom.set(z);
        self.queue_draw();
    }

    /// 媒体内缩比例（0.96 = 四周留 4% 余量）
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

    pub fn set_image(&self, texture: Option<gdk::Texture>, caption: &str) {
        {
            let imp = self.imp();
            *imp.texture.borrow_mut() = texture;
            *imp.caption.borrow_mut() = caption.to_string();
            imp.logged.set(false);
        }
        self.queue_draw();
    }

    pub fn set_video_frame(&self, texture: Option<gdk::Texture>) {
        *self.imp().texture.borrow_mut() = texture;
        self.queue_draw();
    }

    /// 相框遮罩（cairo surface）
    pub fn set_frame_mask(&self, mask: Option<Rc<cairo::ImageSurface>>) {
        *self.imp().frame_mask.borrow_mut() = mask;
        self.queue_draw();
    }

    /// PNG 相框（同时给出内孔遮罩；None 内孔 = 叠图模式）
    pub fn set_frame_texture_with_hole(
        &self,
        texture: Option<gdk::Texture>,
        hole: Option<crate::frame::InnerHole>,
    ) {
        self.imp().inner_hole.set(hole);
        *self.imp().frame.borrow_mut() = texture;
        self.queue_draw();
    }

    /// 缩放预览：目标**媒体**尺寸（0,0 = 结束预览）
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

    /// 拖动绘制偏移
    pub fn set_visual_offset(&self, dx: f64, dy: f64) {
        let imp = self.imp();
        if imp.offset_x.get() == dx && imp.offset_y.get() == dy {
            return;
        }
        imp.offset_x.set(dx);
        imp.offset_y.set(dy);
        self.queue_draw();
    }

    pub fn set_running(&self, running: bool) {
        self.imp().controls.set_running(running);
        self.queue_draw();
    }

    pub fn set_click_handler(&self, cb: impl Fn(HitZone) + 'static) {
        *self.imp().on_click.borrow_mut() = Some(Rc::new(cb));
    }

    pub fn set_drag_handler(&self, cb: impl Fn(crate::controls::DragPhase) + 'static) {
        *self.imp().on_drag.borrow_mut() = Some(Rc::new(cb));
    }

    /// 每次绘制后回调（用于同步输入区域）
    pub fn set_size_hook(&self, cb: impl Fn() + 'static) {
        *self.imp().size_hook.borrow_mut() = Some(Rc::new(cb));
    }

    pub fn caption(&self) -> String {
        self.imp().caption.borrow().clone()
    }
}

impl Default for MediaView {
    fn default() -> Self {
        Self::new()
    }
}
