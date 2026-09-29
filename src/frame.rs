//! PNG 相框：**遮罩化**渲染 —— 媒体只显示在相框的内孔里，不会漏出矩形边角。
//!
//! 为什么不能"直接叠图"：相框若为**异形/不规则外轮廓**（斜切角、波浪边、圆角过大……），
//! 媒体是个矩形，叠上去会从 PNG 的透明缺口漏出**矩形边角**，很不美观。
//!
//! 算法（`build_hole_mask`）：
//! 1. 扫描 alpha，**从四边洪泛**透明像素 —— 能到达的是「外部」，到不了的才是「内孔」
//!    （仅靠"透明像素包围盒"无法区分「圆孔四角」和「外轮廓缺口」，必须洪泛）
//! 2. 允许显示媒体的区域 = 非外部透明（相框本体 ∪ 内孔）；媒体最终被相框 PNG 盖住，
//!    真正要擦掉的只有**外轮廓之外的透明缺口**
//! 3. 遮罩整体做一次 3×3 盒式模糊 → 外轮廓边缘 1px 羽化，媒体与相框自然衔接（半透明渐变观感）
//! 4. 运行时用 cairo `DestOut` 把媒体擦成遮罩形状，再把相框 PNG 叠在最上层
//!
//! 若 PNG 没有内孔（纯装饰框），自动退回原来的叠图模式。

use gdk_pixbuf::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

/// alpha 低于该值视为透明
const ALPHA_CUT: u8 = 24;
/// 内孔面积至少占整图这么大才认为"有内孔"
const MIN_HOLE_AREA: f64 = 0.06;

/// 内孔（相对 PNG 尺寸的比例 0..1）
#[derive(Debug, Clone, Copy)]
pub struct InnerHole {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl InnerHole {
    pub fn width(&self) -> f64 {
        (self.x1 - self.x0).max(0.0)
    }
    pub fn height(&self) -> f64 {
        (self.y1 - self.y0).max(0.0)
    }
    pub fn area_ratio(&self) -> f64 {
        self.width() * self.height()
    }
}

/// 遮罩（灰度）+ 内孔包围盒
pub struct HoleMask {
    /// 灰度图：255 = 允许显示媒体，0 = 擦掉
    pub mask: gdk_pixbuf::Pixbuf,
    pub hole: InnerHole,
}

pub struct FrameRenderer {
    #[allow(dead_code)]
    path: PathBuf,
    /// 原始相框（未缩放）
    source: RefCell<Option<gdk_pixbuf::Pixbuf>>,
    /// 已缓存的相框纹理：(逻辑尺寸, 纹理)
    cache: RefCell<Option<((i32, i32), gdk::Texture)>>,
    /// 遮罩 surface 缓存：(逻辑尺寸, surface)
    mask_cache: RefCell<Option<((i32, i32), std::rc::Rc<cairo::ImageSurface>)>>,
    /// HiDPI 上限倍数
    scale: Cell<f64>,
    /// 内孔（None = 无内孔，走叠图）
    inner: Cell<Option<InnerHole>>,
    /// 遮罩灰度图
    mask: RefCell<Option<gdk_pixbuf::Pixbuf>>,
}

impl FrameRenderer {
    pub fn new(path: &Path, scale: f64) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let loader = gdk_pixbuf::PixbufLoader::new();
        loader.write(&bytes).ok()?;
        loader.close().ok()?;
        let pixbuf = loader.pixbuf()?;
        let built = build_hole_mask(&pixbuf);
        match built.as_ref() {
            Some(m) => crate::debug!(
                "相框 {} ({}x{})：内孔 {:.1}%（x {:.2}~{:.2} y {:.2}~{:.2}），启用遮罩",
                path.to_string_lossy(),
                pixbuf.width(),
                pixbuf.height(),
                m.hole.area_ratio() * 100.0,
                m.hole.x0,
                m.hole.x1,
                m.hole.y0,
                m.hole.y1
            ),
            None => crate::debug!(
                "相框 {} ({}x{})：无内孔，按叠图处理",
                path.to_string_lossy(),
                pixbuf.width(),
                pixbuf.height()
            ),
        }
        let inner = built.as_ref().map(|m| m.hole);
        let mask = built.map(|m| m.mask);
        Some(Self {
            path: path.to_path_buf(),
            source: RefCell::new(Some(pixbuf)),
            cache: RefCell::new(None),
            mask_cache: RefCell::new(None),
            scale: Cell::new(scale.clamp(1.0, 2.0)),
            inner: Cell::new(inner),
            mask: RefCell::new(mask),
        })
    }

    /// 相框自身宽高比（用于"不拉伸地居中放置"）
    pub fn aspect(&self) -> f64 {
        match self.source.borrow().as_ref() {
            Some(p) if p.height() > 0 => p.width() as f64 / p.height() as f64,
            _ => 0.0,
        }
    }

