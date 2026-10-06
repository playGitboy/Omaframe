//! 配置：XDG 目录 + 原子写入 + 容错加载。

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const APP_ID: &str = "dev.sen.omaframe";
pub const CONFIG_SUBDIR: &str = "omarchy-omaframe";
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
    /// 开机启动（XDG autostart 项）。**默认开启**；设置页可关。
    pub autostart: bool,
    /// 素材切换转场
    pub transition: TransitionConfig,
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
    /// 相框相对素材的外扩百分比（默认 5 = 相框每边大 2.5%，四周居中）
    pub grow_percent: u8,
    /// **自适应随机推荐**：开启后忽略 `style`，每次换素材按素材纵横比
    /// 在「横/竖/方」相框里随机挑一个（见 player::pick_frame_for_media）。
    /// 开启时设置页的"相框样式"不可用。
    pub auto_style: bool,
    /// 调试浮层（持久化，重启后保持）
    pub debug_hud: bool,
    /// 相框适配模式：
    /// - `smart`（默认）：智能九宫格 —— 相框可横可竖、四角不变形、素材铺满内孔（不裁切）
    /// - `cover`：保持相框原始比例，素材按 cover 裁切填满（老行为）
    /// - `contain`：等同 smart（内孔按素材比例成形，所以不会留边）
    pub fit: String,
    /// 旧字段（v1 早期）：PNG 绝对路径，仅用于自动迁移到 `style`
    pub path: String,
}

/// 可选转场效果（key → 界面名）。
/// 实现约束：只用 snapshot 的 translate/scale/push_opacity/push_clip，
/// **不做 CPU 像素运算、不建 ImageSurface、不用 filter** —— 保证开销在 GPU 侧。
pub const TRANSITION_EFFECTS: [(&str, &str); 5] = [
    ("fade", "淡入淡出"),
    ("ken_burns", "缓慢推近"),
    ("pull_back", "拉远"),
    ("slide", "横向滑动"),
    ("roll", "垂直卷帘"),
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransitionConfig {
    /// 转场总开关
    pub enabled: bool,
    /// 效果 key（见 TRANSITION_EFFECTS）；random 开启时由程序覆盖
    pub effect: String,
    /// 随机转场：每次切换从已有效果里随机挑一个（开启时设置页的效果下拉置灰）
    pub random: bool,
    /// 单次转场时长（毫秒）。**不叠加到自动轮换间隔上**：
    /// 转场只在切换瞬间播放，轮换计时器按原计划走。
    pub duration_ms: u32,
}

impl Default for TransitionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            effect: "fade".to_string(),
            random: false,
            duration_ms: 1000,
        }
    }
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
            max_width: 350,
            max_height: 350,
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
            interval: 5,
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
            // 默认**启用**相框；style 留空 → 首次加载素材时按方向自动选一套
            // （横版 → 横-花环.png，竖版 → 竖-信笺.png，见 player::auto_pick_frame_style）。
            enabled: true,
            desktop_enabled: true,
            style: String::new(),
            auto_style: false,
            grow_percent: 3,
            fit: "smart".into(),
            debug_hud: false,
            path: String::new(),
        }
    }
}

