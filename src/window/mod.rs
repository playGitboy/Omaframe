//! 窗口后端选择与显示器工具。
//!
//! 首选 **wlr-layer-shell（bottom 层）**：不参与平铺、永不获得键盘焦点、
//! 永远位于普通窗口之下，且指针事件只在其未被覆盖时到达。
//! 合成器不支持 layer-shell 时降级为普通 toplevel（功能可用但不再保证"不抢焦点"）。

pub mod frame_window;
pub mod media_view;

use crate::geometry;
use gdk::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    LayerShell,
    Toplevel,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::LayerShell => "layer-shell",
            Backend::Toplevel => "toplevel",
        }
    }
}

pub fn detect_backend() -> Backend {
    if layer_shell::is_supported() {
        Backend::LayerShell
    } else {
        crate::warn!("当前合成器不支持 wlr-layer-shell，降级为普通窗口（可能抢焦点）");
        Backend::Toplevel
    }
}

/// 按 connector 名找显示器；找不到则回落到主屏 → 第一个屏幕。
pub fn target_monitor(connector: &str) -> Option<gdk::Monitor> {
    let display = gdk::Display::default()?;
    let monitors = display.monitors();
    let n = monitors.n_items();
    if !connector.is_empty() {
        for i in 0..n {
            if let Some(m) = monitors.item(i).and_downcast::<gdk::Monitor>() {
                if m.connector().map(|c| c.to_string()).as_deref() == Some(connector) {
                    return Some(m);
                }
            }
        }
        crate::warn!("找不到显示器 {}，回落到主显示器", connector);
    }
    display
        .monitors()
        .item(0)
        .and_downcast::<gdk::Monitor>()
}

pub fn monitor_bounds(monitor: &gdk::Monitor) -> geometry::Bounds {
    let g = monitor.geometry();
    geometry::Bounds {
        width: g.width(),
        height: g.height(),
    }
}

/// 输出缩放倍数（解码目标尺寸 = 逻辑尺寸 × 该值）。
/// 只用 GTK 4.10 就有的 `scale_factor()`（整数），避免抬高编译期 GTK 版本要求；
/// 上限 2.0 是为了控制内存（HiDPI 下 2x 已足够清晰）。
pub fn monitor_scale(monitor: &gdk::Monitor) -> f64 {
    let sf = monitor.scale_factor();
    if sf > 0 {
        return (sf as f64).clamp(1.0, 2.0);
    }
    1.0
}
