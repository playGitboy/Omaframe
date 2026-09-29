//! 尺寸与位置计算：整个项目唯一的"比例真理函数"都在这里。

/// 相框内孔的**可用区**（相对相框矩形 0..1 的比例）。
///
/// 素材宽高按它最大化 → 不规则内孔也能填满，且不会漏到框体/框外。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HoleFit {
    /// 归一化内接矩形：x0, y0, x1, y1（相对相框矩形）
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl HoleFit {
    pub fn width(&self) -> f64 {
        (self.x1 - self.x0).max(0.0)
    }
    pub fn height(&self) -> f64 {
        (self.y1 - self.y0).max(0.0)
    }
}

/// 求相框尺寸：比例 = PNG 自身比例，且内孔绘制矩形**不超过**上限盒。
///
/// 这是"素材宽高最大化填充内孔"的尺寸解法（相框只由内孔几何 + 上限盒决定，
/// **与素材比例无关** —— 否则横图/竖图切换时相框大小会跳变）。
///
/// - `draw` = 内孔绘制矩形（占相框的比例）
/// - `png_aspect` = PNG 自身宽高比（相框不能被拉伸）
/// - `box_w/box_h` = 用户上限盒（配置里的 max_width/max_height）
///
/// 约束：`fw ≤ box_w/draw.width()`、`fh ≤ box_h/draw.height()`、`fw = fh·png_aspect`
pub fn frame_size_for_box(
    draw: HoleFit,
    png_aspect: f64,
    box_w: f64,
    box_h: f64,
) -> (i32, i32) {
    let (dw, dh) = (draw.width().max(0.02), draw.height().max(0.02));
    let (box_w, box_h) = (box_w.max(1.0), box_h.max(1.0));
    // 由高度定宽、由宽度定高，取更紧的一边
    let by_h = box_h / dh;
    let by_w = if png_aspect > 0.01 {
        box_w / (dw * png_aspect)
    } else {
        by_h
    };
    let fh = by_h.min(by_w).max(1.0);
    let fw = if png_aspect > 0.01 {
        fh * png_aspect
    } else {
        box_w / dw
    };
    (
        fw.round().clamp(1.0, 16384.0) as i32,
        fh.round().clamp(1.0, 16384.0) as i32,
    )
}

/// 右下角拖动 → 目标**上限盒宽度**。
///
/// 拖动手感要跟手：鼠标走多少，媒体宽度就走多少。而"媒体宽 = 盒宽 × k"，
/// 所以盒宽要除以 k 才对应同样的位移（k 由当前几何实时算出）。
pub fn resize_target_box(cur_box_w: f64, cur_media_w: f64, delta: f64, max_w: f64) -> i32 {
    let k = if cur_box_w > 0.0 {
        (cur_media_w / cur_box_w).clamp(0.05, 4.0)
    } else {
        1.0
    };
    let w = cur_box_w + delta / k;
    (w.round().clamp(16.0, max_w.max(16.0)) as i32).max(1)
}

/// 把内孔可用区（归一化）映射到相框矩形里的**媒体矩形**。
///
/// - `fit` = 内孔最大内接矩形的归一化范围
/// - `zoom` = 显示比（0.0~1.0，1.0 = 铺满内孔），以可用区中心缩放
/// - 返回相框矩形内媒体的位置与尺寸
pub fn media_rect_in_hole(
    fx: i32,
    fy: i32,
    fw: i32,
    fh: i32,
    fit: HoleFit,
    zoom: f64,
) -> (i32, i32, i32, i32) {
    let z = if zoom.is_finite() { zoom.clamp(0.0, 1.0) } else { 1.0 };
    // 内孔可用区在相框矩形里的位置与尺寸
    let bx = fx as f64 + fw as f64 * fit.x0;
    let by = fy as f64 + fh as f64 * fit.y0;
    let (bw, bh) = (fw as f64 * fit.width(), fh as f64 * fit.height());
    // 以可用区中心按显示比缩放
    let w = (bw * z).round().max(1.0);
    let h = (bh * z).round().max(1.0);
    let cx = bx + bw / 2.0;
    let cy = by + bh / 2.0;
    (
        (cx - w / 2.0).round() as i32,
        (cy - h / 2.0).round() as i32,
        w as i32,
        h as i32,
    )
}

/// 保持原始比例，在 (max_w, max_h) 内取最大尺寸。
/// 这是"用户设置的宽高 = 允许的最大尺寸"的唯一实现。
pub fn fit(w: i32, h: i32, max_w: i32, max_h: i32) -> (i32, i32) {
    if w <= 0 || h <= 0 || max_w <= 0 || max_h <= 0 {
        return (max_w.max(1), max_h.max(1));
    }
    let scale = (max_w as f64 / w as f64).min(max_h as f64 / h as f64);
    let nw = ((w as f64 * scale).round() as i32).max(1);
    let nh = ((h as f64 * scale).round() as i32).max(1);
    (nw.min(max_w), nh.min(max_h))
}

/// 媒体在组件内的居中内缩矩形（scale=0.96 → 四周各留 2%）。

