#!/usr/bin/env bash
# omaframe 一键版本回滚 / 切换
# 用法:
#   scripts/rollback.sh              列出可选版本
#   scripts/rollback.sh <ref>        切到该版本（tag / 提交号 / 分支）并重新安装
#   scripts/rollback.sh --list       同上
set -euo pipefail
cd "$(dirname "$0")/.."
REPO="$PWD"

list_versions() {
  echo "可用版本（tag / 最近提交）:"
  git tag --sort=-creatordate | head -20 | sed 's/^/  tag  /'
  echo
  echo "最近提交:"
  git log --oneline --decorate -15 | sed 's/^/  /'
  echo
  echo "用法: scripts/rollback.sh v1.0.0     切回 V1 完成版"
  echo "      scripts/rollback.sh <commit>   切到任意提交"
  echo "      scripts/rollback.sh latest     切回最新提交（恢复开发中的版本）"
}

[[ "${1:-}" == "" || "${1:-}" == "--list" || "${1:-}" == "-h" ]] && { list_versions; exit 0; }

REF="$1"
if ! git rev-parse --verify "$REF" >/dev/null 2>&1; then
  echo "找不到版本/提交: $REF" >&2
  list_versions
  exit 1
fi

echo "==> 当前状态: $(git rev-parse --short HEAD) $(git log -1 --pretty=%s)"
read -r -p "    确认切换到 $REF ? 输入 yes 继续: " ans
[[ "$ans" == "yes" ]] || { echo "已取消"; exit 0; }

# 保留当前工作（未提交也留着），用 detached HEAD 切过去
if [[ -n "$(git status --porcelain)" ]]; then
  echo "    检测到未提交改动，将保留（切回 latest 即可）"
fi
git checkout "$REF" 2>&1 | tail -2
echo "==> 重新构建并安装（约 1 分钟）"
cargo build --release
make install
echo "==> 完成。现在运行 'omaframe' 生效；回开发版用 scripts/rollback.sh latest"
