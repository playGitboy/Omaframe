#!/usr/bin/env bash
# 视觉回归测试：真实相框 × 多种素材比例 → 截图（保存到 target/visual/）
#
# 用法：
#   scripts/visual-test.sh                     # 默认测 木纹/花环/炫彩
#   scripts/visual-test.sh 木纹.png 猫线.png    # 指定相框
#   PHOTO_FRAME_DEBUG_OVERLAY=1 scripts/visual-test.sh   # 同时打开九宫格调试网格
#
# 测试素材默认放在 /tmp/pfm/{w16x9,p9x16,s1x1,u21x9}/，每个目录一张图。
# 运行结束会还原用户配置并重启程序。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${HOME}/.local/bin/photo-frame"
OUT="${ROOT}/target/visual"
CFG="${HOME}/.config/omarchy-photo-frame/config.toml"
MEDIA_ROOT="${PF_TEST_MEDIA:-/tmp/pfm}"
FRAMES=("$@")
[ ${#FRAMES[@]} -eq 0 ] && FRAMES=(木纹.png 花环.png 炫彩.png)

mkdir -p "$OUT" "$MEDIA_ROOT"
cp "$CFG" /tmp/cfg.visual.bak

# 生成测试素材（纯色，便于像素级校验：无空隙/不穿框）
gen_media() {
  local dir="$1"; local size="$2"; local color="$3"
  mkdir -p "$MEDIA_ROOT/$dir"
  [ -f "$MEDIA_ROOT/$dir/media.png" ] || magick -size "$size" xc:"$color" "$MEDIA_ROOT/$dir/media.png"
}
gen_media w16x9 1920x1080 '#ff00ff'
gen_media p9x16 1080x1920 '#00ffff'
gen_media s1x1  1200x1200 '#ffff00'
gen_media u21x9 2520x1080 '#00ff88'

restart() {
  pkill -x photo-frame 2>/dev/null || true
  sleep 0.8
  PHOTO_FRAME_LOG=debug setsid nohup "$BIN" > /tmp/vt.log 2>&1 < /dev/null &
  sleep 3.5
}

for frame in "${FRAMES[@]}"; do
  for ratio in w16x9 p9x16 s1x1 u21x9; do
    python3 - "$frame" "$ratio" "$MEDIA_ROOT" "$CFG" <<'PY'
import re, sys
frame, ratio, root, cfg = sys.argv[1:5]
s = open(cfg).read()
s = re.sub(r'(?m)^style = ".*"$', f'style = "{frame}"', s, count=1)
s = re.sub(r'(?m)^path = ".*"$', f'path = "{root}/{ratio}"', s, count=1)
s = re.sub(r'(?m)^enabled = false$', 'enabled = true', s)
# 测试用固定画布上限（脚本结束会整体还原用户配置）
s = re.sub(r'(?m)^max_width = .*$', 'max_width = 720', s, count=1)
s = re.sub(r'(?m)^max_height = .*$', 'max_height = 720', s, count=1)
open(cfg, 'w').write(s)
PY
    restart
    geo=$(grep -oE '输入区域 → [0-9]+x[0-9]+\+[0-9]+\+[0-9]+' /tmp/vt.log | tail -1 | sed 's/输入区域 → //')
    W="${geo%%x*}"; rest="${geo#*x}"; H="${rest%%+*}"; X="${rest#*+}"; X="${X%%+*}"; Y="${geo##*+}"
    timeout 8 grim /tmp/vt_full.png >/dev/null 2>&1
    # 逻辑坐标 → 物理：×1.25，顶栏偏移 +27.5；四周各留 20px 便于看边缘
    magick /tmp/vt_full.png \
      -crop "$(python3 -c "print(int(($W+40)*1.25))")x$(python3 -c "print(int(($H+40)*1.25))")+$(python3 -c "print(max(0,int(float($X)*1.25-25)))")+$(python3 -c "print(max(0,int(float($Y)*1.25+27.5-25)))")" \
      +repage -resize 520x "$OUT/${frame%.png}_${ratio}.png"
    echo "  ${frame%.png} + ${ratio} → ${frame%.png}_${ratio}.png（相框 ${W}x${H}）"
  done
done

cp /tmp/cfg.visual.bak "$CFG"
restart
echo "已还原配置；截图在 $OUT"
