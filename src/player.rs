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
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub struct MediaPlayer {
    state: Rc<AppState>,
    lib: Rc<MediaLibrary>,
    images: Rc<ImageService>,
    slides: Rc<Slideshow>,
    /// PNG 相框（配置启用时）
    frame: RefCell<Option<Rc<crate::frame::FrameRenderer>>>,
    /// 视频播放器（无 GStreamer/无解码器时为 None → 跳过视频）
    video: RefCell<Option<Rc<crate::media::video::VideoPlayer>>>,
    /// 当前是否是视频（决定控制层按钮语义与轮换行为）
    current_is_video: Cell<bool>,
    /// 拖动起点快照：Move = (x, y)，Resize = (max_w, max_h, 宽高比)
    drag: RefCell<DragState>,
}

/// 拖动期间给 surface 加的"活动余量"（像素）
const DRAG_PAD: i32 = 320;

#[derive(Clone, Copy, Default)]
struct DragState {
    active: bool,
    mode_is_resize: bool,
    /// 拖动起点（屏幕坐标，Begin 时快照 —— 不能每次重读配置，否则会累加成 2 倍）
    start_x: f64,
    start_y: f64,
    max_w: f64,
    max_h: f64,
    aspect: f64,
    /// 累计位移（GTK 的 drag-delta 是"相对上一次"的增量，需要累加）
    acc_x: f64,
    acc_y: f64,
    /// 加余量导致的坐标跳变（第一次 Update 要扣掉）
    comp_x: f64,
    comp_y: f64,
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
            frame: RefCell::new(None),
            video: RefCell::new(None),
            current_is_video: Cell::new(false),
            drag: RefCell::new(DragState::default()),
        });

        // 视频播放器（GStreamer 不可用则降级为纯图片）
        {
            let (max_w, max_h, fps, muted) = {
                let cfg = player.state.config.borrow();
                (
                    cfg.display.max_width,
                    cfg.display.max_height,
                    cfg.video.max_fps,
                    cfg.video.muted,
                )
            };
            let connector = player.state.config.borrow().window.monitor.clone();
            let scale = crate::window::target_monitor(&connector)
                .as_ref()
                .map(monitor_scale)
                .unwrap_or(1.0);
            let on_event: Rc<dyn Fn(crate::media::video::VideoEvent)> = Rc::new({
                let p = player.clone();
                move |ev| p.on_video_event(ev)
            });
            let on_frame: Rc<dyn Fn(gdk::Texture, i32, i32)> = Rc::new({
                let p = player.clone();
                move |tex, w, h| p.on_video_frame(tex, w, h)
            });
            match crate::media::video::VideoPlayer::new(
                max_w, max_h, fps, muted, scale, on_event, on_frame,
            ) {
                Some(v) => *player.video.borrow_mut() = Some(v),
                None => crate::warn!("视频不可用，本次仅显示图片"),
            }
        }

        // PNG 相框
        player.load_frame();

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

            // 拖动：移动组件 / 右下角改大小（保持比例）
            let p = player.clone();
            window.view.set_drag_handler(move |phase| p.on_drag(phase));
        }
        Some(player)
    }

    pub fn library(&self) -> &Rc<MediaLibrary> {
        &self.lib
    }

    pub fn slideshow(&self) -> &Rc<Slideshow> {
        &self.slides
    }

    /// 拖动处理：移动位置 / 右下角改大小（始终保持当前媒体比例）
    pub fn on_drag(self: &Rc<Self>, phase: crate::controls::DragPhase) {
        use crate::controls::{DragMode, DragPhase};
        crate::debug!("拖动 {:?}", phase);
        let Some(window) = self.state.window() else {
            return;
        };
        match phase {
            DragPhase::Begin(mode, x, y) => {
                // 给 surface 加"活动余量"，指针才能拖到组件外面：
                //  - 移动：四边对称（内容用绘制偏移跟随，widget 几何不动）
                //  - 改大小：只加右/下（内容真实变大，原点固定）
                let resize = mode == DragMode::Resize;
                window.set_drag_padding_full(DRAG_PAD, !resize);
                let (cw, ch) = window.view.content_size();
                let cfg = self.state.config.borrow();
                let mut d = self.drag.borrow_mut();
                d.active = true;
                d.mode_is_resize = resize;
                let (sx, sy) = {
                    let cfg = self.state.config.borrow();
                    (cfg.window.x as f64, cfg.window.y as f64)
                };
                d.start_x = sx;
                d.start_y = sy;
                let _ = (x, y);
                d.max_w = cfg.display.max_width as f64;
                d.max_h = cfg.display.max_height as f64;
                d.aspect = if cw > 0 && ch > 0 {
                    cw as f64 / ch as f64
                } else {
                    1.0
                };
                d.acc_x = 0.0;
                d.acc_y = 0.0;
                // 余量只加在右/下，widget 坐标不跳变 → 无需补偿
                d.comp_x = 0.0;
                d.comp_y = 0.0;
            }
            DragPhase::Update(dx, dy) => {
                {
                    let mut d = self.drag.borrow_mut();
                    if !d.active {
                        return;
                    }
                    // 注意：GTK 的 drag-delta 是"相对拖动起点"的绝对位移，不是增量
                    d.acc_x = dx - d.comp_x;
                    d.acc_y = dy - d.comp_y;
                    d.comp_x = 0.0;
                    d.comp_y = 0.0;
                }
                let d = *self.drag.borrow();
                if d.mode_is_resize {
                    self.drag_resize(&d);
                } else {
                    // 移动：只改绘制偏移 + 内存中的位置，松手才真正应用
                    window.view.set_visual_offset(d.acc_x, d.acc_y);
                    let (cw, ch) = window.view.content_size();
                    let screen = window.screen_bounds(&self.state);
                    let (nx, ny) = crate::geometry::clamp_to_screen(
                        (d.start_x + d.acc_x).round() as i32,
                        (d.start_y + d.acc_y).round() as i32,
                        cw,
                        ch,
                        screen,
                    );
                    self.state.edit(|c| {
                        c.window.x = nx;
                        c.window.y = ny;
                    });
                }
            }
            DragPhase::End => {
                if self.drag.borrow().active {
                    self.drag.borrow_mut().active = false;
                    window.view.set_visual_offset(0.0, 0.0);
                    window.set_drag_padding_full(0, false);
                    // 重新贴回记录的位置（surface 重建后边距可能丢失）
                    let (px, py) = {
                        let cfg = self.state.config.borrow();
                        (cfg.window.x, cfg.window.y)
                    };
                    window.set_position(px, py);
                    window.sync_input_region();
                    self.state.commit();
                    self.refresh_visibility_rect();
                    if let Some(w) = self.state.window() {
                        w.update_hud(&self.state, "");
                    }
                }
            }
        }
    }

    /// 移动：左上角锚点固定，位置夹在屏幕内
    fn drag_move(self: &Rc<Self>, d: &DragState) {
        let screen = self
            .state
            .window()
            .map(|w| w.screen_bounds(&self.state))
            .unwrap_or(crate::geometry::Bounds {
                width: 1920,
                height: 1080,
            });
        let (cw, ch) = self
            .state
            .window()
            .map(|w| w.view.content_size())
            .unwrap_or((1, 1));
        let start = (d.start_x, d.start_y);
        let (nx, ny) = crate::geometry::clamp_to_screen(
            (start.0 + d.acc_x).round() as i32,
            (start.1 + d.acc_y).round() as i32,
            cw,
            ch,
            screen,
        );
        self.state.edit(|c| {
            c.window.x = nx;
            c.window.y = ny;
        });
        if let Some(w) = self.state.window() {
            w.set_position(nx, ny);
        }
    }

    /// 右下角改大小：按当前媒体比例换算，`fit()` 保证不超 max、不变形
    fn drag_resize(self: &Rc<Self>, d: &DragState) {
        let screen = self
            .state
            .window()
            .map(|w| w.screen_bounds(&self.state))
            .unwrap_or(crate::geometry::Bounds {
                width: 1920,
                height: 1080,
            });
        // 以水平方向为主，取两个方向中位移较大的那个
        // （正=放大，负=缩小；下限由 MIN_WIDTH/MIN_HEIGHT 兜住）
        let delta = d.acc_x.max(d.acc_y);
        let mut w = d.max_w + delta;
        let mut h = w / d.aspect.max(0.01);
        // 最小尺寸：宽高都要满足
        if h < crate::config::MIN_HEIGHT as f64 {
            h = crate::config::MIN_HEIGHT as f64;
            w = h * d.aspect;
        }
        if w < crate::config::MIN_WIDTH as f64 {
            w = crate::config::MIN_WIDTH as f64;
            h = w / d.aspect.max(0.01);
        }
        // 不超过屏幕（按组件在屏幕上的位置算剩余空间）
        let (px, py) = (d.start_x, d.start_y);
        let max_w = (screen.width as f64 - px).max(64.0);
        let max_h = (screen.height as f64 - py).max(64.0);
        if w > max_w {
            w = max_w;
            h = w / d.aspect.max(0.01);
        }
        if h > max_h {
            h = max_h;
            w = h * d.aspect;
        }
        let (nw, nh) = (
            w.round().clamp(crate::config::MIN_WIDTH as f64, crate::config::MAX_DIM as f64) as i32,
            h.round().clamp(crate::config::MIN_HEIGHT as f64, crate::config::MAX_DIM as f64) as i32,
        );
        // 写入 max 限制：媒体比例由 fit() 保证，实际尺寸 = fit(比例, nw, nh)
        self.state.edit(|c| {
            c.display.max_width = nw;
            c.display.max_height = nh;
            c.window.width = nw;
            c.window.height = nh;
        });
        if let Some(w2) = self.state.window() {
            w2.set_size(nw, nh);
        }
        self.refresh_frame();
    }

    /// 视频自检：不需要解码器即可验证管线
    pub fn selftest_video(self: &Rc<Self>) {
        let Some(v) = self.video.borrow().clone() else {
            crate::error!("自检失败：视频后端不可用");
            return;
        };
        self.current_is_video.set(true);
        v.load_test_source();
    }

    /// 启动：后台扫描 → 显示第一项 → 开始轮换
    pub fn start(self: &Rc<Self>) {
        // 自检模式：跳过媒体目录，直接用测试视频源（不需要任何解码器）
        if std::env::var_os("PHOTO_FRAME_SELFTEST_VIDEO").is_some() {
            self.selftest_video();
            return;
        }

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
        self.current_is_video.set(item.kind == MediaKind::Video);
        match item.kind {
            MediaKind::Image => {
                // 图片播放前先停掉视频，避免两路解码同时吃 CPU
                if let Some(v) = self.video.borrow().as_ref() {
                    v.stop();
                }
                self.show_image(&item)
            }
            MediaKind::Video => self.show_video(&item),
        }
        // 控制层按钮图标跟随状态
        if let Some(w) = self.state.window() {
            w.view.set_running(self.is_playing());
        }
    }

    fn show_video(self: &Rc<Self>, item: &MediaItem) {
        let Some(player) = self.video.borrow().clone() else {
            crate::debug!("无视频后端，跳过 {}", item.file_name());
            return;
        };
        let (max_w, max_h, autoplay) = {
            let cfg = self.state.config.borrow();
            (
                cfg.display.max_width,
                cfg.display.max_height,
                cfg.video.autoplay,
            )
        };
        player.set_box(max_w, max_h);
        // 先按配置尺寸占位，拿到视频真实尺寸后再校正（保持比例）
        if let Some(w) = self.state.window() {
            w.view.set_image(None, (max_w, max_h), &item.file_name());
            self.refresh_frame();
        }
        player.load(&item.path, autoplay);
    }

    /// 视频事件（主线程）
    fn on_video_event(self: &Rc<Self>, ev: crate::media::video::VideoEvent) {
        use crate::media::video::VideoEvent as E;
        match ev {
            E::Eos => {
                crate::debug!("视频播放结束");
                if let Some(w) = self.state.window() {
                    w.view.set_image(None, w.view.content_size(), "");
                }
                // complete 模式：播完切下一项；定时模式由轮换计时器负责
                if self.state.config.borrow().video.mode == "complete" {
                    self.slides.stop();
                    self.step(1);
                    if self.slides.running() {
                        self.slides.restart();
                    }
                }
            }
            E::Error(msg) => crate::warn!("视频错误：{msg}"),
            E::Playing(p) => {
                if let Some(w) = self.state.window() {
                    w.view.set_running(p);
                }
            }
        }
    }

    /// 视频帧（主线程）：只在尺寸变化时调整组件大小
    fn on_video_frame(self: &Rc<Self>, tex: gdk::Texture, w: i32, h: i32) {
        let Some(window) = self.state.window() else {
            return;
        };
        let connector = self.state.config.borrow().window.monitor.clone();
        let scale = crate::window::target_monitor(&connector)
            .as_ref()
            .map(monitor_scale)
            .unwrap_or(1.0);
        let logical = (
            ((w as f64 / scale).round() as i32).max(1),
            ((h as f64 / scale).round() as i32).max(1),
        );
        let cur = window.view.content_size();
        if cur != logical {
            window.view.set_content_size(logical.0, logical.1);
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
            self.refresh_frame();
            self.refresh_visibility_rect();
        }
        window.view.set_video_frame(Some(tex), logical);
    }

    /// 当前是否在播放（图片看轮换，视频看管线状态）
    pub fn is_playing(&self) -> bool {
        if self.current_is_video.get() {
            return self
                .video
                .borrow()
                .as_ref()
                .map(|v| v.is_playing())
                .unwrap_or(false);
        }
        self.slides.running()
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

    /// 读取配置的相框 PNG（失败只告警，不影响图片显示）
    pub fn load_frame(&self) {
        let (enabled, path, connector) = {
            let cfg = self.state.config.borrow();
            (cfg.frame.enabled, cfg.frame.path.clone(), cfg.window.monitor.clone())
        };
        let mut slot = self.frame.borrow_mut();
        *slot = None;
        if !enabled || path.trim().is_empty() {
            if let Some(w) = self.state.window() {
                w.view.set_frame_texture(None);
            }
            return;
        }
        let scale = crate::window::target_monitor(&connector)
            .as_ref()
            .map(crate::window::monitor_scale)
            .unwrap_or(1.0);
        match crate::frame::FrameRenderer::new(std::path::Path::new(&path), scale) {
            Some(r) => {
                *slot = Some(Rc::new(r));
                drop(slot);
                self.refresh_frame();
            }
            None => {
                crate::warn!("相框加载失败：{path}");
                if let Some(w) = self.state.window() {
                    w.view.set_frame_texture(None);
                }
            }
        }
    }

    /// 组件尺寸/位置变化后更新覆盖检测矩形
    pub fn refresh_visibility_rect(&self) {
        let monitor = self.state.visibility.borrow().clone();
        let Some(m) = monitor else { return };
        let (x, y) = {
            let cfg = self.state.config.borrow();
            (cfg.window.x, cfg.window.y)
        };
        let (w, h) = self
            .state
            .window()
            .map(|win| win.view.content_size())
            .unwrap_or((1, 1));
        m.set_rect(crate::hypr::Rect { x, y, w, h });
    }

    /// 组件尺寸变化后重新生成相框纹理
    pub fn refresh_frame(&self) {
        let renderer = self.frame.borrow().clone();
        if let Some(r) = renderer {
            if let Some(w) = self.state.window() {
                let (cw, ch) = w.view.content_size();
                w.view.set_frame_texture(r.texture_for(cw, ch));
            }
        }
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
        // 相框跟随组件尺寸
        self.refresh_frame();
        self.refresh_visibility_rect();

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
        if self.current_is_video.get() {
            if let Some(v) = self.video.borrow().as_ref() {
                v.set_playing(!v.is_playing());
            }
            return;
        }
        let paused = self.slides.toggle_paused();
        if let Some(w) = self.state.window() {
            w.view.set_running(!paused);
            w.update_hud(&self.state, if paused { "已暂停轮换" } else { "" });
        }
    }

    /// 被窗口覆盖 / 恢复：图片停轮换、视频暂停管线（省 CPU）
    pub fn set_active(self: &Rc<Self>, active: bool) {
        self.slides.set_active(active);
        if !active {
            if let Some(v) = self.video.borrow().as_ref() {
                if v.is_playing() {
                    v.set_playing(false);
                }
            }
        } else if self.current_is_video.get() {
            let autoplay = self.state.config.borrow().video.autoplay;
            if autoplay {
                if let Some(v) = self.video.borrow().as_ref() {
                    v.set_playing(true);
                }
            }
        }
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