    /// 内孔（相对比例）；None = 叠图模式
    pub fn inner_hole(&self) -> Option<InnerHole> {
        self.inner.get()
    }

    /// 相框纹理（带缓存）
    pub fn texture_for(&self, w: i32, h: i32) -> Option<gdk::Texture> {
        if w <= 1 || h <= 1 {
            return None;
        }
        if let Some(((cw, ch), t)) = self.cache.borrow().as_ref() {
            if *cw == w && *ch == h {
                return Some(t.clone());
            }
        }
        let src = self.source.borrow().clone()?;
        let scale = self.scale.get();
        let tw = ((w as f64 * scale).round() as i32).clamp(1, 8192);
        let th = ((h as f64 * scale).round() as i32).clamp(1, 8192);
        let scaled = if (tw, th) == (src.width(), src.height()) {
            src
        } else {
            src.scale_simple(tw, th, gdk_pixbuf::InterpType::Bilinear)?
        };
        let tex = gdk::Texture::for_pixbuf(&scaled);
        *self.cache.borrow_mut() = Some(((w, h), tex.clone()));
        Some(tex)
    }

    /// 遮罩 cairo surface（白 + 灰度 alpha），供 `DestOut` 擦除媒体
    pub fn mask_surface_for(
        &self,
        w: i32,
        h: i32,
    ) -> Option<std::rc::Rc<cairo::ImageSurface>> {
        if w <= 1 || h <= 1 || self.inner.get().is_none() {
            return None;
        }
        if let Some(((cw, ch), s)) = self.mask_cache.borrow().as_ref() {
            if *cw == w && *ch == h {
                return Some(s.clone());
            }
        }
        let src = self.mask.borrow().clone()?;
        let gray = src.scale_simple(w, h, gdk_pixbuf::InterpType::Bilinear)?;
        let stride = w as usize * 4;
        let mut data = vec![0u8; stride * h as usize];
        {
            // SAFETY: 只读
            let gp = unsafe { gray.pixels() };
            let gs = gray.rowstride() as usize;
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let o = y * stride + x * 4;
                    data[o] = 255;
                    data[o + 1] = 255;
                    data[o + 2] = 255;
                    data[o + 3] = gp[y * gs + x]; // 灰度 → alpha
                }
            }
        }
        let surf = cairo::ImageSurface::create_for_data(
            data,
            cairo::Format::ARgb32,
            w,
            h,
            stride as i32,
        )
        .ok()?;
        let surf = std::rc::Rc::new(surf);
        *self.mask_cache.borrow_mut() = Some(((w, h), surf.clone()));
        Some(surf)
    }
}

