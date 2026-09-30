//! 媒体显示控件：自定义 GtkWidget，精确控制尺寸与绘制。
//!
//! **坐标模型（关键，别再改回去）**
//! - 控件 = 整块 layer surface = **整个显示器**，尺寸恒定、永不重建
//! - 相框左上角 `frame_x/frame_y` 直接来自配置（就是屏幕坐标）
//! - 相框尺寸 = 由 PNG 内孔最大范围 + 上限盒算出（见 `geometry::frame_size_for_box`）
//! - 媒体矩形 = 内孔最大范围 × 显示比（见 `geometry::media_rect_in_hole`）
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
        /// 几何调试日志去重键（frame+media 尺寸变了才记一条）
        pub geo_logged: Cell<u64>,
        /// 缩放预览：目标**上限盒**尺寸（0,0 = 不覆盖；缩放拖动时先预览盒）
        pub preview_box_w: Cell<i32>,
        pub preview_box_h: Cell<i32>,
        /// 拖动绘制偏移（只影响绘制，控件几何不动 → 事件坐标始终有效）
        pub offset_x: Cell<f64>,
        pub offset_y: Cell<f64>,
        /// 最近一次绘制的相框矩形（含偏移）：命中检测与输入区域用它
        pub hit_rect: Cell<(f64, f64, f64, f64)>,
        /// 相框内孔（透明区）比例（None = 无内孔，退回叠图）
        pub inner_hole: Cell<Option<crate::frame::InnerHole>>,
        /// 智能九宫格切片（Some = 走自适应路径；None = 等比缩放回退）
        pub frame_slices: RefCell<Option<Rc<crate::frame::FrameSlices>>>,
        /// 最近一次 geometry() 用的九宫格布局（绘制时复用，避免重复计算）
        pub layout: Cell<Option<crate::geometry::FrameLayout>>,
        /// 最近一次 geometry() 算出的媒体矩形（控件定位、圆角裁剪都用它）
        pub media_rect: Cell<(i32, i32, i32, i32)>,
        /// 底图圆角半径（像素，<=0 = 不圆角）
        pub corner_radius: Cell<i32>,
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

            // ===== 智能九宫格路径 =====
            let slices_now = self.frame_slices.borrow().clone();
            if let (Some(sl), Some(layout)) = (slices_now, self.layout.get()) {
                // 1) 媒体：contain 到中心片（布局已按素材比例定内容区，基本精确贴合）
                snapshot.save();
                let clip = gtk::graphene::Rect::new(
                    mx as f32,
                    my as f32,
                    mw.max(1) as f32,
                    mh.max(1) as f32,
                );
                snapshot.push_clip(&clip);
                if let Some(tex) = self.texture.borrow().clone() {
                    let (tw, th) = (tex.width().max(1) as f32, tex.height().max(1) as f32);
                    let s = (mw.max(1) as f32 / tw).min(mh.max(1) as f32 / th);
                    let (w2, h2) = (tw * s, th * s);
                    let draw = gtk::graphene::Rect::new(
                        mx as f32 + (mw.max(1) as f32 - w2) / 2.0,
                        my as f32 + (mh.max(1) as f32 - h2) / 2.0,
                        w2,
                        h2,
                    );
                    snapshot.append_texture(&tex, &draw);
                }
                snapshot.pop();
                snapshot.restore();

                // 2) 遮罩：九片各自 DestOut（按相框真实 alpha，抗锯齿、绝不漏出框外）
                for i in 0..9 {
                    let d = layout.dst[i];
                    if d.is_empty() {
                        continue;
                    }
                    let surf = sl.mask[i].clone();
                    let (sw_, sh_) = (surf.width().max(1), surf.height().max(1));
                    let rect = gtk::graphene::Rect::new(
                        d.x as f32,
                        d.y as f32,
                        d.w as f32,
                        d.h as f32,
                    );
                    let cr = snapshot.append_cairo(&rect);
                    let _ = cr.save();
                    cr.set_operator(cairo::Operator::DestOut);
                    // 与相框切片使用完全相同的整数矩形 → 擦除边界与绘制边界严格对齐
                    cr.set_antialias(cairo::Antialias::None);
                    cr.translate(d.x as f64, d.y as f64);
                    cr.scale(d.w as f64 / sw_ as f64, d.h as f64 / sh_ as f64);
                    let _ = cr.set_source_surface(&*surf, 0.0, 0.0);
                    let pat = cr.source();
                    let _ = pat.set_filter(cairo::Filter::Good);
                    cr.rectangle(0.0, 0.0, sw_ as f64, sh_ as f64);
                    let _ = cr.fill();
                    let _ = cr.restore();
                }

                // 3) 相框：九片叠在最上层（四角等比、四边拉伸）
                for i in 0..9 {
                    let d = layout.dst[i];
                    if d.is_empty() {
                        continue;
                    }
                    let surf = sl.frame[i].clone();
                    let (sw_, sh_) = (surf.width().max(1), surf.height().max(1));
                    let rect = gtk::graphene::Rect::new(
                        d.x as f32,
                        d.y as f32,
                        d.w as f32,
                        d.h as f32,
                    );
                    let cr = snapshot.append_cairo(&rect);
                    let _ = cr.save();
                    // 关键：路径**不做抗锯齿**。否则每片边缘会被半透明化，
                    // 相邻片之间就会出现一条发丝细缝（用户在花环相框上看到"绘制的线条"）。
                    // 目标矩形已经是整数且精确相接 → 关闭路径抗锯齿即可无缝。
                    // 注意：**不能**再做像素级外扩，那会遮住相邻切片、形成硬边线。
                    cr.set_antialias(cairo::Antialias::None);
                    cr.translate(d.x as f64, d.y as f64);
                    cr.scale(d.w as f64 / sw_ as f64, d.h as f64 / sh_ as f64);
                    let _ = cr.set_source_surface(&*surf, 0.0, 0.0);
                    let pat = cr.source();
                    let _ = pat.set_filter(cairo::Filter::Good);
                    cr.rectangle(0.0, 0.0, sw_ as f64, sh_ as f64);
                    let _ = cr.fill();
                    let _ = cr.restore();
                }

                // 4) 开发用 Debug Overlay（**必须显式** PHOTO_FRAME_DEBUG_OVERLAY=1）
                if debug_overlay_enabled() {
                    let cr = snapshot.append_cairo(&frame_rect);
                    cr.set_source_rgba(0.0, 0.9, 1.0, 0.9);
                    cr.set_line_width(1.0);
                    for i in 0..9 {
                        let d = layout.dst[i];
                        cr.rectangle(
                            d.x as f64 + 0.5,
                            d.y as f64 + 0.5,
                            (d.w - 1).max(1) as f64,
                            (d.h - 1).max(1) as f64,
                        );
                        let _ = cr.stroke();
                    }
                    cr.set_source_rgba(1.0, 0.2, 0.4, 0.95);
                    cr.set_dash(&[6.0, 4.0], 0.0);
                    cr.rectangle(
                        layout.media.x as f64 + 0.5,
                        layout.media.y as f64 + 0.5,
                        (layout.media.w - 1).max(1) as f64,
                        (layout.media.h - 1).max(1) as f64,
                    );
                    let _ = cr.stroke();
                    cr.set_dash(&[], 0.0);
                }

                // 悬停控制层（播放/暂停按钮落在底图正中）
                // 媒体矩形要换算成**相对相框左上角**的坐标，命中检测才在同一套系里
                let (cmx, cmy, cmw, cmh) = self.media_rect.get();
                let layout_ctl = ControlLayout::with_media(
                    fw,
                    fh,
                    (cmx - fx) as f64,
                    (cmy - fy) as f64,
                    cmw as f64,
                    cmh as f64,
                );
                snapshot.save();
                snapshot.translate(&gtk::graphene::Point::new(fx as f32, fy as f32));
                let cr = snapshot.append_cairo(&frame_rect);
                crate::controls::paint(&cr, &layout_ctl, &self.controls);
                snapshot.restore();
                snapshot.restore();
                return;
            }
            // 媒体绘制：若相框检测到内孔，裁剪到内孔（并外扩 MASK_FEATHER 藏边）
            let hole = self.inner_hole.get();
            // 媒体绘制矩形就是内孔最大范围（geometry() 已算好），这里裁剪到它
            let media_w = mw.max(1);
            let media_h = mh.max(1);
            snapshot.save();
            if let Some(h) = hole {
                let ins = MASK_INSET as f32;
                let _ = h;
                // 裁剪到媒体绘制矩形本身（= 内孔最大范围），异形部分由遮罩精确裁掉
                let ix = mx as f32;
                let iy = my as f32;
                let iw = media_w as f32;
                let ih = media_h as f32;
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

            // 底图圆角：把四角擦掉（图片/视频通用，且是抗锯齿的）
            self.paint_corner_cutouts(snapshot, mx, my, mw, mh);

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

            // 悬停控制层（播放/暂停按钮落在底图正中）
            let (cmx, cmy, cmw, cmh) = self.media_rect.get();
            let layout = ControlLayout::with_media(
                fw,
                fh,
                (cmx - fx) as f64,
                (cmy - fy) as f64,
                cmw as f64,
                cmh as f64,
            );
            snapshot.save();
            snapshot.translate(&gtk::graphene::Point::new(fx as f32, fy as f32));
            let cr = snapshot.append_cairo(&frame_rect);
            crate::controls::paint(&cr, &layout, &self.controls);
            snapshot.restore();

            snapshot.restore();
        }
    }
}

