#!/usr/bin/env bash
# 推送到 GitHub：**绕过 ghfast.top 代理，用 gh 的 token 直连**。（以后默认用这个脚本推）
#
# 为什么需要：
#   本机 git 配了 insteadOf 规则（同时在 ~/.gitconfig 与 ~/.config/git/config）：
#     url.https://ghfast.top/https://github.com/.insteadof = https://github.com/
#   代理只支持读、**不支持 push**，且会把任何 github.com 的推送 URL 改写掉。
#
# 怎么绕过（已验证）：
#   1) 用 **https://github.com:443/...** 形式。insteadOf 匹配的是
#      "https://github.com/"（主机后紧跟 /），加了 :443 后前缀不再匹配，
#      于是 URL 不会被改写到代理。
#   2) 凭据用 gh 的 token，**只在本次 push 的进程内**通过 URL 传入，
#      不写进远端配置、不进 shell 历史。token 来自 `gh auth token`（gh keyring）。
#
# 用法：scripts/push.sh [分支名]   （默认当前分支）
set -euo pipefail

BRANCH="${1:-$(git rev-parse --abbrev-ref HEAD)}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

command -v gh >/dev/null || { echo "需要 gh CLI（未安装）" >&2; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "gh 未登录：先跑 gh auth login" >&2; exit 1; }

TOKEN="$(gh auth token)"
[ -n "$TOKEN" ] || { echo "取不到 gh token" >&2; exit 1; }

# 临时关闭 set -x 风险：token 只出现在下面的 URL 参数里
PUSH_URL="https://x-access-token:${TOKEN}@github.com:443/playGitboy/Omaframe.git"

echo "▶ 推送分支 '$BRANCH' → github.com:443（绕过代理，凭据用 gh token）"
GIT_TERMINAL_PROMPT=0 git push "$PUSH_URL" "$BRANCH"
echo "✓ 已推送。origin 仍是代理地址（读取正常），以后推送就跑这个脚本。"
