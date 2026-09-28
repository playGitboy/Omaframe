# photo-frame — Omarchy 桌面电子相框 (V1)

## 0. 本机实测环境（Step 1 结论，勿凭旧资料假设）

| 项目 | 实测值 | 设计影响 |
|---|---|---|
| Omarchy | 4.0.0 (r6663) | — |
| Hyprland | 0.56.2（config 为 Lua 拆分；windowrule v2 新语法） | 本项目 **完全不写 windowrule**，不碰用户 hypr 配置 |
| GTK | 4.22.5（GDK 已并入 `libgtk-4`，无 `gdk-4.0.pc`） | `gdk4-sys 0.11.5` 已适配，`gtk4.pc` 单一 pc 文件，GTK 4.0~4.22 均可编译 |
| libadwaita | 1.9.4 | 设置页用 Adw 原生风格 |
| gtk4-layer-shell | 1.3.0（crate 0.8.1，提供 `is_supported()` 运行时探测） | 窗口后端 |
| GStreamer | 1.28.7，**只有 `gtksink`(GTK3) / `gtkglsink`，没有 `gtk4videosink`** | 视频走 CPU 管线 → `GdkMemoryTexture`（gdk4 0.11 有 `MemoryTextureBuilder`） |
| gdk-pixbuf | 2.44.7，PNG/JPEG/WebP/GIF 全部实测通过 | 图片解码走 pixbuf（GIF 仅首帧，GTK4 无动图支持） |
| 显示器 | `HEADLESS-1` 1920x1080 scale 1.25，HDMI 为其 mirror | 坐标用逻辑像素；位置存"相对该 monitor 原点的逻辑 px" |
| 素材 | `~/.local/state/omarchy/current/theme/backgrounds/`（8 张 webp/jpg，最大 7680x3215） | 顺带验证超大图降采样；视频/竖图另用 `~/.config/omarchy/backgrounds/tokyo-night/` 验证 |

## 1. 核心决策

### 1.1 窗口后端 = wlr-layer-shell（`bottom` 层）
- layer surface 天然不参与平铺、不进 `hyprctl clients`、**永远拿不到键盘输入**（`KeyboardMode::None`），无需任何 compositor 规则。
- 指针事件只在"该位置没有别的窗口"时到达我们 → "被覆盖时不响应/不播放"天然成立。
- 位置用 margin 锚点（左上锚 + x/y margin ≡ 绝对坐标），拖动 = 改 margin。
- 已知代价：与 Omarchy 菜单同处 bottom 层且我们创建更晚 → 会盖在菜单之上；相框放角落即可规避，V1 不处理。
- 降级：`is_supported() == false`（非 wlroots 系 WM）时切 `ToplevelBackend`，仍能显示，只是失去"不抢焦点/被覆盖"保证，UI 里提示。

### 1.2 视频 = CPU 帧
`playbin(video-sink=appsink)` → `videoconvert!videoscale!videorate` → BGRA → `GdkMemoryTexture` → `GtkPicture`。
- 管线输出尺寸锁定为"显示尺寸 × dpr"（上限），`videorate max-rate` 可配（默认 30，低配可 24/15）。
- 被遮挡/无焦点时 `PAUSED`，CPU 归零。
- 未来若要零拷贝：换 `GstGLContext` + `GtkGLArea`，接口已隔离在 `media/video.rs`。

### 1.3 尺寸真理函数
```
fit(aspect, max_w, max_h) = (orig_w, orig_h) * min(max_w/orig_w, max_h/orig_h)
```
用户拖 resize = 改 `display.max_width/max_height`（top-left 锚点固定，比例自动保持）；
换媒体 = 再跑一次 `fit`。所以"拖拽保持比例"和"换图重算"是同一份代码。

### 1.4 被遮挡检测 = Hyprland IPC 事件流
读 `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock2`，消费
`openwindow / closewindow / movewindow / activewindowv2 / workspacev2 / focusedmonv2`，
维护"当前工作区、与本组件矩形相交"的窗口集合 → 事件驱动、零轮询、零常驻 CPU。
非 Hyprland 环境：降级为"永远可见"。

## 2. 通用性 / 兼容性设计（跨硬件、跨版本）

| 维度 | 做法 |
|---|---|
| GTK 版本 | 只用 4.6+ 稳定 API；依赖由 `system-deps` 解析 `gtk4.pc`，GTK 4.0~4.22 通吃（GDK 合并与否由 pc 自动处理） |
| 工具链 | `edition = "2021"`，`rust-version = "1.80"`，不依赖新语言特性 |
| Wayland WM | layer-shell 不支持时自动降级 toplevel；Hyprland 不在场时禁用遮挡暂停 |
| Hyprland 版本 | IPC 事件按行解析 + `serde_json` 反序列化，未知事件/字段忽略；不用 0.5x 新 API |
| GStreamer 差异 | 启动时 `gst::ElementFactory::find()` 探测 `playbin/videoconvert/videoscale/appsink/autodetect` 与各解码器；缺则降级为"纯图片"并在日志/设置页提示，不崩溃 |
| 硬件差异（内存） | 解码前按目标像素降采样；单边像素上限 4096；`Semaphore(2)` 限制并发解码；`GdkTexture` LRU 缓存默认 12 张，切换即释放 |
| 硬件差异（CPU/GPU） | 视频帧尺寸与 fps 上限可配；`videoconvert` 走 CPU，无 GL 依赖 → 无显卡差异；被遮挡/暂停时管线 `PAUSED` |
| 缩放/HDR | 用 GDK 逻辑像素 + `scale_factor()`；DPR 只影响解码目标尺寸与视频管线 |
| 多显示器 | 位置存 `(monitor_name, x, y)`；启动时 monitor 找不到 → 回落主屏并 clamp 进屏幕范围；`monitors-changed` 时重新贴边 |
| 路径 | 全部 XDG（`XDG_CONFIG_HOME` / `XDG_RUNTIME_DIR`），支持 `~` 展开；`~` 展开后落盘为绝对路径 |
| 配置损坏 | 解析失败 → 备份为 `config.toml.broken-<ts>` 后回退默认值，绝不 panic |
| 写入安全 | 临时文件 + `fsync` + `rename` 原子替换，保留一份 `config.toml.bak` |
| 安装 | `make install` 只写 `~/.local/bin/` 与 `~/.config/autostart/`，**零 root、不改 hypr/omarchy 配置** |
| 编译期开关 | `video` / `hyprland` feature 可关，方便裁剪 |
| 依赖体量 | 无 `clap`（手写参数解析）、无 `tracing`（自写 ~80 行日志 + 轮转）、无 `reqwest` 之类 |

