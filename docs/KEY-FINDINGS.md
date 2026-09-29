# photo-frame 关键结论速查（后期回顾用）

> 更新时间：2026-09-28 · 对应提交 `572dbd6`
> 源码 `~/photo-frame`，本文件只记录**已验证有效**的结论和踩过的坑，细节见 `docs/PLAN.md`。

## 一、最终架构（已实测 1:1 精确）

```
layer surface = 整个显示器（1536×864），尺寸恒定、永不重建
└── 相框矩形 = 配置 window.x/y（屏幕坐标）+ fit(素材比例, max×media_scale)×1.03
    ├── 媒体纹理（照片/视频帧）
    ├── PNG 相框（比素材大 3%，居中）
    └── 悬停控制层（底部中央 ▶/⏸；左/右半区点击切上/下一项）
```

**为什么 surface 必须铺满显示器**（本项目最关键的一条）：
拖动/缩放时指针会移出"相框矩形"。如果 surface 只有相框那么大，
指针一移出 → GTK 停止派发事件 → 位移冻结在离开点（"只移动一部分距离"），
而靠轮询补偿又会带来"慢半拍 + 拖尾"。铺满整屏后指针**永远不可能离开控件**，事件全程连续。

## 二、拖动/缩放：四条铁律

1. **事件驱动，禁止轮询**：每个 `drag-update` 立即应用（`d.acc = (dx,dy)`，GTK 的 delta 是**相对按下点的绝对位移**）。
   早期用 16ms 计时器轮询指针位置 → 松手时最后一段常没被读到，且整体半拍延迟。
2. **拖动中输入区域放宽到整屏**（`view.set_dragging(true)`）；平时只覆盖相框矩形，留白点击穿透桌面。
   缩放时指针必然离开相框，不放宽手势会立刻断掉。
3. **拖动/缩放期间绝不碰 layer surface**（不 `set_margin`、不改尺寸）——每帧 configure 往返是卡顿主因。
   移动 = 绘制偏移；缩放 = 预览虚线框；松手才应用一次。
4. **窗口底色必须显式透明**（CSS `window/.background`）。不透明时偏移后原位置会留**黑色残影**；
   而且相框 PNG 的透明处会露出主题深色（用户报的"最外层黑色"）。

**实测（uinput 相对位移 + 按住，等效真鼠标）**
| 场景 | 光标 | 相框 | 误差 |
|---|---|---|---|
| 移动 | Δ(231,134) | Δ(231,135) | (0,1) ✓ |
| 缩放 | +200,+120 | 上限 254×143→455×255 | 比例 1.776→1.784 ✓ |

## 三、已确认的根因速查（按症状查）

| 症状 | 真根因 | 修法 |
|---|---|---|
| 点击/按住出现"放大黑屏" | 拖动中放大 widget → layer surface 重建 → 合成器画成不透明黑块 | 固定画布，绝不在拖动中改 surface 尺寸 |
| 拖动只走一段 / 慢半拍 / 拖尾 | 指针移出控件致事件中断 + 计时器轮询 | surface 铺满显示器 + 事件驱动 |
| 相框"外层一块黑" | ① PNG 四角不透明 ② **窗口透明 CSS 根本没注册成功** | 遮罩 = 外圈圆角 − 中心圆角；CSS 加到 `FrameWindow::new` 并挂解析错误日志 |
| 切素材时窗口"退出又打开" | 换素材改 surface 尺寸 → 重建 | 画布恒定，媒体只改绘制 |
| 视频加载前黑闪 | 切视频时先清纹理，首帧 0.2~0.5s 内 surface 无内容 | 保留上一项画面，首帧到达再替换 |
| 托盘图标点击无反应 | quickshell 发 `Activate(ii)`（两个参数），只声明一个 → GDBus 以 InvalidArgs 拒绝 | XML 声明 `x,y` 两个 int；删掉指向空对象的 `Menu` 属性 |
| **崩了**（SIGABRT）| `GObject::set_property` 写入**不存在的属性**（`propagate-natural-width` 不是 `AdwPreferencesPage` 的属性）→ glib 直接 panic→abort | 删掉该行；凡是设属性一律先确认目标对象真的有这个属性 |
| 设置页窗口缩不小 | `set_size_request()` 设的是**最小**尺寸，误当目标尺寸用（400x560）→ 窗口被撑死 | 只把它当下限（340x360），目标尺寸用 `default_width/height` |
| 设置页 Esc 关不掉 | `EventControllerKey` 默认在 bubble 阶段，焦点在 Entry 里时按键先被输入框消费 | 改用 **capture 阶段**的 `EventControllerKey`（+bubble 兜底），焦点在输入框也能关窗 |
| 位置停靠除居中外都偏 | 用**画布**尺寸贴靠，而相框在画布内居中 | 按**相框矩形**贴靠：`画布位置 = 期望相框位置 − 相框偏移` |
| 改边距/位置后重启丢失 | `apply_anchor` 只 `edit()` 没 `commit()` | 末尾补 `commit()` |
| 调试浮层开关无效 | 运行时没真增删 HUD | `set_debug()` 运行时创建/移除标签 |

