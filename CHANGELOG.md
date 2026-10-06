# Changelog

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [0.5.2] - 2026-10-02

### 修复（转场观感：过渡生硬 / 中间闪一下）
- **上一张没记住自己的绘制矩形**：相框尺寸由素材比例推导（换素材时相框矩形会变），
  而转场把旧图按**新**矩形绘制 → 尺寸不匹配、盖不满内孔 → 中间透出桌面（"闪一下"）。
  现在转场开始时记下旧矩形，旧图**按自身几何**绘制。
- **两帧矩形不同时推动类效果会错位**：`slide` / `roll` 在矩形不一致时"推入缝"对不齐。
  现在只要两帧矩形不同，二者**自动退化为淡入淡出**（不透明度互补、无位移），
  保证过渡连续自然。两帧矩形一致时仍是完整的整屏推动。

### 移除
- **`page_flip`（翻页）**：观感不佳，按用户要求删除。效果列表从 6 种降为 5 种
  （淡入淡出 / 缓慢推近 / 拉远 / 横向滑动 / 垂直卷帘）；配置里若残留 `page_flip`
  会被 `sanitize()` 自动回落为 `fade`。

### 验证
- 新增回归单测：**矩形变化时推动类必须退化为淡入淡出**（不透明度互补 + 无位移），
  并保留"矩形一致时整屏推"的断言 → 41/41 通过、0 编译警告。

## [0.5.1] - 2026-10-02

### 新增
- 设置页 · 转场 · **启用转场**开关（总开关）。关闭后效果/随机/时长三行**一并置灰**；
  随机开启时仅效果下拉置灰（时长仍可调）。

### 改进
- 转场效果参数细化（都仍是 snapshot 变换，开销不变）：
  - `page_flip` 从"擦除"改为**真翻页感**：旧页以**左边缘为轴横向压扁**（掀起来），
    新页自左向右揭开。为此给变换参数加了**非等比缩放（x/y 分开）+ 缩放锚点**。
  - `ken_burns`：旧图同时继续放大淡出、新图从略大收回 1.0 → 画面像"持续靠近"。
  - `pull_back`：旧图略缩、新图从更大处收回 → 收束感更强。
- 新增单测：推动类效果必须"整屏推"（两端只看到一张图、中间不留背景空档）；
  并把参数校验扩展到缩放/锚点/裁剪范围。

### 验证
- 40/40 测试通过、0 编译警告。
- 6 种效果的**端到端渲染**在 0.5.0 已实测确认（红蓝素材混合色/同屏分列/上下分带/
  裁剪揭示）；本版的参数细化由单测锁定其范围与不变量。

## [0.5.0] - 2026-10-02

第二阶段：素材切换**转场效果**。

### 新增
- **设置页 · 转场**（新分组，位于「自动轮换」之后）：
  - **转场效果**下拉：淡入淡出 / 缓慢推近 / 拉远 / 横向滑动 / 垂直卷帘 / 翻页
  - **随机转场**开关：开启后效果下拉**置灰**，每次切换由程序从已有效果里随机挑，
    且**避开与上一次相同的效果**（否则看起来像没转场）
  - **时长**（200–3000ms，默认 600）
- 配置 `[transition]`：`enabled` / `effect` / `random` / `duration_ms`；
  默认 启用、`fade`、随机=关、600ms。
- **不影响自动轮换间隔**：转场由视图自身的 16ms tick 驱动，轮换计时器独立计时，
  转场只在切换瞬间播放、不叠加到停留时间上。

### 实现要点（保持低开销）
- 全部效果只用 snapshot 的 `translate / scale / push_opacity / push_clip` 实现 ——
  **不做 CPU 像素运算、不建 ImageSurface、不用 filter**，开销在合成器侧。