/// 开发用九宫格调试网格的开关。
///
/// 画的是切片边框 + 内容区虚线框，看起来就像相框上被画了"十字线"——
/// 曾被误当成渲染 bug 上报。因此：
/// - 必须显式设置 `PHOTO_FRAME_DEBUG_OVERLAY=1` 才显示（默认关闭）
/// - 一旦开启会在日志里打 WARN，方便区分"调试网格"和"真·渲染问题"
fn debug_overlay_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("PHOTO_FRAME_DEBUG_OVERLAY").is_some();
        if on {
            crate::warn!(
                "已开启九宫格调试网格（PHOTO_FRAME_DEBUG_OVERLAY）——仅供开发排查，正式使用请取消该环境变量"
            );
        }
        on
    })
}

/// 构造"圆角缺口"路径：整块矩形 **减去** 内接的圆角矩形（偶奇填充 → 只剩四个角）。
///
/// 用 `fill` 配合 `Operator::DestOut` 即可把底图四角擦成圆角（图片/视频通用）。
/// 这样画比"四个角分别拼路径"更不容易出错：只要保证**两个子路径方向相反**，
/// 偶奇规则就只会填到四角 ✓（单位测试会验证）。
pub fn corner_cutout_path(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.max(0.0).min(w / 2.0).min(h / 2.0);
    if r < 0.5 {
        return;
    }
    // 每一角 = 直角方块(r×r) 减去 四分之一圆；偶奇填充后正好剩"该擦的角"
    let pi = std::f64::consts::PI;
    for (cx, cy, sx, sy) in [
        (x, y, 1.0f64, 1.0f64),
        (x + w, y, -1.0, 1.0),
        (x, y + h, 1.0, -1.0),
        (x + w, y + h, -1.0, -1.0),
    ] {
        // 直角方块的两条边（从角点出发）
        cr.move_to(cx, cy);
        cr.line_to(cx + sx * r, cy);
        cr.line_to(cx, cy + sy * r);
        cr.close_path();
        // 四分之一圆（与上面两条边端点重合，偶奇规则下相互抵消）
        cr.new_sub_path();
        let (ax, ay) = (cx + sx * r, cy + sy * r);
        let (a0, a1) = if sx > 0.0 {
            (pi, pi * 1.5)
        } else {
            (0.0, pi * 0.5)
        };
        let (a0, a1) = if sy > 0.0 { (a0, a1) } else { (a1 + pi, a0 + pi) };
        cr.arc(ax, ay, r, a0, a1);
        cr.close_path();
    }
    // ③ 用"反向"填充规则：外框减去内框
    cr.set_fill_rule(cairo::FillRule::EvenOdd);
}

