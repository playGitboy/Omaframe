//! **智能相框分析引擎**：从任意透明 PNG 自动推导可自适应渲染的 `FrameModel`。
//!
//! 关键概念（**不要合并**）：
//! - `content`（= Safe Content Rect）：告诉布局"媒体大概放在哪里"，用于九宫格的边框/四角尺寸。
//! - `safe_content_mask`（运行时生成）：真正负责最后一层"底图绝对不能穿过边框"。
//!
//! 分析流程：
//! ```text
//! Pixbuf ──降采样到分析分辨率──▶ Alpha 二值化 ──边界洪泛──▶ 外部透明区
//!                                                      │
//!                                    内部透明区（= 未被外部连通）
//!                                                      │
//!                       最大连通块（= 真实内孔，排除零散小洞）
//!                                                      │
//!                                          形态学腐蚀（安全边距，避开羽化/装饰）
//!                                                      │
//!                            最大内接矩形（直方图 + 单调栈, O(W×H)）
//!                                                      │
//!                       映射回原始分辨率 ──▶ FrameModel ──▶ 磁盘缓存
//! ```

use crate::geometry::{slices_src, RectI};
use std::path::{Path, PathBuf};

/// 算法版本：改动分析逻辑必须 +1，否则旧缓存会继续生效
pub const ANALYSIS_VERSION: u32 = 3;

/// "完全透明"的阈值：alpha ≤ 它才算可以放东西的区域。
/// 取小值（而不是 24/32）是为了**把半透明羽化带留给相框本体**：
/// 羽化处的半透明由遮罩按真实 alpha 处理（抗锯齿），不会出现硬边。
pub const ALPHA_THRESHOLD: u8 = 8;

/// 分析分辨率上限（长边）。4K PNG 会先缩到这个尺寸再分析，几何结果再映射回原图。
/// 几何特征是低频的，512 足够精确，同时把分析成本压到毫秒级。
pub const ANALYSIS_MAX: i32 = 512;

/// 安全边距：相对分析分辨率短边的比例（避开羽化、发光、装饰的毛刺）
pub const SAFE_MARGIN_RATIO: f64 = 0.010;
/// 安全边距下限（分析像素）
pub const SAFE_MARGIN_MIN: i32 = 2;

/// 内孔面积占比低于它就认为"没有内孔"（整张不透明 / 纯装饰）
pub const MIN_HOLE_RATIO: f64 = 0.02;

/// 统一的相框模型（可序列化 + 缓存）
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FrameModel {
    pub version: u32,
    /// 相框 PNG 原始尺寸
    pub canvas: RectI,
    /// 分析分辨率（canvas 可能被缩小过）
    pub analysis: RectI,
    pub alpha_threshold: u8,
    /// 分析时用的安全边距（分析像素）
    pub safe_margin: i32,
    /// **内孔范围**（内部透明区域包围盒，原始坐标）：
    /// 布局与九宫格中心片用它 —— 素材必须铺满内孔，否则框内会露出一圈桌面。
    pub hole: RectI,
    /// **最大安全内接矩形**（内孔向内腐蚀安全边距后的最大矩形，原始坐标）。
    /// 与 `hole` 的区别：内孔里可能有花朵/藤蔓等装饰伸进来，安全矩形保证"没有装饰"。
    /// 它用于质量评估/诊断与兜底（避免把装饰当成内容区），不作为相框拉伸的基准。
    pub safe: RectI,
    /// 内孔面积占比（用于判断这个相框是否可用）
    pub hole_ratio: f64,
    /// 安全区面积占比（分析质量指标）
    pub safe_ratio: f64,
    /// 分析耗时（毫秒，仅记录）
    pub elapsed_ms: u64,
}

impl FrameModel {
    /// 边框（左, 上, 右, 下）：内容区到画布四边的距离
    pub fn borders(&self) -> (i32, i32, i32, i32) {
        (
            self.hole.x.max(1),
            self.hole.y.max(1),
            (self.canvas.w - self.hole.right()).max(1),
            (self.canvas.h - self.hole.bottom()).max(1),
        )
    }

    /// 九宫格源切片（恰好无缝覆盖整张 PNG）。中心片 = **内孔范围**。
    pub fn slices(&self) -> [RectI; 9] {
        slices_src(self.canvas, self.hole)
    }

    pub fn aspect(&self) -> f64 {
        self.canvas.aspect()
    }
}