- 旧/新两张都是已有的 GPU 纹理，转场只是"多画一张 + 一次变换"。
- 效果抽成 `Copy` 枚举（`Effect`），每帧不再分配 String。
- 门控：首帧不转场 / 视频不转场（视频已有自己的淡入淡出）/ 被覆盖或隐藏时
  **直接 settle 并停止 tick**（省电，且不会切回来"正在转场"）。

### 验证
- 逐效果实测（红/蓝两张纯色素材 + 慢速放大时长，抓中间帧）：
  fade / ken_burns / pull_back 出现红蓝**混合色**；slide 出现红蓝**同屏分列**；
  roll 出现红蓝**上下分带**；page_flip 出现**裁剪揭示** —— 6 种全部生效。
- 新增 2 个单测（效果参数范围/无 NaN、效果名解析回落 fade）→ 39/39 通过；
  0 编译警告；发布自检通过。

## [0.4.5] - 2026-10-01

### 修复
- **开机启动（autostart）修复**：去掉 autostart 项的 `OnlyShowIn=X-Hyprland`。
  systemd 的 xdg-autostart 生成器会拿 `$XDG_CURRENT_DESKTOP`（本机 Hyprland）去匹配它，
  规范里的自定义桌面名却要写 `X-Hyprland` → 永远对不上 → 单元被判 `exec-condition`
  **静默跳过、永不自启**。程序在非 Hyprland 下会优雅降级，故不限制桌面环境。
- **贴靠的底/右边距比设置值小一截**（如选左下角+边距30，底部几乎贴边、左侧却
  看着正常）：贴靠原按「整屏几何」算，但相框画在 layer surface 坐标里，
  Hyprland 把 surface 放在顶栏下方（实测 surface xywh=`0 26 1600 900`，屏幕高900，
  底部多出 26px 不可见）→ 底边距平白少 26（30 变 4）。
  改用合成器真实的 reserved 区（`hypr::monitor_reserved()` 读 `hyprctl -j monitors`）
  算出可见可用区再贴靠，取不到则按整屏（行为不变）。
- **首摆放后相框尺寸变化没带动位置**：首摆放用画布尺寸 window.width/height 估算，
  而相框在画布内不占满（实测画布384x384、相框仅403x252）。现在首摆放后随素材
  比例真实确定时跟随贴靠，直到用户自己拖动过（交还控制权）。

验证：bottom-left + 边距30，稳定后左30 / 下30（修复前 左32 / 下4）。

## [0.4.4] - 2026-10-01

### 打包 / 分发
- **GitHub Release 预编译包**（`omaframe.pkg.tar.zst`，x86_64）：README 提供一条命令
  `curl … | sudo pacman -U` 即可安装，不再依赖 AUR。
  AUR 的 `PKGBUILD` 也保留（可从源码构建、任意架构），source 改为可直接下载的
  GitHub tag 归档并填入真实校验和。
- **`make install` / `scripts/install.sh` 的开机启动项改用 XDG `Hidden` 屏蔽**：
  系统包会在 `/etc/xdg/autostart` 装一份，若关闭时删除用户项反而会让系统项生效
  （用户关了却仍自启）；现在关闭时写 `Hidden=true` 覆盖系统项。

### 文档
- **README 拆成中英两份**：`README.md` 改为英文（GitHub 默认展示），
  新增 `readme_zh.md` 中文版，两者互相链接。
- 重写功能介绍：目录即相册 / 自动轮播 / 30+ 套相框 + 智能算法适配 /
  任意位置拖动 / 右下角缩放 / 轻点翻页 / 被遮挡自动暂停 / 托盘设置面板，
  并补上实测支持的格式清单与开箱默认值。

## [0.4.3] - 2026-10-01

### 调整
- 设置页 · 相框：「启用」移到该组**第一行**（总开关应最先看到）
- 设置页 · 常规：整组移到「相框」**下方**，段落顺序变为
  媒体 → 显示 → 自动轮换 → 视频 → 相框 → 常规 → 位置与外观

## [0.4.2] - 2026-10-01

