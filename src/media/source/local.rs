//! 本地目录媒体源：递归扫描 + 扩展名过滤 + 自然排序。
//! 扫描是阻塞的，调用方必须在后台线程执行（见 `MediaSource::scan`）。

use crate::config::expand_user;
use crate::media::{kind_from_extension, MediaItem, MediaSource, SourceError};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub struct LocalMediaSource {
    /// 多个根目录（按用户在设置里列出的顺序扫描并拼接）
    roots: Vec<PathBuf>,
    recursive: bool,
}

impl LocalMediaSource {
    /// 多目录（顺序即轮换顺序：逐目录扫描后拼接）
    pub fn new_multi(paths: Vec<String>, recursive: bool) -> Self {
        Self {
            roots: paths.iter().map(|p| expand_user(p)).collect(),
            recursive,
        }
    }

    #[allow(dead_code)]
    pub fn root(&self) -> &Path {
        &self.roots[0]
    }
}

impl MediaSource for LocalMediaSource {
    fn type_name(&self) -> &'static str {
        "local"
    }

    fn describe(&self) -> String {
        self.roots
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn supports_watch(&self) -> bool {
        true
    }

    /// 多目录：**逐目录扫描后拼接**（保留用户列出的顺序），并按 id 去重。
    /// 单个目录不可用（NAS 掉线等）**不影响其它目录** —— 全部不可用才报错。
    ///
    /// 命中扫描缓存时直接返回（大素材量下**重启/开面板秒开**）。
    fn scan(&self) -> Result<Vec<MediaItem>, SourceError> {
        if let Some(items) = load_cache(&self.roots, self.recursive) {
            crate::info!("素材扫描缓存命中：{} 项（目录未变化）", items.len());
            return Ok(items);
        }
        let items = self.scan_uncached()?;
        save_cache(&self.roots, self.recursive, &items);
        Ok(items)
    }
}

impl LocalMediaSource {
    fn scan_uncached(&self) -> Result<Vec<MediaItem>, SourceError> {
        let mut out: Vec<MediaItem> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut ok_any = false;
        let mut err: Option<SourceError> = None;
        for root in &self.roots {
            match scan_one(root, self.recursive) {
                Ok(items) => {
                    ok_any = true;
                    for it in items {
                        if seen.insert(it.id.clone()) {
                            out.push(it);
                        }
                    }
                }
                Err(e) => {
                    crate::warn!("素材目录不可用，跳过：{}", root.display());
                    if err.is_none() {
                        err = Some(e);
                    }
                }
            }
        }
        if ok_any {
            Ok(out)
        } else {
            Err(err.unwrap_or_else(|| SourceError::NotADirectory(String::new())))
        }
    }
}