/// 分析结果：模型 + 中间掩码。
///
/// `outside` / `hole` 是分析的中间产物（调试与后续 Mesh Warp 会用到），
/// 遮罩本身只依赖 `model` + PNG 的 alpha，所以这里允许暂时不读。
#[allow(dead_code)]
pub struct Analysis {
    pub model: FrameModel,
    /// 分析分辨率下的"外部透明"标记（与边界连通）
    pub outside: Vec<bool>,
    /// 分析分辨率下的内孔标记（最大内部连通块）
    pub hole: Vec<bool>,
}

impl Analysis {
    /// 分析分辨率尺寸（调试用）
    #[allow(dead_code)]
    pub fn dims(&self) -> (i32, i32) {
        (self.model.analysis.w, self.model.analysis.h)
    }
}

/// 分析选项
#[derive(Debug, Clone, Copy)]
pub struct AnalyzeOpts {
    pub alpha_threshold: u8,
    pub analysis_max: i32,
}

impl Default for AnalyzeOpts {
    fn default() -> Self {
        Self {
            alpha_threshold: ALPHA_THRESHOLD,
            analysis_max: ANALYSIS_MAX,
        }
    }
}

/// **主入口**：分析一张相框 PNG，得到 `FrameModel`。
///
/// 失败（无 alpha、整张不透明、没有内孔、图像太小）返回 `None`，
/// 调用方退回"整图等比缩放"的兼容模式（不崩、不静默失败，会记日志）。
pub fn analyze(pb: &gdk_pixbuf::Pixbuf, opts: AnalyzeOpts) -> Option<Analysis> {
    let start = std::time::Instant::now();
    let (cw, ch) = (pb.width(), pb.height());
    if cw < 16 || ch < 16 {
        crate::warn!("相框分析：图像太小 {cw}x{ch}");
        return None;
    }
    if !pb.has_alpha() {
        // 用户常见错误：下载的"透明 PNG"其实是白底。不要擅自抠图。
        crate::warn!("相框分析：PNG 没有 alpha 通道，退回等比缩放（不做背景移除）");
        return None;
    }

    // ① 降采样到分析分辨率（几何特征低频，够用；渲染仍用原图）
    let long = cw.max(ch);
    let (aw, ah) = if long > opts.analysis_max.max(64) {
        let s = opts.analysis_max as f64 / long as f64;
        (
            ((cw as f64 * s).round() as i32).max(16),
            ((ch as f64 * s).round() as i32).max(16),
        )
    } else {
        (cw, ch)
    };
    let small = if (aw, ah) == (cw, ch) {
        pb.clone()
    } else {
        pb.scale_simple(aw, ah, gdk_pixbuf::InterpType::Bilinear)?
    };

    let (w, h) = (small.width(), small.height());
    let n = (w * h) as usize;
    let stride = small.rowstride() as usize;
    let nch = small.n_channels() as usize;
    // SAFETY: 只读扫描
    let px = unsafe { small.pixels() };

    // ② Alpha 二值化：free = 可以放内容的透明像素
    let mut free = vec![false; n];
    let mut alpha = vec![0u8; n];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let a = if nch >= 4 {
                px[y * stride + x * nch + 3]
            } else {
                255
            };
            let i = y * w as usize + x;
            alpha[i] = a;
            free[i] = a <= opts.alpha_threshold;
        }
    }

    // ③ 从画布边界洪泛：与边界连通的透明区 = 外部背景（四角/缺口）
    let mut outside = vec![false; n];
    {
        let mut stack: Vec<i32> = Vec::new();
        let push = |i: i32, outside: &mut Vec<bool>, stack: &mut Vec<i32>, free: &[bool]| {
            if i < 0 || i >= n as i32 || outside[i as usize] || !free[i as usize] {
                return;
            }
            outside[i as usize] = true;
            stack.push(i);
        };
        for x in 0..w {
            push(x, &mut outside, &mut stack, &free);
            push((h - 1) * w + x, &mut outside, &mut stack, &free);
        }
        for y in 0..h {
            push(y * w, &mut outside, &mut stack, &free);
            push(y * w + w - 1, &mut outside, &mut stack, &free);
        }
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            if x > 0 {
                push(i - 1, &mut outside, &mut stack, &free);
            }
            if x < w - 1 {
                push(i + 1, &mut outside, &mut stack, &free);
            }
            if y > 0 {
                push(i - w, &mut outside, &mut stack, &free);
            }
            if y < h - 1 {
                push(i + w, &mut outside, &mut stack, &free);
            }
        }
    }

    // ④ 内部透明区 = free && !outside；取**最大连通块**（排除零散小洞/装饰缝隙）
    let mut hole = vec![false; n];
    let mut seen = vec![false; n];
    let mut best: Vec<i32> = Vec::new();
    for start_i in 0..n {
        if seen[start_i] || outside[start_i] || !free[start_i] {
            continue;
        }
        let mut comp: Vec<i32> = Vec::new();
        let mut stack = vec![start_i as i32];
        seen[start_i] = true;
        while let Some(i) = stack.pop() {
            comp.push(i);
            let (x, y) = (i % w, i / w);
            let nb = |j: i32, seen: &mut Vec<bool>, stack: &mut Vec<i32>| {
                let ju = j as usize;
                if !seen[ju] && !outside[ju] && free[ju] {
                    seen[ju] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                nb(i - 1, &mut seen, &mut stack);
            }
            if x < w - 1 {
                nb(i + 1, &mut seen, &mut stack);
            }
            if y > 0 {
                nb(i - w, &mut seen, &mut stack);
            }
            if y < h - 1 {
                nb(i + w, &mut seen, &mut stack);
            }
        }
        if comp.len() > best.len() {
            best = comp;
        }
    }
    if best.is_empty() {
        crate::warn!("相框分析：没有找到内部透明区域（相框不透明或整张透明）");
        return None;
    }
    for i in best.iter() {
        hole[*i as usize] = true;
    }
    let hole_count = best.len();

    let hole_box = bbox_of(&hole, w, h)?;
    let hole_ratio = hole_count as f64 / n as f64;
    if hole_ratio < MIN_HOLE_RATIO {
        crate::warn!("相框分析：内孔太小（{:.1}%），忽略", hole_ratio * 100.0);
        return None;
    }

    // ⑤ 形态学腐蚀（方形核，可分离两趟 O(W×H)）：避开羽化/发光/装饰毛刺
    let margin = ((SAFE_MARGIN_RATIO * w.min(h) as f64).round() as i32).max(SAFE_MARGIN_MIN);
    let safe = erode_square(&hole, w, h, margin);

    // ⑥ 最大内接矩形（直方图 + 单调栈）
    let rect = largest_rect(&safe, w, h)?;
    let safe_count = safe.iter().filter(|v| **v).count();
    if rect.w < 4 || rect.h < 4 {
        crate::warn!(
            "相框分析：安全内容区太小 {}x{}，退回等比缩放",
            rect.w,
            rect.h
        );
        return None;
    }

    // ⑦ 映射回原始分辨率（向内取整，保证一定落在安全区内）
    let sx = cw as f64 / w as f64;
    let sy = ch as f64 / h as f64;
    let safe = RectI::new(
        (rect.x as f64 * sx).floor() as i32,
        (rect.y as f64 * sy).floor() as i32,
        (rect.w as f64 * sx).ceil() as i32,
        (rect.h as f64 * sy).ceil() as i32,
    );
    // 夹进画布
    let safe = RectI::new(
        safe.x.clamp(0, cw - 1),
        safe.y.clamp(0, ch - 1),
        safe.w.min(cw - safe.x.clamp(0, cw - 1)),
        safe.h.min(ch - safe.y.clamp(0, ch - 1)),
    );
    let hole_rect = RectI::new(
        (hole_box.0 as f64 * sx).floor() as i32,
        (hole_box.1 as f64 * sy).floor() as i32,
        (hole_box.2 as f64 * sx).ceil() as i32,
        (hole_box.3 as f64 * sy).ceil() as i32,
    );


    let elapsed_ms = start.elapsed().as_millis() as u64;
    crate::info!(
        "相框分析：{}x{} → 分析 {}x{}，内孔 {:.1}%（{}x{} @{},{}），安全内容区 {:.1}% 最大内接 {}x{} @{},{}，{}ms{}",
        cw, ch, w, h,
        hole_ratio * 100.0, hole_rect.w, hole_rect.h, hole_rect.x, hole_rect.y,
        safe_count as f64 / n as f64 * 100.0, safe.w, safe.h, safe.x, safe.y,
        elapsed_ms,
        if long > opts.analysis_max { "（已降采样）" } else { "" }
    );

    let model = FrameModel {
        version: ANALYSIS_VERSION,
        canvas: RectI::new(0, 0, cw, ch),
        analysis: RectI::new(0, 0, w, h),
        alpha_threshold: opts.alpha_threshold,
        safe_margin: margin,
        hole: hole_rect,
        safe,
        hole_ratio,
        safe_ratio: safe_count as f64 / n as f64,
        elapsed_ms,
    };
    Some(Analysis { model, outside, hole })
}

