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

/// 媒体裁剪相对内孔的内缩像素。
///
/// **必须是 0**：早前为"把硬边藏进不透明环"留了 1px 内缩，但遮罩本身已按相框真实
/// alpha 做抗锯齿过渡，不再需要内缩；而这 1px 带子里的媒体既被裁掉又被遮罩擦除，
/// 于是**在媒体四周透出桌面/壁纸**，看起来就是一圈"十字细线"（用户报的现象）。
const MASK_INSET: f64 = 0.0;

/// 媒体向相框内沿"探入"的像素数。
/// 相框 PNG 的开口边缘常带半透明斜边（抗锯齿/高光），那一圈若后面没有内容，
/// 桌面就会沿开口边缘透出一条 1px 亮线（"十字细线"）。让媒体多探入几像素、
/// 由相框斜边盖住即可；值过大会让照片钻到框体上，所以取保守的 3px。
const MEDIA_EDGE_BLEED: f64 = 3.0;

/// 转场效果（Copy，避免每帧分配 String）
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Effect {
    Fade,
    KenBurns,
    PullBack,
    Slide,
    Roll,
    PageFlip,
}

impl Effect {
    pub fn parse(s: &str) -> Self {
        match s {
            "ken_burns" => Effect::KenBurns,
            "pull_back" => Effect::PullBack,
            "slide" => Effect::Slide,
            "roll" => Effect::Roll,
            "page_flip" => Effect::PageFlip,
            _ => Effect::Fade,
        }
    }
}

/// 一次转场在进度 p 时的绘制参数。
/// 所有数值只驱动 snapshot 的 translate/scale/opacity/clip —— 无 CPU 像素运算。
struct TransFrame {
    /// 旧图/新图不透明度
    prev_a: f64,
    cur_a: f64,
    /// 旧图/新图平移（逻辑像素）
    prev_t: (f64, f64),
    cur_t: (f64, f64),
    /// 旧图/新图缩放（x,y 分开：翻页需要只压 x）
    prev_s: (f64, f64),
    cur_s: (f64, f64),
    /// 缩放锚点（媒体矩形的比例位置；0,0 = 左上，0.5,0.5 = 中心）
    /// 翻页要挂在**左边缘**压缩才像掀页
    prev_anchor: (f64, f64),
    cur_anchor: (f64, f64),
    /// 新图的揭示裁剪（相对媒体矩形的比例 x,y,w,h）；None = 不裁
    cur_clip: Option<(f64, f64, f64, f64)>,
}

/// 转场动画状态
pub struct TransAnim {
    /// 本次效果
    pub kind: Effect,
    start: std::time::Instant,
    duration: std::time::Duration,
}

impl TransAnim {
    /// 0.0 → 1.0（ease-out，尾段更柔和）
    fn progress(&self) -> f64 {
        let d = self.duration.as_secs_f64().max(0.001);
        let t = (self.start.elapsed().as_secs_f64() / d).clamp(0.0, 1.0);
        1.0 - (1.0 - t).powi(3)
    }
    fn done(&self) -> bool {
        self.start.elapsed() >= self.duration
    }

