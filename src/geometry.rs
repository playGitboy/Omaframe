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