## 3. 目录结构

```
photo-frame/
├── Cargo.toml / Makefile / README.md / docs/PLAN.md
└── src/
    ├── main.rs            # CLI: (无参)=运行  settings  --version  --help
    ├── app.rs             # AdwApplication 组装、单实例、IPC 唤醒设置页
    ├── log.rs             # 轻量日志（stderr + 轮转文件），PHOTO_FRAME_LOG=debug
    ├── config.rs          # ConfigManager：加载/校验/原子保存
    ├── geometry.rs        # fit()/clamp()/位置换算
    ├── hypr.rs            # Hyprland IPC 订阅 → VisibilityState
    ├── media/
    │   ├── mod.rs         # MediaItem / MediaSource trait / MediaKind
    │   ├── source/local.rs# LocalMediaSource：walkdir 递归扫描（后台线程）
    │   ├── image.rs       # pixbuf 后台解码 + 降采样 + LRU 纹理缓存 + 预取
    │   └── video.rs       # GStreamer playbin+appsink → GdkMemoryTexture
    ├── slideshow.rs       # 轮换/随机/计时/暂停
    ├── frame.rs           # PNG 相框 overlay 叠加
    ├── controls.rs        # hover 控制层（▶/⏸，淡入淡出）
    ├── resize.rs          # 右下角 handle：保持比例的拖拽换算
    ├── settings.rs        # Adw 设置窗口
    └── window/
        ├── mod.rs         # WindowBackend 抽象（LayerShell / Toplevel）
        ├── frame_window.rs# 主体窗口：锚点、尺寸、hover、拖动
        └── geometry_store.rs # 位置/尺寸状态与保存
```

## 4. MediaSource 扩展点（V2 预留，V1 只实现 Local）

```rust
pub trait MediaSource: Send + Sync {
    fn id(&self) -> &str;
    fn describe(&self) -> String;
    fn scan(&self) -> BoxFuture<'static, Result<Vec<MediaItem>, ScanError>>; // 后台、可重入
}
pub enum MediaItem { Image(ImageItem), Video(VideoItem) }
```
UI 层只认 `MediaItem`，不认路径语义 → 将来 `SmbMediaSource` 等直接替换 `[source] type`。

## 5. 交互与控制层（新增需求，已并入设计）

鼠标移入相框区域时淡入控制层，移出淡出（150ms，非阻塞、轻量）：

```
┌──────────────────────┐
│  ‹    |    PHOTO     ›  │   ← 左侧：上一项   右侧：下一项
│                      │
│      ▶ / ⏸           │   ← 底部中央：播放/暂停
└──────────────────────┘
```

- 控件全部由 `MediaView` 自绘（cairo 圆底 + 白色矢量图标），不依赖图标主题。
- 按钮位置由 `controls::ControlLayout` 统一计算，**绘制与命中检测共用同一套矩形** → 不会出现"看得见点不到"。
- 播放/暂停语义：当前是**视频** → 切换播放/暂停；当前是**图片** → 切换自动轮换的暂停/继续。
- 图标状态：运行中显示 `⏸`（点击暂停），暂停时显示 `▶`（点击继续）。
- 右下角另有极小的 resize handle（Step 9 实现拖拽逻辑，先占位并响应 hover 光标）。
- 因为是 bottom 层 surface，只有**未被窗口覆盖**时才能收到指针事件 —— 正好等于"用户正在看桌面"的场景。

默认停靠：**左上角**（`window.default_anchor = "top-left"`，边距 32px）。

## 6. 实施步骤与验收（每步真机验证）

1. ✅ 环境勘察（本文件 §0）
2. ✅ 依赖编译验证 + layer-shell 窗口（不抢焦点 / 不进平铺 / 被普通窗口覆盖，已实测）
3. ✅ 本地图片：后台扫描、异步解码、`fit()` 自适应、预取与 LRU（实测 6016x3384 → 600x338）
4. ⏳ 悬停交互控制（左右切换 + 底部中央播放/暂停）+ 默认位置改左上角
5. ⏳ PNG 相框 overlay
6. ⏳ 视频：CPU 管线 + 播放状态（测 CPU）
7. ⏳ 轮换 + 随机 + 视频播完再切
8. ⏳ 被窗口覆盖则暂停（Hyprland IPC 事件流）
9. ⏳ 右下角 resize（保持比例、不可为 0、不超 max）
10. ⏳ 拖动移动 + 位置/尺寸持久化
11. ⏳ Adw 设置窗口
12. ⏳ `~/.config/autostart` 自启 + 卸载脚本

验收场景按用户清单 1~13 逐条实测。