/// 右下角拖动的目标尺寸计算（纯函数，便于测试）
/// - `delta` 取 dx/dy 中较大者（正=放大，负=缩小）

/// 在固定尺寸的框内，按素材比例缩放并居中，四周再按 `scale` 内缩。


/// 素材在相框矩形内**居中**放置，并按显示比以相框中心缩放。
/// 这是"PNG 相框与展示素材中心对齐"的唯一实现点。
pub fn place_media(
    fx: i32,
    fy: i32,
    fw: i32,
    fh: i32,
    mw: i32,
    mh: i32,
    zoom: f64,
) -> (i32, i32, i32, i32) {
    let z = if zoom.is_finite() { zoom.clamp(0.0, 1.0) } else { 1.0 };
    let w = ((mw.max(1) as f64) * z).round().max(1.0) as i32;
    let h = ((mh.max(1) as f64) * z).round().max(1.0) as i32;
    let cx = fx as f64 + fw as f64 / 2.0;
    let cy = fy as f64 + fh as f64 / 2.0;
    (
        (cx - w as f64 / 2.0).round() as i32,
        (cy - h as f64 / 2.0).round() as i32,
        w,
        h,
    )
}

/// 素材与相框同尺寸时的快捷版（缩放为中心基准）
#[allow(dead_code)]
pub fn zoom_in_frame(fx: i32, fy: i32, fw: i32, fh: i32, zoom: f64) -> (i32, i32, i32, i32) {
    let z = if zoom.is_finite() {
        zoom.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let cx = fx as f64 + fw as f64 / 2.0;
    let cy = fy as f64 + fh as f64 / 2.0;
    let mw = ((fw as f64) * z).round().max(1.0) as i32;
    let mh = ((fh as f64) * z).round().max(1.0) as i32;
    (
        (cx - mw as f64 / 2.0).round() as i32,
        (cy - mh as f64 / 2.0).round() as i32,
        mw,
        mh,
    )
}

pub fn clamp(v: i32, lo: i32, hi: i32) -> i32 {
    if hi < lo {
        return lo;
    }
    v.clamp(lo, hi)
}

/// 屏幕边界（逻辑像素，monitor 原点为 0,0）
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    pub width: i32,
    pub height: i32,
}

/// 首次运行按 anchor 名计算位置。
pub fn anchor_pos(anchor: &str, screen: Bounds, w: i32, h: i32, margin: i32) -> (i32, i32) {
    let right = screen.width - w - margin;
    let bottom = screen.height - h - margin;
    match anchor {
        "top-left" => (margin, margin),
        "bottom-right" => (right, bottom),
        "bottom-left" => (margin, bottom),
        "center" => ((screen.width - w) / 2, (screen.height - h) / 2),
        // 默认 top-right
        _ => (right, margin),
    }
}

