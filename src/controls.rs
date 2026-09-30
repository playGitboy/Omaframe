//! 悬停控制层：左右切换 + **底图正中**的播放/暂停 + 右下角 resize handle。
//!
//! 关键设计：**绘制和命中检测共用 `ControlLayout` 的同一套矩形**，
//! 因此不会出现"看得见却点不到"。控件全部用 cairo 矢量绘制，
//! 不依赖图标主题，换主题/换发行版外观都一致。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HitZone {
    #[default]
    None,
    Prev,
    PlayPause,
    Next,
    Resize,
}

/// 控件几何：以组件逻辑像素为单位
/// 拖拽模式：移动组件 / 右下角改大小
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragMode {
    Move,
    Resize,
}

#[derive(Debug, Clone, Copy)]
pub enum DragPhase {
    /// 按下（模式 + 起点坐标）
    Begin(DragMode, f64, f64),
    /// 拖动中（相对按下点的位移）
    Update(f64, f64),
    /// 松开
    End,
}

#[derive(Debug, Clone, Copy)]
pub struct ControlLayout {
    pub w: f64,
    pub h: f64,
    /// 按钮直径
    pub d: f64,
    /// 媒体（底图）矩形，相对相框左上角。播放/暂停按钮落在**它的中心**。
    pub media: (f64, f64, f64, f64),
}

impl ControlLayout {
    /// 只有相框尺寸时：媒体默认铺满整个相框（按钮落在相框中心）
    pub fn new(w: i32, h: i32) -> Self {
        Self::with_media(w, h, 0.0, 0.0, w as f64, h as f64)
    }

    /// 带媒体矩形（相框局部坐标）
    pub fn with_media(w: i32, h: i32, mx: f64, my: f64, mw: f64, mh: f64) -> Self {
        let w = w.max(1) as f64;
        let h = h.max(1) as f64;
        // 小组件小 → 按钮按比例缩小，但不低于 22px 便于点按
        let d = (w.min(h) * 0.16).clamp(22.0, 38.0);
        Self {
            w,
            h,
            d,
            media: (mx, my, mw.max(1.0), mh.max(1.0)),
        }
    }

    /// 媒体中心（相对相框左上角）
    pub fn media_center(&self) -> (f64, f64) {
        (
            self.media.0 + self.media.2 / 2.0,
            self.media.1 + self.media.3 / 2.0,
        )
    }

    /// 播放/暂停按钮：**位于底图正中**
    pub fn play_rect(&self) -> (f64, f64, f64, f64) {
        let (cx, cy) = self.media_center();
        (cx - self.d / 2.0, cy - self.d / 2.0, self.d, self.d)
    }

    /// 右下角 resize 热区（比按钮略大一点，方便抓）
    pub fn resize_rect(&self) -> (f64, f64, f64, f64) {
        let s = (self.d * 0.8).max(18.0);
        (self.w - s, self.h - s, s, s)
    }

    /// 命中检测：
    /// - 右下角小方块 = 改大小
    /// - 底图正中圆钮 = 播放/暂停
    /// - 其余区域按左右半边分：左半 = 上一项，右半 = 下一项（无按钮，纯点击热区）
    pub fn hit(&self, x: f64, y: f64) -> HitZone {
        if inside(self.resize_rect(), x, y) {
            return HitZone::Resize;
        }
        if inside(self.play_rect(), x, y) {
            return HitZone::PlayPause;
        }
        if x < 0.0 || y < 0.0 || x > self.w || y > self.h {
            return HitZone::None;
        }
        if x < self.w / 2.0 {
            HitZone::Prev
        } else {
            HitZone::Next
        }
    }

    /// 拖动模式判定（与点击热区分开：左右半区是"切图"，不是拖动把手）
    pub fn drag_mode_at(&self, x: f64, y: f64) -> Option<DragMode> {
        if inside(self.resize_rect(), x, y) {
            return Some(DragMode::Resize);
        }
        if inside(self.play_rect(), x, y) {
            return None; // 播放/暂停按钮上不启动拖动
        }
        if x < 0.0 || y < 0.0 || x > self.w || y > self.h {
            return None;
        }
        Some(DragMode::Move)
    }
}

fn inside(r: (f64, f64, f64, f64), x: f64, y: f64) -> bool {
    x >= r.0 && y >= r.1 && x <= r.0 + r.2 && y <= r.1 + r.3
}

/// 悬停控制状态：淡入淡出动画 + 当前 hover 区域。
/// 内部用 `Rc<Inner>`，动画定时器才能安全地持有 `'static` 引用。
#[derive(Default)]
struct Inner {
    hover: Cell<bool>,
    progress: Cell<f64>,
    zone: Cell<HitZone>,
    running: Cell<bool>,
    anim: RefCell<Option<glib::SourceId>>,
    /// 动画每一步都要触发重绘（GTK 不会因为 Cell 变化自动重绘）
    redraw: RefCell<Option<Rc<dyn Fn()>>>,
}