/// 同步"开机启动"：写入/删除 XDG autostart 项。
///
/// 不做成"安装时一次性写死"，而是**由配置驱动**：设置页开关改 `config.autostart`，
/// 改完立即调本函数落盘；启动时再对账一次（配置为 true 但文件被删了会补回来）。
/// `Exec` 用当前可执行文件绝对路径 —— autostart 由会话拉起，不保证 PATH 里有 ~/.local/bin。
///
/// **不能写 `OnlyShowIn`**：systemd 的 xdg-autostart 生成器会拿
/// `$XDG_CURRENT_DESKTOP`（本机是 `Hyprland`）去匹配它，而规范里的自定义桌面名要写
/// `X-Hyprland` → 两者永远对不上 → 单元被判 `exec-condition` **静默跳过，从不自启**
/// （实测：`systemd-xdg-autostart-condition "X-Hyprland" ""` 返回 1）。
/// 程序在非 Hyprland 下也会优雅降级（无 layer-shell 则退 toplevel），故不限制桌面环境。
pub fn sync_autostart(enabled: bool) -> std::io::Result<()> {
    let dir = home_dir().join(".config").join("autostart");
    let path = dir.join("omaframe.desktop");
    std::fs::create_dir_all(&dir)?;
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("omaframe"));
    let body = format!(
        "[Desktop Entry]\n\
Type=Application\n\
Name=桌面相框\n\
Name[en]=Desktop Photo Frame\n\
Comment=把本地图片/视频以自适应 PNG 相框摆在桌面上\n\
Comment[en]=Show local photos/videos on the desktop inside an adaptive PNG frame\n\
Exec={}\n\
Terminal=false\n\
X-GNOME-Autostart-Delay=4\n\
X-GNOME-Autostart-NoNotification=true\n\
X-StartupNotify=false\n\
Keywords=oma;omaframe;omf;zm;xk;zmxk;zhuomian;xiangkuang;frame;photo;desktop;相框;照片;桌面;\n\
{hidden}",
        exe.display(),
        // 关闭时用 Hidden（XDG 规定的"屏蔽系统级 autostart 项"方式），
        // 而不是删文件 —— 因为 pacman 包会在 /etc/xdg/autostart 装一份，
        // 删掉用户文件反而会让系统那份生效（用户关了却仍然自启）。
        // 这里总是写用户项；开=正常，关=Hidden=true。两者都能覆盖系统项。
        hidden = if enabled {
            "X-GNOME-Autostart-Enabled=true\n"
        } else {
            "Hidden=true\nX-GNOME-Autostart-Enabled=false\n"
        }
    );
    std::fs::write(&path, body)
}

/// 内置相框库目录。
///
/// 候选顺序很关键：**运行期能确定的安装位置必须优先于编译期源码目录**。
/// 之前源码目录（CARGO_MANIFEST_DIR/frame）排在前面，导致 `make install` 装出来的
/// 程序在开发机上仍读源码目录 —— 用户改的是安装目录（或反之），行为对不上，
/// 表现为"删了相框但设置页里还在"。
///
/// 顺序：
///   1. `$OMA_FRAME_DIR`  —— 显式覆盖（开发/排查用）
///   2. 可执行文件同级的 `frame/`         （便携安装）
///   3. 可执行文件上级的 `frame/`
///   4. `<prefix>/share/omaframe/frame`    （make install / install.sh 布局）
///   5. `/usr/share/omaframe/frame`        （AUR/系统包布局）
///   6. `$XDG_DATA_HOME/omaframe/frame`
///   （仅 debug 构建，排在系统目录**之前**：`cargo run` 必须读你正在改的仓库 frame/，
///    否则本机装了包后会去读安装快照）
///   7. `<target>/../../frame`             （cargo build/run 的开发布局）
///   8. 编译期源码目录
///
/// **7/8 只在 debug 构建里参与**：它们依赖“构建机上的源码目录”，是个人路径。
/// 发布版（release / pacman 包）若带上这条兜底，就会把构建机的绝对路径烧进二进制，
/// 既不通用、也会在换机器后指向不存在的目录。发布版只应使用**系统通用目录**
/// （`$OMA_FRAME_DIR` 覆盖 → exe 相对 → `<prefix>/share` → `/usr/share` → `$XDG_DATA_HOME`）。
pub fn frame_dir() -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();

    // 1) 显式覆盖（最优先，方便开发时指向别处）
    if let Ok(d) = std::env::var("OMA_FRAME_DIR") {
        candidates.push(PathBuf::from(expand_user(&d)));
    }

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));

    if let Some(d) = exe_dir.clone() {
        // 2) 3) 便携布局
        candidates.push(d.join("frame"));
        candidates.push(d.join("../frame"));
        // 7) 仅 debug：cargo 开发布局 target/debug/omaframe → <repo>/frame
        //    必须排在系统目录**之前**，否则本机装了包之后 `cargo run`
        //    会去读安装快照，而不是你正在改的仓库 frame/。
        if cfg!(debug_assertions) {
            if let Some(up2) = d.parent().and_then(|p| p.parent()) {
                candidates.push(up2.join("frame"));
            }
            // 8) 仅 debug：编译期源码目录
            candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("frame"));
        }
        // 4) <prefix>/share/omaframe/frame —— exe 在 <prefix>/bin/omaframe
        if let Some(up) = d.parent() {
            candidates.push(up.join("share").join("omaframe").join("frame"));
        }
    }

    // 5) 系统包布局
    candidates.push(PathBuf::from("/usr/share/omaframe/frame"));
    // 6) XDG
    if let Ok(d) = std::env::var("XDG_DATA_HOME") {
        candidates.push(PathBuf::from(d).join("omaframe").join("frame"));
    }

    for c in &candidates {
        if c.is_dir() {
            return c.clone();
        }
    }
    // 全都不存在：返回第一个（源码），保证有确定行为
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
            x: 12,
            y: 12,
            width: 350,
            height: 350,
            monitor: String::new(),
            default_anchor: "top-left".into(),
            margin: 12,
            placed: false,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            autostart: true,
            transition: Default::default(),
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

        // 转场：时长夹到合理区间；效果名非法则回落 fade（配置被手改也不炸）
        let t = &mut self.transition;
        t.duration_ms = t.duration_ms.clamp(200, 5_000);
        if !TRANSITION_EFFECTS.iter().any(|(k, _)| *k == t.effect) {
            t.effect = "fade".into();
        }
        // 随机转场开启时不保留固定效果（设置页也会把下拉置灰，这里保证语义干净）
        if t.random {
            t.effect = "fade".into();
        }

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
        self.frame.fit = match self.frame.fit.trim().to_ascii_lowercase().as_str() {
            "cover" | "fill" => "cover".into(),
            _ => "smart".into(), // contain / smart / 空 / 未知 → smart
        };
        self.frame.grow_percent = self.frame.grow_percent.min(50);
    }
}