/// 把位置夹进屏幕内（分辨率/显示器变化后仍然可见）。
pub fn clamp_to_screen(x: i32, y: i32, w: i32, h: i32, screen: Bounds) -> (i32, i32) {
    let max_x = (screen.width - w).max(0);
    let max_y = (screen.height - h).max(0);
    (clamp(x, 0, max_x), clamp(y, 0, max_y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landscape_fits_by_width() {
        // 16:9 素材放进 600x500 → 受宽度限制
        assert_eq!(fit(1920, 1080, 600, 500), (600, 338));
    }

    #[test]
    fn portrait_fits_by_height() {
        // 竖图 9:16 放进 600x500 → 受高度限制
        assert_eq!(fit(1080, 1920, 600, 500), (281, 500));
    }

    #[test]
    fn small_image_is_scaled_up_to_fit() {
        // 按需求公式：scale = min(max_w/orig_w, max_h/orig_h)，小图也会放大填满
        let (w, h) = fit(100, 100, 600, 500);
        assert_eq!((w, h), (500, 500));
    }

    #[test]
    fn never_exceeds_max() {
        let (w, h) = fit(8000, 4000, 600, 500);
        assert!(w <= 600 && h <= 500);
        assert_eq!((w, h), (600, 300));
    }






    #[test]
    fn zoom_scales_from_frame_center() {
        // 相框 (100,200) 400x300
        // 100% → 媒体与相框同尺寸，起点相同
        assert_eq!(zoom_in_frame(100, 200, 400, 300, 1.0), (100, 200, 400, 300));
        // 70% → 280x210，中心 (300,350) 不变 → 起点 (160,245)
        let (x, y, w, h) = zoom_in_frame(100, 200, 400, 300, 0.7);
        assert_eq!((w, h), (280, 210));
        assert_eq!((x, y), (160, 245));
        // 中心不变
        assert_eq!(x as f64 + w as f64 / 2.0, 300.0);
        assert_eq!(y as f64 + h as f64 / 2.0, 350.0);
        // 0% 也要至少 1px，不崩
        assert_eq!(zoom_in_frame(0, 0, 400, 300, 0.0), (200, 150, 1, 1));
    }

    #[test]
    fn media_is_centered_in_frame_even_if_aspect_differs() {
        // 相框 400x200（很宽），素材 100x200（很窄）→ 素材应水平居中
        let (x, y, w, h) = place_media(0, 0, 400, 200, 100, 200, 1.0);
        assert_eq!((w, h), (100, 200));
        assert_eq!((x, y), (150, 0));           // 水平居中
        assert_eq!(x as f64 + w as f64 / 2.0, 200.0);
        // 显示比 50%：仍以相框中心为基准
        let (x, y, w, h) = place_media(100, 200, 400, 300, 400, 300, 0.5);
        assert_eq!((w, h), (200, 150));
        assert_eq!((x, y), (200, 275));
        assert_eq!(x as f64 + w as f64 / 2.0, 300.0);
    }

    #[test]
    fn frame_size_from_box_is_independent_of_media_aspect() {
        // 同一个内孔绘制矩形 + 同一个上限盒 → 无论素材横竖，相框都一样
        let draw = HoleFit { x0: 0.07, y0: 0.13, x1: 0.84, y1: 0.85 };
        let a = frame_size_for_box(draw, 1346.0 / 733.0, 515.0, 336.0);
        let b = frame_size_for_box(draw, 1346.0 / 733.0, 515.0, 336.0);
        assert_eq!(a, b);
        // 比例保持 PNG 自身比例
        let ar = a.0 as f64 / a.1 as f64;
        assert!((ar - 1346.0 / 733.0).abs() < 0.02, "相框比例 {ar} 应≈1.836");
        // 媒体绘制矩形不超过上限盒
        assert!(a.0 as f64 * draw.width() <= 515.0 + 1.0, "媒体宽超上限");
        assert!(a.1 as f64 * draw.height() <= 336.0 + 1.0, "媒体高超上限");
        // 上限盒放大 1 倍 → 相框也放大 1 倍（线性）
        let big = frame_size_for_box(draw, 1346.0 / 733.0, 1030.0, 672.0);
        assert!((big.0 as f64 / a.0 as f64 - 2.0).abs() < 0.02);
        assert!((big.1 as f64 / a.1 as f64 - 2.0).abs() < 0.02);
    }

    #[test]
    fn resize_target_box_tracks_mouse_delta() {
        // 媒体宽 = 盒宽 × 0.5 → 鼠标 +40 媒体宽，盒宽应 +80
        let w = resize_target_box(400.0, 200.0, 40.0, 4000.0);
        assert_eq!(w, 480);
        // 夹在上限内
        assert_eq!(resize_target_box(400.0, 200.0, 99999.0, 800.0), 800);
        // 夹在最小以上
        assert!(resize_target_box(100.0, 50.0, -9999.0, 800.0) >= 16);
    }

    #[test]
    fn media_fills_hole_fit_and_zoom_shrinks_from_its_center() {
        // 相框 0,0 700x400，内孔可用区 x 0.1~0.8 y 0.2~0.9
        let fit = HoleFit { x0: 0.1, y0: 0.2, x1: 0.8, y1: 0.9 };
        // 显示比 100%：媒体矩形 == 内孔可用区
        let (x, y, w, h) = media_rect_in_hole(0, 0, 700, 400, fit, 1.0);
        assert_eq!((x, y), (70, 80));
        assert_eq!((w, h), (490, 280));
        // 显示比 50%：以可用区中心缩小
        let (x2, y2, w2, h2) = media_rect_in_hole(0, 0, 700, 400, fit, 0.5);
        assert_eq!((w2, h2), (245, 140));
        // 中心不变（允许 0.5px 取整误差）
        assert!(
            (x2 as f64 + w2 as f64 / 2.0 - 315.0).abs() <= 0.5,
            "中心 x 偏移"
        );
        assert!(
            (y2 as f64 + h2 as f64 / 2.0 - 220.0).abs() <= 0.5,
            "中心 y 偏移"
        );
    }

    #[test]
    fn frame_fits_media_grown_box() {
        // 素材 500x300，grow=5% → 目标盒 525x315
        let sw: f64 = 500.0;
        let sh: f64 = 300.0;
        let (box_w, box_h) = (525, 315);
        // PNG 比例与素材一致 → 相框就是目标盒
        let png: f64 = sw / sh;
        let (fw, fh) = fit((png * 10_000.0).round() as i32, 10_000, box_w, box_h);
        assert_eq!((fw, fh), (525, 315));
        // PNG 是竖图（0.5）→ 保持比例：宽若取 525 则高 1050 超盒 → 改为按高 fit
        let (fw2, fh2) = fit(5_000, 10_000, 525, 315);
        assert_eq!((fw2, fh2), (158, 315)); // 158/315 ≈ 0.5
        assert!(fw2 <= 525 && fh2 <= 315);
    }

    #[test]
    fn top_left_is_the_default_anchor() {
        let s = Bounds {
            width: 1536,
            height: 864,
        };
        assert_eq!(anchor_pos("top-left", s, 600, 338, 32), (32, 32));
        assert_eq!(anchor_pos("top-right", s, 600, 338, 32), (904, 32));
    }

    #[test]
    fn clamps_into_screen() {
        let s = Bounds {
            width: 800,
            height: 600,
        };
        assert_eq!(clamp_to_screen(1900, -50, 600, 500, s), (200, 0));
    }
}
