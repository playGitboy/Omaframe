//! 图片加载：后台线程解码 + 主线程建纹理 + LRU 缓存 + 预取。
//!
//! 关键点（面向"不同硬件"）：
//! - 解码在**独立工作线程**上做，UI 永不阻塞；
//! - 用 `PixbufLoader::set_size` 让解码器**边解码边缩放**，7680x3215 的图
//!   不会先在内存里展开成 100MB 再缩小；
//! - 输出统一转成预乘 BGRA（cairo ARGB32），主线程只需一次纹理上传；
//! - 跨线程只传裸字节（`Vec<u8>` + 尺寸），不跨线程传 GObject，规避 Send 限制。

use gdk_pixbuf::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

pub struct LoadedImage {
    pub id: String,
    pub texture: gdk::Texture,
    /// 实际显示尺寸（已按 max_w/max_h 缩放，比例与原图一致）
    pub size: (i32, i32),
}

impl Clone for LoadedImage {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            texture: self.texture.clone(),
            size: self.size,
        }
    }
}

// 工作线程 → GTK 主线程的桥。
// glib 0.22 没有 `MainContext::channel`，用 `MainContext::invoke` +
// 主线程的 thread_local 引用服务对象（invoke 的闭包必须 Send，不能直接捕获 Rc）。
thread_local! {
    static SERVICE: RefCell<Option<std::rc::Weak<ImageService>>> = const { RefCell::new(None) };
}

enum Job {
    Decode {
        #[allow(dead_code)]
        id: String,
        path: PathBuf,
        box_w: i32,
        box_h: i32,
        max_px: i32,
    },
}

struct CacheEntry {
    texture: gdk::Texture,
    size: (i32, i32),
}

/// 纹理缓存（只在主线程访问）。双上限：条目数 + 字节预算，
/// 保证在 4GB 内存的小机器上也不会被缓存吃满。
struct Cache {
    map: HashMap<String, CacheEntry>,
    order: Vec<String>, // 由旧到新
    cap: usize,
    budget_bytes: usize,
    used_bytes: usize,
}

impl Cache {
    fn new(cap: usize, budget_mb: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: Vec::new(),
            cap: cap.max(1),
            budget_bytes: budget_mb.max(4) * 1024 * 1024,
            used_bytes: 0,
        }
    }

    fn get(&self, id: &str) -> Option<(gdk::Texture, (i32, i32))> {
        self.map.get(id).map(|e| (e.texture.clone(), e.size))
    }

    fn put(&mut self, id: String, texture: gdk::Texture, size: (i32, i32)) {
        if self.map.contains_key(&id) {
            return;
        }
        let bytes = (size.0.max(1) as usize) * (size.1.max(1) as usize) * 4;
        self.map.insert(id.clone(), CacheEntry { texture, size });
        self.order.push(id);
        self.used_bytes += bytes;
        while self.order.len() > self.cap || (self.used_bytes > self.budget_bytes && self.order.len() > 1)
        {
            let victim = self.order.remove(0);
            if let Some(e) = self.map.remove(&victim) {
                self.used_bytes = self
                    .used_bytes
                    .saturating_sub((e.size.0.max(1) as usize) * (e.size.1.max(1) as usize) * 4);
            }
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.used_bytes = 0;
    }
}

type LoadedCb = Box<dyn Fn(LoadedImage)>;

pub struct ImageService {
    tx: Option<Sender<Job>>,
    #[allow(dead_code)]
    ctx: glib::MainContext,
    cache: RefCell<Cache>,
    pending: RefCell<Vec<String>>,
    callbacks: RefCell<Vec<LoadedCb>>,
}

impl ImageService {
    pub fn new(cache_items: usize, cache_budget_mb: usize) -> std::rc::Rc<Self> {
        let (job_tx, job_rx): (Sender<Job>, Receiver<Job>) = channel();
        let ctx = glib::MainContext::default();

        // 后台解码线程：只处理裸数据，不碰 GTK
        {
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name("photo-frame-decode".into())
                .spawn(move || {
                    while let Ok(job) = job_rx.recv() {
                        let result = match job {
                            Job::Decode {
                                id: _,
                                path,
                                box_w,
                                box_h,
                                max_px,
                            } => decode_to_bgra(&path, box_w, box_h, max_px)
                                .map_err(|e| format!("{}: {e}", path.to_string_lossy())),
                        };
                        // 切回主线程处理（GObject 只能在主线程用）
                        ctx.invoke(move || {
                            SERVICE.with(|slot| {
                                if let Some(svc) = slot.borrow().as_ref().and_then(|w| w.upgrade())
                                {
                                    svc.handle(result);
                                }
                            });
                        });
                    }
                })
                .ok();
        }

        let service = std::rc::Rc::new(Self {
            tx: Some(job_tx),
            ctx,
            cache: RefCell::new(Cache::new(cache_items, cache_budget_mb)),
            pending: RefCell::new(Vec::new()),
            callbacks: RefCell::new(Vec::new()),
        });
        SERVICE.with(|slot| *slot.borrow_mut() = Some(std::rc::Rc::downgrade(&service)));
        service
    }

