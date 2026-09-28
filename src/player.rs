//! 播放编排：把「媒体库 + 图片加载器 + 轮换计时器 + 窗口」串起来。
//!
//! 这里只处理图片；视频（Step 5）与遮挡暂停（`set_active`）的接入口已预留。

use crate::app::AppState;
use crate::controls::HitZone;
use crate::media::image::ImageService;
use crate::media::library::MediaLibrary;
use crate::media::{create_source, MediaItem, MediaKind};
use crate::slideshow::Slideshow;
use crate::window::monitor_scale;
use gtk::prelude::*;
use std::rc::Rc;

pub struct MediaPlayer {
    state: Rc<AppState>,
    lib: Rc<MediaLibrary>,
    images: Rc<ImageService>,
    slides: Rc<Slideshow>,
}

impl MediaPlayer {
    pub fn new(state: Rc<AppState>) -> Option<Rc<Self>> {
        let (kind, path, recursive, cache_items, cache_budget, slides_cfg, video_autoplay) = {
            let cfg = state.config.borrow();
            (
                cfg.source.kind.clone(),
                cfg.source.path.clone(),
                cfg.source.recursive,
                cfg.display.cache_items,
                cfg.display.cache_budget_mb,
                (cfg.slideshow.enabled, cfg.slideshow.interval, cfg.slideshow.random),
                cfg.video.autoplay,
            )
        };
        let _ = video_autoplay;
        let source_cfg = crate::config::SourceConfig {
            kind,
            path,
            recursive,
        };
        let source = match create_source(&source_cfg) {
            Ok(s) => s,
            Err(e) => {
                crate::error!("媒体源不可用：{e}");
                return None;
            }
        };
        crate::info!("媒体源：{}（{}）", source.type_name(), source.describe());

        let lib = MediaLibrary::new(source);
        let images = ImageService::new(cache_items, cache_budget);
        let slides = Slideshow::new();
        slides.configure(slides_cfg.0, slides_cfg.1);

        let player = Rc::new(Self {
            state,
            lib,
            images,
            slides,
        });

        // 控制层点击 → 播放/切换
        if let Some(window) = player.state.window() {
            let p = player.clone();
            window.view.set_click_handler(Box::new(move |zone| match zone {
                HitZone::Prev => p.step(-1),
                HitZone::Next => p.step(1),
                HitZone::PlayPause => p.toggle_play(),
                HitZone::Resize => crate::debug!("resize handle（Step 9）"),
                HitZone::None => {}
            }));
            window.view.set_running(player.slides.running());
        }
        Some(player)
    }

    pub fn library(&self) -> &Rc<MediaLibrary> {
        &self.lib
    }

    pub fn slideshow(&self) -> &Rc<Slideshow> {
        &self.slides
    }

    /// 启动：后台扫描 → 显示第一项 → 开始轮换
    pub fn start(self: &Rc<Self>) {
        let this = self.clone();
        self.lib.on_scanned(Box::new(move || {
            this.after_scan();
        }));
        self.lib.scan();

        let this = self.clone();
        self.slides.set_tick(move || {
            this.tick();
        });
    }

    fn after_scan(self: &Rc<Self>) {
        let count = self.lib.count();
        let window = match self.state.window() {
            Some(w) => w,
            None => return,
        };
        if count == 0 {
            window.view.set_placeholder(true);
            window.update_hud(&self.state, "媒体目录里没有可用文件");
            return;
        }
        self.show_current();
        self.slides.restart();
    }

    /// 显示当前项
    pub fn show_current(self: &Rc<Self>) {
        let Some(item) = self.lib.current() else {
            return;
        };
        match item.kind {
            MediaKind::Image => self.show_image(&item),
            MediaKind::Video => {
                // Step 5 接入 GStreamer
                crate::debug!("暂跳过视频：{}", item.file_name());
            }
        }
    }

    /// 解码目标盒：逻辑尺寸 × 屏幕缩放（HiDPI 下更清晰），上限由配置兜底
    fn decode_box(&self) -> (i32, i32) {
        let (mw, mh, max_px, connector) = {
            let cfg = self.state.config.borrow();
            (
                cfg.display.max_width,
                cfg.display.max_height,
                cfg.display.max_decode_px,
                cfg.window.monitor.clone(),
            )
        };
        let scale = crate::window::target_monitor(&connector)
            .as_ref()
            .map(monitor_scale)
            .unwrap_or(1.0);
        let bw = (mw as f64 * scale).round() as i32;
        let bh = (mh as f64 * scale).round() as i32;
        let cap = max_px.min(bw.max(bh) * 2);
        (bw.min(cap), bh.min(cap))
    }

