# Omaframe — Omarchy 桌面电子相框

轻量、原生的桌面电子相框小组件：把本地目录里的图片/视频像相框一样摆在桌面上，
不抢焦点、不参与平铺、永远在普通窗口之下，被窗口盖住时自动暂停以省电。

* 语言/工具链：**Rust + GTK4 + libadwaita**（无 Electron/Tauri）
* 窗口：**wlr-layer-shell**（bottom 层）→ 桌面装饰型窗口，零焦点、零平铺干扰
* 图片：`gdk-pixbuf` 后台解码 + 边解码边缩放 + LRU 缓存（按条目数 **和** 字节预算双限制）
* 视频：**系统 `ffmpeg`**（与系统动态壁纸 `owe` 同一套解码器，**无需安装 GStreamer 解码器**）
* 遮挡检测：**Hyprland IPC 事件流**（零轮询、零常驻 CPU）

## 安装

```bash
make install        # 只写用户目录，不需要 root，不修改 hypr/omarchy 配置
```

安装内容：

| 位置 | 说明 |
|---|---|
| `~/.local/bin/omaframe` | 主程序（**二进制名全小写**；`Omaframe` 只是项目/界面名） |
| `~/.config/autostart/omaframe.desktop` | 登录自启（XDG 标准，延迟 4s，不拖慢桌面加载） |
| `frame/*.png` | **内置相框库**（相框样式下拉的选项来源） |
| `~/.config/omarchy-omaframe/config.toml` | 配置（首次**退出**时落盘） |
| `~/.local/state/omarchy-omaframe/logs/omaframe.log` | 日志（自动轮转 1MB） |
| `~/.cache/omarchy-omaframe/frames/*.json` | 相框 `FrameModel` 分析缓存（二次启动 0 次分析） |

卸载：`make uninstall`（保留配置）。

## 使用

```bash
omaframe            # 启动桌面相框
omaframe settings   # 打开设置面板（已有实例则通知它打开）
omaframe quit       # 退出运行中的实例
PHOTO_FRAME_LOG=debug omaframe   # 调试日志
```

## 状态栏图标

启动后会注册一个 **StatusNotifierItem** 托盘图标（图标名 `emblem-photos-symbolic`）：

* **左键点击** = 开关设置**面板**（顶栏右下方弹出；再点一次 / Esc / **点面板外** / 别的窗口拿到焦点 都会自动收起）
* 没装托盘服务（`org.kde.StatusNotifierWatcher`）的桌面会自动跳过，只记日志，不影响相框

## 窗口架构（重要）

`max_width × max_height` 是**素材绘制矩形的上限**（相框外框可能更大 —— 它要容纳 PNG 的边框）：

```
layer surface（固定 = 整块显示器，尺寸恒定、永不重建）
└── 相框矩形（左上角 = 配置 x/y；尺寸由 PNG 内孔 + 上限盒算出，与素材比例无关）
    ├── 图片/视频：铺满「内孔最大范围」，按 cover 最小裁切，再被遮罩裁成内孔形状
    ├── PNG 相框（叠在最上层，异形透明区由遮罩处理）
    └── 悬停控制层（贴在相框矩形内底部）
```

* **相框不随素材跳变**：相框尺寸只由 PNG 内孔几何 + 上限盒决定 → 换横图/竖图时相框大小不变。
* **layer surface 尺寸恒定 → 永不重建**：所以没有"闪黑""残影""切换素材时窗口闪一下"。
* **输入区域 = 相框矩形**：画布留白处的鼠标事件穿透到桌面，不会挡住其他操作。
* **拖动/缩放的流畅性**：拖动中只做「绘制偏移」、缩放中只画「预览虚线框」，
  全程**不碰 layer surface**（每帧改边距会触发合成器 configure 往返，那才是卡顿来源），
  松手时才应用一次；拖拽判定阈值也从 GTK 默认 8px 调到 2px，起手更快。

## 交互

| 操作 | 效果 |
|---|---|
| 鼠标移入相框 | 淡入控制层（底部中央 ▶/⏸、右下角小箭头） |
| 点击图像**左半边** | 上一项 |
| 点击图像**右半边** | 下一项 |
| 点击底部 ▶/⏸ | 视频：播放/暂停；图片：暂停/继续轮换 |
| 拖动主体 | 移动相框（拖动中只做绘制偏移 → 60Hz 顺滑；松手才真正移动 surface 并保存） |
| 拖右下角 | 改大小（**始终保持媒体比例**，受最大宽高限制，松手保存；拖动中只画预览虚线框） |
| 窗口盖住相框 | 自动暂停轮换与视频解码（CPU → 0） |

## 配置

`~/.config/omarchy-omaframe/config.toml`（原子写入：临时文件 + rename，损坏时自动备份并回退默认值）