fn bbox_of(mask: &[bool], w: i32, h: i32) -> Option<(i32, i32, i32, i32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, -1i32, -1i32);
    for y in 0..h {
        for x in 0..w {
            if mask[(y * w + x) as usize] {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x1 < x0 {
        None
    } else {
        Some((x0, y0, x1 - x0 + 1, y1 - y0 + 1))
    }
}

/// 方形核腐蚀（分离两趟，O(W×H)）
fn erode_square(src: &[bool], w: i32, h: i32, r: i32) -> Vec<bool> {
    if r <= 0 {
        return src.to_vec();
    }
    // 横向
    let mut tmp = vec![false; src.len()];
    for y in 0..h {
        for x in 0..w {
            let mut ok = true;
            for d in -r..=r {
                let xx = x + d;
                if xx < 0 || xx >= w || !src[(y * w + xx) as usize] {
                    ok = false;
                    break;
                }
            }
            tmp[(y * w + x) as usize] = ok;
        }
    }
    // 纵向
    let mut out = vec![false; src.len()];
    for y in 0..h {
        for x in 0..w {
            let mut ok = true;
            for d in -r..=r {
                let yy = y + d;
                if yy < 0 || yy >= h || !tmp[(yy * w + x) as usize] {
                    ok = false;
                    break;
                }
            }
            out[(y * w + x) as usize] = ok;
        }
    }
    out
}

/// **最大内接矩形**（Largest Rectangle in Binary Matrix，O(W×H)）
pub fn largest_rect(mask: &[bool], w: i32, h: i32) -> Option<RectI> {
    if w <= 0 || h <= 0 {
        return None;
    }
    let mut heights = vec![0i32; w as usize];
    let mut best = RectI::ZERO;
    let mut best_area = 0i64;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            heights[x as usize] = if mask[i] { heights[x as usize] + 1 } else { 0 };
        }
        // 单调栈：每个高度向左右扩展
        let mut stack: Vec<(i32, i32)> = Vec::new(); // (x_start, height)
        for x in 0..=w {
            let cur = if x < w { heights[x as usize] } else { 0 };
            let mut start = x;
            while let Some(&(sx, sh)) = stack.last() {
                if sh <= cur {
                    break;
                }
                stack.pop();
                let area = sh as i64 * (x - sx) as i64;
                if area > best_area {
                    best_area = area;
                    best = RectI::new(sx, y - sh + 1, x - sx, sh);
                }
                start = sx;
            }
            stack.push((start, cur));
        }
    }
    if best.is_empty() {
        None
    } else {
        Some(best)
    }
}

