//! 配置：XDG 目录 + 原子写入 + 容错加载。

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const APP_ID: &str = "dev.sen.photo-frame";
pub const CONFIG_SUBDIR: &str = "omarchy-photo-frame";
pub const CONFIG_FILE: &str = "config.toml";

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn config_dir() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() && Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => home_dir().join(".config"),
    }
    .join(CONFIG_SUBDIR)
}

pub fn state_dir() -> PathBuf {
    match std::env::var_os("XDG_STATE_HOME") {
        Some(v) if !v.is_empty() && Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => home_dir().join(".local/state"),
    }
    .join(CONFIG_SUBDIR)
}

pub fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(|v| PathBuf::from(v).join(CONFIG_SUBDIR))
}

/// `~` / `$HOME` 展开；已是绝对路径则原样返回。
pub fn expand_user(p: &str) -> PathBuf {
    let s = p.trim();
    if s == "~" {
        return home_dir();
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    if let Some(rest) = s.strip_prefix('$') {
        // 简单处理 $HOME/... 与 ${HOME}/...
        for prefix in ["${HOME}", "HOME"] {
            if let Some(r) = rest.strip_prefix(prefix) {
                if r.is_empty() || r.starts_with('/') {
                    return home_dir().join(r.trim_start_matches('/'));
                }
            }
        }
    }
    PathBuf::from(s)
}

// ---------------------------------------------------------------- 配置结构

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub source: SourceConfig,
    pub display: DisplayConfig,
    pub slideshow: SlideshowConfig,
    pub video: VideoConfig,
    pub frame: FrameConfig,
    pub window: WindowConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceConfig {
    /// 媒体源类型；V1 只有 "local"，V2 扩展 smb/nfs/webdav/http
    #[serde(rename = "type")]
    pub kind: String,
    /// 本地目录（保存时为展开后的绝对路径）
    pub path: String,
    /// 是否递归扫描子目录
    pub recursive: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    /// 媒体显示的最大宽度（逻辑像素）——保持比例的"上限"，不是强制窗口宽
    pub max_width: i32,
    /// 媒体显示的最大高度（逻辑像素）
    pub max_height: i32,
    /// 图像解码缓存条目数（内存受限设备可调小）
    pub cache_items: usize,
    /// 图像解码缓存字节预算（MB）
    pub cache_budget_mb: usize,
    /// 单边解码像素上限（防止超大图吃内存）
    pub max_decode_px: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SlideshowConfig {
    pub enabled: bool,
    /// 图片停留秒数
    pub interval: u32,
    pub random: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoConfig {
    /// 视频加载后是否自动播放
    pub autoplay: bool,
    /// 静音播放（桌面组件默认静音）
    pub muted: bool,
    /// complete = 播完整段再切下一项；timed = 到轮换时间就切
    pub mode: String,
    /// 视频帧率上限（低配设备可降到 24/15）
    pub max_fps: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FrameConfig {
    /// 是否在桌面显示相框（关闭后隐藏窗口，媒体与设置照常可用）
    pub desktop_enabled: bool,
    pub enabled: bool,
    /// 相框样式：程序目录 `frame/` 下的 PNG 文件名（如 `木纹.png`）；空 = 不加相框
    pub style: String,
    /// 素材显示比（0-100，100 = 铺满相框内孔）
    pub zoom: u8,
    /// 相框相对素材的外扩百分比（默认 5 = 相框每边大 2.5%，四周居中）
    pub grow_percent: u8,
    /// 调试浮层（持久化，重启后保持）
    pub debug_hud: bool,
    /// 旧字段（v1 早期）：PNG 绝对路径，仅用于自动迁移到 `style`
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    /// 位置（相对 monitor 原点的逻辑像素）
    pub x: i32,
    pub y: i32,
    /// 上次实际窗口尺寸（仅作恢复提示，真实尺寸由媒体比例 + max_* 决定）
    pub width: i32,
    pub height: i32,
    /// 所在显示器 connector 名
    pub monitor: String,
    /// 首次运行（x/y 未生效时）的默认停靠：top-right / top-left / bottom-right / bottom-left / center
    pub default_anchor: String,
    /// 与屏幕边缘的最小间距（默认停靠时使用）
    pub margin: i32,
    /// 组件是否已定位过（false 时用 default_anchor 计算）
    pub placed: bool,
}

// 尺寸/间隔的硬边界：任何机器上都不会失控
pub const MIN_WIDTH: i32 = 160;
pub const MIN_HEIGHT: i32 = 120;
pub const MAX_DIM: i32 = 8_000;

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            kind: "local".into(),
            path: String::new(),
            recursive: true,
        }
    }
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            max_width: 400,
            max_height: 600,
            cache_items: 12,
            cache_budget_mb: 32,
            max_decode_px: 4096,
        }
    }
}

impl Default for SlideshowConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: 300,
            random: true,
        }
    }
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            autoplay: true,
            muted: true,
            mode: "complete".into(),
            max_fps: 30,
        }
    }
}

impl Default for FrameConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            desktop_enabled: true,
            style: String::new(),
            zoom: 100,
            grow_percent: 3,
            debug_hud: false,
            path: String::new(),
        }
    }
}

/// 内置相框库目录（程序目录下的 `frame/`）
pub fn frame_dir() -> PathBuf {
    // 可执行文件所在目录的 frame/（安装后为 ~/.local/bin/../frame 不成立，
    // 因此优先用编译期源码目录，其次用可执行文件同级的 frame）
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(d) = exe_dir {
        candidates.push(d.join("frame"));
        candidates.push(d.join("../frame"));
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("frame"));
    for c in &candidates {
        if c.is_dir() {
            return c.clone();
        }
    }
    candidates.remove(0)
}

