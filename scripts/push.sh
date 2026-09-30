#!/usr/bin/env bash
# 推送到 GitHub：**绕过 ghfast.top 代理，用 gh 的 token 直连**。
#
# 为什么需要这个脚本：
#   本机 git 配了全局 insteadOf 规则
#     url.https://ghfast.top/https://github.com/.insteadOf = https://github.com/
#   克隆/读取走代理没问题，但**代理不支持 push**，且它会拦截任何 github.com 的推送 URL。
#
# 做法：
#   1) 复制一份全局 git 配置，去掉 url.*.insteadof（保留 http.proxy 等其它设置）
#      → 用 GIT_CONFIG_GLOBAL 指向它，克隆/推送就不再被改写到代理
#   2) 凭据用 gh 的 token：`gh auth setup-git` 的等价物，这里直接用
#      `!gh auth git-credential`，不把 token 写进远端 URL 或 shell 历史
#
# 用法：scripts/push.sh [分支名]   （默认当前分支）
set -euo pipefail

BRANCH="${1:-$(git rev-parse --abbrev-ref HEAD)}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

command -v gh >/dev/null || { echo "需要 gh CLI（未安装）" >&2; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "gh 未登录：先跑 gh auth login" >&2; exit 1; }

# ① 构造一份"去掉 insteadOf"的全局配置
TMP_GLOBAL="$(mktemp)"
trap 'rm -f "$TMP_GLOBAL"' EXIT
if [ -f "${XDG_CONFIG_HOME:-$HOME/.config}/git/config" ]; then
  grep -v 'insteadof' "${XDG_CONFIG_HOME:-$HOME/.config}/git/config" >"$TMP_GLOBAL" || true
else
  : >"$TMP_GLOBAL"
fi

REPO="https://github.com/playGitboy/Omaframe.git"
echo "▶ 推送 $BRANCH → $REPO （已绕过代理，凭据用 gh token）"

# ② 直连推送；用 -c 注入凭据助手，避免改动仓库/全局配置
GIT_CONFIG_GLOBAL="$TMP_GLOBAL" GIT_TERMINAL_PROMPT=0 \
  git -c credential.helper= -c "credential.helper=!gh auth git-credential" \
  push -u "https://github.com/playGitboy/Omaframe.git" "$BRANCH"

echo "✓ 已推送。origin 仍是代理地址，读取不受影响；以后要推就跑这个脚本。"