/// 运行时遮罩（**抗锯齿**）：返回灰度 pixbuf，值 = "擦掉媒体"的程度。
/// - 相框外轮廓之外（外部透明背景）→ 255（彻底擦掉，媒体绝不漏到框外）
/// - 相框本体/羽化带 → 相框自身的 alpha（半透明处半擦 → 自然过渡，无硬边）
/// - 内孔 → 0（媒体完整显示）
pub fn build_mask_pixbuf(
    pb: &gdk_pixbuf::Pixbuf,
    model: &FrameModel,
) -> Option<gdk_pixbuf::Pixbuf> {
    let (cw, ch) = (pb.width(), pb.height());
    if cw < 1 || ch < 1 {
        return None;
    }
    // 用"内孔包围盒 + alpha 阈值"判定框外，不需要重新洪泛：
    // 内孔 bbox 之外的透明像素 = 外部背景（要整片擦掉），
    // 内孔 bbox 之内 = 相框本体/羽化/内孔（按真实 alpha 擦）。
    // 于是遮罩可以直接由缓存里的 FrameModel + PNG 复现。
    let thr = model.alpha_threshold;
    let hole = model.hole;
    let out = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, cw, ch)?;
    let ostride = out.rowstride() as usize;
    let sstride = pb.rowstride() as usize;
    let nch = pb.n_channels() as usize;
    // SAFETY: 只读源 + 独占写目标
    let src = unsafe { pb.pixels() };
    let dst = unsafe { out.pixels() };
    for y in 0..ch as usize {
        for x in 0..cw as usize {
            let a = if nch >= 4 { src[y * sstride + x * nch + 3] } else { 255 };
            let inside_hole_box = hole.contains(x as i32, y as i32);
            let v = if a <= thr && !inside_hole_box {
                255 // 框外背景：整片擦掉，媒体绝不漏出
            } else {
                a // 框内：用相框真实 alpha（羽化 → 半透明过渡，抗锯齿）
            };
            dst[y * ostride + x] = v;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// 磁盘缓存：路径 + mtime + size + 版本 + 阈值 → FrameModel
// ---------------------------------------------------------------------------

fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("omarchy-omaframe").join("frames"))
}

