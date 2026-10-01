//! 视频播放：调用系统 `ffmpeg` 解码 → BGRA 原始帧 → `GdkMemoryTexture`。
//!
//! 为什么用 ffmpeg 而不是 GStreamer：
//! - 本机 GStreamer 只装了 base 插件，**没有 h264/hevc/vp8 解码器**（也不该为了
//!   一个桌面组件去装一整套 codec 包）；
//! - 系统动态壁纸 `owe` 的后端用的就是 ffmpeg/mpv 那一套库 —— 直接复用系统
//!   已有能力，零新增依赖、零 root。
//!
//! 实现要点：
//! - `ffmpeg -re -i 文件 -an -vf scale=W:H,fps=N -pix_fmt bgra -f rawvideo pipe:1`，
//!   主循环之外的读线程按 `W*H*4` 字节一帧读出，回主线程建纹理（零拷贝 `glib::Bytes`）。
//! - 暂停/被遮挡 = **停止读取** → ffmpeg 很快写满管道缓冲并阻塞，CPU 直接归零，
//!   恢复后继续，无需信号或 IPC。
//! - 播完（EOF）= 进程退出 → 上报 Eos → 切下一项。
//! - 尺寸先用 `ffprobe` 探测真实宽高，按媒体比例算目标尺寸，保证不变形；
//!   没有 ffprobe 时退化为"等比缩放 + 补边"（会有黑边但不变形）。

use std::cell::{Cell, RefCell};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

pub enum VideoEvent {
    /// 播放结束（complete 模式：播完切下一项）
    Eos,
    Error(String),
    Playing(bool),
}

/// 跨线程只传裸数据
struct FrameData {
    data: Vec<u8>,
    w: i32,
    h: i32,
    stride: usize,
}

thread_local! {
    static FRAME_HANDLER: RefCell<Option<Box<dyn Fn(FrameData)>>> = const { RefCell::new(None) };
    static EVENT_HANDLER: RefCell<Option<Box<dyn Fn(VideoEvent)>>> = const { RefCell::new(None) };
    /// 播放器的强引用（只读线程回主线程时用，invoke 保证在主线程执行）
    static PLAYER: RefCell<Option<Rc<VideoPlayer>>> = const { RefCell::new(None) };
}

fn emit_event(ev: VideoEvent) {
    EVENT_HANDLER.with(|slot| {
        if let Some(cb) = slot.borrow().as_ref() {
            cb(ev);
        }
    });
}

pub struct VideoPlayer {
    child: RefCell<Option<Child>>,
    ctx: glib::MainContext,
    on_event: Rc<dyn Fn(VideoEvent)>,
    playing: Cell<bool>,
    path: RefCell<Option<PathBuf>>,
    /// 目标显示尺寸（已按视频比例算好）
    target: RefCell<(i32, i32)>,
    box_w: Cell<i32>,
    box_h: Cell<i32>,
    fps: Cell<i32>,
    /// 用户主动暂停（被遮挡暂停不改变它）
    user_paused: Cell<bool>,
    /// ffprobe 结果缓存（path → 显示尺寸）。
    /// ffprobe 是**子进程调用**，在主线程上跑会直接冻住 UI（4K 素材可达数百毫秒~秒级），
    /// 而且每次翻页到同一个视频都会重跑。缓存 + 后台探测两件事一起解决。
    probe_cache: RefCell<std::collections::HashMap<PathBuf, (i32, i32)>>,
    /// 读线程也要能读到：用原子量跨线程共享（thread_local 是每线程独立的，不能用）
    generation: Arc<AtomicU64>,
    playing_flag: Arc<AtomicBool>,
}

impl VideoPlayer {
    /// 能力探测：ffmpeg 可用才有视频功能
    pub fn is_supported() -> bool {
        has_binary("ffmpeg")
    }

