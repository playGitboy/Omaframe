#!/usr/bin/env bash
# 安装 git 钩子：push 前自动跑发布自检，防止依赖/版本漂移。
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOOK="$ROOT/.git/hooks/pre-push"
mkdir -p "$(dirname "$HOOK")"
cat >"$HOOK" <<HOOKEOF
#!/usr/bin/env bash
# Omaframe pre-push：发布自检（版本/依赖/动态库/打包文件一致性）
bash "$ROOT/scripts/check-release.sh" || {
  echo "❌ 发布自检未通过，已阻止推送。" >&2
  echo "   若确认无误可临时用 git push --no-verify 跳过。" >&2
  exit 1
}
HOOKEOF
chmod +x "$HOOK"
echo "✓ 已安装 pre-push 钩子：$HOOK"