#[derive(Clone, Default)]
pub struct Controls {
    inner: Rc<Inner>,
}

const FADE_MS: f64 = 150.0;

impl Controls {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn hover(&self) -> bool {
        self.inner.hover.get()
    }

    pub fn progress(&self) -> f64 {
        self.inner.progress.get()
    }

    pub fn zone(&self) -> HitZone {
        self.inner.zone.get()
    }

    pub fn running(&self) -> bool {
        self.inner.running.get()
    }

    pub fn set_hover(&self, inside: bool) {
        if self.inner.hover.replace(inside) != inside {
            if !inside {
                self.inner.zone.set(HitZone::None);
            }
            self.start_anim();
            self.request_redraw();
        }
    }

    /// 注册重绘回调（由 MediaView 提供：queue_draw）
    pub fn set_redraw_hook(&self, f: impl Fn() + 'static) {
        *self.inner.redraw.borrow_mut() = Some(Rc::new(f));
    }

    fn request_redraw(&self) {
        if let Some(f) = self.inner.redraw.borrow().as_ref() {
            f();
        }
    }

    pub fn set_zone(&self, zone: HitZone) {
        if self.inner.zone.replace(zone) != zone {
            self.request_redraw();
        }
    }

    pub fn set_running(&self, running: bool) {
        if self.inner.running.replace(running) != running {
            self.request_redraw();
        }
    }

    /// 淡入淡出：只在过渡期间挂一个 16ms 定时器，静止时零 CPU
    fn start_anim(&self) {
        if self.inner.anim.borrow().is_some() {
            return; // 已有动画在跑
        }
        let step = 16.0 / FADE_MS;
        let inner = self.inner.clone();
        let id = glib::timeout_add_local(Duration::from_millis(16), move || {
            let target = if inner.hover.get() { 1.0 } else { 0.0 };
            let cur = inner.progress.get();
            let delta = target - cur;
            if delta.abs() < step {
                inner.progress.set(target);
                if let Some(id) = inner.anim.borrow_mut().take() {
                    id.remove();
                }
                if let Some(f) = inner.redraw.borrow().as_ref() {
                    f();
                }
                glib::ControlFlow::Break
            } else {
                inner.progress.set(cur + delta * 0.35);
                if let Some(f) = inner.redraw.borrow().as_ref() {
                    f();
                }
                glib::ControlFlow::Continue
            }
        });
        *self.inner.anim.borrow_mut() = Some(id);
    }

    /// 组件尺寸变化（例如切换图片导致窗口大小变了）：
    /// 只清掉"当前高亮区域"，**不动淡入淡出状态** ——
    /// 否则光标还停在组件里时按钮会突然消失。
    pub fn on_resize(&self) {
        self.inner.zone.set(HitZone::None);
        let target = if self.inner.hover.get() { 1.0 } else { 0.0 };
        if (self.inner.progress.get() - target).abs() > 0.001 {
            self.inner.progress.set(target);
        }
        self.request_redraw();
    }
}

/// 绘制控制层。`alpha` = 0..1 整体透明度。
pub fn paint(cr: &cairo::Context, layout: &ControlLayout, controls: &Controls) {
    let a = controls.progress().clamp(0.0, 1.0);
    if a <= 0.01 {
        return;
    }
    let zone = controls.zone();

    button(
        cr,
        layout.play_rect(),
        a,
        zone == HitZone::PlayPause,
        if controls.running() {
            Icon::Pause
        } else {
            Icon::Play
        },
    );

    // 右下角 resize handle：弱化的小箭头
    let r = layout.resize_rect();
    let cx = r.0 + r.2 / 2.0;
    let cy = r.1 + r.3 / 2.0;
    let s = r.2 * 0.22;
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.35 * a);
    cr.set_line_width(1.6);
    cr.move_to(cx - s * 0.9, cy + s * 0.9);
    cr.line_to(cx + s * 0.9, cy - s * 0.9);
    let _ = cr.stroke();
}

#[derive(Clone, Copy, PartialEq)]
enum Icon {
    Play,
    Pause,
}

fn button(cr: &cairo::Context, r: (f64, f64, f64, f64), alpha: f64, hot: bool, icon: Icon) {
    let (x, y, w, h) = r;
    let cx = x + w / 2.0;
    let cy = y + h / 2.0;
    let rad = w / 2.0;

    // 背板
    cr.set_source_rgba(0.0, 0.0, 0.0, if hot { 0.62 * alpha } else { 0.42 * alpha });
    cr.arc(cx, cy, rad, 0.0, std::f64::consts::TAU);
    let _ = cr.fill();
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.22 * alpha);
    cr.set_line_width(1.0);
    cr.arc(cx, cy, rad - 0.5, 0.0, std::f64::consts::TAU);
    let _ = cr.stroke();

    // 图标
    let s = w * 0.24;
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.95 * alpha);
    match icon {
        Icon::Play => {
            triangle(cr, cx - s * 0.75, cy - s, cx - s * 0.75, cy + s, cx + s * 0.95, cy);
        }
        Icon::Pause => {
            let bw = s * 0.62;
            let _ = cr.rectangle(cx - s * 0.85, cy - s, bw, s * 2.0);
            let _ = cr.rectangle(cx + s * 0.23, cy - s, bw, s * 2.0);
            let _ = cr.fill();
        }
    }
}

