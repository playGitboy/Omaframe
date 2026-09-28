//! 尺寸与位置计算：整个项目唯一的"比例真理函数"都在这里。

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
/// 用于"相框内沿与照片之间留细边"以及按同样比例缩小解码分辨率。
pub fn inset(w: i32, h: i32, scale: f64) -> (i32, i32, i32, i32) {
    let s = if scale.is_finite() { scale.clamp(0.2, 1.0) } else { 1.0 };
    let iw = ((w as f64 * s).round() as i32).max(1);
    let ih = ((h as f64 * s).round() as i32).max(1);
    ((w - iw) / 2, (h - ih) / 2, iw.min(w), ih.min(h))
}

/// 右下角拖动的目标尺寸计算（纯函数，便于测试）
/// - `delta` 取 dx/dy 中较大者（正=放大，负=缩小）
/// - 始终保持 `aspect`，并夹在最小尺寸与屏幕可用空间之间
#[allow(clippy::too_many_arguments)]
pub fn resize_target(
    cur_w: f64,
    aspect: f64,
    dx: f64,
    dy: f64,
    avail_w: f64,
    avail_h: f64,
    min_w: f64,
    min_h: f64,
) -> (i32, i32) {
    let aspect = if aspect.is_finite() && aspect > 0.01 {
        aspect
    } else {
        1.0
    };
    let delta = if dx.abs() >= dy.abs() { dx } else { dy };
    let mut w = cur_w + delta;
    let mut h = w / aspect;
    if h < min_h {
        h = min_h;
        w = h * aspect;
    }
    if w < min_w {
        w = min_w;
        h = w / aspect;
    }
    if w > avail_w {
        w = avail_w;
        h = w / aspect;
    }
    if h > avail_h {
        h = avail_h;
        w = h * aspect;
    }
    (
        w.round().clamp(min_w, 1.0e9) as i32,
        h.round().clamp(min_h, 1.0e9) as i32,
    )
}

/// 在固定尺寸的框内，按素材比例缩放并居中，四周再按 `scale` 内缩。
/// 返回 (x, y, w, h)。框尺寸恒定 → layer surface 尺寸恒定。
pub fn fit_rect(sw: i32, sh: i32, box_w: i32, box_h: i32, scale: f64) -> (i32, i32, i32, i32) {
    let s = if scale.is_finite() { scale.clamp(0.2, 1.0) } else { 1.0 };
    let max_w = ((box_w as f64) * s).round().max(1.0);
    let max_h = ((box_h as f64) * s).round().max(1.0);
    let (w, h) = fit(sw, sh, max_w as i32, max_h as i32);
    let w = w.min(box_w).max(1);
    let h = h.min(box_h).max(1);
    ((box_w - w) / 2, (box_h - h) / 2, w, h)
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
    fn inset_is_centered_and_scaled() {
        // 600x337 缩到 96% → 576x324，四周各留 12 / 6 像素
        let (x, y, w, h) = inset(600, 337, 0.96);
        assert_eq!((w, h), (576, 324));
        assert_eq!((x, y), (12, 6));
        // 留白居中：奇数差时允许 1px 偏差
        assert!((x - (600 - w - x)).abs() <= 1);
        assert!((y - (337 - h - y)).abs() <= 1);
    }

    #[test]
    fn inset_scale_one_is_noop() {
        assert_eq!(inset(600, 337, 1.0), (0, 0, 600, 337));
    }

    #[test]
    fn resize_keeps_aspect_and_clamps() {
        // 当前 600x338（比例 1.775），向右下拖 +100/+60 → 取较大位移 100
        let (w, h) = resize_target(600.0, 600.0 / 338.0, 100.0, 60.0, 1000.0, 800.0, 160.0, 120.0);
        assert_eq!((w, h), (700, 394));
        // 缩小：|dy| > |dx| → 跟 dy（-60）
        let (w, h) = resize_target(600.0, 600.0 / 338.0, -30.0, -60.0, 1000.0, 800.0, 160.0, 120.0);
        assert_eq!((w, h), (540, 304));
        // 缩小：|dx| > |dy| → 跟 dx（-100）
        let (w, h) = resize_target(600.0, 600.0 / 338.0, -100.0, -20.0, 1000.0, 800.0, 160.0, 120.0);
        assert_eq!((w, h), (500, 282));
        // 不会小于最小尺寸
        let (w, h) = resize_target(200.0, 1.5, -5000.0, -5000.0, 1000.0, 800.0, 160.0, 120.0);
        assert_eq!((w, h), (180, 120));
        // 不会超过屏幕剩余空间
        let (w, h) = resize_target(600.0, 2.0, 5000.0, 5000.0, 800.0, 600.0, 160.0, 120.0);
        assert_eq!((w, h), (800, 400));
    }

    #[test]
    fn fit_rect_centers_media_in_fixed_box() {
        // 框固定 600x500，横图 16:9，内缩 0.96 → 媒体宽 576，高 576/1.7778 = 324
        let (x, y, w, h) = fit_rect(1920, 1080, 600, 500, 0.96);
        assert_eq!((w, h), (576, 324));
        assert_eq!((x, y), (12, 88));
        // 竖图 9:16 → 受高度限制
        let (x, y, w, h) = fit_rect(1080, 1920, 600, 500, 0.96);
        assert_eq!((w, h), (270, 480));
        assert_eq!((x, y), (165, 10));
        // 框外不越界
        assert!(x >= 0 && y >= 0 && x + w <= 600 && y + h <= 500);
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
