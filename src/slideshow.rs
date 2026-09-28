//! 自动轮换：计时 + 顺序/随机。
//!
//! - 计时器只在"可见且未被窗口覆盖"时跑（`set_active`），被覆盖立刻停表；
//! - 用 glib 主循环源实现，不额外占用线程；
//! - 视频"播完再切"由播放器接管（Step 5/6），这里只负责图片计时与"下一项"节奏。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

type Tick = Rc<dyn Fn()>;

pub struct Slideshow {
    source: RefCell<Option<glib::SourceId>>,
    interval: Cell<u32>,
    enabled: Cell<bool>,
    /// 被窗口覆盖 → false
    active: Cell<bool>,
    /// 用户按了暂停按钮 → true
    paused: Cell<bool>,
    on_tick: RefCell<Option<Tick>>,
}

impl Slideshow {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            source: RefCell::new(None),
            interval: Cell::new(300),
            enabled: Cell::new(true),
            active: Cell::new(true),
            paused: Cell::new(false),
            on_tick: RefCell::new(None),
        })
    }

    /// 是否正在计时（配置允许 && 未被覆盖 && 未被用户暂停）
    pub fn running(&self) -> bool {
        self.enabled.get() && self.active.get() && !self.paused.get()
    }

    /// 用户点击播放/暂停（图片时暂停/继续轮换）
    pub fn toggle_paused(self: &Rc<Self>) -> bool {
        let p = !self.paused.get();
        self.paused.set(p);
        if p {
            self.stop();
        } else {
            self.restart();
        }
        crate::debug!("轮换暂停 = {}", p);
        p
    }

    #[allow(dead_code)]
    pub fn is_paused(&self) -> bool {
        self.paused.get()
    }

    /// 注入 tick 回调（只需设置一次）
    pub fn set_tick(&self, cb: impl Fn() + 'static) {
        *self.on_tick.borrow_mut() = Some(Rc::new(cb));
    }

    pub fn configure(self: &Rc<Self>, enabled: bool, interval: u32) {
        let changed =
            self.interval.replace(interval.max(1)) != interval.max(1) || self.enabled.replace(enabled) != enabled;
        if changed && self.running() {
            self.restart();
        } else if !enabled {
            self.stop();
        }
    }

    /// 可见性：被窗口覆盖时 false → 停表；恢复时 true → 重新计时
    pub fn set_active(self: &Rc<Self>, active: bool) {
        if self.active.replace(active) == active {
            return;
        }
        if active {
            self.restart();
        } else {
            self.stop();
            crate::debug!("被覆盖：暂停轮换与播放");
        }
    }

    #[allow(dead_code)]
    pub fn is_active(&self) -> bool {
        self.active.get()
    }

    pub fn stop(&self) {
        if let Some(src) = self.source.borrow_mut().take() {
            src.remove();
        }
    }

    /// 重新开始计时（倒计时重置）
    pub fn restart(self: &Rc<Self>) {
        self.stop();
        if !self.running() {
            return;
        }
        let Some(cb) = self.on_tick.borrow().clone() else {
            return;
        };
        let me = self.clone();
        let source = glib::timeout_add_local(
            Duration::from_secs(self.interval.get() as u64),
            move || {
                cb();
                if me.running() {
                    glib::ControlFlow::Continue
                } else {
                    glib::ControlFlow::Break
                }
            },
        );
        *self.source.borrow_mut() = Some(source);
    }

    #[allow(dead_code)]
    pub fn interval(&self) -> u32 {
        self.interval.get()
    }
}
