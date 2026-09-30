#!/usr/bin/env python3
"""相框库压缩：**默认方案**。

做两件事（可分别开关）：
  1. 等比缩放到最长边 ≤ --maxdim（默认 1000）
  2. 无损重压缩（Pillow optimize + ImageMagick 多种 filter/strategy，取最小）

硬性约束：
  * **保持透明通道**：RGBA 降采样必须「预乘 alpha → 插值 → 去预乘」。
    否则透明区 RGB 会被邻域颜色污染（实测 75 种杂色），桌面上会出现彩边/黑边。
  * **无损部分逐位校验**：重压缩只接受 RGBA 逐位相同的结果；且仅当严格更小才覆盖。

为什么不装 pngquant/optipng：
  * 量化（pngquant）会改像素 —— 相框的**内孔/边框是运行时按 alpha 分析出来的**，
    改 alpha 会直接改变九宫格几何，违背“保持透明信息”。
  * 缩放已能把相框库从 ~50MB 降到 ~18MB（-65%），无量化工具也能达标。

用法：
  scripts/optimize-frames.py                 # 按默认参数优化 frame/ 下所有 PNG
  scripts/optimize-frames.py --maxdim 1400   # 换上限
  scripts/optimize-frames.py --no-scale      # 只做无损重压缩，不改尺寸
  scripts/optimize-frames.py --dry-run       # 只报告，不落盘

依赖：Pillow + ImageMagick（都不需要 sudo 安装）。退出码非 0 表示有文件未通过校验。
"""
import argparse
import os
import shutil
import subprocess
import sys
from concurrent.futures import ProcessPoolExecutor

import numpy as np
from PIL import Image

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FRAME_DIR = os.path.join(ROOT, "frame")
IM = "magick"

_CFG = {}


def _cfg():
    if not _CFG:
        a = _cfg.args
        _CFG.update(maxdim=a.maxdim, scale=not a.no_scale, dry=not a.write,
                    work=os.path.join(FRAME_DIR, ".pngopt_work"))
    return _CFG


# ---------------------------------------------------------------- 预乘缩放
def premul_resize(im: Image.Image, nw: int, nh: int) -> Image.Image:
    """RGBA 正确降采样：预乘 alpha → LANCZOS → 去预乘。

    直接对 RGBA 做 LANCZOS 是错的：Lanczos 核会看到透明像素里"残留"的 RGB，
    把不透明区的颜色带到透明边，形成彩边/黑边。预乘后透明像素的 RGB 归零，
    插值就不会互相污染。
    """
    arr = np.asarray(im.convert("RGBA")).astype(np.float64)
    a = arr[..., 3:4] / 255.0
    pm = arr[..., :3] * a                                   # 预乘
    pm_r = np.asarray(
        Image.fromarray(pm.astype(np.uint8)).resize((nw, nh), Image.LANCZOS)
    ).astype(np.float64)
    a_r = np.asarray(
        im.convert("RGBA").getchannel("A").resize((nw, nh), Image.LANCZOS)
    ).astype(np.float64) / 255.0
    safe = np.maximum(a_r, 1e-6)[..., None]
    un = np.where((a_r > 0)[..., None], pm_r / safe, 0.0)  # 去预乘
    out = np.dstack([np.clip(un, 0, 255), a_r * 255.0]).round().astype(np.uint8)
    return Image.fromarray(out, "RGBA")


def check_alpha(img: Image.Image):
    """返回 (透明区 RGB 种类数, 是否含 alpha=0, 是否含 alpha=255)。"""
    a = np.asarray(img.convert("RGBA"))
    alpha = a[..., 3]
    tr = a[alpha == 0][:, :3]
    uniq = len(np.unique(tr.reshape(-1, 3), axis=0)) if tr.size else 0
    return uniq, bool((alpha == 0).any()), bool((alpha == 255).any())