### 修复
- **开启「自适应随机推荐」后相框无限轮动**：`auto_pick_frame_style` 会改配置并重载相框，
  而这条链路会经回调再次回到 `apply_image` → 又随机选一个相框 → 自我无限触发。
  实测 7 秒内**写盘 181 次**、相框疯狂跳动、完全不按轮换间隔。
  修法两条：
  1. **重入守卫**（作用域守卫，所有提前返回路径都会解除），杜绝同步自触发；
  2. **每个素材只随机选一次**（记忆 `索引/总数/尺寸`），同一素材重复调用直接跳过。
  实测：写盘 181 → **1 次**，相框只随自动轮换/手动翻页变化。

## [0.4.1] - 2026-10-01

### 性能
- **修复启动时主线程被 `ffprobe` 冻住**：视频探测是**子进程调用**，
  原来在 `spawn_reader` 里**同步**执行，4K 素材可阻塞主线程数百毫秒到秒级，
  表现为"相框出来了但底图迟迟不出现"。改为：
  1. 探测放到**后台线程**，结果经主循环回填后**再**启动读帧线程，主线程全程不阻塞；
  2. 新增 `probe_cache`（path → 显示尺寸），翻页回到同一个视频不再重复探测。
- 实测（首个素材为 3840×2160 视频）：
  **窗口+相框 0.25s → 首帧 0.29s → 视频开播 0.34~0.39s**；
  修复前 ffprobe 期间主线程被占住，首帧在测量窗口内从未出现。

### 说明
- 媒体目录扫描本来就是后台线程（`LocalMediaSource` + `walkdir`），
  绘制/几何热路径**不**调用 `lib.current()` / `list_frame_styles()`（只换素材时调），
  所以素材数量增大不会拖慢每帧开销与翻页速度；本次补齐了视频探测这一处遗漏的阻塞点。

## [0.4.0] - 2026-10-01

### 新增
- **设置页 · 常规 · 开机启动（默认开启）**：开关直接读写
  `~/.config/autostart/omaframe.desktop`，改动即时生效；启动时对账一次
  （配置为开但项被删则补回，为关则移除）。`Exec` 用当前可执行文件**绝对路径**，
  不依赖 PATH。原来自启项只在安装时写一次，删掉就没了。
- **设置页 · 相框 · 自适应随机推荐**：开启后**禁用"相框样式"下拉**（置灰并提示），
  改为每次换素材时按素材纵横比判定「横/竖/方」，在该类别相框里**随机挑一个**。
  类别按相框库命名约定识别（`横-*` / `竖-*` / 其余算"方"，如 `方-*`、`大头贴-*`）；
  纵横比判定留 ±15% 容差（>1.15 横 / <0.87 竖），避免 4:3 与 5:4 之间来回跳。
  关闭时保持原行为（仅在 style 为空时按首个素材方向选一次）。

## [0.3.1] - 2026-10-01

### 变更
- **首装默认相框**：首个素材为横向用 `横-花环.png`，竖向（或方形）用 `竖-花环.png`
  （此前竖向是 `竖-信笺.png`）。
- 明确"用户自定义过相框就不覆盖"的契约：默认配置 `frame.style` 留空 → 首次按方向自动选
  并写回配置；此后 style 非空，自动选择逻辑直接跳过，**重启以用户设置为准**。

## [0.3.0] - 2026-10-01

### 修复
- **删除相框后设置页仍显示旧文件名**：两个根因
  1. `frame_dir()` 候选顺序错 —— 编译期源码目录排在**安装目录之前**，
     开发构建永远读源码、`make install` 的目录形同虚设，用户改哪边都对不上。
     现改为「$OMA_FRAME_DIR → exe 同级 → exe 上级 → <prefix>/share/omaframe/frame
     → /usr/share/omaframe/frame → $XDG_DATA_HOME → cargo 布局 → 源码目录（最后兜底）」。
  2. `settings::show()` 在**面板已打开**时提前 return，把媒体/相框重扫整个跳过。
     现把同步逻辑前移，幂等且有指纹比对兜底，不会白重载。