    fn show_image(self: &Rc<Self>, item: &MediaItem) {
        let (bw, bh) = self.decode_box();
        let max_px = self.state.config.borrow().display.max_decode_px;
        let hit = self
            .images
            .request(&item.id, &item.path, bw, bh, max_px);
        if let Some((tex, size)) = hit {
            let name = item.file_name();
            self.apply_image(tex, size, &name);
        } else {
            // 等待后台解码完成（保留当前画面，避免闪白）
            let want = item.id.clone();
            let this = self.clone();
            self.images.on_loaded(Box::new(move |img| {
                let still_current = this
                    .lib
                    .current()
                    .map(|c| c.id == want)
                    .unwrap_or(false);
                if still_current {
                    let name = this
                        .lib
                        .current()
                        .map(|c| c.file_name())
                        .unwrap_or_default();
                    this.apply_image(img.texture, img.size, &name);
                }
            }));
        }
        // 提前解码下一张，切换时基本无延迟
        self.prefetch();
    }

    fn apply_image(&self, tex: gdk::Texture, size: (i32, i32), caption: &str) {
        let Some(window) = self.state.window() else {
            return;
        };
        let connector = self.state.config.borrow().window.monitor.clone();
        let scale = crate::window::target_monitor(&connector)
            .as_ref()
            .map(monitor_scale)
            .unwrap_or(1.0);
        // 解码尺寸是设备像素，窗口要用逻辑像素
        let logical = (
            ((size.0 as f64 / scale).round() as i32).max(1),
            ((size.1 as f64 / scale).round() as i32).max(1),
        );
        window.view.set_image(Some(tex), logical, caption);

        // 尺寸变了才落盘
        let changed = {
            let cfg = self.state.config.borrow();
            cfg.window.width != logical.0 || cfg.window.height != logical.1
        };
        if changed {
            self.state.edit(|c| {
                c.window.width = logical.0;
                c.window.height = logical.1;
            });
            self.state.commit();
        }
        window.update_hud(&self.state, caption);
    }

    /// 预取下一张（同一时间只预取一张，避免内存与 CPU 抖动）
    fn prefetch(self: &Rc<Self>) {
        let random = self.state.config.borrow().slideshow.random;
        let Some(next) = self.lib.peek_next(random) else {
            return;
        };
        if next.kind != MediaKind::Image {
            return;
        }
        let (bw, bh) = self.decode_box();
        let max_px = self.state.config.borrow().display.max_decode_px;
        let _ = self
            .images
            .request(&next.id, &next.path, bw, bh, max_px);
    }

    /// 计时到点：下一项
    pub fn tick(self: &Rc<Self>) {
        let random = self.state.config.borrow().slideshow.random;
        if self.lib.advance(random).is_some() {
            self.show_current();
        }
    }

    /// 控制层：上一项 / 下一项
    pub fn step(self: &Rc<Self>, delta: i32) {
        if self.lib.count() == 0 {
            return;
        }
        let len = self.lib.count() as i32;
        let cur = self.lib.index() as i32;
        let next = ((cur + delta) % len + len) % len;
        self.lib.set_index(next as usize);
        self.show_current();
        // 手动切换后重新计时，避免刚点完就被切走
        if self.slides.running() {
            self.slides.restart();
        }
    }

    /// 控制层：播放/暂停（视频 → 播放暂停；图片 → 轮换暂停继续）
    pub fn toggle_play(self: &Rc<Self>) {
        let is_video = self
            .lib
            .current()
            .map(|i| i.kind == MediaKind::Video)
            .unwrap_or(false);
        if is_video {
            // Step 6 接入 GStreamer 播放控制
            crate::debug!("视频播放/暂停（Step 6）");
            return;
        }
        let paused = self.slides.toggle_paused();
        if let Some(w) = self.state.window() {
            w.view.set_running(!paused);
            w.update_hud(&self.state, if paused { "已暂停轮换" } else { "" });
        }
    }

    /// 被窗口覆盖 / 恢复
    pub fn set_active(self: &Rc<Self>, active: bool) {
        self.slides.set_active(active);
    }

    /// 重新扫描（设置里改了目录时调用）
    pub fn rescan(self: &Rc<Self>) {
        let this = self.clone();
        self.lib.on_scanned(Box::new(move || {
            this.after_scan();
        }));
        self.lib.scan();
    }
}
