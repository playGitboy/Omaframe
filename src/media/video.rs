//! 视频播放：GStreamer `playbin` + `appsink` → CPU 帧 → `GdkMemoryTexture`。
//!
//! 为什么不用 gtk4videosink：Arch 的 `gst-plugin-gtk` 1.28 只提供 GTK3 的
//! `gtksink`/`gtkglsink`，**没有 GTK4 视频 sink**。因此这里把帧解到内存，
//! 在 `snapshot` 里作为纹理绘制。
//!
//! 面向低配机器的措施：
//! - caps 限制输出分辨率（上限 = 组件尺寸 × 缩放）与帧率（`video.max_fps`）；
//! - 显式串联 `videoconvert → videoscale → capsfilter → appsink`，
//!   不依赖 playbin 自动插转换器（实测某些解码器会 not-negotiated）；
//! - 被窗口覆盖/用户暂停时把管线置为 `PAUSED`，CPU 立刻归零；
//! - 静音时用 `fakesink` 丢音频。

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub enum VideoEvent {
    /// 播放结束（complete 模式：播完切下一项）
    Eos,
    Error(String),
    /// 播放状态变化
    Playing(bool),
}

/// 视频帧（跨线程只传裸数据）
struct FrameData {
    data: Vec<u8>,
    w: i32,
    h: i32,
    stride: usize,
}

/// 主线程侧的处理器（GStreamer 回调必须 Send，故用 thread_local 桥接）
thread_local! {
    static FPS_COUNT: Cell<(u64, i64)> = const { Cell::new((0, 0)) };
    static FRAME_HANDLER: RefCell<Option<Box<dyn Fn(FrameData)>>> = const { RefCell::new(None) };
    static EVENT_HANDLER: RefCell<Option<Box<dyn Fn(VideoEvent)>>> = const { RefCell::new(None) };
}

/// 安全设置 GObject 属性：属性不存在或类型不符时**只告警不 panic**
/// （GStreamer 元素属性随版本变化，直接 set_property 崩掉整个桌面组件不可接受）
pub fn set_prop_safe(obj: &impl IsA<gst::glib::Object>, name: &str, value: &gst::glib::Value) {
    let obj = obj.as_ref();
    match obj.find_property(name) {
        None => crate::warn!("属性 {name} 不存在，跳过"),
        Some(p) => match value.type_() {
            t if t == p.value_type() => obj.set_property(name, value),
            t => crate::warn!(
                "属性 {name} 类型不符（期望 {:?}，实际 {:?}），跳过",
                p.value_type(),
                t
            ),
        },
    }
}

fn set_prop<T>(obj: &impl IsA<gst::glib::Object>, name: &str, value: T)
where
    gst::glib::Value: for<'a> From<&'a T>,
{
    set_prop_safe(obj, name, &gst::glib::Value::from(&value));
}

/// 包一层 panic 隔离：GStreamer 回调里 panic 会直接 abort
fn guarded<T>(what: &str, f: impl FnOnce() -> T) -> Option<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(v) => Some(v),
        Err(_) => {
            crate::error!("{what} 内部 panic 已隔离");
            None
        }
    }
}

fn emit_event(ev: VideoEvent) {
    EVENT_HANDLER.with(|slot| {
        if let Some(cb) = slot.borrow().as_ref() {
            cb(ev);
        }
    });
}

pub struct VideoPlayer {
    pipeline: RefCell<Option<gst::Element>>,
    bus_watch: RefCell<Option<gst::bus::BusWatchGuard>>,
    ctx: glib::MainContext,
    on_event: Rc<dyn Fn(VideoEvent)>,
    playing: Cell<bool>,
    path: RefCell<Option<PathBuf>>,
    box_w: Cell<i32>,
    box_h: Cell<i32>,
    fps: Cell<i32>,
    muted: Cell<bool>,
    scale: Cell<f64>,
}