### 新增
- **跨发行版默认媒体目录**：`default_media_dir()` 原来只认 Omarchy 布局，
  现按"存在即用"探测 Omarchy current → omarchy/backgrounds → XDG backgrounds →
  hypr/backgrounds → /usr/share/backgrounds → ~/Pictures 等，
  支持 `OMA_MEDIA_DIR` 显式覆盖；使程序在 debian/arch/ubuntu 上也能默认加载系统壁纸。
- 设置页最下方新增**项目主页**行（可点击，用 xdg-open 打开 GitHub 仓库）。
- Cargo.toml 补 `repository` 字段（链接取自该字段，不硬编码）。

## [0.2.3] - 2026-10-01

### 新增
- 打开设置面板时**重建相框库索引**：扫描 frame/ 算出"文件名+大小+mtime"指纹，
  与上次一致就跳过（不做无谓重载），变了就 `load_frame()` 整体重建。
  用户手动往相框目录**新增/删除/替换**相框图后，不用重启即可在设置里看到并使用。
  之前相框列表与渲染切片只在启动时读一次。
- 相框库新增 5 个：横-马里奥 简约、竖-我的世界、竖-林克、竖-耀西、竖-花环（共 34 个）

## [0.2.2] - 2026-10-01

### 维护
- 相框库更新：`横-木纹.png` 换成 1200×651 更高分辨率源图、`横-科技.png` 换成更高版本；
  移除下载残留 `横-科技1.png`（git 索引已与磁盘对齐：29 = 29）

### 修复
- **安装目录从不清理陈旧相框**：`make install` / `scripts/install.sh` 只覆盖同名文件，
  源码里删掉的相框会一直留在 `share/omaframe/frame` 里被程序列出来。
  现在改为先清空再拷贝，保证安装目录与源码目录一致。

## [0.2.1] - 2026-10-01

### 新增
- **应用菜单支持拼音首字母搜索**：`.desktop` 增加 Keywords
  （`zm` / `xk` / `zmxk` 覆盖"桌面""相框""桌面相框"，另含完整拼音与英文别名）。
  应用菜单本身**不做拼音转换**，只按字面匹配 Name/Keywords/Comment，
  所以把缩写显式写进 Keywords 才能用首字母定位。
  同时补 `Keywords[zh_CN]` 本地化条目。

## [0.2.0] - 2026-10-01

首个功能完整版本。相对 0.1.0 变更较大，故按 minor 递增。

### 新增
- **智能自适应 PNG 相框引擎**（`frame_model.rs`）：alpha 分析 → 边界洪泛 →
  最大连通块内孔 → 腐蚀 → 最大内接矩形 → 缓存到
  `~/.cache/omarchy-omaframe/frames/`；任意四角+中心透明的 PNG 丢进 `frame/` 即用
- **HEIC/HEIF 相框素材**支持（gdk-pixbuf 解不了时回退 ffmpeg / ImageMagick）
- **iPhone MOV 竖屏修正**（按容器旋转矩阵 90/270 交换宽高）
- **一键安装脚本** `scripts/install.sh`：自动体检/安装依赖 → 编译 → 安装 → 自检
- **AUR 包** `PKGBUILD` + `.SRCINFO`，`yay -S omaframe` 可装
- **依赖单一事实来源** `packaging/dependencies.conf`（安装脚本与 PKGBUILD 同源）
- **发布自检** `scripts/check-release.sh`（版本/依赖/动态库/打包文件一致性）
- **系统菜单入口**：安装时生成 `~/.local/share/applications/omaframe.desktop`
  与配套 SVG 图标，在 Omarchy 应用菜单里搜"桌面相框"即可启动
  （autostart 项按设计不会出现在菜单里，必须单独装 applications/ 项）