/// 列出内置相框（返回文件名，按名称排序）
pub fn list_frame_styles() -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(frame_dir()) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.to_ascii_lowercase().ends_with(".png"))
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort_by_key(|n| n.to_lowercase());
    names
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            width: 400,
            height: 600,
            monitor: String::new(),
            default_anchor: "top-left".into(),
            margin: 32,
            placed: false,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            source: Default::default(),
            display: Default::default(),
            slideshow: Default::default(),
            video: Default::default(),
            frame: Default::default(),
            window: Default::default(),
        }
    }
}

impl Config {
    /// 夹紧到合法范围（配置被手改也不会让程序异常）
    pub fn sanitize(&mut self) {
        let d = &mut self.display;
        d.max_width = d.max_width.clamp(MIN_WIDTH, MAX_DIM);
        d.max_height = d.max_height.clamp(MIN_HEIGHT, MAX_DIM);
        d.cache_items = d.cache_items.clamp(1, 64);
        d.cache_budget_mb = d.cache_budget_mb.clamp(4, 512);
        d.max_decode_px = d.max_decode_px.clamp(1024, 16_384);

        self.slideshow.interval = self.slideshow.interval.clamp(1, 86_400);
        // 注意：**不要**在这里用 interval 反推 enabled。
        // 以前 0 表示"停用"，现在 interval 被夹到 ≥1，那行赋值等于恒为 true，
        // 会把设置页「自动轮换 → 启用」开关的写入直接覆盖掉（开关点了没反应）。

        if self.video.mode != "timed" {
            self.video.mode = "complete".into();
        }
        self.video.max_fps = self.video.max_fps.clamp(1, 60);

        let w = &mut self.window;
        w.width = w.width.clamp(MIN_WIDTH, MAX_DIM);
        w.height = w.height.clamp(MIN_HEIGHT, MAX_DIM);
        w.margin = w.margin.clamp(0, 512);
        if w.default_anchor.is_empty() {
            w.default_anchor = "top-left".into();
        }
        if self.source.path.is_empty() {
            self.source.path = default_media_dir().to_string_lossy().into_owned();
        }
        self.source.path = expand_user(&self.source.path).to_string_lossy().into_owned();
        if !self.frame.path.is_empty() {
            let p = expand_user(&self.frame.path);
            // 迁移：旧配置里的绝对路径 → 内置相框库里的文件名
            if self.frame.style.is_empty() {
                if let Some(name) = p.file_name() {
                    let name = name.to_string_lossy().into_owned();
                    if frame_dir().join(&name).is_file() {
                        self.frame.style = name;
                    }
                }
            }
        }
        if !self.frame.style.is_empty() {
            let name = std::path::Path::new(&self.frame.style)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.frame.style.clone());
            self.frame.style = name;
        }
        self.frame.zoom = self.frame.zoom.min(100);
        self.frame.grow_percent = self.frame.grow_percent.min(50);
    }
}

/// 首次运行的默认媒体目录：优先 ~/Pictures/PhotoFrame，其次当前主题壁纸目录。
pub fn default_media_dir() -> PathBuf {
    let conventional = home_dir().join("Pictures").join("PhotoFrame");
    if conventional.is_dir() {
        return conventional;
    }
    let theme_bg = home_dir()
        .join(".local/state/omarchy/current/theme/backgrounds");
    if theme_bg.is_dir() {
        return theme_bg;
    }
    conventional
}

// ---------------------------------------------------------------- 读写

pub struct ConfigManager {
    pub path: PathBuf,
}

pub struct Loaded {
    pub config: Config,
    /// true = 首次运行（没有配置文件），用于决定是否落盘默认值
    pub fresh: bool,
    /// true = 读取时做过迁移/夹取（如旧 path → 内置相框库 style），需要落盘
    pub migrated: bool,
}

impl ConfigManager {
    pub fn new() -> Self {
        Self {
            path: config_dir().join(CONFIG_FILE),
        }
    }

    /// 容错加载：文件不存在/损坏都不致命。
    pub fn load(&self) -> Loaded {
        let mut fresh = true;
        let mut config = match std::fs::read_to_string(&self.path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(c) => {
                    fresh = false;
                    c
                }
                Err(e) => {
                    crate::error!("配置解析失败（{}），已备份并回退默认值", e);
                    self.quarantine();
                    Config::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => {
                crate::error!("读取配置失败：{}；使用默认值", e);
                Config::default()
            }
        };
        let before = config.clone();
        config.sanitize();
        let migrated = !fresh && config != before;
        Loaded {
            config,
            fresh,
            migrated,
        }
    }

    fn quarantine(&self) {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let broken = self.path.with_file_name(format!("{CONFIG_FILE}.broken-{stamp}"));
        if let Err(e) = std::fs::rename(&self.path, &broken) {
            crate::warn!("损坏配置备份失败：{}", e);
        } else {
            crate::warn!("损坏配置已备份到 {}", broken.display());
        }
    }

    /// 原子保存：写临时文件 → fsync → rename；旧文件保留为 .bak
    pub fn save(&self, config: &Config) -> std::io::Result<()> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| std::io::Error::other("配置路径无父目录"))?;
        std::fs::create_dir_all(dir)?;

        if self.path.exists() {
            let _ = std::fs::copy(&self.path, dir.join(format!("{CONFIG_FILE}.bak")));
        }

        let text = toml::to_string_pretty(config)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = dir.join(format!("{CONFIG_FILE}.tmp.{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        // 目录项也刷一下，保证断电后 rename 结果可见
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    }
}

impl Default for ConfigManager {
    fn default() -> Self {
        Self::new()
    }
}
