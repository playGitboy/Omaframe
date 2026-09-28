//! 媒体抽象：V1 只有本地目录，但 UI 层只依赖这里的类型，
//! 以后加 `SmbMediaSource` / `WebdavMediaSource` 不需要改 UI。

pub mod image;
pub mod library;
pub mod source;
pub mod video;

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
}

/// 一个媒体条目。`natural` 是原始像素尺寸：
/// - 图片：解码时探测
/// - 视频：V1 为 None（拿到 GstCaps 之前不知道）
#[derive(Debug, Clone, PartialEq)]
pub struct MediaItem {
    pub path: PathBuf,
    pub kind: MediaKind,
    pub natural: Option<(i32, i32)>,
    /// 稳定 id（当前用路径；将来远程源用 url/opaque id）
    pub id: String,
}

impl MediaItem {
    pub fn new(path: PathBuf, kind: MediaKind) -> Self {
        let id = path.to_string_lossy().into_owned();
        Self {
            path,
            kind,
            natural: None,
            id,
        }
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.id.clone())
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum SourceError {
    NotADirectory(String),
    Unreadable(String, std::io::Error),
    Unsupported(String),
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceError::NotADirectory(p) => write!(f, "不是目录：{p}"),
            SourceError::Unreadable(p, e) => write!(f, "无法读取 {p}：{e}"),
            SourceError::Unsupported(t) => write!(f, "暂不支持的媒体源类型：{t}"),
        }
    }
}

/// 媒体源。**scan 会在后台线程被调用**，实现里不要碰 GTK。
/// 未来的远程实现可以在这里做挂载探测、列目录、鉴权，UI 无需改动。
pub trait MediaSource: Send + Sync {
    /// 配置里的 `type` 字段值
    fn type_name(&self) -> &'static str;
    /// 人类可读描述（设置页展示）
    fn describe(&self) -> String;
    /// 是否实现了"外部变更检测"（V2 远程源预留）
    #[allow(dead_code)]
    fn supports_watch(&self) -> bool {
        false
    }
    fn scan(&self) -> Result<Vec<MediaItem>, SourceError>;
}

/// 按 `[source] type` 创建媒体源；未知类型返回错误（V2 在此扩展分支）。
pub fn create_source(cfg: &crate::config::SourceConfig) -> Result<Box<dyn MediaSource>, SourceError> {
    match cfg.kind.as_str() {
        "local" | "file" => Ok(Box::new(source::local::LocalMediaSource::new(
            cfg.path.clone(),
            cfg.recursive,
        ))),
        other => Err(SourceError::Unsupported(other.to_string())),
    }
}

/// 扩展名 → 媒体类型。GIF 归为图片（GTK4 只显示首帧）。
pub fn kind_from_extension(path: &std::path::Path) -> Option<MediaKind> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())?;
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" | "tif" | "tiff" | "avif" => {
            Some(MediaKind::Image)
        }
        "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi" | "mpg" | "mpeg" | "ts" | "ogv" => {
            Some(MediaKind::Video)
        }
        _ => None,
    }
}