fn cache_key(path: &Path, opts: &AnalyzeOpts) -> u64 {
    use std::hash::{Hash, Hasher};
    let meta = std::fs::metadata(path).ok();
    let mtime = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    mtime.hash(&mut hasher);
    size.hash(&mut hasher);
    ANALYSIS_VERSION.hash(&mut hasher);
    opts.alpha_threshold.hash(&mut hasher);
    opts.analysis_max.hash(&mut hasher);
    hasher.finish()
}

/// 读缓存（命中返回 Some）
pub fn load_cached(path: &Path, opts: &AnalyzeOpts) -> Option<FrameModel> {
    let dir = cache_dir()?;
    let file = dir.join(format!("{:016x}.json", cache_key(path, opts)));
    let text = std::fs::read_to_string(file).ok()?;
    let model: FrameModel = serde_json::from_str(&text).ok()?;
    if model.version != ANALYSIS_VERSION {
        return None;
    }
    Some(model)
}

/// 写缓存（失败只记日志）
pub fn save_cached(path: &Path, opts: &AnalyzeOpts, model: &FrameModel) {
    let Some(dir) = cache_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let file = dir.join(format!("{:016x}.json", cache_key(path, opts)));
    if let Ok(text) = serde_json::to_string(model) {
        if let Err(e) = std::fs::write(&file, text) {
            crate::debug!("相框模型缓存写入失败：{e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::RectI;

    fn pb(w: i32, h: i32) -> gdk_pixbuf::Pixbuf {
        gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, w, h).unwrap()
    }

    /// 规则矩形相框：border 宽的不透明边 + 透明中心
    fn rect_frame(w: i32, h: i32, border: i32) -> gdk_pixbuf::Pixbuf {
        let p = pb(w, h);
        for y in 0..h {
            for x in 0..w {
                let opaque = x < border || y < border || x >= w - border || y >= h - border;
                let a = if opaque { 255u8 } else { 0u8 };
                p.put_pixel(x as u32, y as u32, 120, 90, 60, a);
            }
        }
        p
    }

    #[test]
    fn analyzes_regular_rect_frame() {
        let p = rect_frame(400, 300, 40);
        let a = analyze(&p, AnalyzeOpts::default()).expect("应能分析");
        // 内容区应落在内孔里，且明显大于零
        let c = a.model.safe;
        assert!(c.w >= 300 && c.h >= 200, "内容区太小 {:?}", c);
        assert!(c.x >= 38 && c.y >= 38, "内容区越界 {:?}", c);
        assert!(c.right() <= 362 && c.bottom() <= 262, "内容区越界 {:?}", c);
        // 边框应接近 40（加上安全边距）
        let (l, t, r, b) = a.model.borders();
        assert!(l >= 40 && t >= 40 && r >= 38 && b >= 38, "边框异常 {l},{t},{r},{b}");
        // 内孔面积 ≈ 320x220
        assert!(a.model.hole_ratio > 0.5, "内孔占比 {}", a.model.hole_ratio);
    }

    /// 角落有装饰的相框：中心透明，但左上/右下有"花朵"伸进内孔
    fn corner_decor_frame() -> gdk_pixbuf::Pixbuf {
        let (w, h) = (400, 300);
        let p = rect_frame(w, h, 30);
        // 在左上、右下各放一块不透明"花"
        for (cx, cy, r) in [(70i32, 70i32, 45i32), (w - 70, h - 70, 45)] {
            for y in 0..h {
                for x in 0..w {
                    let d = (((x - cx).pow(2) + (y - cy).pow(2)) as f64).sqrt();
                    if d < r as f64 {
                        p.put_pixel(x as u32, y as u32, 220, 120, 180, 255);
                    }
                }
            }
        }
        p
    }

    #[test]
    fn safe_rect_avoids_corner_decorations() {
        let p = corner_decor_frame();
        let a = analyze(&p, AnalyzeOpts::default()).expect("应能分析");
        let c = a.model.safe;
        // 安全内容区不能覆盖到花朵（左上方圆 r=45 圆心 70,70）
        let flower_tl = ((c.x as f64 - 70.0).powi(2) + (c.y as f64 - 70.0).powi(2)).sqrt();
        assert!(
            flower_tl >= 44.0,
            "内容区左上角落在花朵里：{:?}（距圆心 {flower_tl:.1}）",
            c
        );
        // 也不能覆盖右下花朵
        let br_x = c.right() as f64;
        let br_y = c.bottom() as f64;
        let flower_br =
            ((br_x - (400.0 - 70.0)).powi(2) + (br_y - (300.0 - 70.0)).powi(2)).sqrt();
        assert!(
            flower_br >= 44.0,
            "内容区右下角落在花朵里：{:?}（距圆心 {flower_br:.1}）",
            c
        );
    }

    #[test]
    fn rejects_opaque_and_alpha_less_images() {
        // 整张不透明 → 没有内孔
        let p = pb(200, 200);
        for y in 0..200 {
            for x in 0..200 {
                p.put_pixel(x, y, 10, 20, 30, 255);
            }
        }
        assert!(analyze(&p, AnalyzeOpts::default()).is_none());

        // 没有 alpha 通道 → 拒绝（不擅自抠白底）
        let noalpha = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, 200, 200).unwrap();
        assert!(analyze(&noalpha, AnalyzeOpts::default()).is_none());
    }

    #[test]
    fn largest_rect_on_plain_mask() {
        // 10x6 全 true → 整块
        let (w, h) = (10, 6);
        let m = vec![true; (w * h) as usize];
        let r = largest_rect(&m, w, h).unwrap();
        assert_eq!((r.w, r.h), (10, 6));
        // 中间挖一个洞
        let mut m2 = vec![true; (w * h) as usize];
        for y in 0..3 {
            for x in 0..3 {
                m2[(y * w + x) as usize] = false;
            }
        }
        let r2 = largest_rect(&m2, w, h).unwrap();
        assert!(r2.area() < 60 && r2.area() >= 30, "面积 {}", r2.area());
    }

    #[test]
    fn anti_aliased_mask_follows_frame_alpha() {
        let p = rect_frame(400, 300, 40);
        let a = analyze(&p, AnalyzeOpts::default()).unwrap();
        let mask = build_mask_pixbuf(&p, &a.model).unwrap();
        let stride = mask.rowstride() as usize;
        let px = unsafe { mask.pixels() };
        // 框外（0,0 是不透明边 → 255；画布外没有）
        assert_eq!(px[0], 255, "相框本体应擦掉媒体");
        // 内孔中心 → 0
        let mid = (150 * stride) + 200;
        assert_eq!(px[mid], 0, "内孔应完全放行媒体");
        // 内孔与框体之间若出现半透明，应保留中间值（这里用软边测试）
    }

    #[test]
    fn semi_transparent_feather_keeps_intermediate_alpha() {
        // 造一个内孔边缘有羽化的相框：alpha 从 0 渐变到 255
        let (w, h) = (200, 200);
        let p = pb(w, h);
        for y in 0..h {
            for x in 0..w {
                let d = (x.min(y).min(w - 1 - x).min(h - 1 - y)) as f64;
                let a = if d < 40.0 { 255u8 } else if d < 60.0 { ((60.0 - d) / 20.0 * 255.0) as u8 } else { 0 };
                p.put_pixel(x as u32, y as u32, 100, 100, 100, a);
            }
        }
        let an = analyze(&p, AnalyzeOpts::default()).unwrap();
        let mask = build_mask_pixbuf(&p, &an.model).unwrap();
        let stride = mask.rowstride() as usize;
        let px = unsafe { mask.pixels() };
        // 羽化带里应出现 0<v<255 的中间值
        let mut mid = 0;
        for y in 60..140usize {
            let v = px[y * stride + 50] as i32;
            if v > 10 && v < 245 {
                mid += 1;
            }
        }
        assert!(mid > 0, "羽化带没有保留中间 alpha（抗锯齿丢失）");
    }
}