## 三点五、相框遮罩算法（异形相框的正确解法）

- **症状**：PNG 相框是**异形/不规则外轮廓**（斜切角、圆角过大、波浪边）时，媒体矩形会从
  PNG 的透明缺口**漏出矩形边角**，非常难看。
- **错误做法**：只裁一个矩形（内孔包围盒）→ 圆孔的四个角仍会漏。
- **正确算法**（`frame.rs::build_hole_mask`）：
  1. 扫描 alpha，**从四边洪泛**透明像素：能到达的 = 外部，到不了的 = 内孔
     （"透明像素包围盒"分不清「圆孔四角」和「外轮廓缺口」，必须洪泛）
  2. 允许显示媒体的区域 = 非外部透明（相框本体 ∪ 内孔）；真正要擦掉的只有外轮廓之外的缺口
  3. 遮罩整体 3×3 盒式模糊 → 外轮廓边缘 1px 羽化（半透明渐变观感）
  4. 运行时 cairo `DestOut` 用遮罩擦媒体，再把相框 PNG 叠在最上层
  5. 没有内孔（纯装饰框）→ 自动退回原叠图模式
- **实测**（用户的 floral frame `fower.png`，2688×1515，薄边框+四角花，只有 23% 像素不透明）：
  框外像素 rgb(109,164,85) 与远处壁纸 rgb(109,164,81) 一致 → 相框外**无任何媒体** ✓
- **坑**：遮罩是**灰度图**（无 alpha），读像素必须按 **1 字节/像素**；
  用 `n_channels()` 去乘会把行内位置算错，内孔边界整体偏移（曾导致 40px 的孔变成 14px）。
- 已撤销：`FRAME_GROWTH`(相框×1.03) 与 `media_scale`(媒体×0.96) 两个"百分比缩放" ——
  现在相框矩形 == 媒体矩形，**让位交给遮罩**，`media_scale` 字段仅为兼容旧配置保留。

## 三点六、视频清晰度（易回退的一个坑）

- **症状**：视频在相框里发糊（图片清晰）。
- **根因**：ffmpeg 管线只按**逻辑像素**出帧（`max × media_scale`，如 358×200），
  而显示器有缩放（本机 1.25×，GDK `scale_factor()` 报 2）→ 合成器把帧再放大 → 模糊。
- **修法**：`show_video` 的目标框 = `max × media_scale × monitor_scale`（与图片 `decode_box` 一致），
  实测 358×200 → **713×401**；缩放器从默认/bilinear 换成 **lanczos**（小窗缩小画质差别明显）。

## 三点七、内置相框库 + 相框样式 + 显示比

- 相框库在**程序目录 `frame/`**（原 `~/frame` 已移入，8 个中文名 PNG）。
  `config::frame_dir()` 解析顺序：`<exe同级>/frame` → `<exe上级>/frame` → 源码目录 `frame/`；
  `list_frame_styles()` 读取其中所有 `.png`（按名称排序）作为设置页下拉选项。
- 配置：`frame.style = "木纹.png"`（**文件名**，不是绝对路径）；`frame.zoom = 0..100`（默认 100）。
  旧的 `frame.path` 会在 `Config::sanitize()` 里**自动迁移**成 `style`
  （`Loaded.migrated` 标记 → boot 立即落盘，不等退出）。
- **中心对齐**：相框按 **PNG 自身比例**在上限盒内取最大（`fit(png_aspect, box)`，**不拉伸**），
  素材 `fit(素材比例, 相框尺寸)` 后由 `geometry::place_media(fx,fy,fw,fh,mw,mh,zoom)`
  以**相框中心**为基准放置/缩放 → 两者中心恒等（实测误差 0.5px = 整数取整残差）。
  注意：很多相框的**内孔本身偏心**（电视TV/猫线/木纹/炫彩），所以"按孔位置放素材"会偏，
  必须以**相框中心**为准。
- 显示比实现：`geometry::place_media(...)`（`zoom_in_frame` 是同尺寸快捷版） —— **以相框矩形中心为基准**缩放，
  四周对称留边（单测断言 100%/70%/0% 三种情形下中心不变）。