impl imp::MediaView {
    /// 底图圆角：把四角擦掉（图片/视频通用，都在媒体绘制之后用 DestOut）
    fn paint_corner_cutouts(
        &self,
        snapshot: &gtk::Snapshot,
        mx: i32,
        my: i32,
        mw: i32,
        mh: i32,
    ) {
        if mw <= 2 || mh <= 2 {
            return;
        }
        let r = self.effective_corner_radius(mw, mh);
        if r < 1.0 {
            return;
        }
        let rect = gtk::graphene::Rect::new(mx as f32, my as f32, mw as f32, mh as f32);
        let cr = snapshot.append_cairo(&rect);
        let _ = cr.save();
        cr.set_operator(cairo::Operator::DestOut);
        // 圆角要平滑 → 这里**开**抗锯齿（与切片填充相反：切片边界要硬，圆角要柔）
        cr.set_antialias(cairo::Antialias::Default);
        corner_cutout_path(&cr, mx as f64, my as f64, mw as f64, mh as f64, r);
        let _ = cr.fill();
        let _ = cr.restore();
    }

    /// 当前底图圆角半径（像素）
    /// - 配置 >0：直接用
    /// - 配置 -1（跟随相框）：内孔圆角 × 九宫格缩放系数
    /// - 配置 0：关闭
    fn effective_corner_radius(&self, media_w: i32, media_h: i32) -> f64 {
        let cfg = self.corner_radius.get();
        if cfg > 0 {
            return cfg as f64;
        }
        if cfg == 0 {
            return 0.0;
        }
        let Some(sl) = self.frame_slices.borrow().clone() else {
            return 0.0;
        };
        let k = self.layout.get().map(|l| l.corner_scale).unwrap_or(1.0);
        let r = sl.model.corner_radius * k;
        r.min(media_w.min(media_h) as f64 / 2.0).max(0.0)
    }


