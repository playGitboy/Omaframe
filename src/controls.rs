//! 悬停控制层：左右切换 + 底部中央播放/暂停 + 右下角 resize handle。
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
#[derive(Debug, Clone, Copy)]
pub struct ControlLayout {
    pub w: f64,
    pub h: f64,
    /// 按钮直径
    pub d: f64,
    /// 按钮与边缘的间距
    pub pad: f64,
}

impl ControlLayout {
    pub fn new(w: i32, h: i32) -> Self {
        let w = w.max(1) as f64;
        let h = h.max(1) as f64;
        // 小组件小 → 按钮按比例缩小，但不低于 22px 便于点按
        let d = (w.min(h) * 0.16).clamp(22.0, 38.0);
        Self {
            w,
            h,
            d,
            pad: (d * 0.45).max(6.0),
        }
    }

    pub fn prev_rect(&self) -> (f64, f64, f64, f64) {
        (self.pad, (self.h - self.d) / 2.0, self.d, self.d)
    }

    pub fn next_rect(&self) -> (f64, f64, f64, f64) {
        (
            self.w - self.pad - self.d,
            (self.h - self.d) / 2.0,
            self.d,
            self.d,
        )
    }

    pub fn play_rect(&self) -> (f64, f64, f64, f64) {
        (
            (self.w - self.d) / 2.0,
            self.h - self.pad - self.d,
            self.d,
            self.d,
        )
    }

    /// 右下角 resize 热区（比按钮略大一点，方便抓）
    pub fn resize_rect(&self) -> (f64, f64, f64, f64) {
        let s = (self.d * 0.8).max(18.0);
        (self.w - s, self.h - s, s, s)
    }

    pub fn hit(&self, x: f64, y: f64) -> HitZone {
        // resize 优先（它在角落，与其它按钮不重叠）
        if inside(self.resize_rect(), x, y) {
            return HitZone::Resize;
        }
        if inside(self.play_rect(), x, y) {
            return HitZone::PlayPause;
        }
        if inside(self.prev_rect(), x, y) {
            return HitZone::Prev;
        }
        if inside(self.next_rect(), x, y) {
            return HitZone::Next;
        }
        HitZone::None
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

    button(cr, layout.prev_rect(), a, zone == HitZone::Prev, Icon::Prev);
    button(cr, layout.next_rect(), a, zone == HitZone::Next, Icon::Next);
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
    Prev,
    Next,
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
        Icon::Prev => {
            triangle(cr, cx + s * 0.15, cy - s, cx + s * 0.15, cy + s, cx - s * 0.85, cy);
            let _ = cr.rectangle(cx + s * 0.35, cy - s, s * 0.42, s * 2.0);
            let _ = cr.fill();
        }
        Icon::Next => {
            triangle(cr, cx - s * 0.15, cy - s, cx - s * 0.15, cy + s, cx + s * 0.85, cy);
            let _ = cr.rectangle(cx - s * 0.77, cy - s, s * 0.42, s * 2.0);
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
    fn hit_zones_do_not_overlap() {
        let l = ControlLayout::new(600, 338);
        // 左右按钮
        assert_eq!(l.hit(l.pad + 5.0, l.h / 2.0), HitZone::Prev);
        assert_eq!(l.hit(l.w - l.pad - 5.0, l.h / 2.0), HitZone::Next);
        // 底部中央播放/暂停
        let (px, py, pw, ph) = l.play_rect();
        assert_eq!(l.hit(px + pw / 2.0, py + ph / 2.0), HitZone::PlayPause);
        // 空白处
        assert_eq!(l.hit(l.w / 2.0, 20.0), HitZone::None);
        // 右下角
        let (rx, ry, rw, rh) = l.resize_rect();
        assert_eq!(l.hit(rx + rw / 2.0, ry + rh / 2.0), HitZone::Resize);
    }

    #[test]
    fn tiny_widget_still_clickable() {
        let l = ControlLayout::new(160, 120);
        assert!(l.d >= 22.0);
        let (px, py, pw, ph) = l.play_rect();
        assert!(px >= 0.0 && py >= 0.0 && px + pw <= 160.0 && py + ph <= 120.0);
    }
}