    pub fn new(
        box_w: i32,
        box_h: i32,
        fps: i32,
        _muted: bool,
        _scale: f64,
        on_event: Rc<dyn Fn(VideoEvent)>,
        on_frame: Rc<dyn Fn(gdk::Texture, i32, i32)>,
    ) -> Option<Rc<Self>> {
        if !Self::is_supported() {
            return None;
        }
        let target = (box_w.max(16), box_h.max(16));
        FRAME_HANDLER.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |fd| {
                let bytes = glib::Bytes::from_owned(fd.data);
                let tex = gdk::MemoryTexture::new(
                    fd.w,
                    fd.h,
                    gdk::MemoryFormat::B8g8r8a8,
                    &bytes,
                    fd.stride,
                );
                on_frame(tex.into(), fd.w, fd.h);
            }));
        });
        let bridge = on_event.clone();
        EVENT_HANDLER.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |ev| bridge(ev)));
        });
        Some(Rc::new(Self {
            child: RefCell::new(None),
            ctx: glib::MainContext::default(),
            probe_cache: RefCell::new(std::collections::HashMap::new()),
            on_event,
            playing: Cell::new(false),
            path: RefCell::new(None),
            target: RefCell::new(target),
            box_w: Cell::new(box_w.max(16)),
            box_h: Cell::new(box_h.max(16)),
            fps: Cell::new(fps.clamp(1, 60)),
            user_paused: Cell::new(false),
            generation: Arc::new(AtomicU64::new(0)),
            playing_flag: Arc::new(AtomicBool::new(false)),
        }))
    }

    /// 目标显示尺寸（设备像素；调用方按上限盒 × 外扩 × 屏幕缩放折算）
    pub fn set_box(&self, w: i32, h: i32) {
        self.box_w.set(w.max(16));
        self.box_h.set(h.max(16));
    }

    pub fn is_playing(&self) -> bool {
        self.playing.get()
    }

    #[allow(dead_code)]
    pub fn current(&self) -> Option<PathBuf> {
        self.path.borrow().clone()
    }

    pub fn stop(&self) {
        // 让读线程下一轮就退出
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.playing_flag.store(false, Ordering::SeqCst);
        if let Some(mut c) = self.child.borrow_mut().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        *self.path.borrow_mut() = None;
        self.playing.set(false);
    }

    /// 播放/暂停
    pub fn set_playing(self: &Rc<Self>, playing: bool) {
        self.user_paused.set(!playing);
        self.playing.set(playing);
        self.playing_flag.store(playing, Ordering::SeqCst);
        if playing {
            self.spawn_reader();
        }
        (self.on_event)(VideoEvent::Playing(playing));
    }

    /// 载入一个视频（按 autoplay 决定是否立刻开始读帧）
    pub fn load(self: &Rc<Self>, path: &Path, autoplay: bool) {
        self.stop();
        *self.path.borrow_mut() = Some(path.to_path_buf());
        self.user_paused.set(!autoplay);
        self.playing.set(autoplay);
        if !autoplay {
            // 默认暂停：仍然把首帧取出来显示
            self.spawn_reader();
            self.playing.set(false);
        } else {
            self.spawn_reader();
        }
        crate::debug!("载入视频 {}", path.to_string_lossy());
    }

    /// 启动 ffmpeg + 读帧线程；暂停/被覆盖时不启动（= 零 CPU）
    fn spawn_reader(self: &Rc<Self>) {
        if self.child.borrow().is_some() {
            return;
        }
        let Some(path) = self.path.borrow().clone() else {
            return;
        };
        let (bw, bh) = (self.box_w.get(), self.box_h.get());
        // 先查缓存；没有就**后台**探测并稍后重启读线程，绝不阻塞主线程
        let probed = match self.probe_cache.borrow().get(&path).copied() {
            Some(v) => Some(v),
            None => {
                let this = self.clone();
                let p = path.clone();
                self.ctx.spawn_local(async move {
                    // 放到后台线程跑 ffprobe（子进程 + 解析都在那儿）
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::Builder::new()
                        .name("omaframe-ffprobe".into())
                        .spawn(move || {
                            let _ = tx.send(probe_size(&p));
                        })
                        .ok();
                    let res = async move {
                        // 每 20ms 轮询一次结果（探测通常几十 ms；不阻塞主循环）
                        for _ in 0..500 {
                            match rx.try_recv() {
                                Ok(v) => return v,
                                Err(std::sync::mpsc::TryRecvError::Empty) => {
                                    glib::timeout_future(std::time::Duration::from_millis(20)).await;
                                }
                                Err(_) => return None,
                            }
                        }
                        None
                    }
                    .await;
                    if let Some(v) = res {
                        this.probe_cache.borrow_mut().insert(path.clone(), v);
                    }
                    // 结果回来时如果**还是**这个视频且还没起读线程，就现在起
                    let still = this.path.borrow().as_deref() == Some(path.as_path());
                    if still && this.child.borrow().is_none() {
                        this.spawn_reader();
                    }
                });
                return;
            }
        };
        let (nw, nh) = match probed {
            Some((vw, vh)) if vw > 0 && vh > 0 => {
                // 已知真实比例（已按旋转矩阵修正）→ 精确缩放到目标尺寸（不变形）
                let (w, h) = crate::geometry::fit(vw, vh, bw, bh);
                // 取偶数：yuv420p 等格式对奇数尺寸敏感，偶数最稳
                ((w / 2) * 2, (h / 2) * 2)
            }
            _ => (bw, bh),
        };
        self.target.replace((nw, nh));
        let fps = self.fps.get();
        // 探测不到比例时用"等比缩放 + 补边"，保证不变形（可能有黑边）
        let vf = match probed {
            // lanczos：缩小画质明显更好（视频是小窗高频缩放）
            Some(_) => format!("scale={nw}:{nh}:flags=lanczos,fps={fps}"),
            None => format!(
                "scale={bw}:{bh}:force_original_aspect_ratio=decrease:flags=lanczos,\
                 pad={bw}:{bh}:(ow-iw)/2:(oh-ih)/2,fps={fps}"
            ),
        };

        let mut cmd = Command::new("ffmpeg");
        cmd.arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-nostdin")
            // 注意：**不要**显式传 -autorotate。
            // 自动旋转默认就是开启的（容器有旋转矩阵时 ffmpeg 会自己插转置滤镜），
            // 而某些 ffmpeg 构建把 -autorotate 当**输出**选项，写在 -i 前会让
            // ffmpeg 直接报错退出 → 一帧都读不到 → 视频被"跳过"。
            .arg("-re")
            .arg("-i")
            .arg(&path)
            .arg("-an")
            .arg("-sn")
            .arg("-dn")
            .args(["-vf", &vf])
            .args(["-pix_fmt", "bgra"])
            .args(["-f", "rawvideo"])
            .arg("pipe:1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                (self.on_event)(VideoEvent::Error(format!("ffmpeg 启动失败：{e}")));
                return;
            }
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            return;
        };
        *self.child.borrow_mut() = Some(child);
        self.playing.set(true);
        let gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.playing_flag.store(true, Ordering::SeqCst);
        crate::debug!("ffmpeg 启动：{}x{} @ {}fps", nw, nh, fps);

        let ctx = self.ctx.clone();
        // 读线程需要的状态用原子量共享（不能跨线程持有 Rc）
        let generation = self.generation.clone();
        let playing_flag = self.playing_flag.clone();
        PLAYER.with(|p| *p.borrow_mut() = Some(self.clone()));
        std::thread::Builder::new()
            .name("omaframe-video".into())
            .spawn(move || {
                // 读线程只做 I/O + 裸数据搬运；所有 GTK/状态操作切回主线程
                let mut reader = stdout;
                let frame_bytes = (nw as usize) * (nh as usize) * 4;
                let mut buf = vec![0u8; frame_bytes];
                let mut frames = 0u64;
                loop {
                    if generation.load(Ordering::SeqCst) != gen {
                        return; // 换了视频 / 被停止
                    }
                    if read_exact_or_eof(&mut reader, &mut buf) {
                        crate::debug!("视频播放结束（共 {} 帧）", frames);
                        let ctx = ctx.clone();
                        ctx.invoke(move || {
                            if generation.load(Ordering::SeqCst) == gen {
                                PLAYER.with(|p| {
                                    if let Some(p) = p.borrow().as_ref() {
                                        p.stop();
                                    }
                                });
                                emit_event(VideoEvent::Eos);
                            }
                        });
                        return;
                    }
                    if !playing_flag.load(Ordering::SeqCst) {
                        // 暂停 / 被覆盖：不读管道 → ffmpeg 很快写满并阻塞，CPU 归零
                        std::thread::sleep(std::time::Duration::from_millis(80));
                        continue;
                    }
                    frames += 1;
                    let data = buf.clone();
                    let ctx = ctx.clone();
                    let generation2 = generation.clone();
                    ctx.invoke(move || {
                        if generation2.load(Ordering::SeqCst) != gen {
                            return;
                        }
                        FRAME_HANDLER.with(|slot| {
                            if let Some(cb) = slot.borrow().as_ref() {
                                cb(FrameData {
                                    data,
                                    w: nw,
                                    h: nh,
                                    stride: nw as usize * 4,
                                });
                            }
                        });
                    });
                }
            })
            .ok();
    }

    /// 目标显示尺寸：按视频真实比例在 (box_w, box_h) 内取最大
    #[allow(dead_code)]
    fn target_size(&self, path: &Path) -> (i32, i32) {
        let (bw, bh) = (self.box_w.get(), self.box_h.get());
        match probe_size(path) {
            Some((vw, vh)) if vw > 0 && vh > 0 => {
                crate::geometry::fit(vw, vh, bw, bh)
            }
            // 探测失败：按素材盒直接给（ffmpeg 侧用等比缩放+补边保证不变形）
            _ => (bw, bh),
        }
    }
}

