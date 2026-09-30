//! 媒体库：后台扫描目录，维护条目列表与当前索引。
//! UI 只跟 `MediaItem` 打交道，不关心来源是本地还是未来的 SMB/WebDAV。

use super::{MediaItem, MediaSource, SourceError};
use rand::Rng;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub enum ScanStatus {
    Idle,
    Scanning,
    Ready(usize),
    Failed(String),
}

thread_local! {
    static LIB: RefCell<Option<std::rc::Weak<MediaLibrary>>> = const { RefCell::new(None) };
}

pub struct MediaLibrary {
    /// 媒体源可在设置页改目录后**热替换**（见 `set_source`）
    source: RefCell<Arc<dyn MediaSource>>,
    items: RefCell<Vec<MediaItem>>,
    index: Cell<usize>,
    status: RefCell<ScanStatus>,
    ctx: glib::MainContext,
    on_scanned: RefCell<Vec<Box<dyn Fn()>>>,
}

impl MediaLibrary {
    pub fn new(source: Box<dyn MediaSource>) -> Rc<Self> {
        let lib = Rc::new(Self {
            source: RefCell::new(Arc::from(source)),
            items: RefCell::new(Vec::new()),
            index: Cell::new(0),
            status: RefCell::new(ScanStatus::Idle),
            ctx: glib::MainContext::default(),
            on_scanned: RefCell::new(Vec::new()),
        });
        LIB.with(|slot| *slot.borrow_mut() = Some(Rc::downgrade(&lib)));
        lib
    }

    /// 扫描回调（主线程）。一次性的：触发后失效。
    pub fn on_scanned(&self, cb: Box<dyn Fn()>) {
        self.on_scanned.borrow_mut().push(cb);
    }

    /// **更换媒体源**（设置页改了媒体目录后必须调用）：
    /// source 是创建时固定的，不换的话 rescan 扫的还是旧目录。
    pub fn set_source(&self, source: Box<dyn MediaSource>) {
        *self.source.borrow_mut() = Arc::from(source);
    }

    /// 后台线程扫描（不阻塞 UI）
    pub fn scan(self: &Rc<Self>) {
        *self.status.borrow_mut() = ScanStatus::Scanning;
        let source = self.source.borrow().clone();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("omaframe-scan".into())
            .spawn(move || {
                let result: Result<Vec<MediaItem>, String> = match source.scan() {
                    Ok(items) => Ok(items),
                    Err(e) => Err(describe(e)),
                };
                ctx.invoke(move || {
                    LIB.with(|slot| {
                        if let Some(l) = slot.borrow().as_ref().and_then(|w| w.upgrade()) {
                            l.apply_scan(result);
                        }
                    });
                });
            })
            .ok();
    }

    fn apply_scan(&self, result: Result<Vec<MediaItem>, String>) {
        match result {
            Ok(items) => {
                crate::info!("扫描到 {} 个媒体文件", items.len());
                *self.status.borrow_mut() = ScanStatus::Ready(items.len());
                *self.items.borrow_mut() = items;
                self.index.set(0);
            }
            Err(e) => {
                crate::error!("扫描媒体目录失败：{e}");
                *self.status.borrow_mut() = ScanStatus::Failed(e);
                self.items.borrow_mut().clear();
                self.index.set(0);
            }
        }
        for cb in self.on_scanned.borrow_mut().drain(..) {
            cb();
        }
    }

    #[allow(dead_code)]
    pub fn status(&self) -> ScanStatus {
        self.status.borrow().clone()
    }

    pub fn count(&self) -> usize {
        self.items.borrow().len()
    }

    #[allow(dead_code)]
    pub fn items(&self) -> Vec<MediaItem> {
        self.items.borrow().clone()
    }

    pub fn index(&self) -> usize {
        self.index.get()
    }

    pub fn current(&self) -> Option<MediaItem> {
        let items = self.items.borrow();
        items.get(self.index.get()).cloned()
    }

    /// 前进一张（顺序或随机），返回新的当前项
    pub fn advance(&self, random: bool) -> Option<MediaItem> {
        let len = self.count();
        if len == 0 {
            return None;
        }
        if len == 1 {
            self.index.set(0);
            return self.current();
        }
        let next = if random {
            let mut rng = rand::rng();
            let mut i = self.index.get();
            // 避免随机到同一张
            while i == self.index.get() {
                i = rng.random_range(0..len);
            }
            i
        } else {
            (self.index.get() + 1) % len
        };
        self.index.set(next);
        self.current()
    }

    /// 预取用：下一项（不改变索引）
    pub fn peek_next(&self, random: bool) -> Option<MediaItem> {
        let len = self.count();
        if len < 2 {
            return None;
        }
        let i = if random {
            rand::rng().random_range(0..len)
        } else {
            (self.index.get() + 1) % len
        };
        self.items.borrow().get(i).cloned()
    }

    pub fn set_index(&self, i: usize) {
        if i < self.count() {
            self.index.set(i);
        }
    }
}

fn describe(e: SourceError) -> String {
    match e {
        SourceError::NotADirectory(p) => format!("目录不存在：{p}"),
        SourceError::Unreadable(p, e) => format!("无法读取 {p}：{e}"),
        SourceError::Unsupported(t) => format!("暂不支持的媒体源类型：{t}"),
    }
}