fn triangle(cr: &cairo::Context, x1: f64, y1: f64, x2: f64, y2: f64, x3: f64, y3: f64) {
    cr.move_to(x1, y1);
    cr.line_to(x2, y2);
    cr.line_to(x3, y3);
    cr.close_path();
    let _ = cr.fill();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_right_halves_switch_items() {
        let l = ControlLayout::new(600, 338);
        assert_eq!(l.hit(60.0, 150.0), HitZone::Prev);
        assert_eq!(l.hit(540.0, 150.0), HitZone::Next);
        let (px, py, pw, ph) = l.play_rect();
        assert_eq!(l.hit(px + pw / 2.0, py + ph / 2.0), HitZone::PlayPause);
        let (rx, ry, rw, rh) = l.resize_rect();
        assert_eq!(l.hit(rx + rw / 2.0, ry + rh / 2.0), HitZone::Resize);
        assert_eq!(l.hit(-5.0, 100.0), HitZone::None);
    }

    #[test]
    fn play_button_sits_at_media_center() {
        // 相框 600x400，但媒体偏在右上（不规则相框的常见情况）
        let l = ControlLayout::with_media(600, 400, 300.0, 40.0, 260.0, 200.0);
        let (px, py, pw, ph) = l.play_rect();
        let cx = px + pw / 2.0;
        let cy = py + ph / 2.0;
        // 按钮中心 == 媒体中心
        assert!((cx - 430.0).abs() < 0.01, "按钮中心 x {cx} 应为媒体中心 430");
        assert!((cy - 140.0).abs() < 0.01, "按钮中心 y {cy} 应为媒体中心 140");
        // 命中与拖动判定都按这个位置
        assert_eq!(l.hit(cx, cy), HitZone::PlayPause);
        assert_eq!(l.drag_mode_at(cx, cy), None);
        // 相框中心不再是播放键（播放键跟随媒体，避免和左右热区/拖动混淆）
        assert_ne!(l.hit(300.0, 200.0), HitZone::PlayPause);
        assert_eq!(l.hit(300.0, 200.0), HitZone::Next);
    }

    #[test]
    fn resize_handle_sits_at_frame_bottom_right() {
        // 相框 500x300（几何重构后：相框按 PNG 自身比例，可能小于上限盒）
        let l = ControlLayout::new(500, 300);
        let (rx, ry, rw, rh) = l.resize_rect();
        // 手柄在右下角，且尺寸合理（14~25px）
        assert!(rx + rw <= 500.0 && ry + rh <= 300.0, "手柄越界: {rx},{ry},{rw},{rh}");
        assert!(rw >= 14.0 && rw <= 34.0, "手柄尺寸不合理: {rw}");
        // 命中点在手柄内
        let hx = rx + rw / 2.0;
        let hy = ry + rh / 2.0;
        assert_eq!(l.hit(hx, hy), HitZone::Resize);
        assert_eq!(l.drag_mode_at(hx, hy), Some(DragMode::Resize));
        // 非按钮区域是 Move（不是 Resize）；中心现在是播放/暂停键，不启动拖动
        assert_eq!(l.drag_mode_at(250.0, 60.0), Some(DragMode::Move));
        assert_eq!(l.drag_mode_at(250.0, 150.0), None);
        // 播放/暂停按钮上不启动拖动
        let (px, py, pw, ph) = l.play_rect();
        assert_eq!(l.drag_mode_at(px + pw / 2.0, py + ph / 2.0), None);
    }

    #[test]
    fn drag_handles_ignore_half_zones() {
        let l = ControlLayout::new(600, 338);
        // 左右半区是"切图热区"，但仍然可以拖动移动
        assert_eq!(l.drag_mode_at(60.0, 150.0), Some(DragMode::Move));
        let (px, py, pw, ph) = l.play_rect();
        assert_eq!(l.drag_mode_at(px + pw / 2.0, py + ph / 2.0), None);
        let (rx, ry, rw, rh) = l.resize_rect();
        assert_eq!(
            l.drag_mode_at(rx + rw / 2.0, ry + rh / 2.0),
            Some(DragMode::Resize)
        );
    }

    #[test]
    fn tiny_widget_still_clickable() {
        let l = ControlLayout::new(160, 120);
        assert!(l.d >= 22.0);
        let (px, py, pw, ph) = l.play_rect();
        assert!(px >= 0.0 && py >= 0.0 && px + pw <= 160.0 && py + ph <= 120.0);
    }
}
