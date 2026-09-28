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
    /// 拖动刷新计时器（16ms）：让预览与手势事件速率解耦 → 流畅
    drag_timer: RefCell<Option<glib::SourceId>>,
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
    /// 拖动起点（控件坐标，来自 GestureDrag::start_point）
    origin_x: f64,
    origin_y: f64,
    max_w: f64,
    max_h: f64,
    aspect: f64,
    /// 相对按下点的位移（由"控件坐标差"得到，不依赖 GTK 的 drag-delta 语义）
    acc_x: f64,
    acc_y: f64,
    /// 是否已真正移动过（没移动过就不要动 surface，否则会出现"黑屏"）
    padded: bool,
    moved: bool,
    /// Begin 时缓存的屏幕尺寸（避免每帧问 GDK）
    screen_w: f64,
    screen_h: f64,
    /// 改大小的目标尺寸（End 时才真正应用）
    target_w: i32,
    target_h: i32,
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
            drag_timer: RefCell::new(None),
        });

        // 视频播放器（系统 ffmpeg 不可用则降级为纯图片）
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
                // 这里**不**加活动余量。
                // GTK 在"按下+松开"（没有移动）时也会发 begin/end，
                // 若此时就把 surface 膨胀 320px：相框会瞬间变成一大块透明
                // surface（用户看到的"放大黑屏"），还会把边距写坏导致位置
                // 跳到屏幕角落。真正移动后（第一个 Update）再加。
                let resize = mode == DragMode::Resize;
                let (cw, ch) = window.view.content_size();
                let cfg = self.state.config.borrow();
                let mut d = self.drag.borrow_mut();
                d.active = true;
                d.padded = false;
                d.moved = false;
                d.mode_is_resize = resize;
                let (sx, sy) = {
                    let cfg = self.state.config.borrow();
                    (cfg.window.x as f64, cfg.window.y as f64)
                };
                d.start_x = sx;
                d.start_y = sy;
                d.origin_x = x;
                d.origin_y = y;
                d.max_w = cfg.display.max_width as f64;
                d.max_h = cfg.display.max_height as f64;
                d.aspect = if cw > 0 && ch > 0 {
                    cw as f64 / ch as f64
                } else {
                    1.0
                };
                d.acc_x = 0.0;
                d.acc_y = 0.0;
                d.target_w = 0;
                d.target_h = 0;
                // 屏幕尺寸只在 Begin 时取一次
                let b = window.screen_bounds(&self.state);
                d.screen_w = b.width as f64;
                d.screen_h = b.height as f64;
                self.ensure_drag_timer();
            }
            DragPhase::Update(dx, dy) => {
                let _ = (dx, dy);
                // 手势事件只用来"唤醒"，实际计算由 drag_tick 统一做
                // （Update 立刻算一次保证低延迟，16ms 计时器负责补帧保证流畅）
                self.drag_tick();
            }
            DragPhase::End => {
                let (active, was_resize, moved) = {
                    let d = self.drag.borrow();
                    (d.active, d.mode_is_resize, d.moved)
                };
                if !active {
                    return;
                }
                let (target_w, target_h) = {
                    let mut d = self.drag.borrow_mut();
                    d.active = false;
                    d.padded = false;
                    d.moved = false;
                    (d.target_w, d.target_h)
                };
                if let Some(t) = self.drag_timer.borrow_mut().take() {
                    t.remove();
                }
                // 只是点了一下（没移动）→ 什么都不做，避免跳动与"黑屏"
                if !moved {
                    return;
                }
                if let Some(t) = self.drag_timer.borrow_mut().take() {
                    t.remove();
                }
                window.view.set_visual_offset(0.0, 0.0);
                if was_resize && target_w > 0 {
                    // 预览结束：把最终尺寸真正应用上去（surface 只在这里变一次）
                    window.view.set_preview_size(0, 0);
                    window.set_size(target_w, target_h);
                    self.state.edit(|c| {
                        c.display.max_width = target_w;
                        c.display.max_height = target_h;
                        c.window.width = target_w;
                        c.window.height = target_h;
                    });
                    self.refresh_frame();
                } else if !was_resize {
                    // 移动：把最终位置写进配置（拖动过程中只做绘制偏移，不写盘）
                    let d = *self.drag.borrow();
                    let (cw, ch) = window.view.content_size();
                    let screen = crate::geometry::Bounds {
                        width: d.screen_w as i32,
                        height: d.screen_h as i32,
                    };
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
                // 重新贴回记录的位置
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
    /// 拖动刷新：16ms 一次，与手势事件速率解耦（解决"缩放卡顿"）
    fn drag_tick(self: &Rc<Self>) {
        let Some(window) = self.state.window() else {
            return;
        };
        // 位移一律用"控件坐标差"：控件原点固定 → last - origin 永远等于真实屏幕位移
        let (lx, ly) = window.view.last_pointer();
        let d = {
            let mut d = self.drag.borrow_mut();
            if !d.active {
                return;
            }
            d.acc_x = lx - d.origin_x;
            d.acc_y = ly - d.origin_y;
            if d.acc_x.abs() > 1.0 || d.acc_y.abs() > 1.0 {
                d.moved = true;
            }
            *d
        };
        if d.mode_is_resize {
            self.drag_resize(&d);
        } else {
            // 移动：只改绘制偏移（不写配置、不动 surface）→ 跟手且零开销
            window.view.set_visual_offset(d.acc_x, d.acc_y);
        }
    }

    /// 拖动期间 16ms 刷新计时器（只在拖动时存在，抬手即移除 → 静止时零开销）
    fn ensure_drag_timer(self: &Rc<Self>) {
        if self.drag_timer.borrow().is_some() {
            return;
        }
        let me = self.clone();
        let id = glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
            if !me.drag.borrow().active {
                *me.drag_timer.borrow_mut() = None;
                glib::ControlFlow::Break
            } else {
                me.drag_tick();
                glib::ControlFlow::Continue
            }
        });
        *self.drag_timer.borrow_mut() = Some(id);
    }

    /// 改大小：只算目标尺寸 + 更新**预览绘制**，
    /// 真正的组件/窗口尺寸在松手时一次性应用（每帧重建 surface 是卡顿的主因）。
    fn drag_resize(self: &Rc<Self>, d: &DragState) {
        let Some(window) = self.state.window() else {
            return;
        };
        // 屏幕剩余空间用 Begin 时缓存的值（不每帧问 GDK）
        let avail_w = (d.screen_w - d.start_x).max(64.0);
        let avail_h = (d.screen_h - d.start_y).max(64.0);
        let (nw, nh) = crate::geometry::resize_target(
            d.max_w,
            d.aspect,
            d.acc_x,
            d.acc_y,
            avail_w,
            avail_h,
            crate::config::MIN_WIDTH as f64,
            crate::config::MIN_HEIGHT as f64,
        );
        self.drag.borrow_mut().target_w = nw;
        self.drag.borrow_mut().target_h = nh;
        // 预览被限制在现有控件内 → 拖动中 surface 尺寸恒定
        let (cw, ch) = window.view.content_size();
        window
            .view
            .set_preview_size(nw.min(cw).max(1), nh.min(ch).max(1));
    }

    /// 启动：后台扫描 → 显示第一项 → 开始轮换
    pub fn start(self: &Rc<Self>) {
        self.apply_media_scale();
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
        let (max_w, max_h, autoplay, media_scale) = {
            let cfg = self.state.config.borrow();
            (
                cfg.display.max_width,
                cfg.display.max_height,
                cfg.video.autoplay,
                cfg.display.media_scale,
            )
        };
        // 视频帧也按 96% 渲染（与图片一致），绘制时再居中
        player.set_box(
            (max_w as f64 * media_scale).round() as i32,
            (max_h as f64 * media_scale).round() as i32,
        );
        // 关键：**不要**在这里清空纹理或改尺寸。
        // 视频首帧要等 ffprobe + ffmpeg 启动（约 0.2~0.5s），期间如果先把纹理清掉，
        // surface 就变成"无内容"，合成器会把它画成黑块 → 看到一瞬间黑闪。
        // 保留上一项的画面，等第一帧到了再无缝替换。
        if let Some(w) = self.state.window() {
            w.update_hud(&self.state, &item.file_name());
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

    /// 视频帧（主线程）：只在尺寸变化时调整组件大小。
    /// 注意：ffmpeg 后端已经按**逻辑像素**缩放过（目标尺寸来自 max_width/max_height），
    /// 所以这里不要再除以屏幕缩放。
    fn on_video_frame(self: &Rc<Self>, tex: gdk::Texture, w: i32, h: i32) {
        let Some(window) = self.state.window() else {
            return;
        };
        // 组件按满盒 fit；视频帧在组件内按 media_scale 居中绘制
        let (mw, mh) = {
            let cfg = self.state.config.borrow();
            (cfg.display.max_width, cfg.display.max_height)
        };
        let logical = crate::geometry::fit(w.max(1), h.max(1), mw, mh);
        if !self.current_is_video.get() {
            return;
        }
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
        let (mw, mh, max_px, connector, media_scale) = {
            let cfg = self.state.config.borrow();
            (
                cfg.display.max_width,
                cfg.display.max_height,
                cfg.display.max_decode_px,
                cfg.window.monitor.clone(),
                cfg.display.media_scale,
            )
        };
        let scale = crate::window::target_monitor(&connector)
            .as_ref()
            .map(monitor_scale)
            .unwrap_or(1.0);
        // 与显示一致：媒体只占组件的 media_scale，所以也只需解码那么多像素
        let bw = (mw as f64 * scale * media_scale).round() as i32;
        let bh = (mh as f64 * scale * media_scale).round() as i32;
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
        // 组件尺寸 = 媒体按**满盒**的 fit（media_scale 只影响组件**内部**的绘制留边），
        // 所以这里用解码尺寸的比例重新 fit 一次满盒，与是否 96% 解码无关。
        let (mw, mh) = {
            let cfg = self.state.config.borrow();
            (cfg.display.max_width, cfg.display.max_height)
        };
        let logical = crate::geometry::fit(size.0.max(1), size.1.max(1), mw, mh);
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

    /// 把配置里的媒体内缩比例同步给绘制控件
    pub fn apply_media_scale(self: &Rc<Self>) {
        let s = self.state.config.borrow().display.media_scale;
        if let Some(w) = self.state.window() {
            w.view.set_media_scale(s);
        }
    }

    /// 设置变更后即时生效：尺寸上限、轮换参数、相框、视频参数
    pub fn apply_settings(self: &Rc<Self>) {
        let (slides_enabled, interval, random, fps, muted, max_w, max_h) = {
            let cfg = self.state.config.borrow();
            (
                cfg.slideshow.enabled,
                cfg.slideshow.interval,
                cfg.slideshow.random,
                cfg.video.max_fps,
                cfg.video.muted,
                cfg.display.max_width,
                cfg.display.max_height,
            )
        };
        self.slides.configure(slides_enabled, interval);
        if let Some(v) = self.video.borrow().as_ref() {
            v.set_box(max_w, max_h);
        }
        let _ = (random, fps, muted);
        self.apply_media_scale();
        // 重新按新的尺寸上限计算当前媒体的显示尺寸
        self.show_current();
        self.load_frame();
        self.refresh_visibility_rect();
        crate::info!("设置已应用：最大 {}x{}", max_w, max_h);
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