    fn handle(self: &std::rc::Rc<Self>, msg: Result<RawImage, String>) {
        let raw = match msg {
            Ok(r) => r,
            Err(e) => {
                crate::warn!("图片解码失败：{e}");
                return;
            }
        };
        self.pending.borrow_mut().retain(|id| *id != raw.id);

        let Some(texture) = texture_from_raw(&raw) else {
            crate::warn!("无法为 {} 创建纹理", raw.name);
            return;
        };
        self.cache
            .borrow_mut()
            .put(raw.id.clone(), texture.clone(), (raw.width, raw.height));
        crate::debug!("已解码 {} → {}x{}", raw.name, raw.width, raw.height);

        let loaded = LoadedImage {
            id: raw.id,
            texture,
            size: (raw.width, raw.height),
        };
        let cbs: Vec<LoadedCb> = self.callbacks.borrow_mut().drain(..).collect();
        for cb in cbs {
            cb(loaded.clone());
        }
    }

    /// 缓存命中立即返回；否则排队解码并返回 None。
    pub fn request(
        &self,
        id: &str,
        path: &Path,
        box_w: i32,
        box_h: i32,
        max_px: i32,
    ) -> Option<(gdk::Texture, (i32, i32))> {
        if let Some(hit) = self.cache.borrow().get(id) {
            return Some(hit);
        }
        if self.pending.borrow().iter().any(|p| p == id) {
            return None;
        }
        self.pending.borrow_mut().push(id.to_string());
        if let Some(tx) = &self.tx {
            let _ = tx.send(Job::Decode {
                id: id.to_string(),
                path: path.to_path_buf(),
                box_w: box_w.max(16),
                box_h: box_h.max(16),
                max_px,
            });
        }
        None
    }

    /// 注册"解码完成"回调（在主线程执行，触发一次后失效）
    pub fn on_loaded(&self, cb: LoadedCb) {
        self.callbacks.borrow_mut().push(cb);
    }

    #[allow(dead_code)]
    pub fn is_pending(&self, id: &str) -> bool {
        self.pending.borrow().iter().any(|p| p == id)
    }

    #[allow(dead_code)]
    pub fn set_cache_limits(&self, items: usize, budget_mb: usize) {
        *self.cache.borrow_mut() = Cache::new(items, budget_mb);
    }

    pub fn clear_cache(&self) {
        self.cache.borrow_mut().clear();
    }
}

/// 跨线程传递的裸图像数据（预乘 BGRA）
struct RawImage {
    id: String,
    name: String,
    width: i32,
    height: i32,
    rowstride: i32,
    data: Vec<u8>,
}

/// 该扩展名是否属于"gdk-pixbuf 常缺 loader"的格式
fn looks_undecodable(path: &Path, bytes: &[u8]) -> bool {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let by_ext = matches!(ext.as_str(), "heic" | "heif" | "hif" | "avif");
    // 也可以按魔数判断（ftyp 盒子）
    let ftyp = bytes.len() > 12 && &bytes[4..8] == b"ftyp";
    by_ext || ftyp
}

/// 用外部解码器把图片转成 PNG 字节（ffmpeg 优先，其次 ImageMagick）
fn external_to_png(path: &Path) -> Option<Vec<u8>> {
    use std::process::{Command, Stdio};
    // ffmpeg：-frames:v 1 取首帧，输出 PNG 到 stdout
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "pipe:1"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok();
    if let Some(o) = out {
        if o.status.success() && o.stdout.len() > 16 {
            return Some(o.stdout);
        }
    }
    // ImageMagick（系统装了 HEIC delegate）
    let out = Command::new("magick")
        .arg(path)
        .arg("png:-")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if out.status.success() && out.stdout.len() > 16 {
        Some(out.stdout)
    } else {
        None
    }
}

