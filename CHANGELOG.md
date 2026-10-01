# Changelog

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

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

[0.3.1]: https://github.com/playGitboy/Omaframe/releases/tag/v0.3.1
[0.3.0]: https://github.com/playGitboy/Omaframe/releases/tag/v0.3.0
[0.2.3]: https://github.com/playGitboy/Omaframe/releases/tag/v0.2.3
[0.2.2]: https://github.com/playGitboy/Omaframe/releases/tag/v0.2.2
[0.2.1]: https://github.com/playGitboy/Omaframe/releases/tag/v0.2.1
[0.2.0]: https://github.com/playGitboy/Omaframe/releases/tag/v0.2.0
