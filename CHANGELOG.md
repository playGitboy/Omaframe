# Changelog

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

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

[0.2.0]: https://github.com/playGitboy/Omaframe/releases/tag/v0.2.0
