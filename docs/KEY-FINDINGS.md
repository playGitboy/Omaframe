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
| 位置停靠除居中外都偏 | 用**画布**尺寸贴靠，而相框在画布内居中 | 按**相框矩形**贴靠：`画布位置 = 期望相框位置 − 相框偏移` |
| 改边距/位置后重启丢失 | `apply_anchor` 只 `edit()` 没 `commit()` | 末尾补 `commit()` |
| 调试浮层开关无效 | 运行时没真增删 HUD | `set_debug()` 运行时创建/移除标签 |

## 四、语义与配置约定

- `display.max_width/max_height` = **上限**（媒体尺寸 = `fit(比例, max×media_scale)`），不是相框大小。
- `window.x/y` = **相框左上角**的屏幕坐标（不是 layer 边距；layer 边距恒为 0）。
- `display.media_scale` = 0.96：媒体在相框内再内缩 4%，照片不顶框沿。
- 相框 = 媒体 × **1.03**（居中），`FRAME_GROWTH` 常量在 `src/window/media_view.rs`。
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

## 八、常用命令

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