- **MIT LICENSE** 文件（AUR 与发布自检要求）
- **相框库压缩工具** `scripts/optimize-frames.py`：预乘 alpha 缩放到 ≤1000px，
  50MB → 18MB（-65%），透明通道逐位无损
- **GitHub 推送脚本** `scripts/push.sh`：绕过 ghfast.top 代理，用 gh token 直连
- 每次打开设置面板自动重新扫描媒体目录（可发现手动新增的素材）
- 首装默认值：媒体目录=当前系统壁纸、350×350、轮换 5 秒、视频静音 30fps、
  默认启用相框、按首个素材方向自动选相框（横→横-花环 / 竖→竖-信笺）、左上角边距 12

### 修复
- **九宫格接缝"十字线/缝隙"三层根因**：
  1. 绘制时每片外扩 1px 导致内容错位 → 改为严格按整数矩形拼接；
  2. 模型内孔比 PNG 真实透明区内缩约 1 像素 → 改为原图精修到 alpha 接近实心处；
  3. 九片各自成为独立 GSK 渲染节点、边界落在分数设备像素 → 改为画进同一个 cairo 节点
- **播放/暂停按钮消失或只显示一半**：控制层 cairo 节点裁剪框被二次平移成
  `(2fx,2fy,…)`；改为先建节点再平移上下文
- **`omaframe quit` 无效**：补控制通道 `quit` 命令，并在 GTK 初始化前返回
- **`GLib-GIO-CRITICAL: This application can not open files`**：
  Application flags 收敛为只保留 `NON_UNIQUE`
- **状态栏图标时有时无**：固定 bus 名改为 SNI 规范要求的每实例唯一名
- **托盘 Activate 无回复导致宿主判定失效**：补空回复
- **IconPixmap 类型错误被宿主拒收**：改为 `a(iiay)` 数组
- **视觉回归脚本静默跳过全部相框**：改用仓库自带相框库并真正过滤
- 设置页除「媒体」「显示」外删除冗余说明文字；相框样式不再显示路径
- 托盘图标加深，与顶栏其它系统图标统一色系

### 打包
- 修复 `make install` **未安装相框库**的缺陷（换机器后相框库为空）
- `frame_dir()` 增加安装布局探测（`share/omaframe/frame`、`/usr/share/omaframe/frame`）
- 自启项 `Exec` 不再硬编码个人路径；`OnlyShowIn` 改为规范的 `X-Hyprland`
- 菜单项 `Categories` 去掉重复主分类（原值会让它在菜单里出现两次）
- 消除全部编译警告（0 warning）

[0.5.2]: https://github.com/playGitboy/Omaframe/releases/tag/v0.5.2
[0.5.1]: https://github.com/playGitboy/Omaframe/releases/tag/v0.5.1
[0.5.0]: https://github.com/playGitboy/Omaframe/releases/tag/v0.5.0
[0.4.5]: https://github.com/playGitboy/Omaframe/releases/tag/v0.4.5
[0.4.4]: https://github.com/playGitboy/Omaframe/releases/tag/v0.4.4
[0.4.3]: https://github.com/playGitboy/Omaframe/releases/tag/v0.4.3
[0.4.2]: https://github.com/playGitboy/Omaframe/releases/tag/v0.4.2
[0.4.1]: https://github.com/playGitboy/Omaframe/releases/tag/v0.4.1
[0.4.0]: https://github.com/playGitboy/Omaframe/releases/tag/v0.4.0
[0.3.1]: https://github.com/playGitboy/Omaframe/releases/tag/v0.3.1
[0.3.0]: https://github.com/playGitboy/Omaframe/releases/tag/v0.3.0
[0.2.3]: https://github.com/playGitboy/Omaframe/releases/tag/v0.2.3
[0.2.2]: https://github.com/playGitboy/Omaframe/releases/tag/v0.2.2
[0.2.1]: https://github.com/playGitboy/Omaframe/releases/tag/v0.2.1
[0.2.0]: https://github.com/playGitboy/Omaframe/releases/tag/v0.2.0
