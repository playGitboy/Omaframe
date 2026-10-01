#!/usr/bin/env bash
# 发布自检：版本号 / 依赖 / 动态库 三处一致性校验。
#
# 目的：防止"改了依赖或版本，忘了同步 PKGBUILD / 安装脚本"。
# 已挂到 git 的 pre-push 钩子（scripts/setup-git-hooks.sh）。
# 手动跑：scripts/check-release.sh
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
FAIL=0
ok()  { printf '  \033[32m✓\033[0m %s\n' "$*"; }
bad() { printf '  \033[31m✗\033[0m %s\n' "$*"; FAIL=1; }
warn(){ printf '  \033[33m!\033[0m %s\n' "$*"; }

printf '\033[1;36m==>\033[0m 发布自检\n'

# ---------------------------------------------------------------- 1. 版本号一致
V_CARGO=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
V_PKG=$(sed -n 's/^pkgver=\(.*\)/\1/p' PKGBUILD | head -1)
V_CHANGELOG=$(sed -n 's/^## \[\([0-9.]*\)\].*/\1/p' CHANGELOG.md | head -1)

[ -n "$V_CARGO" ] && ok "Cargo.toml 版本 $V_CARGO" || bad "读不到 Cargo.toml 版本"
[ "$V_CARGO" = "$V_PKG" ] && ok "PKGBUILD 版本一致 ($V_PKG)" \
  || bad "版本不一致：Cargo.toml=$V_CARGO vs PKGBUILD=$V_PKG"
[ "$V_CARGO" = "$V_CHANGELOG" ] && ok "CHANGELOG 最新条目一致 ($V_CHANGELOG)" \
  || bad "版本不一致：Cargo.toml=$V_CARGO vs CHANGELOG=$V_CHANGELOG"

# 源码版本与二进制 --version 是否一致（版本号真的编进去了）
if [ -x target/release/omaframe ]; then
  BIN_V=$(./target/release/omaframe --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
  [ "$BIN_V" = "$V_CARGO" ] && ok "二进制内版本一致 ($BIN_V)" \
    || bad "二进制版本=$BIN_V 与 Cargo.toml=$V_CARGO 不一致（需重新编译）"
else
  warn "未找到 target/release/omaframe，跳过二进制版本核对"
fi

# ---------------------------------------------------------------- 2. 依赖一致
DEP=packaging/dependencies.conf
RUNTIME=$(awk '$1=="runtime"{print $2}' "$DEP" | sort | tr '\n' ' ')
BUILD=$(awk '$1=="build"{print $2}' "$DEP" | sort | tr '\n' ' ')
PKG_DEP=$(sed -n "s/^depends=('\(.*\)').*/\1/p" PKGBUILD | tr "'" '\n' | grep -v '^$' | sort | tr '\n' ' ')
PKG_MAKE=$(sed -n 's/^makedepends=(\(.*\)).*/\1/p' PKGBUILD | tr "'" '\n' | grep -vE '^$|cargo|rust|pkgconf|git' | sort | tr '\n' ' ')

MISS=""
for p in $PKG_DEP; do echo "$RUNTIME" | grep -qw "$p" || MISS="$MISS $p"; done
[ -z "$MISS" ] && ok "PKGBUILD depends ⊆ dependencies.conf runtime" \
  || bad "PKGBUILD depends 里这些包不在 dependencies.conf：$MISS"

MISS=""
for p in $RUNTIME; do echo "$PKG_DEP" | grep -qw "$p" || MISS="$MISS $p"; done
[ -z "$MISS" ] && ok "dependencies.conf runtime 全部出现在 PKGBUILD" \
  || bad "dependencies.conf 的 runtime 依赖没进 PKGBUILD：$MISS"

MISS=""
for p in $PKG_MAKE; do echo "$BUILD" | grep -qw "$p" || MISS="$MISS $p"; done
[ -z "$MISS" ] && ok "PKGBUILD makedepends ⊆ dependencies.conf build" \
  || bad "PKGBUILD makedepends 多出：$MISS"

# 安装脚本是否也读同一份清单
grep -q 'packaging/dependencies.conf' scripts/install.sh \
  && ok "install.sh 使用同一份依赖清单" \
  || bad "install.sh 没有引用 packaging/dependencies.conf（会漂移）"

# ---------------------------------------------------------------- 2b. 相框库必须已提交
# 打包用的 source 是 **GitHub tag 归档**，不是本地工作区。
# 若 frame/ 里有未提交的新增/改名，打出来的包仍是旧快照（曾出现
# 安装版还在用 `大头贴-*`、仓库已改成 `方-*` 的错位）。
# 规则：**以本机 frame/ 为最新资源**，改完必须先提交，再打 tag 打包。
if [ -d .git ]; then
  FRAME_DIRTY="$(git status --porcelain -- frame 2>/dev/null)"
  if [ -n "$FRAME_DIRTY" ]; then
    bad "frame/ 有未提交改动，打包会打进旧快照（source 用的是 tag 归档）："
    echo "$FRAME_DIRTY" | head -10 | sed 's/^/      /'
    echo "      → 先 git add -A frame \&\& git commit（以本机为最新资源），再打包" >&2
  else
    n=$(ls frame/*.png 2>/dev/null | wc -l)
    ok "frame/ 已全部提交（$n 个相框），打包不会漏"
  fi
  # tag 必须指向当前提交，否则 source 归档是旧的
  if [ -n "$(git tag --points-at HEAD 2>/dev/null)" ]; then
    ok "HEAD 已有 tag：$(git tag --points-at HEAD | tr '\n' ' ')"
  else
    warn "HEAD 没有 tag —— PKGBUILD 的 source 指向 tag 归档，请先 git tag v<版本> 并推送"
  fi
fi

# ---------------------------------------------------------------- 3. 动态库齐
if [ -x target/release/omaframe ]; then
  for so in libgtk-4.so.1 libadwaita-1.so.0 libgtk4-layer-shell.so.0 \
            libgdk_pixbuf-2.0.so.0 libcairo.so.2; do
    ldd target/release/omaframe 2>/dev/null | grep -q "$so" \
      || bad "二进制未链接 $so"
  done
  ok "关键动态库链接检查完成"
else
  warn "未编译，跳过动态库检查"
fi

# ---------------------------------------------------------------- 4. 打包必需文件
for f in PKGBUILD CHANGELOG.md LICENSE README.md readme_zh.md packaging/omaframe.desktop \
         packaging/omaframe-app.desktop packaging/omaframe.svg \
         packaging/dependencies.conf scripts/install.sh scripts/optimize-frames.py; do
  [ -f "$f" ] && ok "$f 存在" || bad "缺少 $f"
done
[ -d frame ] && [ "$(ls frame/*.png 2>/dev/null | wc -l)" -gt 0 ] \
  && ok "frame/ 相框库非空（$(ls frame/*.png|wc -l) 个，会被打包）" \
  || bad "frame/ 为空"

# PKGBUILD 的 source 用的是仓库 tarball，但 sha256 需发布时填
if grep -q "sha256sums=('SKIP')" PKGBUILD; then
  warn "PKGBUILD sha256sums 还是 SKIP —— 上架 AUR 前必须填真实校验和"
fi

echo
[ "$FAIL" = 0 ] && printf '\033[1;32m发布自检通过\033[0m\n' || printf '\033[1;31m发布自检未通过\033[0m\n'
exit $FAIL