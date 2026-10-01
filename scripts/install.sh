#!/usr/bin/env bash
# Omaframe 一键安装：自动判断系统依赖 → 安装缺失 → 编译 → 安装 → 自检
#
#   curl -fsSL <repo>/scripts/install.sh | bash          # 从源码目录安装（需已 clone）
#   scripts/install.sh                                   # 同上
#   scripts/install.sh --deps-only                      # 只装依赖不编译
#   scripts/install.sh --check                          # 只做依赖体检，不装不改
#
# 依赖清单来自 packaging/dependencies.conf（与 PKGBUILD 同源）。
# 支持：Arch 系（pacman / yay）、Debian 系（apt）、Fedora 系（dnf）。
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEP_FILE="$ROOT/packaging/dependencies.conf"
PREFIX="${PREFIX:-$HOME/.local}"
BINDIR="$PREFIX/bin"
FRAMEDIR="$PREFIX/share/omaframe/frame"
AUTOSTART="$HOME/.config/autostart"
APPDIR="$PREFIX/share/applications"
ICONDIR="$PREFIX/share/icons"
CONFIG_DIR="$HOME/.config/omarchy-omaframe"

MODE=install
case "${1:-}" in
  --deps-only) MODE=deps ;;
  --check)     MODE=check ;;
  -h|--help)   sed -n '2,12p' "$0"; exit 0 ;;
esac

say()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$*"; }
die()  { printf '  \033[31m✗\033[0m %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------- 依赖清单解析
# 输出：RUNTIME / BUILD / OPTIONAL 三个空格分隔列表
parse_deps() {
  RUNTIME=""; BUILD=""; OPTIONAL=""
  [ -f "$DEP_FILE" ] || die "找不到依赖清单：$DEP_FILE"
  local kind pkg rest
  while read -r kind pkg rest; do
    case "$kind" in
      runtime) RUNTIME="$RUNTIME $pkg" ;;
      build)   BUILD="$BUILD $pkg" ;;
      optional) OPTIONAL="$OPTIONAL $pkg" ;;
    esac
  done <"$DEP_FILE"
}

# ---------------------------------------------------------------- 包管理器
detect_pm() {
  if command -v pacman >/dev/null; then PM=pacman
  elif command -v apt-get >/dev/null; then PM=apt
  elif command -v dnf >/dev/null; then PM=dnf
  else PM=none
  fi
}

# 某包是否已安装（各包管理器查询方式不同）
pkg_installed() {
  local p="$1"
  case "$PM" in
    pacman) pacman -Qq "$p" >/dev/null 2>&1 ;;
    apt)    dpkg -s "$p" >/dev/null 2>&1 ;;
    dnf)    rpm -q "$p" >/dev/null 2>&1 ;;
    *)      return 1 ;;
  esac
}

# 安装一批包（缺什么装什么；能免交互就用 sudo -n）
install_pkgs() {
  local pkgs="$*"
  [ -n "$(echo "$pkgs" | tr -d ' ')" ] || return 0
  case "$PM" in
    pacman)
      if command -v yay >/dev/null; then
        yay -S --needed --noconfirm $pkgs && return 0
      fi
      sudo pacman -S --needed --noconfirm $pkgs
      ;;
    apt)    sudo apt-get update -qq && sudo apt-get install -y $pkgs ;;
    dnf)    sudo dnf install -y $pkgs ;;
    *)      die "未识别的包管理器，请手动安装：$pkgs" ;;
  esac
}

# ---------------------------------------------------------------- 体检
parse_deps
detect_pm
say "包管理器：$PM"

MISS_R=""; MISS_B=""; MISS_O=""
for p in $RUNTIME; do pkg_installed "$p" || MISS_R="$MISS_R $p"; done
for p in $BUILD;   do pkg_installed "$p" || MISS_B="$MISS_B $p"; done
for p in $OPTIONAL; do pkg_installed "$p" || MISS_O="$MISS_O $p"; done

