//! 本地目录媒体源：递归扫描 + 扩展名过滤 + 自然排序。
//! 扫描是阻塞的，调用方必须在后台线程执行（见 `MediaSource::scan`）。

use crate::config::expand_user;
use crate::media::{kind_from_extension, MediaItem, MediaSource, SourceError};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub struct LocalMediaSource {
    root: PathBuf,
    recursive: bool,
}

impl LocalMediaSource {
    pub fn new(path: String, recursive: bool) -> Self {
        Self {
            root: expand_user(&path),
            recursive,
        }
    }

    #[allow(dead_code)]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl MediaSource for LocalMediaSource {
    fn type_name(&self) -> &'static str {
        "local"
    }

    fn describe(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    fn supports_watch(&self) -> bool {
        true
    }

    fn scan(&self) -> Result<Vec<MediaItem>, SourceError> {
        if self.root.as_os_str().is_empty() {
            return Err(SourceError::NotADirectory(String::new()));
        }
        if !self.root.is_dir() {
            return Err(SourceError::NotADirectory(
                self.root.to_string_lossy().into_owned(),
            ));
        }

        let walker = if self.recursive {
            WalkDir::new(&self.root).follow_links(false)
        } else {
            WalkDir::new(&self.root).max_depth(1).follow_links(false)
        };

        let mut items: Vec<MediaItem> = walker
            .into_iter()
            .filter_map(|e| match e {
                Ok(entry) => Some(entry),
                Err(e) => {
                    crate::debug!("扫描跳过 {:?}: {}", e.path(), e);
                    None
                }
            })
            .filter(|e| e.file_type().is_file())
            .filter(|e| !is_hidden(e.path()))
            .filter_map(|e| {
                let path = e.into_path();
                kind_from_extension(&path).map(|kind| MediaItem::new(path, kind))
            })
            .collect();

        // 自然排序（2.jpg 排在 10.jpg 前面），无自然序时退回路径字典序
        items.sort_by(|a, b| {
            natural_cmp(&a.file_name().to_lowercase(), &b.file_name().to_lowercase())
        });
        Ok(items)
    }
}

fn is_hidden(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('.'))
        .unwrap_or(false)
}

/// 轻量自然排序：把数字段按数值比较。
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let na = take_number(&mut ai);
                    let nb = take_number(&mut bi);
                    match na.cmp(&nb) {
                        std::cmp::Ordering::Equal => continue,
                        o => return o,
                    }
                }
                let oa = ca.to_ascii_lowercase();
                let ob = cb.to_ascii_lowercase();
                ai.next();
                bi.next();
                match oa.cmp(&ob) {
                    std::cmp::Ordering::Equal => continue,
                    o => return o,
                }
            }
        }
    }
}

fn take_number<I: Iterator<Item = char>>(it: &mut std::iter::Peekable<I>) -> u128 {
    let mut s = String::new();
    while let Some(c) = it.peek().copied() {
        if c.is_ascii_digit() {
            s.push(c);
            it.next();
        } else {
            break;
        }
    }
    s.parse().unwrap_or(u128::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["10.jpg", "2.jpg", "1.jpg", "abc.jpg"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["1.jpg", "2.jpg", "10.jpg", "abc.jpg"]);
    }

    #[test]
    fn extension_classification() {
        use std::path::Path;
        assert_eq!(kind_from_extension(Path::new("a.JPG")), Some(crate::media::MediaKind::Image));
        assert_eq!(kind_from_extension(Path::new("a.webm")), Some(crate::media::MediaKind::Video));
        assert_eq!(kind_from_extension(Path::new("a.txt")), None);
    }
}
