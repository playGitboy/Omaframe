//! Hyprland IPC 事件订阅 → 「组件是否被窗口覆盖」判断。
//!
//! 设计要点（跨版本兼容）：
//! - **只用事件做触发，不用事件里的数据**：0.56 的 socket 负载从 JSON 变成了
//!   `openwindow>>ADDR,WS,CLASS,TITLE` 这样的 CSV，而且**不含窗口矩形**；
//!   而 `hyprctl -j clients` 的 JSON 字段（at/size/workspace/hidden）跨版本稳定。
//!   所以：事件 → 去抖 → 查一次 `hyprctl` → 判断。
//! - 去抖必需：拖动窗口时 `movewindow` 会连续触发，不去抖会 spawn 风暴。
//! - 非 Hyprland 环境（无 socket / 无 hyprctl）→ 永远视为可见，功能降级不出错。

use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
}

/// 焦点类事件：会影响覆盖关系，同时意味着"弹出面板该收起来了"
fn is_focus_change(event: &str) -> bool {
    matches!(
        event,
        "activewindow"
            | "activewindowv2"
            | "openwindow"
            | "workspace"
            | "workspacev2"
            | "focusedmon"
    )
}

/// 会影响覆盖关系的事件
fn is_relevant(event: &str) -> bool {
    matches!(
        event,
        "openwindow"
            | "closewindow"
            | "movewindow"
            | "resizewindow"
            | "activewindow"
            | "activewindowv2"
            | "workspace"
            | "workspacev2"
            | "focusedmon"
            | "minimize"
            | "fullscreen"
    )
}

struct State {
    me: Rect,
    covered: bool,
    checked: bool,
    timer: RefCell<Option<glib::SourceId>>,
}

pub struct VisibilityMonitor {
    state: RefCell<State>,
    on_change: RefCell<Option<Rc<dyn Fn(bool)>>>,
}

impl VisibilityMonitor {
    /// 启动监听。内部先做一次判定，之后由事件驱动。
    pub fn start(rect: Rect) -> Rc<Self> {
        let this = Rc::new(Self {
            state: RefCell::new(State {
                me: rect,
                covered: false,
                checked: false,
                timer: RefCell::new(None),
            }),
            on_change: RefCell::new(None),
        });
        MONITOR.with(|slot| *slot.borrow_mut() = Some(this.clone()));

        let Some(path) = event_socket() else {
            crate::info!("未找到 Hyprland 事件 socket：关闭「被覆盖则暂停」");
            return this;
        };
        AVAILABLE.with(|c| c.set(true));
        crate::debug!("Hyprland IPC: {}", path.display());

        let ctx = glib::MainContext::default();
        std::thread::Builder::new()
            .name("photo-frame-hypr".into())
            .spawn(move || {
                let Ok(stream) = UnixStream::connect(&path) else {
                    crate::warn!("无法连接 Hyprland IPC：{}", path.display());
                    return;
                };
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    let Some((ev, _payload)) = line.split_once(">>") else {
                        continue;
                    };
                    if !is_relevant(ev) {
                        continue;
                    }
                    crate::debug!("IPC 事件 {}", ev);
                    // 焦点类事件 → 通知弹出面板（"失去焦点就收起来"）
                    if is_focus_change(ev) {
                        let ctx2 = ctx.clone();
                        ctx2.invoke(|| {
                            FOCUS_CB.with(|slot| {
                                if let Some(cb) = slot.borrow().as_ref() {
                                    cb();
                                }
                            });
                        });
                    }
                    // 只传事件名（Send），判定在主线程做
                    let ctx = ctx.clone();
                    ctx.invoke(move || {
                        MONITOR.with(|slot| {
                            if let Some(m) = slot.borrow().as_ref() {
                                m.schedule_check();
                            }
                        });
                    });
                }
                crate::debug!("Hyprland IPC 连接结束");
            })
            .ok();

        this.schedule_check();
        this
    }