if [ -n "$(echo "$MISS_R" | tr -d ' ')" ]; then
  say "缺少运行依赖：$MISS_R"
else
  ok "运行依赖齐全"
fi
if [ -n "$(echo "$MISS_O" | tr -d ' ')" ]; then
  warn "缺少可选依赖：$MISS_O（对应功能会降级，不影响启动）"
fi

# 兼容性判断：layer-shell 是硬需求（Hyprland/wlroots 系）
if [ "$PM" != none ] && ! pkg_installed gtk4-layer-shell; then
  warn "未检测到 gtk4-layer-shell：本程序依赖 wlr-layer-shell 协议，"
  warn "在不支持 layer-shell 的合成器（Sway/GNOME/Wayland 原生）上无法显示相框。"
fi

[ "$MODE" = check ] && { say "体检结束（--check 未做任何改动）"; exit 0; }

# ---------------------------------------------------------------- 装依赖
NEED="$MISS_R $MISS_B"
if [ -n "$(echo "$NEED" | tr -d ' ')" ]; then
  say "安装缺失依赖：$NEED"
  install_pkgs $NEED || die "依赖安装失败（可能需要 sudo 权限）"
  ok "依赖已安装"
fi
[ -n "$(echo "$MISS_O" | tr -d ' ')" ] && {
  say "是否安装可选依赖：$MISS_O（y/N）"
  read -r ans || ans=N
  case "$ans" in [yY]*) install_pkgs $MISS_O && ok "可选依赖已安装" ;; *) warn "跳过可选依赖" ;; esac
}

[ "$MODE" = deps ] && { say "依赖安装完成（--deps-only）"; exit 0; }

# ---------------------------------------------------------------- 编译
command -v cargo >/dev/null || die "没有 cargo，请先安装 rust 工具链"
say "编译（cargo build --release，可能需要几分钟）"
( cd "$ROOT" && cargo build --release ) || die "编译失败"

# ---------------------------------------------------------------- 安装
say "安装到 $PREFIX"
install -Dm755 "$ROOT/target/release/omaframe" "$BINDIR/omaframe"
# 相框库：必须一起装，否则用户机器上 frame_dir() 找不到任何相框
install -d "$FRAMEDIR"
install -Dm644 "$ROOT"/frame/*.png "$FRAMEDIR"/ 2>/dev/null
[ -d "$HOME/.config/autostart" ] && install -Dm644 "$ROOT/packaging/omaframe.desktop" "$AUTOSTART/omaframe.desktop"
# 系统菜单入口：autostart 项不会出现在应用菜单里，必须另装 applications/ 项
install -Dm644 "$ROOT/packaging/omaframe-app.desktop" "$APPDIR/omaframe.desktop"
install -Dm644 "$ROOT/packaging/omaframe.svg" "$ICONDIR/hicolor/scalable/apps/omaframe.svg"

ok "程序      $BINDIR/omaframe"
ok "相框库    $FRAMEDIR（$(ls "$FRAMEDIR"/*.png 2>/dev/null | wc -l) 个）"
ok "登录自启  $AUTOSTART/omaframe.desktop"
ok "菜单入口  $APPDIR/omaframe.desktop（应用菜单里搜“桌面相框”）"
ok "配置目录  $CONFIG_DIR"

# ---------------------------------------------------------------- 自检
say "自检"
"$BINDIR/omaframe" --version >/dev/null 2>&1 && ok "二进制可执行" || warn "二进制自检未返回版本号（非致命）"
miss=""
for so in libgtk-4.so.1 libadwaita-1.so.0 libgtk4-layer-shell.so.0 libgdk_pixbuf-2.0.so.0 libcairo.so.2; do
  ldd "$BINDIR/omaframe" 2>/dev/null | grep -q "$so" || miss="$miss $so"
done
[ -z "$miss" ] && ok "动态库链接完整" || warn "缺少动态库：$miss"

say "完成。启动：$BINDIR/omaframe    设置：$BINDIR/omaframe settings"