/// 首次运行的默认媒体目录：**当前系统的壁纸目录**（跨发行版）。
///
/// 不能只认 Omarchy 的布局 —— 本程序也要能在 debian/arch/ubuntu 等系统上跑，
/// 那些系统的壁纸位置完全不同。按"存在即用"顺序探测，全部落空才用 ~/Pictures/PhotoFrame。
///
/// 需要 `OMA_MEDIA_DIR` 可显式覆盖（自检/排查用）。
pub fn default_media_dir() -> PathBuf {
    let home = home_dir();
    if let Ok(d) = std::env::var("OMA_MEDIA_DIR") {
        let p = PathBuf::from(expand_user(&d));
        if p.is_dir() {
            return p;
        }
    }
    // ① Omarchy（current 是指向正在使用主题的符号链接）
    let mut cands = vec![
        home.join(".local/state/omarchy/current/theme/backgrounds"),
        home.join(".config/omarchy/backgrounds"),
    ];
    // ② 常见桌面环境/发行版的壁纸目录（XDG user-xdg-graphic installed）
    if let Ok(cfg_home) = std::env::var("XDG_CONFIG_HOME") {
        let b = PathBuf::from(&cfg_home);
        cands.push(b.join("backgrounds"));
        cands.push(b.join("hypr/backgrounds"));
    } else {
        let b = home.join(".config");
        cands.push(b.join("backgrounds"));
        cands.push(b.join("hypr/backgrounds"));
    }
    if let Ok(d) = std::env::var("XDG_DATA_HOME") {
        cands.push(PathBuf::from(d).join("backgrounds"));
    } else {
        cands.push(home.join(".local/share/backgrounds"));
    }
    cands.push(PathBuf::from("/usr/share/backgrounds"));
    cands.push(PathBuf::from("/usr/share/backgrounds/omarchy"));
    // ③ 常见图片目录（用户自己的照片）
    for d in ["Pictures", "Pictures/Wallpapers", "图片", "Pictures/Photos"] {
        cands.push(home.join(d));
    }
    for c in cands {
        if c.is_dir() {
            return c;
        }
    }
    home.join("Pictures").join("PhotoFrame")
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