fn decode_to_bgra(
    path: &Path,
    box_w: i32,
    box_h: i32,
    max_px: i32,
) -> Result<RawImage, String> {
    // gdk-pixbuf 读不了（HEIC/HEIF 等）→ 先试系统解码器转成 PNG 再走原流程。
    // 依赖系统已有的 ffmpeg（视频/系统壁纸同款），失败再试 ImageMagick。
    let bytes = match std::fs::read(path) {
        Ok(b) if !looks_undecodable(path, &b) => b,
        Ok(b) => match external_to_png(path) {
            Some(png) => {
                crate::debug!("外部解码器转换成功：{}", path.display());
                png
            }
            None => {
                crate::warn!("图片解码失败（gdk-pixbuf 与外部解码器都不支持）：{}", path.display());
                b
            }
        },
        Err(e) => return Err(e.to_string()),
    };

    // 先便宜地探尺寸（只读文件头），自己算出**保持比例**的目标尺寸。
    // 注意：gdk_pixbuf_loader_set_size 会把图**拉伸**到给定尺寸，不会保持比例，
    // 所以必须由我们自己算 fit()，否则图片会变形。
    let natural = gdk_pixbuf::Pixbuf::file_info(path).map(|(_, w, h)| (w, h));
    let (tw, th) = match natural {
        Some((nw, nh)) if nw > 0 && nh > 0 => {
            // 先限到解码上限，再 fit 到目标盒
            let (cw, ch) = if max_px > 0 && (nw > max_px || nh > max_px) {
                let s = (max_px as f64 / nw.max(nh) as f64).min(1.0);
                (
                    ((nw as f64 * s).round() as i32).max(1),
                    ((nh as f64 * s).round() as i32).max(1),
                )
            } else {
                (nw, nh)
            };
            crate::geometry::fit(cw, ch, box_w, box_h)
        }
        // 探测失败：交给解码器原始尺寸，解码后再缩（内存峰値略高，但不会出错）
        _ => (0, 0),
    };

    let loader = gdk_pixbuf::PixbufLoader::new();
    if tw > 0 && th > 0 {
        loader.set_size(tw, th);
    }
    loader.write(&bytes).map_err(|e| e.to_string())?;
    loader.close().map_err(|e| e.to_string())?;
    let src = loader
        .pixbuf()
        .ok_or_else(|| "解码器未产出图像".to_string())?;

    let (mut w, mut h) = (src.width(), src.height());
    if w <= 0 || h <= 0 {
        return Err("尺寸非法".into());
    }

    // 探测失败或解码器未按尺寸缩放时的兜底缩放
    if (tw, th) != (0, 0) && (w, h) != (tw, th) {
        let (w2, h2) = crate::geometry::fit(w, h, tw, th);
        if (w2, h2) != (w, h) {
            w = w2;
            h = h2;
        }
    }
    if max_px > 0 && (w > max_px || h > max_px) {
        let s = (max_px as f64 / w.max(h) as f64).min(1.0);
        w = ((w as f64 * s).round() as i32).max(1);
        h = ((h as f64 * s).round() as i32).max(1);
    }

    let src = if (w, h) != (src.width(), src.height()) {
        src.scale_simple(w, h, gdk_pixbuf::InterpType::Bilinear)
            .ok_or_else(|| "缩放失败".to_string())?
    } else {
        src
    };

    // 统一为预乘 BGRA：cairo ARGB32 内存布局 == GDK_MEMORY_B8G8R8A8_PREMULTIPLIED
    let dst = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, w, h)
        .ok_or_else(|| "无法分配目标缓冲区".to_string())?;
    src.composite(
        &dst,
        0,
        0,
        w,
        h,
        0.0,
        0.0,
        1.0,
        1.0,
        gdk_pixbuf::InterpType::Bilinear,
        255, // overall_alpha（0-255；传 -1 会被 gdk-pixbuf 拒绝）
    );

    // pixels() 是 unsafe 的原始内存视图，这里立刻拷成 Vec（可跨线程）
    let data = unsafe { dst.pixels() }.to_vec();
    Ok(RawImage {
        id: path.to_string_lossy().into_owned(),
        name: path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        width: w,
        height: h,
        rowstride: dst.rowstride(),
        data,
    })
}

fn texture_from_raw(raw: &RawImage) -> Option<gdk::Texture> {
    let bytes = glib::Bytes::from_owned(raw.data.clone());
    let pixbuf = gdk_pixbuf::Pixbuf::from_bytes(
        &bytes,
        gdk_pixbuf::Colorspace::Rgb,
        true,
        8,
        raw.width,
        raw.height,
        raw.rowstride,
    );
    Some(gdk::Texture::for_pixbuf(&pixbuf))
}