# ------------------------------------------------------------ 无损重压缩
def recompress(img: Image.Image, workdir: str, stem: str):
    """多种无损编码器各出一份，返回体积最小者。"""
    cands = []
    p = os.path.join(workdir, f"pil_{stem}.png")
    try:
        img.save(p, format="PNG", optimize=True, compress_level=9)
        cands.append(p)
    except Exception:
        pass
    for filt in range(6):          # png:compression-filter 0..5
        for strat in range(5):     # png:compression-strategy 0..4
            c = os.path.join(workdir, f"im{filt}_{strat}_{stem}.png")
            r = subprocess.run(
                [IM, p, "-strip",
                 "-define", "png:compression-level=9",
                 "-define", f"png:compression-strategy={strat}",
                 "-define", f"png:compression-filter={filt}",
                 f"PNG32:{c}"],
                capture_output=True)
            if r.returncode == 0 and os.path.exists(c):
                cands.append(c)
    return min(cands, key=os.path.getsize) if cands else None


# ---------------------------------------------------------------- 单文件
def process(fname: str):
    c = _cfg()
    src = os.path.join(FRAME_DIR, fname)
    orig = os.path.getsize(src)
    im = Image.open(src)
    W, H = im.size
    workdir = os.path.join(c["work"], os.path.splitext(fname)[0])
    os.makedirs(workdir, exist_ok=True)
    try:
        if c["scale"] and max(W, H) > c["maxdim"]:
            s = c["maxdim"] / max(W, H)
            out = premul_resize(im, max(1, round(W * s)), max(1, round(H * s)))
        else:
            out = None
        base = out if out is not None else im
        best = recompress(base, workdir, os.path.splitext(fname)[0])
        if best is None:
            return fname, orig, orig, 0.0, "重压缩失败", im.size
        new = os.path.getsize(best)
        # 只在严格更小时落盘
        if new < orig and not c["dry"]:
            shutil.copyfile(best, src)
        elif new >= orig:
            new = orig
        note = f"{W}x{H}→{base.size[0]}x{base.size[1]}" if out is not None else f"{W}x{H} 不缩放"
        if out is not None:
            u, h0, h255 = check_alpha(out)
            note += f" 透明区RGB={u}种 alpha0={h0} alpha255={h255}"
        if c["dry"]:
            note = "[dry-run] " + note
        return fname, orig, new, (1 - new / orig) * 100, note, base.size
    finally:
        shutil.rmtree(workdir, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser(description="相框库压缩（默认：最长边≤1000 + 预乘缩放 + 无损重压缩）")
    ap.add_argument("workers", nargs="?", type=int, default=max(1, (os.cpu_count() or 4) - 1))
    ap.add_argument("--maxdim", type=int, default=1000, help="最长边上限（默认 1000）")
    ap.add_argument("--no-scale", action="store_true", help="只做无损重压缩，不改尺寸")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不落盘")
    ap.add_argument("--write", action="store_true", help="实际写入（默认 dry-run 之外也需要此开关才落盘）")
    a = ap.parse_args()
    if a.dry_run:
        a.write = False
    _cfg.args = a

    files = sorted(f for f in os.listdir(FRAME_DIR) if f.lower().endswith(".png"))
    os.makedirs(_cfg()["work"], exist_ok=True)
    to = tn = 0
    rc = 0
    with ProcessPoolExecutor(max_workers=a.workers) as ex:
        for fname, orig, new, pct, note, size in ex.map(process, files):
            to += orig; tn += new
            if "透明区RGB=1种" in note and "alpha0=True alpha255=True" in note:
                pass
            elif "不缩放" not in note:
                print(f"⚠ {fname}: {note}")
                rc = 1
            print(f"{fname:<20} {orig:>10,}→{new:>9,} 省{pct:5.1f}%  {size}  {note}")
    shutil.rmtree(_cfg()["work"], ignore_errors=True)
    print(f"\n合计: {to:,} → {tn:,}  省 {100*(1-tn/to):.1f}%")
    return rc


if __name__ == "__main__":
    sys.exit(main())