```toml
[source]
type = "local"          # V2 预留 smb/nfs/webdav
path = "~/Pictures/PhotoFrame"
recursive = true

[display]
max_width = 400         # **素材绘制矩形的上限**（不是窗口宽！相框外框可以比它大）
max_height = 600
                        # （解码分辨率也按它缩 ×(1+grow)×显示器缩放，省内存/带宽）
cache_items = 12
cache_budget_mb = 32    # 缓存字节预算（低端机可调小）
max_decode_px = 4096    # 单边解码像素上限

[slideshow]
enabled = true
interval = 300          # 图片停留秒数
random = true

[video]
autoplay = true
muted = true
mode = "complete"       # complete=播完整段再切 / timed=到点就切
max_fps = 30

[frame]
desktop_enabled = true  # 关掉后相框从桌面隐藏（媒体/设置/托盘照常）
enabled = false         # ★默认 false：首次生成配置时不加 PNG 相框；在设置页选样式即打开
style = "横-木纹.png"     # 内置相框库 frame/ 下的 PNG 文件名（必须与文件名完全一致）
grow_percent = 3        # 相框比素材每边大多少（形成“卡纸”感）
fit = "smart"           # smart=九宫格自适应（相框可横可竖） / cover=保持 PNG 原比例
debug_hud = false       # 调试浮层
# 相框 PNG 会被自动分析出内孔（透明区），**不用手写坐标**；
# 任何四角+中心透明的 PNG 丢进 frame/ 即可。分析结果缓存到
# ~/.cache/omarchy-omaframe/frames/*.json，二次启动 0 次分析。

[window]
x = 32
y = 32
width = 400
height = 600
monitor = "HEADLESS-1"
default_anchor = "top-left"   # 首次运行默认位置
margin = 32
placed = true
```

**位置规则**：`x/y` 是**相框左上角的屏幕坐标**（逻辑像素）。
layer surface 恒为整块显示器、边距恒为 0，所以相框在 surface 内偏移 `x/y` 即屏幕坐标；
Hyprland 的 bar 保留区不影响它（layer surface 不会被压进去）。
（早期版本把 x/y 当成 layer 边距、并把它当成“素材左上角”再居中相框，
导致相框一大就整体偏出屏幕 —— 别改回那种语义。）

**尺寸规则**：`scale = min(max_width/w, max_height/h)`，素材尺寸 = 原图 × scale，
再乘 `(1 + grow_percent/200)` 作为相框外框的目标盒。所以换横图/竖图时相框会跟着变形尺寸，
但**素材本身永不拉伸变形**；拖右下角改的是 `max_*`，因此“拖出来的比例”与“当前媒体比例”始终一致。
相框**外框可以大于** `max_width/max_height`（要容纳 PNG 边框），`layout_adaptive`
会在超标时整体等比缩小，验收不变量：`相框 ≤ 上限盒 && 素材 ≤ 上限盒`。

## 目录结构

```
src/
├── main.rs / app.rs      入口与装配（配置→后端→窗口→播放器）
├── config.rs             配置结构 + sanitize + 原子保存
├── geometry.rs           fit()/位置夹取/**九宫格 layout_adaptive**（全项目唯一的比例真理函数）
├── frame_model.rs        ★智能相框引擎：alpha 二值化→边界洪泛→最大连通块→腐蚀
│                         →最大内接矩形→FrameModel→磁盘缓存
├── hypr.rs               Hyprland IPC 事件 → 覆盖判定
├── log.rs                轻量日志（无 tracing 依赖）
├── player.rs             编排：媒体库 + 图片加载 + 轮换 + 视频 + 拖动
├── slideshow.rs          计时/随机/暂停
├── tray.rs               StatusNotifierItem 托盘图标
├── controls.rs           控制层布局、命中检测与绘制
├── frame.rs              FrameSlices：相框 9 片 + 遮罩 9 片（按原始分辨率保存）
├── media/
│   ├── mod.rs            MediaItem / MediaSource trait（远程源扩展点）
│   ├── source/local.rs   本地递归扫描
│   ├── image.rs          后台解码（HEIC 回退 ffmpeg/magick）+ 缩放 + LRU 缓存 + 预取
│   ├── library.rs        媒体列表与当前索引
│   └── video.rs          ffmpeg 帧管道 → GdkMemoryTexture（含 MOV 旋转矩阵）
├── settings.rs           libadwaita 设置**面板**（弹出式，改动即时生效并落盘）
└── window/
    ├── frame_window.rs   layer-shell 主体窗口：锚点、hover、拖动/缩放、输入区域
    └── media_view.rs     自绘：九宫格相框 + 抗锯齿遮罩 + 控制层 + Debug Overlay
```

## 兼容性与性能设计

* **跨版本**：GTK 只用 4.10+ 稳定 API；Hyprland 事件按行解析、未知事件忽略；
  非 Hyprland / 无 layer-shell / 无 ffmpeg 都会**降级而不是报错**。
* **跨硬件**：解码前按目标尺寸缩放、单边像素上限、缓存双上限、视频帧率与尺寸可配、
  被覆盖即暂停（实测 CPU 0.0%）。空闲时常驻 CPU ≈ 0%，RSS ≈ 90MB。
* **稳定性**：GObject 属性设置全部走安全封装（属性缺失只告警不 abort），
  GStreamer/ffmpeg 回调与拖动处理都有 panic 隔离。

深入踩坑记录见 [`docs/PLAN.md`](docs/PLAN.md)；
**需求清单与禁忌速查**（功能验收 / 审美要求 / 开发流程 / "别再犯"对照表）见
[`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md)；
**后期快速回顾的结论速查**（已验证有效的修复点 / 症状→根因对照表 / 环境事实）见
[`docs/KEY-FINDINGS.md`](docs/KEY-FINDINGS.md)。