    /// (相框 x,y,w,h, 媒体 x,y,w,h)，单位 px，控件（=屏幕）坐标
    #[allow(clippy::type_complexity)]
    pub fn geometry(&self) -> (i32, i32, i32, i32, i32, i32, i32, i32) {
        // 缩放拖动时用预览盒（媒体尺寸已由"内孔几何"决定，拖动要改的是盒）
        let (pw, ph) = (self.preview_box_w.get(), self.preview_box_h.get());
        let (bw, bh) = if pw > 0 && ph > 0 {
            (pw, ph)
        } else {
            (self.box_w.get().max(16), self.box_h.get().max(16))
        };

        // ① 素材矩形：按素材比例在上限盒内取最大（换素材时尺寸随之变化）
        let (sw, sh) = if let Some(t) = self.texture.borrow().as_ref() {
            crate::geometry::fit(t.width().max(1), t.height().max(1), bw, bh)
        } else {
            (bw, bh)
        };

        // ② 相框目标盒：素材 × 外扩 grow（默认 5%）
        //    grow 均匀放大素材与相框 → 相框内不会露出空隙
        let grow = 1.0 + self.frame_grow.get().clamp(0.0, 0.5);
        let gsw = ((sw as f64) * grow).round().max(1.0) as i32;
        let gsh = ((sh as f64) * grow).round().max(1.0) as i32;
        let aspect = self.frame_aspect.get();
        // 内孔可用区 = 整片透明区域的**最大宽高范围**（不规则形状交给遮罩裁剪）
        let fit = self.inner_hole.get().map(|h| crate::geometry::HoleFit {
            x0: h.x0,
            y0: h.y0,
            x1: h.x1,
            y1: h.y1,
        });

        // ③-a **智能自适应路径**：相框可以被拉伸成任意宽高比，四角按同一比例缩放（不变形）
        let slices = self.frame_slices.borrow().clone();
        if let Some(sl) = slices.as_ref() {
            // 素材显示比：缩放后的大小就是内容区（相框会跟着收紧 → 不会露桌面）
            let zoom = self.media_zoom.get().clamp(0.0, 1.0);
            let mw = ((sw as f64) * zoom).round().max(1.0) as i32;
            let mh = ((sh as f64) * zoom).round().max(1.0) as i32;
            let grow = self.frame_grow.get().clamp(0.0, 0.5);
            let canvas = sl.model.canvas;
            // 中心片 = 内孔范围（素材铺满内孔，才不会露一圈桌面）
            let content = sl.model.hole;
            // 先按 (0,0) 算一次拿尺寸 → 夹进屏幕 → 再按最终原点算一次
            // 上限：配置里的"最大宽度/最大高度"（相框整体不得超过），且不超整块 surface
            let (sfw0, sfh0) = (self.surf_w.get().max(16), self.surf_h.get().max(16));
            let max_frame = (bw.min(sfw0), bh.min(sfh0));
            let probe =
                crate::geometry::layout_adaptive((0, 0), canvas, content, mw, mh, grow, max_frame);
            let fx0 = crate::geometry::clamp(self.frame_x.get(), 0, (sfw0 - probe.frame.w).max(0));
            let fy0 = crate::geometry::clamp(self.frame_y.get(), 0, (sfh0 - probe.frame.h).max(0));
            let layout = crate::geometry::layout_adaptive(
                (fx0, fy0),
                canvas,
                content,
                mw,
                mh,
                grow,
                max_frame,
            );
            self.layout.set(Some(layout));
            let f = layout.frame;
            let m = layout.media;
            // 控件（播放/暂停按钮）要用媒体矩形定位，必须在 return 前写入缓存
            self.media_rect.set((m.x, m.y, m.w.max(1), m.h.max(1)));
            return (f.x, f.y, f.w, f.h, m.x, m.y, m.w, m.h);
        }
        self.layout.set(None);

        // ③ 相框尺寸
        //    有内孔：让"内孔最大范围"正好等于**素材 × (1+grow)**（默认 3%）
        //    → 相框宽高随素材自适应（自适应比例与大小），且保持 PNG 自身比例
        let (mut fw, mut fh) = match fit {
            Some(f) => crate::geometry::frame_size_for_box(f, aspect, gsw as f64, gsh as f64),
            // 叠图模式（无内孔）：按 PNG 比例 fit 进"素材 × 外扩"目标盒
            None if aspect > 0.01 => crate::geometry::fit(
                (aspect * 10_000.0).round() as i32,
                10_000,
                gsw,
                gsh,
            ),
            None => (gsw, gsh),
        };
        // 相框不能比整块 surface 还大（否则被裁掉一半）：按 PNG 比例缩到刚好装下
        let (sfw, sfh) = (self.surf_w.get().max(16), self.surf_h.get().max(16));
        if fw > sfw || fh > sfh {
            let (cw, ch) = if aspect > 0.01 {
                crate::geometry::fit(
                    (aspect * 10_000.0).round() as i32,
                    10_000,
                    sfw,
                    sfh,
                )
            } else {
                (sfw, sfh)
            };
            fw = cw;
            fh = ch;
        }
        // 相框左上角 = 配置里的 window.x/y
        //    （拖动、停靠、输入区域、可见性判定都以"相框左上角"为准，这里必须一致；
        //      之前把 x/y 当素材左上角再居中，相框一大就整体偏出屏幕）
        let fx = crate::geometry::clamp(self.frame_x.get(), 0, (sfw - fw).max(0));
        let fy = crate::geometry::clamp(self.frame_y.get(), 0, (sfh - fh).max(0));

        // ④ 素材矩形：内孔可用区 × 显示比（1.0 = 铺满内孔，不裁切素材）
        let (mx, my, mw, mh) = match fit {
            Some(f) => crate::geometry::media_rect_in_hole(
                fx,
                fy,
                fw,
                fh,
                f,
                self.media_zoom.get(),
            ),
            // 叠图模式：把素材再 fit 进相框后居中（**不能**直接按上限盒放，否则会溢出相框）
            None => {
                let (iw, ih) = crate::geometry::fit(sw, sh, fw, fh);
                crate::geometry::place_media(fx, fy, fw, fh, iw, ih, self.media_zoom.get())
            }
        };
        // 媒体矩形缓存（控件定位、圆角裁剪都用它）
        self.media_rect.set((mx, my, mw.max(1), mh.max(1)));

        // 几何变化时记一条调试日志（尺寸不变就不记，避免每帧刷屏）
        let key = ((fw as u64) << 48)
            | ((fh as u64) << 32)
            | ((mw as u64) << 16)
            | (mh as u64);
        if self.geo_logged.replace(key) != key {
            crate::debug!(
                "几何：素材 {sw}x{sh} → 相框 {fw}x{fh} @({fx},{fy})，媒体 {mw}x{mh} @({mx},{my})，内孔 {:?}",
                fit.map(|f| (
                    (f.x0 * 1000.0).round() / 1000.0,
                    (f.y0 * 1000.0).round() / 1000.0,
                    (f.x1 * 1000.0).round() / 1000.0,
                    (f.y1 * 1000.0).round() / 1000.0,
                ))
            );
        }
        (fx, fy, fw, fh, mx, my, mw, mh)
    }
}

