# Omaframe · 桌面电子相框

> **把青春回忆摆回桌面上。**
>
> 选一个目录，常见图片与视频自动轮播；内置 30+ 套相框随心搭配，智能算法自动适配。
> **让一切美好回忆常伴左右。**

[English](README.md) · **简体中文**

---

## 它能做什么

| | |
|---|---|
| 📁 **目录即相册** | 选一个目录，常见图片与视频自动收录，递归扫描子目录 |
| 🔁 **自动轮播** | 间隔、随机、暂停全可调；手动翻页后自动重新计时 |
| 🖼️ **30+ 套内置相框** | 横 / 竖 / 方三种版式，多风格自由搭配；**智能算法自动分析相框的透明内孔**，按素材比例重构，任意 PNG 丢进相框库即可用 |
| 🖱️ **随手摆放** | 按住相框**任意位置**即可拖动；拖**右下角**自由缩放，始终保持素材比例 |
| ⏯️ **轻点即换** | 点画面**左半边**上一张、**右半边**下一张；悬停出现播放/暂停 |
| 😴 **被挡住就停** | 其他应用窗口盖住相框时**自动暂停**轮播与视频解码，CPU 归零 |
| 🎛️ **设置面板** | 点**状态栏图标**弹出，改一项立即生效 |

**支持的格式**：JPG / PNG / WebP / GIF / BMP / TIFF / AVIF / **HEIC**（iPhone 照片）与
MP4 / MOV / MKV / WebM / AVI / M4V 等 —— 视频走系统 `ffmpeg`，**无需额外装解码器**。

---

## 快速开始

### 安装

**Arch / Omarchy** —— 从
[Releases](https://github.com/playGitboy/Omaframe/releases) 取预编译包，一条命令装上：

```bash
# 取最新 release 的包并安装
curl -fsSL https://github.com/playGitboy/Omaframe/releases/latest/download/omaframe.pkg.tar.zst -o /tmp/omaframe.pkg.tar.zst \
  && sudo pacman -U /tmp/omaframe.pkg.tar.zst
```

卸载：`sudo pacman -Rns omaframe`（保留 `~/.config/omarchy-omaframe/` 里的配置）。

**任意发行版 · 从源码**：一键脚本自动判断并安装依赖 → 编译 → 安装 → 自检：

```bash
git clone https://github.com/playGitboy/Omaframe && cd Omaframe
scripts/install.sh
```

安装后**应用菜单搜「桌面相框」**（支持拼音首字母 `zm` / `xk` / `zmxk`）即可启动。

### 使用

| 操作 | 效果 |
|---|---|
| 鼠标移入相框 | 淡入控制层（▶/⏸、右下角缩放手柄） |
| 点画面左 / 右半边 | 上一张 / 下一张 |
| 按住相框任意位置拖动 | 移动摆放 |
| 拖右下角 | 缩放（保持比例） |
| 点状态栏图标 | 打开设置面板（Esc / 点面板外收起） |
| `omaframe settings` / `omaframe quit` | 打开设置 / 退出运行中的实例 |

### 首次运行的默认值

开箱即用：媒体目录取**当前系统壁纸**、350×350、**5 秒**轮播、视频静音 30fps、
**默认启用相框**并按首个素材方向自动挑版式、停靠左上角边距 12。

---

## 为什么它很轻

- **原生 Rust + GTK4 + libadwaita**，不用 Electron / Tauri
- 桌面层 `wlr-layer-shell`（bottom）：**不抢焦点、不参与平铺**、不进窗口列表，
  被其他窗口盖住时由 Hyprland IPC 事件驱动暂停（零轮询、零常驻 CPU）
- 图片后台解码 + 边解码边缩放 + 缓存（条目数与字节双上限）
- 视频用系统 `ffmpeg`（与系统动态壁纸同一套解码器）
- 空闲时 CPU ≈ 0%，完全不影响你干正事

## 兼容性

GTK 4.10+ 均可编译；**非 Hyprland / 无 layer-shell / 无 ffmpeg 都会优雅降级**而不是报错。
程序也能跑在 Omarchy 之外的 Debian / Arch / Ubuntu 上。

## 文档

- [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md) —— 需求清单与「禁忌速查」
- [`docs/KEY-FINDINGS.md`](docs/KEY-FINDINGS.md) —— 实现结论与踩坑记录
- [`CHANGELOG.md`](CHANGELOG.md) —— 版本变更

## 许可

[MIT](LICENSE)
