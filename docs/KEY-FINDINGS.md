# photo-frame 关键结论速查（后期回顾用）

> 更新时间：2026-09-28 · 对应提交 `572dbd6`
> 源码 `~/photo-frame`，本文件只记录**已验证有效**的结论和踩过的坑，细节见 `docs/PLAN.md`。

> 需求清单与禁忌速查见 [`REQUIREMENTS.md`](REQUIREMENTS.md)；本文只放技术根因与推导。

## 一、最终架构（已实测 1:1 精确）

```
layer surface = 整个显示器（1536×864），尺寸恒定、永不重建
└── 相框矩形 = 配置 window.x/y（相框左上角）+ fit(PNG比例, 目标盒)
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
| 设置页窗口缩不小 | `set_size_request()` 设的是**最小**尺寸，误当目标尺寸用（400x560）→ 窗口被撑死 | 面板尺寸用 `set_default_size`；`set_size_request` 只当宽度下限 |
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
  让位交给遮罩；`display.media_scale` 字段已**彻底删除**（旧配置里那行会被 serde 忽略，不影响加载）。
- **三个必须记住的坑（都真踩过，症状都是"媒体从相框漏出/露边"）**：
  1. **`DestOut` 必须先 `translate` 到相框矩形**：遮罩按"相框矩形尺寸"生成，
     但 cairo 上下文原点是 surface (0,0)。少了平移就擦错区域 → 媒体整片漏在相框外。
  2. **`ALPHA_CUT` 取 160（约 0.63）而不是 24**：真实相框外沿常有**半透明羽化带**
     （alpha 0.13~1.0 渐变）。若把它当"透明内孔"，媒体会一路画到相框外沿，
     羽化带后面透出一条图片（用户报的"图片上边缘会露出相框"）。
  3. **媒体绘制矩形要比内孔略微内缩 1px**（`MASK_INSET`）：让媒体硬边藏进不透明环内，
     不能外扩（外扩必从羽化带透出）。

## 三点五点一、内孔可用范围 = 整片透明区域的**最大宽高**

- 用户明确要求：素材宽高要**最大化填充**内孔，所以内孔范围取
  **整片透明区域的外接 min/max**（`build_hole_mask` 里的 `hx0/hy0/hx1/hy1`）。
- **不要再改成"逐行最宽段的中位数/密集核心"** —— 那是为避让顶部报纸角装饰想出来的保守算法，
  会把可用区算小：木纹上沿从真实的 `0.008` 被压到 `0.135`，素材白小一圈；
  竖图时相框还会缩成 280×153（横图 668×364）→ **换张图相框就跳变**。
  不规则形状/装饰让**遮罩**去裁，素材该占满最大范围（花环的花朵自然会压在照片上，观感正常）。

## 三点六、视频清晰度（易回退的一个坑）

- **症状**：视频在相框里发糊（图片清晰）。
- **根因**：ffmpeg 管线只按**逻辑像素**出帧（如 358×200），
  而显示器有缩放（本机 1.25×，GDK `scale_factor()` 报 2）→ 合成器把帧再放大 → 模糊。
- **修法**：`show_video` 的目标框 = `上限盒 × (1+grow) × monitor_scale`（与图片 `decode_box` 一致），
  实测 358×200 → **713×401**；缩放器从默认/bilinear 换成 **lanczos**（小窗缩小画质差别明显）。

## 三点七、内置相框库 + 相框样式 + 显示比

- 相框库在**程序目录 `frame/`**（原 `~/frame` 已移入，8 个中文名 PNG）。
  `config::frame_dir()` 解析顺序：`<exe同级>/frame` → `<exe上级>/frame` → 源码目录 `frame/`；
  `list_frame_styles()` 读取其中所有 `.png`（按名称排序）作为设置页下拉选项。
- 配置：`frame.style = "木纹.png"`（**文件名**，不是绝对路径）；`frame.zoom = 0..100`（默认 100）。
  旧的 `frame.path` 会在 `Config::sanitize()` 里**自动迁移**成 `style`
  （`Loaded.migrated` 标记 → boot 立即落盘，不等退出）。
- **尺寸关系（相框宽高自适应：比素材大 grow%，默认 3）**：
  1. 内孔范围 `draw` = 整片透明区域的最大外接范围（相对 PNG 的 0..1 比例，见 3.5.1）
  2. 素材尺寸 = `fit(素材比例, 上限盒)`；目标盒 = **素材 × (1+grow%)**（默认 3%）
  3. 相框尺寸 = `geometry::frame_size_for_box(draw, PNG比例, 目标盒)`
     约束：`fw ≤ 目标盒.w/draw.w`、`fh ≤ 目标盒.h/draw.h`、**`fw/fh = PNG 自身比例`**（不拉伸）
  3. 素材绘制矩形 = `geometry::media_rect_in_hole(相框矩形, draw, 显示比)`
     → 正好铺满内孔最大范围，再按显示比以该区域中心缩放
  4. 纹理按 **cover** 画进绘制矩形：素材比例与内孔比例不同时做**最小裁切**（真实相框的"填充"做法）
- **实测（上限 490×320、grow 3%、木纹）**：内孔最大范围 = 素材(490×276) × 1.03 → 素材 **505×281**，
  相框外框 **610×332**（外框比素材大是因为 PNG 自身边框厚）。
  约束更紧的一边精确等于"素材 × 1.03"，另一边不超过它（相框比例恒等于 PNG 比例）。
  各相框内孔最大范围（相对 PNG）：木纹 0.07~0.898 × 0.008~0.854；花环 0.051~0.969 × 0.05~0.937；
  猫线 0.061~0.933 × 0.251~0.91（内孔本身就是扁的 → 素材自然被裁成扁幅面）。
- **图片/视频清晰度（又一个易回退的坑）**：解码尺寸必须 **≥ 实际绘制矩形**。
  历史上 `decode_box`/视频 `set_box` 乘过一个"媒体内缩 0.96"的系数，导致解码比绘制小 4%，
  合成器再放大回来 → 图片/视频发糊（用户报的"加载图片模糊"）。该字段已删除，别再引入类似系数。
  正确做法：`上限盒 × (1+grow%) × monitor_scale() × 1.06`（1.06 是 cover 裁切余量）。
- 显示比实现：`media_rect_in_hole(...)` —— 以**内孔可用区中心**为基准缩放
  （不规则内孔的中心未必是相框中心，所以不能再按相框中心放）。
- 换样式/改显示比都是**立即生效**：`player.load_frame()` + `player.apply_zoom()`。

## 四、语义与配置约定

- `display.max_width/max_height` = **素材绘制矩形的上限**（相框会随内孔比例变大，
  所以相框外框**可以大于**这两个值；420 行夹在整屏尺寸内）。
- `window.x/y` = **相框左上角**的屏幕坐标（不是 layer 边距；layer 边距恒为 0）。
  `geometry()` 里必须**直接用 x/y 当相框左上角** —— 拖动、停靠、输入区域、可见性判定全都这么假设；
  曾经把 x/y 当"素材左上角"再居中相框，相框一大就整体偏出屏幕（日志 `输入区域 → …+0+112`）。
- 右下角缩放拖动改的是**上限盒**（`geometry::resize_target_box`）：盒宽增量 = 鼠标位移 / k，
  `k = 当前素材宽 / 当前盒宽` → 手感依旧"鼠标走多少照片变多少"；松手才写配置。
- `display.media_scale`：**已删除**（旧配置里残留该行会被忽略）。
- `frame.desktop_enabled` = 默认 true；关掉后相框从桌面隐藏（媒体继续解码，设置/托盘不受影响）。
- `frame.style` = 内置相框库里的 PNG 文件名；`frame.zoom` = 素材显示比（0-100，以相框中心缩放）；
  `frame.grow_percent` = 相框比素材大多少（默认 5）；`frame.debug_hud` = 调试浮层（持久化）。
- **所有设置即时落盘**：设置页每行都走 `AppState::update()`（内部 = `edit` + `commit`），
  拖动/贴靠在松手时 `commit`；重启后全部自动恢复（含调试浮层）。
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

## 三点八、设置面板（弹出式，2026-09-29）

需求：点状态栏图标弹出设置页；**点面板外 / Esc / 失去焦点**都自动隐藏（Omarchy 插件那种手感），
且面板里的下拉、开关必须能用鼠标正常操作。

**最终形态（`settings.rs`）—— 只有一个窗口**：
- 面板窗口铺满"顶栏以下"（`Layer::Overlay`、锚 `Top|Left`、`set_size_request(显示器尺寸)`、
  `KeyboardMode::Exclusive`），左上锚定 + **显式尺寸**（不做四边拉伸）。
- 面板卡片（固定宽 360 + 圆角 popover 背景 + 内部 ScrolledWindow）在窗口内对齐右上角。
- **点卡片外 → 收起**：根容器 capture 阶段 `GestureClick`，用 `shell.compute_bounds()` 判断
  落点是否在卡片内 → 在卡片外才 `hide()`。窗口铺满屏幕 → 点击被本窗口吞掉（真弹窗语义），
  且顶栏（y<22）不在窗口内 → 状态栏图标仍可点（再点一次也能关）。
- Esc → `close_request` → `hide()`；`photo-frame settings` / 托盘图标 → `toggle()`。
- 隐藏即**销毁**窗口 → 下次打开必是最新配置（拖动改过的宽高立刻反映）。
- 另一个窗口**真的**拿到焦点（Hyprland `activewindow(v2)` 事件，按负载去重）→ 也收起。

**踩过的坑（都很隐蔽，别重犯）**：
1. **四边锚定的 layer surface 收不到鼠标事件**：合成器把这种 surface 拉伸，但 GTK 侧收不到
   configure，控件分配停在最小值 → 无输入。（`hyprctl layers` 看得到、`a: 1`、输入区域也显式设了
   —— 都没用。）同层同尺寸的**面板窗口**能收点击，区别就在"是否显式尺寸/非拉伸"。
   → 结论：别用第二个"透明遮罩表面"做点外关闭，直接让面板窗口铺满屏幕。
2. **不能用 `is_active_notify` 做"失去焦点就隐藏"**：面板里的下拉（AdwComboRow）打开时是 GTK 弹窗，
   会让 toplevel 的 `is_active` 变 false → "点下拉面板立刻消失"（用户报的 bug）。
3. **Hyprland 焦点事件只认 `activewindow(v2)`**：把 `workspace` 也算进来会误关面板
   （点下拉时也会补发 workspace 事件）。另外事件按负载去重（同一窗口补发的事件不带变化）。
4. **全透明的 layer surface 会被当成不可见**（`hyprctl layers` 里 `a: 0`）；现在面板根容器有一层
   很淡的压暗（rgba 0.08），既是模态观感也保证 alpha>0。
5. 面板内容比屏幕高 → 必须自己包一层 `ScrolledWindow`（`propagate_natural_height(false)`），
   AdwPreferencesPage 单用会直接把高度报给窗口、被裁掉且滚不动。

**托盘 SNI 顺带修的一个真 bug**：`ToolTip` 的签名必须是 `(sa(iiay)ss)`（图标名、图标像素数组、
标题、描述）。之前写成 `(("photo-frame",), Vec<(i32,i32,i32,i32)>, tooltip)` —— 少一个字段、
内层类型也不对，quickshell 每 30 秒报一次 DBus 签名错误刷日志。现在用
`("photo-frame", Vec::<(i32,i32,Vec<u8>)>::new(), "桌面相框", tooltip)`。

## 三点九、控制层（播放/暂停按钮）显隐的两个坑（2026-09-29）

用户报：播放/暂停按钮应"指针进窗口才显示、移出就隐藏"，实际是**点过暂停后按钮一直挂着**。

**根因 1（主因）：控制层的"重绘钩子"从来没注册。**
`Controls` 的淡入淡出是自绘的（`progress` 0→1），GTK 不会因为 Cell 变化自动重画，
所以它内部有 `redraw` 回调 —— 但 `MediaView::new()` 里**没有调用 `set_redraw_hook()`**，
于是 `request_redraw()` 是空操作：动画只在"别的重绘顺便带上"时才看得见
（指针移动本身会 `queue_draw`），**光标停下后最后一帧（progress=0）永远刷不出来** →
按钮留在屏幕上。修法：构造时把 `view.queue_draw` 注册进去（用 `WeakRef` 避免循环引用）。
→ 教训：自绘 + 动画的组件，一定确认"请求重绘"这条链路真的接上了（这类 bug 表现为
"动画看起来在工作，但最后一个状态刷不出来"）。

**根因 2（鲁棒性）：不能只靠 enter/leave 判断 hover。**
按下/拖动时输入区域会临时扩到整屏（为了拖出相框也不丢事件），此时指针移出相框
**不会再收到 leave** → hover 冻结在"显示"。修法：每次 motion 都按**指针实际位置**判断
（`ControlLayout::hit()` 落在框外返回 `None`，正好当判据）→ `set_hover(zone != None)`。

## 三十、layer-shell 面板里开对话框：点"打开目录"闪退（2026-09-30）

**现象**：设置面板点"媒体 → 打开"选目录，程序立刻闪退；`coredumpctl` **没有**新记录，
日志最后一行是 `Gdk-Message: Lost connection to Wayland compositor`，
Hyprland 侧报 `error in client communication (pid …)`。

**根因**：`gtk::FileDialog::open(parent, …)` 把**设置面板**当对话框的 transient parent，
而面板是 **layer-shell surface**（不是 `xdg_toplevel`）→ 触发 Wayland 协议错误 →
**合成器直接踢掉客户端**（所以没有 coredump、不是 Rust 侧的 panic）。
另外面板在 **Overlay 层且盖满屏幕**，普通 toplevel 的对话框会被它整个挡住。

**修法**（`settings.rs` 的目录选择按钮）：
1. `dialog.open(None::<&gtk::Window>, …)` —— **不传父窗口**（避免给 layer surface 设 parent）。
2. 点按钮先 `hide(panel)`，对话框关闭（选中或取消）后再 `show(panel)`。
3. 顺带加"刚显示 500ms 宽限期"（`within_show_grace`）：从对话框关掉回来时，
   焦点正好切回原窗口，不该被 `activewindow` 事件当成"失去焦点"立刻把面板又关掉。

**同类推广**：任何 `xdg_toplevel` 相关的父子/瞬态关系都不能挂到 layer-shell surface 上；
需要模态/父窗口时，要么让对话框独立开，要么先把 layer surface 藏起来。

**验证**：点"打开" → 对话框正常弹出且可见、程序不退出；点"取消" → 对话框关闭、
面板回来并保持（不被焦点事件误关）、配置未被改动。