/// 读满一帧；返回 true 表示 EOF
fn read_exact_or_eof(reader: &mut impl Read, buf: &mut [u8]) -> bool {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => return true,
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return true,
        }
    }
    false
}

/// 用 ffprobe 探测视频的**显示尺寸**（快速，一次）。
///
/// 关键：手机（尤其 iPhone）拍的 MOV 会用容器的旋转矩阵表示朝向，
/// 例如流里是 1920x1080 但 `rotation=-90` → 实际应显示为 **1080x1920** 竖屏。
/// 只看 width/height 会得到横屏，缩放到横屏目标里 → 画面被压扁（用户报的比例错误）。
/// 所以这里必须把 rotation 取出来并在 90/270 时交换宽高。
fn probe_size(path: &Path) -> Option<(i32, i32)> {
    if !has_binary("ffprobe") {
        return None;
    }
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height:stream_side_data=rotation:stream_tags=rotate",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let st = json.get("streams")?.get(0)?;
    let w = st.get("width")?.as_i64()? as i32;
    let h = st.get("height")?.as_i64()? as i32;
    // 旋转信息可能来自 side_data（新）或 tags.rotate（旧）
    let rot = st
        .get("side_data_list")
        .and_then(|a| a.as_array())
        .and_then(|a| {
            a.iter()
                .find_map(|d| d.get("rotation").and_then(|r| r.as_i64()))
        })
        .or_else(|| {
            st.get("tags")
                .and_then(|t| t.get("rotate"))
                .and_then(|r| r.as_str())
                .and_then(|s| s.parse::<i64>().ok())
        })
        .unwrap_or(0);
    let rot = ((rot % 360) + 360) % 360;
    let swapped = rot == 90 || rot == 270;
    crate::debug!("视频探测：{w}x{h} rotation={rot} → 显示 {}x{}", if swapped { h } else { w }, if swapped { w } else { h });
    Some(if swapped { (h, w) } else { (w, h) })
}

fn has_binary(name: &str) -> bool {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(name);
            if p.is_file() {
                return true;
            }
        }
    }
    false
}