glib::wrapper! {
    pub struct MediaView(ObjectSubclass<imp::MediaView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MediaView {
    /// 底图圆角半径：-1 跟随相框 / 0 关闭 / >0 固定像素
    pub fn set_corner_radius(&self, r: i32) {
        self.imp().corner_radius.set(r);
        self.queue_draw();
    }

    pub fn new() -> Self {
        let view: Self = glib::Object::builder().build();
        view.imp().media_zoom.set(1.0);
        view.imp().frame_grow.set(0.05); // 相框默认比素材大 5%
        view.add_css_class("photo-frame-view");
        // 控制层淡入淡出必须自己请求重绘：GTK 不会因为 Cell 变化就重画。
        // 少了这一句，动画只在"别的重绘顺便带上"时才可见，
        // 光标停下后最后一帧（透明度=0）永远刷不出来 → 按钮留在屏幕上（用户报的 bug）。
        {
            let weak: glib::WeakRef<MediaView> = glib::WeakRef::new();
            weak.set(Some(&view));
            view.imp().controls.set_redraw_hook(move || {
                if let Some(v) = weak.upgrade() {
                    v.queue_draw();
                }
            });
        }
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
            let (mx, my, mw, mh) = imp.media_rect.get();
            let layout = ControlLayout::with_media(
                fw as i32,
                fh as i32,
                (mx - fx as i32) as f64,
                (my - fy as i32) as f64,
                mw as f64,
                mh as f64,
            );
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
        let (mx, my, mw, mh) = self.imp().media_rect.get();
        let layout = ControlLayout::with_media(
            fw as i32,
            fh as i32,
            (mx - fx as i32) as f64,
            (my - fy as i32) as f64,
            mw as f64,
            mh as f64,
        );
        let zone = layout.hit(x - fx, y - fy);
        // 控制层显隐**以指针实际位置为准**（而不只靠 enter/leave）：
        // 只要指针移出相框矩形就收起。
        // 为什么必须这样：按下/拖动时输入区域会临时扩到整屏，此时指针移出相框
        // 不会再收到 leave 事件，hover 会冻结在"显示" → 按钮一直挂着不收
        // （用户报的"点击暂停后按钮一直显示"）。
        // 落在框外时 hit() 返回 None，正好当判据。
        self.imp().controls.set_hover(zone != HitZone::None);
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

    /// 完整几何（相框矩形 + 媒体矩形）——给解码尺寸等外部逻辑用
    pub fn media_geometry(&self) -> (i32, i32, i32, i32, i32, i32, i32, i32) {
        self.imp().geometry()
    }

    /// 媒体尺寸（相框内实际显示的照片大小）
    pub fn media_size(&self) -> (i32, i32) {
        let (.., mw, mh) = self.imp().geometry();
        (mw, mh)
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
    /// 缩放拖动预览：临时改上限盒（松手才写配置）
    pub fn set_preview_box(&self, w: i32, h: i32) {
        self.imp().preview_box_w.set(w.max(0));
        self.imp().preview_box_h.set(h.max(0));
    }

    /// 设置九宫格切片（None = 回退到整图等比缩放）
    pub fn set_frame_slices(&self, slices: Option<Rc<crate::frame::FrameSlices>>) {
        *self.imp().frame_slices.borrow_mut() = slices;
        self.imp().layout.set(None);
        self.queue_draw();
    }

    pub fn set_frame_texture_with_hole(
        &self,
        texture: Option<gdk::Texture>,
        hole: Option<crate::frame::InnerHole>,
    ) {
        self.imp().inner_hole.set(hole);
        *self.imp().frame.borrow_mut() = texture;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 圆角缺口路径：填充后四角 alpha 应被擦掉，四边中心保持不透明
    #[test]
    fn corner_cutout_erases_only_the_corners() {
        let (w, h, r) = (100i32, 80i32, 20.0f64);
        let data = vec![255u8; (w * h * 4) as usize];
        let mut surf = cairo::ImageSurface::create_for_data(
            data,
            cairo::Format::ARgb32,
            w,
            h,
            w * 4,
        )
        .unwrap();
        {
            let cr = cairo::Context::new(&surf).unwrap();
            cr.set_operator(cairo::Operator::DestOut);
            cr.set_antialias(cairo::Antialias::Default);
            corner_cutout_path(&cr, 0.0, 0.0, w as f64, h as f64, r);
            cr.fill().unwrap();
        }
        surf.flush();
        let stride = surf.stride() as usize;
        let data = surf.data().expect("读回 surface 数据");
        let bytes: &[u8] = &data;
        let alpha = |x: i32, y: i32| -> u8 {
            let o = (y as usize) * stride + (x as usize) * 4 + 3;
            bytes[o]
        };
        // 四角被擦掉
        assert!(alpha(0, 0) < 40, "左上角应被擦掉: {}", alpha(0, 0));
        assert!(alpha(w - 1, 0) < 40, "右上角应被擦掉: {}", alpha(w - 1, 0));
        assert!(alpha(0, h - 1) < 40, "左下角应被擦掉: {}", alpha(0, h - 1));
        assert!(alpha(w - 1, h - 1) < 40, "右下角应被擦掉: {}", alpha(w - 1, h - 1));
        // 边中心保持
        assert!(alpha(w / 2, 0) > 200, "上边中心应保留");
        assert!(alpha(0, h / 2) > 200, "左边中心应保留");
        assert!(alpha(w / 2, h - 1) > 200, "下边中心应保留");
        assert!(alpha(w - 1, h / 2) > 200, "右边中心应保留");
        // 圆角内侧（沿对角线）应该是通的
        let diag = alpha((r as i32) + 2, (r as i32) + 2);
        assert!(diag > 200, "圆角内侧应保留（圆角不能切太狠）: {diag}");
    }

    #[test]
    fn corner_cutout_zero_radius_is_noop() {
        let (w, h) = (40i32, 40i32);
        let mut surf = cairo::ImageSurface::create_for_data(
            vec![255u8; (w * h * 4) as usize],
            cairo::Format::ARgb32,
            w,
            h,
            w * 4,
        )
        .unwrap();
        {
            let cr = cairo::Context::new(&surf).unwrap();
            cr.set_operator(cairo::Operator::DestOut);
            corner_cutout_path(&cr, 0.0, 0.0, w as f64, h as f64, 0.0);
            cr.fill().unwrap();
        }
        surf.flush();
        let data = surf.data().expect("读回 surface 数据");
        let bytes: &[u8] = &data;
        let alpha0 = bytes[3];
        assert!(alpha0 > 200, "半径 0 时不应擦任何像素，实际 {alpha0}");
    }
}