/// 覆盖状态变化回调（true = 可见/未被覆盖）
    pub fn on_change(&self, cb: impl Fn(bool) + 'static) {
        *self.on_change.borrow_mut() = Some(Rc::new(cb));
    }

    /// 组件矩形变化（尺寸/位置变了要重算）
    pub fn set_rect(self: &Rc<Self>, rect: Rect) {
        self.state.borrow_mut().me = rect;
        self.schedule_check();
    }

    /// 去抖：把连续事件合并成一次判定
    pub fn schedule_check(self: &Rc<Self>) {
        {
            let st = self.state.borrow();
            if st.timer.borrow().is_some() {
                crate::debug!("覆盖判定：已有判定在排队，跳过");
                return; // 已有 80ms 内的判定在排队
            }
        }
        crate::debug!("覆盖判定：排队 80ms 后执行");
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local(Duration::from_millis(80), move || {
            crate::debug!("判定定时器触发");
            MONITOR.with(|slot| {
                if slot.borrow().is_none() {
                    crate::warn!("MONITOR thread_local 为空！");
                }
                if let Some(m) = slot.borrow().as_ref() {
                    m.state.borrow_mut().timer.borrow_mut().take();
                    m.check_now();
                }
            });
            let _ = &weak;
            glib::ControlFlow::Break
        });
        *self.state.borrow().timer.borrow_mut() = Some(id);
    }

    /// 立即判定（读一次 hyprctl）
    fn check_now(&self) {
        let Some((windows, active_ws)) = query_windows() else {
            return; // 查不到就不改变状态，避免误暂停
        };
        let me = self.state.borrow().me;
        // 保守：当前工作区上任何与组件相交的窗口都算覆盖（浮窗同样算）
        let covered = windows.iter().any(|r| r.intersects(&me));
        let changed = {
            let mut st = self.state.borrow_mut();
            let c = st.covered != covered || !st.checked;
            st.covered = covered;
            st.checked = true;
            c
        };
        if !changed {
            crate::debug!(
                "覆盖判定（未变化）：工作区={} 窗口数={} 组件={:?} 仍为 {}",
                active_ws,
                windows.len(),
                me,
                if covered { "被覆盖" } else { "可见" }
            );
        }
        if changed {
            crate::debug!(
                "覆盖判定：工作区={} 窗口数={} 组件={:?} → {}",
                active_ws,
                windows.len(),
                me,
                if covered { "被覆盖（暂停）" } else { "可见（继续）" }
            );
            let cb = self.on_change.borrow().clone();
            if let Some(cb) = cb {
                cb(!covered);
            }
        }
    }
}

// 主线程侧的 monitor（读线程通过 invoke 回调里取用）
/// 注册"焦点变化"回调（主线程调用；替换式注册）。
/// 弹出面板用它实现"失去焦点自动收起"（别的窗口激活 / 换工作区时触发）。
pub fn on_focus_change(cb: impl Fn() + 'static) {
    FOCUS_CB.with(|slot| *slot.borrow_mut() = Some(Rc::new(cb)));
}

thread_local! {
    /// 焦点变化回调（主线程）；弹出面板用它实现"失去焦点自动收起"
    static FOCUS_CB: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };

    static MONITOR: RefCell<Option<Rc<VisibilityMonitor>>> = const { RefCell::new(None) };
    static AVAILABLE: Cell<bool> = const { Cell::new(false) };
}

/// 定位事件 socket。0.5x 之前叫 `.socket2.sock2`，0.56 叫 `.socket2.sock`，
/// 两个都试，再兜底扫目录 —— 避免版本差异导致功能静默失效。
fn event_socket() -> Option<PathBuf> {
    let sig = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let dir = PathBuf::from(runtime)
        .join("hypr")
        .join(sig.to_string_lossy().into_owned());
    for name in [".socket2.sock2", ".socket2.sock"] {
        let p = dir.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with(".socket2") {
                return Some(e.path());
            }
        }
    }
    None
}

type WinList = Vec<Rect>;

/// 读一次窗口列表（只保留当前工作区、未最小化的窗口）
fn query_windows() -> Option<(WinList, String)> {
    let clients = std::process::Command::new("hyprctl")
        .args(["-j", "clients"])
        .output()
        .ok()?;
    let active = std::process::Command::new("hyprctl")
        .args(["-j", "activeworkspace"])
        .output()
        .ok()?;
    let clients: Vec<Value> = serde_json::from_slice(&clients.stdout).ok()?;
    let active: Value = serde_json::from_slice(&active.stdout).ok()?;
    let active_id = active.get("id").and_then(|i| i.as_i64()).unwrap_or(-1);
    let active_name = active
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();

    let mut list = Vec::new();
    for c in clients {
        if c.get("hidden").and_then(|h| h.as_bool()).unwrap_or(false) {
            continue; // 最小化
        }
        let ws_ok = c
            .get("workspace")
            .and_then(|w| w.get("id"))
            .and_then(|i| i.as_i64())
            .map(|id| id == active_id)
            .unwrap_or(false);
        if !ws_ok {
            continue;
        }
        let (Some(at), Some(sz)) = (
            c.get("at").and_then(|a| a.as_array()),
            c.get("size").and_then(|a| a.as_array()),
        ) else {
            continue;
        };
        let (Some(x), Some(y), Some(w), Some(h)) = (
            at.first().and_then(|v| v.as_i64()),
            at.get(1).and_then(|v| v.as_i64()),
            sz.first().and_then(|v| v.as_i64()),
            sz.get(1).and_then(|v| v.as_i64()),
        ) else {
            continue;
        };
        list.push(Rect {
            x: x as i32,
            y: y as i32,
            w: w as i32,
            h: h as i32,
        });
    }
    Some((list, active_name))
}