- 换样式/改显示比都是**立即生效**：`player.load_frame()` + `player.apply_zoom()`。

## 四、语义与配置约定

- `display.max_width/max_height` = **上限**（媒体尺寸 = `fit(比例, max×media_scale)`），不是相框大小。
- `window.x/y` = **相框左上角**的屏幕坐标（不是 layer 边距；layer 边距恒为 0）。
- `display.media_scale`：**已废弃**（相框改遮罩后不再需要百分比内缩），仅为兼容旧配置保留。
- `frame.desktop_enabled` = 默认 true；关掉后相框从桌面隐藏（媒体继续解码，设置/托盘不受影响）。
- `frame.style` = 内置相框库里的 PNG 文件名；`frame.zoom` = 素材显示比（0-100，以相框中心缩放）。
- 相框矩形 == 媒体矩形；`frame.enabled` 关闭时纯展示媒体。
- 输入区域平时 = 相框矩形（**相框外点击穿透桌面**，不挡操作）；拖动中 = 整屏。
- 拖拽判定阈值 2px（`gtk-dnd-drag-threshold`，默认 8px 太迟钝）。

## 五、环境事实（本机实测，别再重复踩）

- GStreamer **只有 base 插件**（无 h264/hevc/vp8/theora/matroska）→ 视频走**系统 ffmpeg**（`owe` 同款后端），零新增依赖。
- mpv 0.41 **无 `--window-layer`** → 不能像动态壁纸那样开层表面。
- Hyprland 0.56：dispatch 是 Lua 语法（`hl.dsp.focus({workspace=3})`）；事件 socket 是 `.socket2.sock`（旧版 `.sock2`）；
  事件负载是 CSV 且**不含几何**（几何要用 `hyprctl -j clients`）。
- 本机 `reserved: 23 0 0 0`，layer surface 原点 ≈ 屏幕 (0,22)：屏幕坐标与 surface 坐标差 22px（y）。
- GTK 4.22 已把 GDK 并入 `libgtk-4`（无 `gdk-4.0.pc`）；glib 0.22 **没有 `clone!` 宏**，用 `glib::WeakRef`/`Rc<Cell<..>>`。
- 托盘、遮挡暂停、ffmpeg 缺失都会**降级不报错**；全程不写 hypr/omarchy 配置、不需要 root。

## 六、验收状态

| 项 | 结果 |
|---|---|
| 相框在 layer bottom、不进平铺、不抢焦点 | ✓ 不在 `hyprctl clients` |
| 被窗口覆盖 | ✓ 自动暂停，CPU **0.0%** |
| 移动 / 缩放跟手 | ✓ 误差 (0,1)px |
| 比例保持 | ✓ 1.776 → 1.784 |
| MP4 播放 + ▶/⏸ | ✓ ffmpeg 后端 |
| PNG 相框 | ✓ 四角透明、比媒体大 3% |
| 重启恢复 | ✓ 位置/尺寸/设置一致 |
| 编译警告 / 测试 | 0 / 11 通过 |
| 空闲 | CPU 0.0%，RSS ~89MB |

## 七、复盘：这次为什么反复

前几轮我一直在**调参数**（换余量方案、换 delta 来源、加计时器补帧），
但症状"只移动一部分距离"的根因是**事件被中断**，参数怎么调都治不了。
**教训：先量化症状（打印每次收到的 delta），再动架构** —— 量化后一眼就看出"delta 冻结了"。

## 八、版本管理与一键回滚

```bash
cd ~/photo-frame
make versions            # 列出可回滚的 tag / 提交
scripts/rollback.sh v1.0.0   # 切到 V1 完成版并重新构建安装（含 make install）
scripts/rollback.sh latest    # 切回最新提交（恢复开发中的版本）
```
* `v1.0.0` = 遮罩算法改造**之前**的 V1 完成版（拖动精确、缩放顺滑、视频清晰）。
* 回滚是 `git checkout` + 重新 `make install`，未提交的改动会保留，不丢代码。

## 九、常用命令

```bash
cd ~/photo-frame
make install / make uninstall          # 只写 ~/.local/bin 与 ~/.config/autostart
~/.local/bin/photo-frame                # 启动
~/.local/bin/photo-frame settings       # 设置（400×560 浮动窗口）
~/.local/bin/photo-frame quit           # 退出
PHOTO_FRAME_LOG=debug ~/.local/bin/photo-frame   # 调试日志
grep -E "输入区域|Update" ~/.local/state/omarchy-photo-frame/logs/photo-frame.log -c
coredumpctl list | grep photo-frame      # 崩溃自查（用户要求：主动盯崩溃并修）
```