impl VideoPlayer {
    /// 能力探测：缺关键元件时返回 None（调用方降级为纯图片）
    pub fn is_supported() -> bool {
        if gst::init().is_err() {
            crate::warn!("GStreamer 初始化失败");
            return false;
        }
        for e in ["playbin", "appsink", "videoconvert", "videoscale", "capsfilter"] {
            if gst::ElementFactory::find(e).is_none() {
                crate::warn!("GStreamer 缺少元件 {e}，视频功能不可用");
                return false;
            }
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        box_w: i32,
        box_h: i32,
        fps: i32,
        muted: bool,
        scale: f64,
        on_event: Rc<dyn Fn(VideoEvent)>,
        on_frame: Rc<dyn Fn(gdk::Texture, i32, i32)>,
    ) -> Option<Rc<Self>> {
        if !Self::is_supported() {
            return None;
        }
        let on_frame_reg = on_frame.clone();
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
                on_frame_reg(tex.into(), fd.w, fd.h);
            }));
        });
        let bridge = on_event.clone();
        EVENT_HANDLER.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |ev| bridge(ev)));
        });
        Some(Rc::new(Self {
            pipeline: RefCell::new(None),
            bus_watch: RefCell::new(None),
            ctx: glib::MainContext::default(),
            on_event,
            playing: Cell::new(false),
            path: RefCell::new(None),
            box_w: Cell::new(box_w.max(16)),
            box_h: Cell::new(box_h.max(16)),
            fps: Cell::new(fps.clamp(1, 60)),
            muted: Cell::new(muted),
            scale: Cell::new(scale.clamp(1.0, 2.0)),
        }))
    }

    pub fn set_box(&self, w: i32, h: i32) {
        self.box_w.set(w.max(16));
        self.box_h.set(h.max(16));
    }

    pub fn set_muted(self: &Rc<Self>, muted: bool) {
        if self.muted.replace(muted) == muted {
            return;
        }
        // 音频 sink 需要重建才生效
        if let Some(path) = self.path.borrow().clone() {
            self.load(&path, self.playing.get());
        }
    }

    pub fn is_playing(&self) -> bool {
        self.playing.get()
    }

    pub fn current(&self) -> Option<PathBuf> {
        self.path.borrow().clone()
    }

    pub fn stop(&self) {
        if let Some(p) = self.pipeline.borrow().as_ref() {
            let _ = p.set_state(gst::State::Null);
        }
        self.pipeline.borrow_mut().take();
        self.bus_watch.borrow_mut().take(); // 丢弃 guard 即移除 watch
        self.path.borrow_mut().take();
        self.playing.set(false);
    }

    pub fn set_playing(&self, playing: bool) {
        let Some(p) = self.pipeline.borrow().clone() else {
            return;
        };
        let target = if playing {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        if p.set_state(target).is_err() {
            crate::warn!("切换播放状态失败");
        }
        self.playing.set(playing);
        (self.on_event)(VideoEvent::Playing(playing));
    }

    /// 载入并（按 autoplay）播放一个视频文件
    pub fn load(self: &Rc<Self>, path: &Path, autoplay: bool) {
        self.stop();
        *self.path.borrow_mut() = Some(path.to_path_buf());

        let Some(uri) = to_file_uri(path) else {
            (self.on_event)(VideoEvent::Error(format!("路径无法转 URI：{path:?}")));
            return;
        };
        let Ok(playbin) = gst::ElementFactory::make("playbin").build() else {
            (self.on_event)(VideoEvent::Error("无法创建 playbin".into()));
            return;
        };
        set_prop(&playbin, "uri", &uri);

        let Some(chain) = self.build_chain() else {
            return;
        };
        let Some(sink) = self.chain_to_bin(chain) else {
            return;
        };
        set_prop(&playbin, "video-sink", &sink);
        if self.muted.get() {
            if let Ok(fake) = gst::ElementFactory::make("fakesink").build() {
                set_prop(&fake, "sync", true);
                set_prop(&playbin, "audio-sink", &fake);
            }
        }
        self.attach_bus_watch(&playbin);
        *self.pipeline.borrow_mut() = Some(playbin);
        self.apply_state(autoplay);
        crate::debug!("载入视频 {}", path.to_string_lossy());
    }

    /// 自检：用 `videotestsrc` 代替文件，验证整条管线（不需要任何解码器）
    /// 启用方式：`PHOTO_FRAME_SELFTEST_VIDEO=1 photo-frame`
    pub fn load_test_source(self: &Rc<Self>) {
        let Ok(src) = gst::ElementFactory::make("videotestsrc").build() else {
            crate::warn!("自检失败：缺少 videotestsrc");
            return;
        };
        set_prop(&src, "is-live", true);
        // 用较大分辨率贴近真实场景（caps 上限会等比缩到组件大小）
        set_prop(
            &src,
            "caps",
            gst_video::VideoCapsBuilder::new()
                .format(gst_video::VideoFormat::Bgra)
                .width(1280)
                .height(720)
                .build(),
        );
        let Some((convert, vscale, capsfilter, appsink)) = self.build_chain() else {
            return;
        };

        // 用 Pipeline（自带 clock，sync=true 才会按帧率节流；普通 Bin 不会）
        let bin = gst::Pipeline::new();
        let src_el = src.upcast_ref::<gst::Element>();
        let app_el = appsink.upcast_ref::<gst::Element>();
        let _ = bin.add_many([src_el, &convert, &vscale, &capsfilter, app_el]);
        if gst::Element::link_many([src_el, &convert, &vscale, &capsfilter, app_el]).is_err() {
            crate::warn!("自检失败：无法连接测试源");
            return;
        }
        if let Some(pad) = appsink.static_pad("sink") {
            if let Ok(ghost) = gst::GhostPad::with_target(&pad) {
                let _ = bin.add_pad(&ghost);
            }
        }
        let bin_el = bin.upcast::<gst::Element>();
        self.attach_bus_watch(&bin_el);
        *self.pipeline.borrow_mut() = Some(bin_el);
        self.apply_state(true);
        crate::info!("视频自检模式：videotestsrc → BGRA → 纹理");
    }

    fn apply_state(&self, playing: bool) {
        let Some(p) = self.pipeline.borrow().clone() else {
            return;
        };
        let target = if playing {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        let _ = p.set_state(target);
        self.playing.set(playing);
        (self.on_event)(VideoEvent::Playing(playing));
    }

    /// 总线：EOS / ERROR
    fn attach_bus_watch(self: &Rc<Self>, element: &gst::Element) {
        let Some(bus) = element.bus() else {
            return;
        };
        let ctx = self.ctx.clone();
        match bus.add_watch(move |_, msg| {
            let ev = match msg.view() {
                gst::MessageView::Eos(_) => Some(VideoEvent::Eos),
                gst::MessageView::Error(err) => {
                    let text = err.error().to_string();
                    crate::warn!("视频错误：{text}");
                    Some(VideoEvent::Error(text))
                }
                _ => None,
            };
            if let Some(ev) = ev {
                // 总线回调可能在任意线程，统一切回主线程
                ctx.invoke(move || emit_event(ev));
            }
            glib::ControlFlow::Continue
        }) {
            Ok(watch) => *self.bus_watch.borrow_mut() = Some(watch),
            Err(e) => crate::warn!("无法监听视频总线：{e}"),
        }
    }

    /// 创建转换链元件（videoconvert → videoscale → capsfilter → appsink）
    fn build_chain(self: &Rc<Self>) -> Option<(gst::Element, gst::Element, gst::Element, gst_app::AppSink)> {
        let scale = self.scale.get();
        let caps = gst_video::VideoCapsBuilder::new()
            .format(gst_video::VideoFormat::Bgra)
            .framerate_range(
                gst::Fraction::new(0, 1)..gst::Fraction::new(self.fps.get() as i32, 1),
            )
            // 只给上限：GStreamer 会保持比例缩放
            .width_range(1..(self.box_w.get() as f64 * scale) as i32)
            .height_range(1..(self.box_h.get() as f64 * scale) as i32)
            .build();

        let appsink = gst_app::AppSink::builder()
            .max_buffers(2)
            .drop(true) // 处理不过来就丢帧，不堆积
            .sync(true)
            .build();

        let ctx = self.ctx.clone();
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let Some(r) = guarded("视频帧处理", || process_sample(sink, &ctx)) else {
                        return Err(gst::FlowError::Error);
                    };
                    r
                })
                .build(),
        );

        let make = |name: &str| gst::ElementFactory::make(name).build().ok();
        let (Some(convert), Some(vscale), Some(capsfilter)) = (
            make("videoconvert"),
            make("videoscale"),
            make("capsfilter"),
        ) else {
            (self.on_event)(VideoEvent::Error("缺少视频转换元件".into()));
            return None;
        };
        let make = |name: &str| gst::ElementFactory::make(name).build().ok();
        let (Some(convert), Some(vscale), Some(capsfilter)) = (
            make("videoconvert"),
            make("videoscale"),
            make("capsfilter"),
        ) else {
            (self.on_event)(VideoEvent::Error("缺少视频转换元件".into()));
            return None;
        };
        set_prop(&capsfilter, "caps", &caps);

        Some((
            convert.upcast::<gst::Element>(),
            vscale.upcast::<gst::Element>(),
            capsfilter.upcast::<gst::Element>(),
            appsink,
        ))
    }

    /// 把转换链装进 bin，并挂 ghost sink pad
    fn chain_to_bin(
        &self,
        chain: (gst::Element, gst::Element, gst::Element, gst_app::AppSink),
    ) -> Option<gst::Bin> {
        let (convert, vscale, capsfilter, appsink) = chain;
        let bin = gst::Bin::new();
        let app_el = appsink.upcast_ref::<gst::Element>();
        let _ = bin.add_many([&convert, &vscale, &capsfilter, app_el]);
        if gst::Element::link_many([&convert, &vscale, &capsfilter, app_el]).is_err() {
            (self.on_event)(VideoEvent::Error("视频转换链连接失败".into()));
            return None;
        }
        if let Some(sink_pad) = appsink.static_pad("sink") {
            if let Ok(ghost) = gst::GhostPad::with_target(&sink_pad) {
                let _ = bin.add_pad(&ghost);
            }
        }
        Some(bin)
    }
}