    /// 按效果算出本帧参数。`mw/mh` = 媒体矩形尺寸（逻辑像素）。
    fn frame(&self, mw: f64, mh: f64) -> TransFrame {
        let p = self.progress();
        let mut f = TransFrame {
            prev_a: 1.0 - p,
            cur_a: p,
            prev_t: (0.0, 0.0),
            cur_t: (0.0, 0.0),
            prev_s: (1.0, 1.0),
            cur_s: (1.0, 1.0),
            prev_anchor: (0.5, 0.5),
            cur_anchor: (0.5, 0.5),
            cur_clip: None,
        };
        match self.kind {
            // 纯淡入淡出：最贴合"回忆"的克制感，也最省
            Effect::Fade => {}
            // 缓慢推近：旧图继续放大淡出、新图从略大收回 1.0 → 画面像"持续在靠近"
            Effect::KenBurns => {
                f.prev_s = (1.0 + 0.04 * p, 1.0 + 0.04 * p);
                f.cur_s = (1.06 - 0.06 * p, 1.06 - 0.06 * p);
            }
            // 拉远：新图从更大处收回，旧图略缩 → 收束感
            Effect::PullBack => {
                f.prev_s = (1.0 - 0.03 * p, 1.0 - 0.03 * p);
                f.cur_s = (1.10 - 0.10 * p, 1.10 - 0.10 * p);
            }
            // 横向推动：旧图左退、新图右入（整屏推，无空隙）
            Effect::Slide => {
                f.prev_a = 1.0;
                f.cur_a = 1.0;
                f.cur_t = ((1.0 - p) * mw, 0.0);
                f.prev_t = (-p * mw, 0.0);
            }
            // 垂直卷帘：新图自上而下揭开，旧图下移
            Effect::Roll => {
                f.prev_a = 1.0;
                f.cur_a = 1.0;
                f.cur_t = (0.0, -(1.0 - p) * mh);
                f.prev_t = (0.0, p * mh);
            }
            // 翻页：旧页以**左边缘**为轴横向压扁（掀起来），新页自左向右揭开
            Effect::PageFlip => {
                f.prev_a = 1.0;
                f.cur_a = 1.0;
                f.prev_anchor = (0.0, 0.5);
                f.prev_s = (1.0 - p, 1.0);
                f.cur_clip = Some((0.0, 0.0, p, 1.0));
            }
        }
        f
    }
}
/// 媒体绘制区域相对中心片（CENTER）内缩的像素数。
///
/// **保持 0**（媒体正好铺满中心片）。两个必须同时成立的条件：
///   ① 媒体不得越过中心片边界 —— 模型内孔比 PNG 真实透明区**内缩约 1 个分析像素**
///      （降采样+alpha 阈值的必然结果），每条边片内缘因此带 1px 透明行：
///      遮罩不擦、相框不盖，照片的 bleed 就会沿内缘露出 1px，
///      在四角与边片的 T 型交点处连成**十字细线**（用户报的“割裂感”）。
///      把媒体严格 clip 在中心片内，物理上杜绝越界。
///   ② 媒体边缘不得半透明 —— 靠 MEDIA_EDGE_BLEED：纹理被缩放到
///      “媒体矩形 ± 3px” 再被 clip 裁回媒体矩形，所以 clip 边界上的采样点
///      距纹理自身边缘还有 3px，双线性取到的是纹理内部像素 → 完全不透明。
///      两者同时满足，就不需要早期“把边片向外扩 1px 去盖住半透明边”的做法
///      （那个做法会重新缩放每一片 → 接缝内容错位 = 十字线）。

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
        /// 转场：上一张纹理（动画期间与当前纹理同时绘制）
    pub prev_tex: RefCell<Option<gdk::Texture>>,
    /// 转场动画状态（None = 不在转场）
    pub anim: RefCell<Option<TransAnim>>,
    /// 转场 tick 代号：每次新动画 +1，旧 tick 回调发现代号变了就自行退出
    pub anim_gen: std::cell::Cell<u64>,
    /// 是否允许转场（被覆盖/隐藏时置 false，直接跳到终态，不浪费 tick）。
    /// **默认 true** —— 早期版本默认 false，而它只在覆盖状态"变化"时才被
    /// set_transition_allowed(true) 置位，初始若没有状态变化就永远不开 → 转场不生效。
    pub trans_allowed: std::cell::Cell<bool>,
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
            if std::env::var_os("PF_FORCE_HOVER").is_some() {
                self.controls.debug_force_hover();
            }
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
                // 1) 媒体：严格铺满中心片（CENTER），且不越界
                //
                // 为什么内缩 1px（而不是直接铺满、或向外扩）：
                // 中心片是相框内孔，四条边紧邻的是**边片**（不透明框体）。
                // 若媒体正好铺到中心片边界：
                //   · 纹理节点在目标矩形边缘会采到纹理外的像素 → 照片最外一圈
                //     变半透明，透出桌面（三十五排查的“1px 亮线”）；
                //   · 为了遮这个半透明边而把边片向外扩 1px（早期做法）→ 每片被
                //     重新缩放，相邻两片内容错位 → 接缝处 1px 断层 = “十字线”。
                // 严格 clip 在中心片矩形内（不越界）→ 边片内缘那 1px 透明行
                // 里不会有任何媒体 → 十字细线从根上消失。
                snapshot.save();
                let clip = media_rect;
                snapshot.push_clip(&clip);
                // 转场：按效果算出本帧的 translate/scale/opacity/clip 再画旧/新两张。
                // 开销全在合成器侧（都是已有 GPU 纹理 + 快照变换），**不做 CPU 像素运算**。
                let g = MEDIA_EDGE_BLEED as f32;
                let bleed = gtk::graphene::Rect::new(
                    media_rect.x() - g,
                    media_rect.y() - g,
                    media_rect.width() + g * 2.0,
                    media_rect.height() + g * 2.0,
                );
                let tframe = self
                    .anim
                    .borrow()
                    .as_ref()
                    .map(|a| a.frame(media_rect.width() as f64, media_rect.height() as f64));
                if let (Some(tf), Some(prev)) = (tframe.as_ref(), self.prev_tex.borrow().clone()) {
                    if tf.prev_a > 0.004 {
                        snapshot.save();
                        snapshot.push_opacity(tf.prev_a);
                        if (tf.prev_t.0, tf.prev_t.1) != (0.0, 0.0) {
                            snapshot.translate(&gtk::graphene::Point::new(
                                tf.prev_t.0 as f32,
                                tf.prev_t.1 as f32,
                            ));
                        }
                        if (tf.prev_s.0 - 1.0).abs() > 1e-6 || (tf.prev_s.1 - 1.0).abs() > 1e-6 {
                            let ax = media_rect.x() as f64 + tf.prev_anchor.0 * media_rect.width() as f64;
                            let ay = media_rect.y() as f64 + tf.prev_anchor.1 * media_rect.height() as f64;
                            snapshot.translate(&gtk::graphene::Point::new(ax as f32, ay as f32));
                            snapshot.scale(tf.prev_s.0 as f32, tf.prev_s.1 as f32);
                            snapshot.translate(&gtk::graphene::Point::new(-ax as f32, -ay as f32));
                        }
                        snapshot.append_texture(&prev, &bleed);
                        snapshot.restore();
                    }
                }
                if let Some(tex) = self.texture.borrow().clone() {
                    snapshot.save();
                    if let Some(tf) = tframe.as_ref() {
                        snapshot.push_opacity(tf.cur_a);
                        if let Some((fx, fy, fw, fh)) = tf.cur_clip {
                            // 揭示裁剪：按比例裁在媒体矩形内（翻页用）
                            let clip = gtk::graphene::Rect::new(
                                media_rect.x() + (fx as f32) * media_rect.width(),
                                media_rect.y() + (fy as f32) * media_rect.height(),
                                (fw as f32) * media_rect.width(),
                                (fh as f32) * media_rect.height(),
                            );
                            snapshot.push_clip(&clip);
                        }
                        if (tf.cur_t.0, tf.cur_t.1) != (0.0, 0.0) {
                            snapshot.translate(&gtk::graphene::Point::new(
                                tf.cur_t.0 as f32,
                                tf.cur_t.1 as f32,
                            ));
                        }
                        if (tf.cur_s.0 - 1.0).abs() > 1e-6 || (tf.cur_s.1 - 1.0).abs() > 1e-6 {
                            let ax = media_rect.x() as f64 + tf.cur_anchor.0 * media_rect.width() as f64;
                            let ay = media_rect.y() as f64 + tf.cur_anchor.1 * media_rect.height() as f64;
                            snapshot.translate(&gtk::graphene::Point::new(ax as f32, ay as f32));
                            snapshot.scale(tf.cur_s.0 as f32, tf.cur_s.1 as f32);
                            snapshot.translate(&gtk::graphene::Point::new(-ax as f32, -ay as f32));
                        }
                    }
                    // 绘制到比 clip 再大 MEDIA_EDGE_BLEED 的范围（clip 会裁掉多余的）：
                    // 纹理被缩放到 bleed 矩形，clip 边界离纹理自身边缘还有 2px，
                    // 双线性采样取到的是纹理内部像素 → 照片边缘不半透明。
                    snapshot.append_texture(&tex, &bleed);
                    if tframe.is_some() {
                        if tframe.as_ref().is_some_and(|t| t.cur_clip.is_some()) {
                            snapshot.pop(); // clip
                        }
                        snapshot.pop(); // opacity
                    }
                    snapshot.restore();
                }
                snapshot.pop();
                snapshot.restore();

                // 2) 遮罩：九片各自 DestOut（按相框真实 alpha、抗锯齿、绝不漏出框外）
                //
                // 遮罩矩形与下面第 3 步的相框矩形**完全相同**（都用 layout.dst），
                // 两步边界严格对齐，媒体不可能从相框与遮罩的错位缝里透出桌面。
                //
                // 注意：早期版本这里给中心片开过 1px 外扩（与相框的 1px overlap
                // 各扩各的）—— 那正是「十字线」的来源之一，已一并去掉。
                // 媒体纹理自身的边缘补偿由上面的 MEDIA_EDGE_BLEED 负责。
                {
                    // **九片必须画进同一个 cairo 节点**（单次 append_cairo）。
                    //
                    // 每片各自 append_cairo 时，GSK 会为每片生成一个独立渲染节点，
                    // 节点边界在**分数设备像素**上（本机缩放 1.6×：逻辑 112 → 设备 179.2）
                    // → 相邻节点各自做像素对齐、取整方向不同 → **每条切片边界出现 1px
                    // 亮线/缝隙**（用户报的“四角十字线”“拼接没对齐”）。
                    // 已实测：切片 surface 的内容完全正确（第一列颜色与源图一致），
                    // 所以问题只在节点边界，不在切片。
                    let cr = snapshot.append_cairo(&frame_rect);
                    let _ = cr.save();
                    cr.set_operator(cairo::Operator::DestOut);
                    cr.set_antialias(cairo::Antialias::None);
                    for i in 0..9 {
                        let d = layout.dst[i];
                        if d.is_empty() {
                            continue;
                        }
                        let (dx, dy, dw, dh) = (d.x as f64, d.y as f64, d.w as f64, d.h as f64);
                        let surf = sl.mask[i].clone();
                        let (sw_, sh_) = (surf.width().max(1) as f64, surf.height().max(1) as f64);
                        let _ = cr.save();
                        cr.translate(dx, dy);
                        cr.scale(dw / sw_, dh / sh_);
                        let _ = cr.set_source_surface(&*surf, 0.0, 0.0);
                        let pat = cr.source();
                        let _ = pat.set_filter(cairo::Filter::Good);
                        cr.rectangle(0.0, 0.0, sw_, sh_);
                        let _ = cr.fill();
                        let _ = cr.restore();
                    }
                    let _ = cr.restore();
                }
                // 3) 相框：九片叠在最上层（四角等比、四边拉伸）
                //
                // **九片必须严格按 layout.dst 的整数矩形逐片拼接，不做任何外扩/重叠。**
                //
                // 为什么不能外扩：layout_raw 已用"整数累加"生成 9 个矩形
                // （x0→x1→x2→x3、y0→y1→y2→y3），相邻片天然像素级相接。
                // 之前给每片加了 1px overlap 去接缝，结果适得其反：
                //   * 每片被放大 1px 绘制，缩放系数从 dw/sw 变成 (dw+1)/sw；
                //   * 相邻两片内容来自 PNG 的**不同区域**，重叠 1px 就等于
                //     把 A 片的边缘内容盖到 B 片上 → 接缝处出现 1px 内容断层；
                //   * T 型交点（角片与边片相接）处两条断层相交 → 看起来就是
                //     一条"十字线"把相框割成九宫格（用户报的"割裂"）。
                //
                // 不留缝靠三件事，不靠外扩：
                //   ① layout_raw 的整数累加（几何层已保证，勿改回浮点）
                //   ② set_antialias(None)（否则共享边被半透明化 → 发丝缝）
                //   ③ translate 到整数原点后 fill 整数矩形，cairo 光栅化正好
                //      填满 [d.x, d.x+d.w) × [d.y, d.y+d.h)，与邻片无缝相接
                //
                // 遮罩（上面第 2 步）用的是**完全相同**的矩形，两者边界严格对齐；
                // 若将来只改其中一处，必须同步改另一处，否则又会出现透光线。
                {
                    let cr = snapshot.append_cairo(&frame_rect);
                    let _ = cr.save();
                    // 路径关闭抗锯齿：否则每片边缘被半透明化 → 接缝发丝线
                    cr.set_antialias(cairo::Antialias::None);
                    for i in [4usize, 0, 1, 2, 3, 5, 6, 7, 8] {
                        let d = layout.dst[i];
                        if d.is_empty() {
                            continue;
                        }
                        let (dx, dy, dw, dh) = (
                            d.x as f64,
                            d.y as f64,
                            d.w as f64,
                            d.h as f64,
                        );
                        let surf = sl.frame[i].clone();
                        let (sw_, sh_) = (surf.width().max(1) as f64, surf.height().max(1) as f64);
                        let _ = cr.save();
                        cr.translate(dx, dy);
                        cr.scale(dw / sw_, dh / sh_);
                        let _ = cr.set_source_surface(&*surf, 0.0, 0.0);
                        let pat = cr.source();
                        let _ = pat.set_filter(cairo::Filter::Good);
                        // 边缘钳制：双线性不去取源矩形之外的透明像素
                        let _ = pat.set_extend(cairo::Extend::Pad);
                        cr.rectangle(0.0, 0.0, sw_, sh_);
                        let _ = cr.fill();
                        let _ = cr.restore();
                    }
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
                // **先建 cairo 节点，再平移上下文**（顺序不能反）。
                //
                // `append_cairo` 的**裁剪框**取“调用那一刻”的快照变换：
                // 此刻快照只被拖动偏移 (ox,oy) 平移过，裁剪框正好落在相框处 ✓。
                // 若反过来写成 `snapshot.translate(fx,fy)` → `append_cairo(&frame_rect)`，
                // 裁剪框会变成 **(2fx, 2fy, fw, fh)** —— 控制层左/上各被裁掉 fx/fy 像素。
                // 相框缩小后媒体中心往左上移动，正好移进被裁掉的区域 →
                // 用户报的“播放/暂停按钮消失或只显示一半”。
                let cr = snapshot.append_cairo(&frame_rect);
                cr.translate(fx as f64, fy as f64);
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

            // 悬停控制层（播放/暂停按钮落在底图正中），绘制用表面坐标
            let (cmx, cmy, cmw, cmh) = self.media_rect.get();
            let layout =
                ControlLayout::with_media(fw, fh, cmx as f64, cmy as f64, cmw as f64, cmh as f64);
            let cr = snapshot.append_cairo(&frame_rect);
            crate::controls::paint(&cr, &layout, &self.controls);

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

impl imp::MediaView {

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
    pub fn new() -> Self {
        let view: Self = glib::Object::builder().build();
        view.imp().media_zoom.set(1.0);
        view.imp().frame_grow.set(0.05); // 相框默认比素材大 5%
        // 转场默认**允许**；只有被覆盖/隐藏时才由 set_transition_allowed(false) 关掉。
        // 默认 false 会导致：初始若没有覆盖状态"变化"事件，就永远不被置位 → 转场全程不生效。
        view.imp().trans_allowed.set(true);
        view.add_css_class("omaframe-view");
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
        self.set_image_transitioned(texture, caption, None);
    }

    /// 设置媒体，并可指定本次切换使用的转场。
    /// `tr = None` 表示不转场（视频切换 / 被覆盖 / 配置关闭时走这里）。
    ///
    /// 转场只在**切换瞬间**播放：计时由本视图自己驱动，
    /// **不影响自动轮换的间隔**（轮换计时器在别处，不因动画重启）。
    pub fn set_image_transitioned(
        &self,
        texture: Option<gdk::Texture>,
        caption: &str,
        tr: Option<(&str, u32)>,
    ) {
        let imp = self.imp();
        let can = tr.is_some() && imp.trans_allowed.get() && imp.anim.borrow().is_none();
        if can {
            let cur = imp.texture.borrow().clone();
            if let (Some(cur), Some(_new)) = (cur, texture.clone()) {
                let (kind, ms) = tr.unwrap();
                *imp.prev_tex.borrow_mut() = Some(cur);
                *imp.anim.borrow_mut() = Some(TransAnim {
                    kind: Effect::parse(kind),
                    start: std::time::Instant::now(),
                    duration: std::time::Duration::from_millis(ms.max(1) as u64),
                });
                imp.anim_gen.set(imp.anim_gen.get().wrapping_add(1));
                self.start_anim_tick();
            } else {
                // 首张 / 没有上一张：没有可淡出的对象，直接显示
                *imp.prev_tex.borrow_mut() = None;
                *imp.anim.borrow_mut() = None;
            }
        }
        {
            *imp.texture.borrow_mut() = texture;
            *imp.caption.borrow_mut() = caption.to_string();
            imp.logged.set(false);
        }
        self.queue_draw();
    }

    /// 16ms tick 驱动动画（与 controls.rs 同一范式）。
    /// 用代号（gen）识别过期回调，避免叠加多个 tick；WeakRef 避免延长 widget 生命周期。
    fn start_anim_tick(&self) {
        let weak = glib::WeakRef::new();
        weak.set(Some(self));
        let gen = self.imp().anim_gen.get();
        glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
            let Some(v) = weak.upgrade() else {
                return glib::ControlFlow::Break; // widget 已销毁
            };
            let imp = v.imp();
            if imp.anim_gen.get() != gen {
                return glib::ControlFlow::Break; // 被更新的动画接管
            }
            let running = match imp.anim.borrow().as_ref() {
                Some(a) if imp.trans_allowed.get() && !a.done() => true,
                _ => false,
            };
            if !running {
                // 结束 / 被覆盖 / 无动画：清干净并停止 tick
                imp.anim_gen.set(imp.anim_gen.get().wrapping_add(1));
                *imp.anim.borrow_mut() = None;
                *imp.prev_tex.borrow_mut() = None;
                v.queue_draw();
                return glib::ControlFlow::Break;
            }
            v.queue_draw();
            glib::ControlFlow::Continue
        });
    }

    /// 立即结束转场（保留当前纹理，丢弃上一张）
    pub fn settle_transition(&self) {
        let imp = self.imp();
        imp.anim_gen.set(imp.anim_gen.get().wrapping_add(1));
        *imp.anim.borrow_mut() = None;
        *imp.prev_tex.borrow_mut() = None;
    }

    /// 是否允许转场。被覆盖 / 隐藏时置 false（省电，且不会"切回来正在转场"）
    pub fn set_transition_allowed(&self, ok: bool) {
        let imp = self.imp();
        imp.trans_allowed.set(ok);
        if !ok {
            self.settle_transition();
            self.queue_draw();
        }
    }

    pub fn set_video_frame(&self, texture: Option<gdk::Texture>) {
        // 视频走自己的帧管线，**不做转场**（视频自身已有淡入淡出，叠加会脏）
        self.settle_transition();
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
mod trans_tests {
    use super::*;

    fn anim(kind: Effect) -> TransAnim {
        TransAnim {
            kind,
            start: std::time::Instant::now(),
            duration: std::time::Duration::from_millis(600),
        }
    }

    /// 效果参数在 p=0/p=1 两端必须落在预期位置，且全程无 NaN、不越界。
    #[test]
    fn effect_frames_are_sane() {
        let (mw, mh) = (400.0, 300.0);
        for k in [
            Effect::Fade,
            Effect::KenBurns,
            Effect::PullBack,
            Effect::Slide,
            Effect::Roll,
            Effect::PageFlip,
        ] {
            let a = anim(k);
            // progress 是时间函数，这里直接验证 frame() 在两个端点的形状：
            // 用 mock 不了时间，所以只做"形状/范围"校验
            let f = a.frame(mw, mh);
            let nums = [
                f.prev_a, f.cur_a, f.prev_t.0, f.prev_t.1, f.cur_t.0, f.cur_t.1,
                f.prev_s.0, f.prev_s.1, f.cur_s.0, f.cur_s.1,
                f.prev_anchor.0, f.prev_anchor.1, f.cur_anchor.0, f.cur_anchor.1,
            ];
            for v in nums {
                assert!(v.is_finite(), "{k:?} 出现 NaN/Inf");
            }
            assert!((0.0..=1.0).contains(&f.prev_a), "{k:?} prev_a 越界");
            assert!((0.0..=1.0).contains(&f.cur_a), "{k:?} cur_a 越界");
            for s_ in [f.prev_s.0, f.prev_s.1, f.cur_s.0, f.cur_s.1] {
                assert!(s_ > 0.5 && s_ < 2.0, "{k:?} 缩放越界 {s_}");
            }
            for a in [f.prev_anchor.0, f.prev_anchor.1, f.cur_anchor.0, f.cur_anchor.1] {
                assert!((0.0..=1.0).contains(&a), "{k:?} 锚点越界");
            }
            if let Some((x, y, w, h)) = f.cur_clip {
                assert!(
                    (0.0..=1.0).contains(&x)
                        && (0.0..=1.0).contains(&y)
                        && (0.0..=1.0).contains(&w)
                        && (0.0..=1.0).contains(&h),
                    "{k:?} 裁剪越界"
                );
            }
    }
    }

    /// 效果名解析：未知/空串一律回落 fade（配置被手改也不至于不转场或崩）
    #[test]
    fn effect_parse_falls_back_to_fade() {
        assert_eq!(Effect::parse("fade"), Effect::Fade);
        assert_eq!(Effect::parse("slide"), Effect::Slide);
        assert_eq!(Effect::parse("roll"), Effect::Roll);
        assert_eq!(Effect::parse("page_flip"), Effect::PageFlip);
        assert_eq!(Effect::parse("ken_burns"), Effect::KenBurns);
        assert_eq!(Effect::parse("pull_back"), Effect::PullBack);
        assert_eq!(Effect::parse(""), Effect::Fade);
        assert_eq!(Effect::parse("不存在"), Effect::Fade);
    }

    /// 推动类效果在两端必须"整屏推"：起点只看得到旧图、终点只看得到新图，
    /// 中间不许出现"两边都露背景"的空档（否则会闪一条背景）。
    #[test]
    fn effect_slide_and_roll_push_full_width() {
        let (mw, mh) = (400.0, 300.0);
        for k in [Effect::Slide, Effect::Roll] {
            let f = anim(k).frame(mw, mh);
            assert!((f.prev_a - 1.0).abs() < 1e-9 && (f.cur_a - 1.0).abs() < 1e-9);
            // 横向效果：位移在 x 轴上；纵向效果：位移在 y 轴上（另一个为 0）
            if k == Effect::Slide {
                assert!(f.cur_t.1 == 0.0 && f.prev_t.1 == 0.0);
            } else {
                assert!(f.cur_t.0 == 0.0 && f.prev_t.0 == 0.0);
            }
        }
    }
}
