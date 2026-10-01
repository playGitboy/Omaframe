# Omaframe · Desktop Photo Frame

> **Keep your youth on your desktop.**
>
> Point it at a folder and your photos and videos start playing on their own —
> with 30+ built-in frames to mix and match and a smart engine that adapts each one
> to your media. **All your good memories, always in sight.**

**English** · [简体中文](readme_zh.md)

---

## What it does

| | |
|---|---|
| 📁 **A folder becomes an album** | Pick a directory and common images/videos are picked up automatically, subfolders included |
| 🔁 **Automatic slideshow** | Adjustable interval, shuffle, pause — manual paging restarts the timer |
| 🖼️ **30+ built-in frames** | Landscape / portrait / square styles. The engine **analyses each PNG's transparent hole** and rebuilds the frame around your media's aspect ratio — drop any PNG into the library and it just works |
| 🖱️ **Place it anywhere** | **Drag from anywhere** on the frame to move it; drag the **bottom-right corner** to resize — the media aspect ratio is always preserved |
| ⏯️ **One tap to switch** | Click the **left half** for the previous item, the **right half** for the next; play/pause appears on hover |
| 😴 **Pauses when covered** | As soon as another window covers the frame, slideshow and video decoding pause — CPU drops to zero |
| 🎛️ **Settings panel** | Click the **tray icon**; every change applies instantly |

**Supported formats:** JPG / PNG / WebP / GIF / BMP / TIFF / AVIF / **HEIC** (iPhone photos)
and MP4 / MOV / MKV / WebM / AVI / M4V and more — video goes through the system `ffmpeg`,
so **no extra codecs to install**.

---

## Quick start

### Install

**Arch / Omarchy** — grab the prebuilt package from
[Releases](https://github.com/playGitboy/Omaframe/releases) and install it:

```bash
# one-liner: fetch the latest release package and install it
curl -fsSL https://github.com/playGitboy/Omaframe/releases/latest/download/omaframe.pkg.tar.zst -o /tmp/omaframe.pkg.tar.zst \
  && sudo pacman -U /tmp/omaframe.pkg.tar.zst
```

To uninstall: `sudo pacman -Rns omaframe` (your settings in `~/.config/omarchy-omaframe/` are kept).

**From source (any distribution)** — the one-click script detects and installs
dependencies, builds, installs and self-checks:

```bash
git clone https://github.com/playGitboy/Omaframe && cd Omaframe
scripts/install.sh
```

Then search **"Desktop Photo Frame"** in your app menu to launch it.

### Usage

| Action | Result |
|---|---|
| Move the pointer onto the frame | Controls fade in (▶/⏸, resize handle) |
| Click left / right half of the image | Previous / next item |
| Drag from anywhere on the frame | Move it |
| Drag the bottom-right corner | Resize (aspect ratio preserved) |
| Click the tray icon | Open the settings panel (Esc or click outside to dismiss) |
| `omaframe settings` / `omaframe quit` | Open settings / quit the running instance |

### Out-of-the-box defaults

Media folder set to your **current wallpaper directory**, 350×350, a **5-second** slideshow,
muted video at 30 fps, frames **enabled by default** with the style chosen from your first
item's orientation, docked top-left with a 12 px margin.

---

## Why it stays out of your way

- **Native Rust + GTK4 + libadwaita** — no Electron, no Tauri
- `wlr-layer-shell` on the **bottom** layer: never takes focus, never joins tiling,
  never shows up in the window list. When covered, it pauses via **Hyprland IPC events**
  (zero polling, zero idle CPU)
- Background image decoding with on-the-fly scaling and a bounded cache
  (both item count and byte budget)
- Video decoded by the system `ffmpeg` — the same decoder your dynamic wallpaper uses
- Idle CPU ≈ 0%, so it never competes with real work

## Compatibility

Builds against GTK 4.10+; **non-Hyprland, missing layer-shell or missing ffmpeg all
degrade gracefully** instead of erroring out. It also runs on Debian / Arch / Ubuntu
outside of Omarchy.

## Documentation

- [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md) — requirements and "never do this again" checklist
- [`docs/KEY-FINDINGS.md`](docs/KEY-FINDINGS.md) — findings and pitfalls
- [`CHANGELOG.md`](CHANGELOG.md) — release history

## License

[MIT](LICENSE)
