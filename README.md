# photo-frame — Omarchy 桌面电子相框

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
| `~/.local/bin/photo-frame` | 主程序 |
| `~/.config/autostart/photo-frame.desktop` | 登录自启（XDG 标准，延迟 4s，不拖慢桌面加载） |
| `frame/*.png` | **内置相框库**（相框样式下拉的选项来源） |
| `~/.config/omarchy-photo-frame/config.toml` | 配置（首次启动自动生成） |
| `~/.local/state/omarchy-photo-frame/logs/photo-frame.log` | 日志（自动轮转 1MB） |

卸载：`make uninstall`（保留配置）。

## 使用

```bash
photo-frame            # 启动桌面相框
photo-frame settings   # 打开设置面板（已有实例则通知它打开）
photo-frame quit       # 退出运行中的实例
PHOTO_FRAME_LOG=debug photo-frame   # 调试日志
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

`~/.config/omarchy-photo-frame/config.toml`（原子写入：临时文件 + rename，损坏时自动备份并回退默认值）

```toml
[source]
type = "local"          # V2 预留 smb/nfs/webdav
path = "~/Pictures/PhotoFrame"
recursive = true

[display]
max_width = 400         # 相框**固定画布**尺寸（surface 就是这个大小）
max_height = 600
                        # （解码分辨率也按它缩，省 8% 内存/带宽）
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
enabled = true
style = "木纹.png"          # 内置相框库 frame/ 下的 PNG 文件名
zoom = 100                 # 素材显示比 0-100（以相框中心为基准缩放）
# 相框 PNG 建议：900x760 左右，两层圆角框，**四角与中心都透明**；
# 框宽（外沿到内孔）约 60/900 ≈ 6.7%，照片正好落在内孔最大范围上

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

**位置规则**：`x/y` 是**距"可用区域"边缘的边距**（Hyprland 会为 bar 等组件保留 reserved 区域，
例如本机左侧有 23px 保留，则 `x=32` 实际显示在 55）。

**尺寸规则**：`scale = min(max_width/w, max_height/h)`，实际显示尺寸 = 原图 × scale。
所以换横图/竖图时组件会自动变宽变高但**永不拉伸变形**；拖右下角改的是 `max_*`，
因此"拖出来的比例"和"当前媒体的比例"始终一致。

## 目录结构

```
src/
├── main.rs / app.rs      入口与装配（配置→后端→窗口→播放器）
├── config.rs             配置结构 + 原子保存
├── geometry.rs           fit()/位置夹取（全项目唯一的比例真理函数）
├── hypr.rs               Hyprland IPC 事件 → 覆盖判定
├── log.rs                轻量日志（无 tracing 依赖）
├── player.rs             编排：媒体库 + 图片加载 + 轮换 + 视频 + 拖动
├── slideshow.rs          计时/随机/暂停
├── controls.rs           控制层布局、命中检测与绘制
├── frame.rs              PNG 相框纹理
├── media/
│   ├── mod.rs            MediaItem / MediaSource trait（远程源扩展点）
│   ├── source/local.rs   本地递归扫描
│   ├── image.rs          后台解码 + 缩放 + LRU 缓存 + 预取
│   ├── library.rs        媒体列表与当前索引
│   └── video.rs          ffmpeg 帧管道 → GdkMemoryTexture
├── settings.rs           libadwaita 设置**面板**（弹出式，改动即时生效）
└── window/               layer-shell 窗口 + 自绘媒体控件
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