/// 扫描单个目录（原单目录实现原样搬到这里）
fn scan_one(root: &Path, recursive: bool) -> Result<Vec<MediaItem>, SourceError> {
    {
        let root: &Path = root;
        if root.as_os_str().is_empty() {
            return Err(SourceError::NotADirectory(String::new()));
        }
        if !root.is_dir() {
            return Err(SourceError::NotADirectory(
                root.to_string_lossy().into_owned(),
            ));
        }

        let walker = if recursive {
            WalkDir::new(root).follow_links(false)
        } else {
            WalkDir::new(root).max_depth(1).follow_links(false)
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

/// 只**计数**（不构造 MediaItem）：用于"素材过多"提示，成本比完整扫描低得多。
pub fn count_media(root: &Path, recursive: bool) -> usize {
    if !root.is_dir() {
        return 0;
    }
    let walker = if recursive {
        WalkDir::new(root).follow_links(false)
    } else {
        WalkDir::new(root).max_depth(1).follow_links(false)
    };
    walker
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| !is_hidden(e.path()))
        .filter(|e| kind_from_extension(e.path()).is_some())
        .count()
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

#[cfg(test)]
mod multi_scan_tests {
    use super::*;
    use crate::media::MediaSource;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "omaframe-multiscan-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 多目录：按用户顺序拼接、目录内自然排序、同名不同目录都保留；
    /// 单个目录不可用**不影响**其它目录；全部不可用才报错。
    #[test]
    fn scans_all_dirs_in_order_and_tolerates_missing() {
        let a = tmpdir("a");
        let b = tmpdir("b");
        std::fs::write(a.join("1.jpg"), b"x").unwrap();
        std::fs::write(a.join("2.jpg"), b"x").unwrap();
        std::fs::write(b.join("1.jpg"), b"x").unwrap();
        std::fs::write(b.join("10.png"), b"x").unwrap();

        let src = LocalMediaSource::new_multi(
            vec![
                a.to_string_lossy().into_owned(),
                b.to_string_lossy().into_owned(),
            ],
            false,
        );
        let names: Vec<String> = src.scan().unwrap().iter().map(|i| i.file_name()).collect();
        assert_eq!(
            names,
            vec!["1.jpg", "2.jpg", "1.jpg", "10.png"],
            "应先出 A 目录（自然排序）再出 B 目录；同名不同目录都要保留"
        );

        // 一个目录不存在 → 其它目录照常
        let mixed = LocalMediaSource::new_multi(
            vec![
                "/definitely/not/here".into(),
                b.to_string_lossy().into_owned(),
            ],
            false,
        );
        assert_eq!(
            mixed.scan().unwrap().len(),
            2,
            "不可用目录不应拖垮可用目录"
        );

        // 全部不可用 → 报错
        let none = LocalMediaSource::new_multi(vec!["/nope1".into(), "/nope2".into()], false);
        assert!(none.scan().is_err());

        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }
}

// ---------------- 扫描缓存 ----------------
//
// 为什么需要：大素材量（上万~十万项）时每次启动/开面板都全量 walk，用户能明显感到卡。
// 命中条件 = **目录集合 + recursive + 各目录 mtime** 全都没变，且缓存不超过 TTL。
//
// TTL 为什么必要：递归模式下在**子目录**里增删文件只会改子目录的 mtime，
// 根目录 mtime 不变 → 只比根目录 mtime 会读到过期缓存。所以再加一个时间上限。
// 需要立刻刷新时：设置页的「重新扫描素材」会先删掉缓存文件（见 settings.rs）。

/// 缓存最多复用这么久（秒）。超过就重新全量扫描一次。
const SCAN_CACHE_TTL_SECS: u64 = 600;

#[derive(serde::Serialize, serde::Deserialize)]
struct ScanCache {
    /// 目录集合 + recursive + 各目录 mtime
    key: String,
    /// 写入时间（unix 秒）
    stamp: u64,
    /// 只存路径：类型在读取时按扩展名重新推断（避免再维护一份 kind 映射）
    paths: Vec<String>,
}

fn cache_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("omarchy-omaframe").join("library.json"))
}

/// 目录集合 + recursive + 各目录 mtime 组成的缓存键
fn cache_key(roots: &[PathBuf], recursive: bool) -> String {
    let mut parts: Vec<String> = vec![format!("r={recursive}")];
    for r in roots {
        let mt = std::fs::metadata(r)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        parts.push(format!("{}@{mt}", r.display()));
    }
    parts.join("|")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 缓存是否可用：键一致 **且** 未超过 TTL
fn cache_is_usable(key: &str, want: &str, stamp: u64, now: u64) -> bool {
    // `stamp <= now` 必须显式判断：时钟被回拨时 `stamp` 会在未来，
    // saturating_sub 会算出 0 → 把一份可能很旧的缓存当成"刚写的"。
    // 保守起见：时间戳在未来 → 一律当不可用（重扫一次总是安全的）。
    key == want && stamp <= now && now - stamp <= SCAN_CACHE_TTL_SECS
}

fn load_cache(roots: &[PathBuf], recursive: bool) -> Option<Vec<MediaItem>> {
    let f = cache_file()?;
    let raw = std::fs::read(&f).ok()?;
    let c: ScanCache = serde_json::from_slice(&raw).ok()?;
    let want = cache_key(roots, recursive);
    if !cache_is_usable(&c.key, &want, c.stamp, now_secs()) {
        return None;
    }
    let items: Vec<MediaItem> = c
        .paths
        .into_iter()
        .filter_map(|p| {
            let path = PathBuf::from(p);
            kind_from_extension(&path).map(|k| MediaItem::new(path, k))
        })
        .collect();
    if items.is_empty() {
        None
    } else {
        Some(items)
    }
}

fn save_cache(roots: &[PathBuf], recursive: bool, items: &[MediaItem]) {
    let Some(f) = cache_file() else { return };
    if let Some(dir) = f.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let c = ScanCache {
        key: cache_key(roots, recursive),
        stamp: now_secs(),
        paths: items
            .iter()
            .map(|i| i.path.to_string_lossy().into_owned())
            .collect(),
    };
    match serde_json::to_vec(&c) {
        Ok(buf) => {
            if let Err(e) = std::fs::write(&f, buf) {
                crate::debug!("写扫描缓存失败：{e}");
            }
        }
        Err(e) => crate::debug!("序列化扫描缓存失败：{e}"),
    }
}

/// 删掉扫描缓存 → 下次 scan 强制全量重扫（设置页「重新扫描素材」用）
pub fn invalidate_scan_cache() {
    if let Some(f) = cache_file() {
        let _ = std::fs::remove_file(&f);
    }
}

#[cfg(test)]
mod scan_cache_tests {
    use super::*;

    /// 键一致 + 未超 TTL 才算命中；键变了或过期都要重扫
    #[test]
    fn cache_key_and_ttl_gate() {
        assert!(cache_is_usable("a", "a", 1000, 1000));
        assert!(cache_is_usable("a", "a", 1000, 1000 + SCAN_CACHE_TTL_SECS));
        assert!(!cache_is_usable("a", "b", 1000, 1000), "键变了必须重扫");
        assert!(
            !cache_is_usable("a", "a", 1000, 1001 + SCAN_CACHE_TTL_SECS),
            "超过 TTL 必须重扫（递归模式下子目录变化不改根目录 mtime）"
        );
        // 时钟回拨也不能误判为命中
        assert!(!cache_is_usable("a", "a", 2000, 1000));
    }

    /// 键必须对目录内容变化敏感（目录 mtime 变了 → 键变）
    #[test]
    fn cache_key_tracks_dir_mtime() {
        let d = std::env::temp_dir().join(format!("omaframe-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let roots = vec![d.clone()];
        let k1 = cache_key(&roots, false);
        std::thread::sleep(std::time::Duration::from_millis(1100)); // mtime 秒级精度
        std::fs::write(d.join("new.jpg"), b"x").unwrap();
        let k2 = cache_key(&roots, false);
        assert_ne!(k1, k2, "目录里增删文件后键必须变化");
        assert_ne!(k1, cache_key(&roots, true), "recursive 也是键的一部分");
        let _ = std::fs::remove_dir_all(&d);
    }
}