/// `file://` URI（路径做百分号编码，兼容中文与空格）
fn to_file_uri(path: &Path) -> Option<String> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let s = abs.to_str()?;
    let mut out = String::from("file://");
    for ch in s.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' | '/' | ':' => out.push(ch),
            _ => {
                let mut buf = [0u8; 4];
                for b in ch.encode_utf8(&mut buf).as_bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
        }
    }
    Some(out)
}

/// 取一帧 → 拷贝到裸数据 → 切回主线程建纹理（带帧率统计）
fn process_sample(
    sink: &gst_app::AppSink,
    ctx: &glib::MainContext,
) -> Result<gst::FlowSuccess, gst::FlowError> {
    let Ok(sample) = sink.pull_sample() else {
        return Err(gst::FlowError::Eos);
    };
    let Some(caps) = sample.caps() else {
        return Err(gst::FlowError::Error);
    };
    let Ok(info) = gst_video::VideoInfo::from_caps(caps) else {
        return Err(gst::FlowError::Error);
    };
    let Some(buf) = sample.buffer() else {
        return Err(gst::FlowError::Error);
    };
    let Ok(map) = buf.map_readable() else {
        return Err(gst::FlowError::Error);
    };
    let w = info.width() as i32;
    let h = info.height() as i32;
    let stride = info.stride()[0] as usize;
    {
        // 统计实际帧率（诊断用）
        let now = glib::monotonic_time();
        FPS_COUNT.with(|c| {
            let (n, t0) = c.get();
            if now - t0 >= 2_000_000 {
                crate::info!(
                    "视频帧率 ≈ {:.1} fps（{w}x{h}）",
                    (n as f64) / ((now - t0) as f64 / 1e6)
                );
                c.set((0, now));
            } else {
                c.set((n + 1, t0));
            }
        });
    }
    // 拷一份再回主线程（GdkTexture 只能在主线程创建）
    let data = map.as_slice().to_vec();
    let ctx = ctx.clone();
    ctx.invoke(move || {
        FRAME_HANDLER.with(|slot| {
            if let Some(cb) = slot.borrow().as_ref() {
                cb(FrameData { data, w, h, stride });
            }
        });
    });
    Ok(gst::FlowSuccess::Ok)
}