/// 生成遮罩 + 内孔；无内孔时返回 None
pub fn build_hole_mask(pb: &gdk_pixbuf::Pixbuf) -> Option<HoleMask> {
    let (w, h) = (pb.width(), pb.height());
    if w < 8 || h < 8 {
        return None;
    }
    let (nch, rowstride) = (pb.n_channels(), pb.rowstride());
    // SAFETY: 只读扫描这块 pixbuf
    let px = unsafe { pb.pixels() };
    let transparent =
        |x: i32, y: i32| -> bool { px[(y * rowstride + x * nch + 3) as usize] <= ALPHA_CUT };

    // 1) 从四边洪泛 → 外部透明
    let mut outside = vec![false; (w * h) as usize];
    let mut stack: Vec<(i32, i32)> = Vec::new();
    fn push_if(
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        transparent: &dyn Fn(i32, i32) -> bool,
        outside: &mut Vec<bool>,
        stack: &mut Vec<(i32, i32)>,
    ) {
        if x < 0 || y < 0 || x >= w || y >= h {
            return;
        }
        let i = (y * w + x) as usize;
        if outside[i] || !transparent(x, y) {
            return;
        }
        outside[i] = true;
        stack.push((x, y));
    }
    for x in 0..w {
        push_if(x, 0, w, h, &transparent, &mut outside, &mut stack);
        push_if(x, h - 1, w, h, &transparent, &mut outside, &mut stack);
    }
    for y in 0..h {
        push_if(0, y, w, h, &transparent, &mut outside, &mut stack);
        push_if(w - 1, y, w, h, &transparent, &mut outside, &mut stack);
    }
    while let Some((x, y)) = stack.pop() {
        push_if(x + 1, y, w, h, &transparent, &mut outside, &mut stack);
        push_if(x - 1, y, w, h, &transparent, &mut outside, &mut stack);
        push_if(x, y + 1, w, h, &transparent, &mut outside, &mut stack);
        push_if(x, y - 1, w, h, &transparent, &mut outside, &mut stack);
    }

    // 2) 允许区（相框本体 ∪ 内孔）+ 内孔包围盒
    let mut allow = vec![0u8; (w * h) as usize];
    let (mut hx0, mut hy0) = (i32::MAX, i32::MAX);
    let (mut hx1, mut hy1) = (-1i32, -1i32);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            if outside[i] {
                continue;
            }
            allow[i] = 255;
            if transparent(x, y) {
                if x < hx0 { hx0 = x; }
                if x > hx1 { hx1 = x; }
                if y < hy0 { hy0 = y; }
                if y > hy1 { hy1 = y; }
            }
        }
    }
    if hx1 <= hx0 || hy1 <= hy0 {
        return None; // 没有内孔
    }
    let hole = InnerHole {
        x0: hx0 as f64 / w as f64,
        y0: hy0 as f64 / h as f64,
        x1: (hx1 + 1) as f64 / w as f64,
        y1: (hy1 + 1) as f64 / h as f64,
    };
    if hole.area_ratio() < MIN_HOLE_AREA {
        return None;
    }

    // 3) **不做羽化**：外缘必须硬切，否则相框外会残留一圈半透明媒体。
    //    视觉自然度靠"媒体向内多压一点、被相框不透明环压住"实现（见 MASK_OVERLAP）。
    let pb8 = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, w, h)?;
    let dstride = pb8.rowstride() as usize;
    {
        // SAFETY: 独占写入
        let dst = unsafe { pb8.pixels() };
        for y in 0..h as usize {
            for x in 0..w as usize {
                dst[y * dstride + x] = allow[y * w as usize + x];
            }
        }
    }
    Some(HoleMask { mask: pb8, hole })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 外圈 border 像素不透明，中间透明
    fn sample(w: i32, h: i32, border: i32) -> gdk_pixbuf::Pixbuf {
        let pb = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, w, h).unwrap();
        for y in 0..h {
            for x in 0..w {
                let opaque =
                    x < border || y < border || x >= w - border || y >= h - border;
                let a = if opaque { 255u8 } else { 0u8 };
                pb.put_pixel(x as u32, y as u32, 100, 80, 40, a);
            }
        }
        pb
    }

    #[test]
    fn finds_rect_hole_and_masks_outside() {
        let pb = sample(300, 200, 40);
        let m = build_hole_mask(&pb).expect("应有内孔");
        // 内孔 40..259 x 40..159
        assert!((m.hole.x0 * 300.0 - 40.0).abs() <= 2.0, "x0={}", m.hole.x0);
        assert!((m.hole.y0 * 200.0 - 40.0).abs() <= 2.0, "y0={}", m.hole.y0);
        // 遮罩：框体与内孔 = 255（这个样本框体贴边，没有外部区域）
        let stride = m.mask.rowstride() as usize;
        let px = unsafe { m.mask.pixels() };
        assert!(px[100 * stride + 10] > 200, "外框应保留");
        assert!(px[100 * stride + 150] > 200, "内孔应保留");
    }

    /// 外轮廓不规则（四周留 20px 透明，框体 20..w-20），内孔 40..w-40
    fn sample_irregular(w: i32, h: i32) -> gdk_pixbuf::Pixbuf {
        let pb = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, w, h).unwrap();
        for y in 0..h {
            for x in 0..w {
                let opaque = x >= 20 && x < w - 20 && y >= 20 && y < h - 20 && !(x >= 40 && x < w - 40 && y >= 40 && y < h - 40);
                let a = if opaque { 255u8 } else { 0u8 };
                pb.put_pixel(x as u32, y as u32, 100, 80, 40, a);
            }
        }
        pb
    }

    #[test]
    fn irregular_outer_erased_but_frame_kept() {
        let pb = sample_irregular(300, 200);
        let m = build_hole_mask(&pb).expect("应有内孔");
        let stride = m.mask.rowstride() as usize;
        let px = unsafe { m.mask.pixels() };
        // 外部透明缺口 → 擦掉（媒体不会漏出矩形边角）
        assert!(px[100 * stride + 5] < 40, "外侧应擦掉");
        // 框体 → 保留（相框会盖住）
        assert!(px[100 * stride + 30] > 200, "框体应保留");
        // 内孔 → 保留
        assert!(px[100 * stride + 150] > 200, "内孔应保留");
    }

    #[test]
    fn rejects_solid_frame() {
        let mut pb =
            gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, 200, 200).unwrap();
        for y in 0..200 {
            for x in 0..200 {
                pb.put_pixel(x as u32, y as u32, 10, 10, 10, 255);
            }
        }
        assert!(build_hole_mask(&pb).is_none());
    }

    #[test]
    fn rejects_tiny_hole() {
        let pb = sample(400, 400, 196); // 只剩 8x8 透明
        assert!(build_hole_mask(&pb).is_none());
    }
}
