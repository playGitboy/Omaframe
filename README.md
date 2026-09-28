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
| `~/.config/autostart/photo-frame.desktop` | 登录自启（XDG 标准，Omarchy/uwsm 会自动拉起） |
| `~/.config/omarchy-photo-frame/config.toml` | 配置（首次启动自动生成） |
| `~/.local/state/omarchy-photo-frame/logs/photo-frame.log` | 日志（自动轮转 1MB） |

卸载：`make uninstall`（保留配置）。

## 使用

```bash
photo-frame            # 启动桌面相框
photo-frame settings   # 打开设置窗口（已有实例则通知它打开）
photo-frame quit       # 退出运行中的实例
PHOTO_FRAME_LOG=debug photo-frame   # 调试日志
```

## 状态栏图标

启动后会注册一个 **StatusNotifierItem** 托盘图标（图标名 `emblem-photos-symbolic`）：

* **左键点击** = 显示 / 隐藏设置窗口
* 没装托盘服务（`org.kde.StatusNotifierWatcher`）的桌面会自动跳过，只记日志，不影响相框

## 窗口架构（重要）

`max_width × max_height` 是**上限画布**，不是相框大小：

```
layer surface（固定 = max_width × max_height，只有它存在时不重建）
└── 素材矩形 = fit(素材比例, max_width×media_scale, max_height×media_scale)，居中
    ├── 图片/视频（按素材矩形 1:1 绘制）
    ├── PNG 相框（贴合素材矩形 → 视觉上"框随照片"）
    └── 悬停控制层（贴在素材矩形内底部）
```

* **相框贴合素材**：换横图/竖图时相框跟着变，画布只保证"不超过上限"。
* **layer surface 尺寸恒定 → 永不重建**：所以没有"闪黑""残影""切换素材时窗口闪一下"。
* **输入区域 = 素材矩形**：画布留白处的鼠标事件穿透到桌面，不会挡住其他操作。

## 交互

| 操作 | 效果 |
|---|---|
| 鼠标移入相框 | 淡入控制层（底部中央 ▶/⏸、右下角小箭头） |
| 点击图像**左半边** | 上一项 |
| 点击图像**右半边** | 下一项 |
| 点击底部 ▶/⏸ | 视频：播放/暂停；图片：暂停/继续轮换 |
| 拖动主体 | 移动相框（surface 跟着指针走，松手保存位置） |
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
media_scale = 0.96      # 媒体相对组件的内缩比例：0.96 = 照片四周留 2% 细边
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
path = "~/.config/omarchy-photo-frame/frame.png"   # 透明 PNG，叠在媒体之上
# 相框 PNG 建议：900x760 左右，两层圆角框，**四角与中心都透明**；
# 框宽（外沿到内孔）约 60/900 ≈ 6.7%，比媒体内缩 4% 略大，看起来才有"卡纸"感

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
├── settings.rs           libadwaita 设置窗口（改动即时生效）
└── window/               layer-shell 窗口 + 自绘媒体控件
```

## 兼容性与性能设计

* **跨版本**：GTK 只用 4.10+ 稳定 API；Hyprland 事件按行解析、未知事件忽略；
  非 Hyprland / 无 layer-shell / 无 ffmpeg 都会**降级而不是报错**。
* **跨硬件**：解码前按目标尺寸缩放、单边像素上限、缓存双上限、视频帧率与尺寸可配、
  被覆盖即暂停（实测 CPU 0.0%）。空闲时常驻 CPU ≈ 0%，RSS ≈ 90MB。
* **稳定性**：GObject 属性设置全部走安全封装（属性缺失只告警不 abort），
  GStreamer/ffmpeg 回调与拖动处理都有 panic 隔离。

详细踩坑记录与踩过的坑见 [`docs/PLAN.md`](docs/PLAN.md)。
