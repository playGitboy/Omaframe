//! 轻量日志：stderr + 轮转文件，不引入 tracing/log 依赖。
//! 级别由 `PHOTO_FRAME_LOG` 决定（error|warn|info|debug|trace），默认 info。

use std::fmt::Arguments;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

impl Level {
    pub fn parse(s: &str) -> Level {
        match s.trim().to_ascii_lowercase().as_str() {
            "error" => Level::Error,
            "warn" | "warning" => Level::Warn,
            "debug" => Level::Debug,
            "trace" => Level::Trace,
            _ => Level::Info,
        }
    }
    fn tag(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN ",
            Level::Info => "INFO ",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        }
    }
}

const MAX_LOG_BYTES: u64 = 1_000_000;

struct Sink {
    level: Level,
    file: Option<File>,
    truncated: bool,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

pub fn init(state_dir: &std::path::Path, level: Level) {
    let dir = state_dir.join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("omaframe.log");

    // 轮转：超过 1MB 时把旧文件挪成 .1
    let mut file = None;
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_LOG_BYTES {
            let _ = std::fs::rename(&path, dir.join("omaframe.log.1"));
        }
    }
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => file = Some(f),
        Err(e) => eprintln!("photo-frame: 日志文件不可写（{}），仅输出到 stderr", e),
    }

    let _ = SINK.set(Mutex::new(Sink {
        level,
        file,
        truncated: false,
    }));
}

pub fn level() -> Level {
    SINK.get()
        .and_then(|s| s.lock().ok().map(|s| s.level))
        .unwrap_or(Level::Info)
}

pub fn enabled(l: Level) -> bool {
    l <= level()
}

fn timestamp() -> String {
    // 不引入 chrono：取 wall clock 的秒/分/时 + 日期用 libc-free 近似
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = now / 86_400;
    let secs = now % 86_400;
    format!(
        "d{}-{:02}:{:02}:{:02}",
        days,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

pub fn emit(l: Level, args: Arguments<'_>) {
    let Some(sink) = SINK.get() else {
        eprintln!("[{}] {}", l.tag(), args);
        return;
    };
    let Ok(mut s) = sink.lock() else { return };
    if l > s.level {
        return;
    }
    let line = format!("[{}] [{}] {}\n", timestamp(), l.tag(), args);
    eprint!("{line}");
    if let Some(f) = s.file.as_mut() {
        if f.write_all(line.as_bytes()).is_err() && !s.truncated {
            s.truncated = true;
            s.file = None;
        }
    }
}

#[macro_export]
macro_rules! log_at {
    ($lvl:expr, $($arg:tt)*) => {
        $crate::log::emit($lvl, format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => { $crate::log_at!($crate::log::Level::Error, $($arg)*) };
}
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::log_at!($crate::log::Level::Warn, $($arg)*) };
}
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::log_at!($crate::log::Level::Info, $($arg)*) };
}
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {
        if $crate::log::enabled($crate::log::Level::Debug) {
            $crate::log_at!($crate::log::Level::Debug, $($arg)*)
        }
    };
}
