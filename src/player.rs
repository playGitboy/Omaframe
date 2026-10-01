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
use std::cell::{Cell, RefCell};
use std::rc::Rc;

thread_local! {
    /// 相框库索引指纹（见 MediaPlayer::refresh_frame_index）
    static FRAME_INDEX_SIG: RefCell<String> = const { RefCell::new(String::new()) };
}

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

#[derive(Clone, Copy, Default)]
struct DragState {
    active: bool,
    mode_is_resize: bool,
    /// 是否真的移动过
    moved: bool,
    /// 拖动起点：相框左上角（屏幕坐标）
    start_x: f64,
    start_y: f64,
    /// 拖动起点：媒体尺寸与比例
    media_w: f64,
    media_h: f64,
    aspect: f64,
    /// GTK 的 drag-delta（相对按下点、控件坐标；控件整屏不动 → 精确等于屏幕位移）
    acc_x: f64,
    acc_y: f64,
    /// 屏幕尺寸（Begin 缓存）
    screen_w: f64,
    screen_h: f64,
    /// 移动过程中的当前位置（End 落盘）
    cur_x: f64,
    cur_y: f64,
    /// 改大小的目标**媒体**尺寸（End 落盘为上限）
    target_media_w: i32,
    target_media_h: i32,
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

    #[allow(dead_code)]
    pub fn library(&self) -> &Rc<MediaLibrary> {
        &self.lib
    }

    #[allow(dead_code)]
    pub fn slideshow(&self) -> &Rc<Slideshow> {
        &self.slides
    }

    /// 拖动处理：移动位置 / 右下角改大小（始终保持当前媒体比例）
    pub fn on_drag(self: &Rc<Self>, phase: crate::controls::DragPhase) {
        use crate::controls::{DragMode, DragPhase};
        let Some(window) = self.state.window() else {
            return;
        };
        match phase {
            DragPhase::Begin(mode, _x, _y) => {
                let resize = mode == DragMode::Resize;
                window.view.set_dragging(true);
                window.sync_input_region();
                let (sx, sy) = window.view.frame_pos();
                let (mw, mh) = window.view.media_size();
                let bounds = window.screen_bounds(&self.state);
                let mut d = self.drag.borrow_mut();
                d.active = true;
                d.mode_is_resize = resize;
                d.moved = false;
                d.start_x = sx as f64;
                d.start_y = sy as f64;
                d.media_w = mw.max(1) as f64;
                d.media_h = mh.max(1) as f64;
                d.aspect = (mw.max(1) as f64) / (mh.max(1) as f64);
                d.acc_x = 0.0;
                d.acc_y = 0.0;
                d.cur_x = sx as f64;
                d.cur_y = sy as f64;
                d.target_media_w = 0;
                d.target_media_h = 0;
                d.screen_w = bounds.width as f64;
                d.screen_h = bounds.height as f64;
            }
            DragPhase::Update(dx, dy) => {
                // 事件驱动、立即应用：不轮询、不延时 → 不会"慢半拍/少走一段"。
                // 控件（整屏）全程不动，所以 GTK 的 delta 就是精确的屏幕位移。
                let d = {
                    let mut d = self.drag.borrow_mut();
                    if !d.active {
                        return;
                    }
                    d.acc_x = dx;
                    d.acc_y = dy;
                    if dx.abs() > 0.5 || dy.abs() > 0.5 {
                        d.moved = true;
                    }
                    *d
                };
                if d.mode_is_resize {
                    self.drag_resize(&d);
                } else {
                    self.drag_move(&d);
                }
            }
            DragPhase::End => {
                let d = {
                    let mut d = self.drag.borrow_mut();
                    if !d.active {
                        return;
                    }
                    d.active = false;
                    *d
                };
                window.view.set_dragging(false);
                if !d.moved {
                    window.sync_input_region();
                    return;
                }
                if d.mode_is_resize {
                    // 预览期间 target_media_w/h 存的是**上限盒**尺寸，直接落盘
                    let (box_w, box_h) = (d.target_media_w, d.target_media_h);
                    if box_w > 0 && box_h > 0 {
                        window.view.set_preview_box(0, 0);
                        window.set_box(box_w, box_h);
                        self.state.edit(|c| {
                            c.display.max_width = box_w;
                            c.display.max_height = box_h;
                            c.window.width = box_w;
                            c.window.height = box_h;
                        });
                        self.refresh_frame();
                    }
                } else {
                    let (nx, ny) = (d.cur_x.round() as i32, d.cur_y.round() as i32);
                    window.view.set_visual_offset(0.0, 0.0);
                    window.set_position(nx, ny);
                    self.state.edit(|c| {
                        c.window.x = nx;
                        c.window.y = ny;
                        c.window.placed = true;
                    });
                }
                window.sync_input_region();
                self.state.commit();
                self.refresh_visibility_rect();
                if let Some(w) = self.state.window() {
                    w.update_hud(&self.state, "");
                }
            }
        }
    }

    /// 移动：只改绘制偏移（控件不动 → 事件不断、零合成器往返）
    fn drag_move(self: &Rc<Self>, d: &DragState) {
        let Some(window) = self.state.window() else {
            return;
        };
        let (fw, fh) = window.view.frame_size();
        let screen = crate::geometry::Bounds {
            width: d.screen_w as i32,
            height: d.screen_h as i32,
        };
        let (nx, ny) = crate::geometry::clamp_to_screen(
            (d.start_x + d.acc_x).round() as i32,
            (d.start_y + d.acc_y).round() as i32,
            fw,
            fh,
            screen,
        );
        {
            let mut st = self.drag.borrow_mut();
            st.cur_x = nx as f64;
            st.cur_y = ny as f64;
        }
        window
            .view
            .set_visual_offset(nx as f64 - d.start_x, ny as f64 - d.start_y);
    }

    /// 改大小：只更新预览（不碰 surface），松手才写上限
    fn drag_resize(self: &Rc<Self>, d: &DragState) {
        let delta = if d.acc_x.abs() >= d.acc_y.abs() {
            d.acc_x
        } else {
            d.acc_y
        };
        // 拖动改的是**上限盒**：媒体尺寸由"内孔几何"从盒推导出来。
        // 为了手感不变（鼠标走多少、照片就变多少），把位移换算到盒宽上。
        let (cur_box_w, cur_box_h) = self
            .state
            .window()
            .map(|w| w.view.box_size())
            .unwrap_or((d.media_w as i32, d.media_h as i32));
        let cur_media_w = d.media_w as f64;
        let nw = crate::geometry::resize_target_box(
            cur_box_w as f64,
            cur_media_w,
            delta,
            (d.screen_w as f64) * 2.0,
        );
        // 盒高按同比例缩放（相框比例由 PNG 决定，高度会自己算出来）
        let nh = if cur_box_w > 0 {
            ((cur_box_h as f64) * (nw as f64 / cur_box_w as f64)).round() as i32
        } else {
            cur_box_h
        }
        .max(crate::config::MIN_HEIGHT as i32);
        if let Some(window) = self.state.window() {
            window.view.set_preview_box(nw, nh);
        }
        let mut st = self.drag.borrow_mut();
        st.target_media_w = nw;
        st.target_media_h = nh;
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
        let (max_w, max_h, autoplay, grow, connector) = {
            let cfg = self.state.config.borrow();
            (
                cfg.display.max_width,
                cfg.display.max_height,
                cfg.video.autoplay,
                cfg.frame.grow_percent.min(50),
                cfg.window.monitor.clone(),
            )
        };
        // 视频必须按**设备像素**解码：显示器有缩放（如 1.25x / HiDPI），
        // 只按逻辑像素出帧会被合成器再放大 → 模糊。
        // 这里与图片路径（decode_box）保持一致：上限盒 × (1+grow) × 屏幕缩放。
        let screen_scale = crate::window::target_monitor(&connector)
            .as_ref()
            .map(monitor_scale)
            .unwrap_or(1.0);
        let k = (1.0 + grow as f64 / 100.0) * screen_scale * 1.06;
        player.set_box(
            (max_w as f64 * k).round() as i32,
            (max_h as f64 * k).round() as i32,
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

    /// 视频帧（主线程）：画在相框内（控件=整屏，尺寸恒定）
    fn on_video_frame(self: &Rc<Self>, tex: gdk::Texture, _w: i32, _h: i32) {
        let Some(window) = self.state.window() else {
            return;
        };
        if !self.current_is_video.get() {
            return;
        }
        // 首次拿到视频帧尺寸 → 按方向自动选默认相框（style 为空时）
        self.auto_pick_frame_style(_w, _h);
        window.view.set_video_frame(Some(tex));
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
    /// 图片解码尺寸（设备像素）。按"上限盒 × (1+grow)"当上界 → 永不上采样（上采样 = 糊）。
    fn decode_box(&self) -> (i32, i32) {
        let (mw, mh, grow, max_px, connector) = {
            let cfg = self.state.config.borrow();
            (
                cfg.display.max_width,
                cfg.display.max_height,
                cfg.frame.grow_percent.min(50),
                cfg.display.max_decode_px,
                cfg.window.monitor.clone(),
            )
        };
        let k = 1.0 + grow as f64 / 100.0;
        let scale = crate::window::target_monitor(&connector)
            .as_ref()
            .map(monitor_scale)
            .unwrap_or(1.0);
        // **解码按设备像素、且必定覆盖实际绘制矩形**：
        // 实际绘制矩形 = 内孔最大范围 ≤ 上限盒 × (1+grow)，按这个上界解码就永不上采样。
        // 关键：解码尺寸必须 ≥ 实际绘制矩形，否则合成器放大 → 图片发糊。
        // （历史上曾乘过一个"媒体内缩 0.96"的系数，比绘制小 4%，就是模糊的根因。）
        // 1.06 是 cover 裁切余量。
        let bw = (mw as f64 * k * scale * 1.06).round().max(16.0) as i32;
        let bh = (mh as f64 * k * scale * 1.06).round().max(16.0) as i32;
        let cap = max_px.min(bw.max(bh) * 2);
        (bw.min(cap).max(16), bh.min(cap).max(16))
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

    /// 应用「素材显示比」与「相框外扩百分比」
    pub fn apply_zoom(self: &Rc<Self>) {
        // 注意：素材显示比选项已从设置页移除 —— 媒体恒为 100%（正好铺满内孔，不裁切）。
        let grow = {
            let cfg = self.state.config.borrow();
            cfg.frame.grow_percent.min(50)
        };
        if let Some(w) = self.state.window() {
            w.view.set_media_zoom(1.0);
            w.view.set_frame_grow(grow as f64 / 100.0);
        }
        crate::debug!("相框外扩 = {grow}%（素材恒铺满内孔）");
    }

    /// 记录当前相框库指纹（load_frame 成功后调用，避免下次打开设置误判为"变了"）
    fn sync_frame_index_sig() {
        let dir = crate::config::frame_dir();
        let mut names: Vec<String> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case("png")) != Some(true) { continue; }
                let (sz, mt) = match e.metadata() {
                    Ok(m) => (m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)),
                    Err(_) => (0, 0),
                };
                names.push(format!("{}:{sz}:{mt}", p.file_name().unwrap_or_default().to_string_lossy()));
            }
        }
        names.sort();
        FRAME_INDEX_SIG.with(|s| *s.borrow_mut() = names.join("|"));
    }

    /// 相框库"索引指纹"：文件名 + 大小 + mtime 的指纹。
    /// 用来判断用户是否手动增删/替换了相框图片。
    /// 重建相框库索引：扫描 frame/ 下的 PNG，算出指纹。
    ///
    /// 用户可能**手动往相框目录里加/删/换图片**，而相框列表与渲染切片
    /// 之前只在启动时读一次 → 新加的相框要重启才看得到、被替换的相框仍是旧的。
    /// 这里在打开设置面板时调用：指纹没变就什么都不做（不白重载），
    /// 变了就 `load_frame()` 整体重建（切片缓存从空开始，会按
    /// path+mtime+size+版本 的模型缓存判断是否需要重新分析）。
    pub fn refresh_frame_index(self: &Rc<Self>) {
        let dir = crate::config::frame_dir();
        let mut names: Vec<String> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case("png")) != Some(true) {
                    continue;
                }
                let (sz, mt) = match e.metadata() {
                    Ok(m) => (m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)),
                    Err(_) => (0, 0),
                };
                names.push(format!("{}:{sz}:{mt}", p.file_name().unwrap_or_default().to_string_lossy()));
            }
        }
        names.sort();
        let sig = names.join("|");
        let changed = FRAME_INDEX_SIG.with(|s| s.borrow().as_str() != sig);
        if !changed {
            crate::debug!("相框库索引无变化（{} 个）", names.len());
            return;
        }
        FRAME_INDEX_SIG.with(|s| *s.borrow_mut() = sig);
        crate::info!("相框库索引已更新（{} 个），重建相框", names.len());
        self.load_frame();
    }

    pub fn load_frame(&self) {
        let (enabled, style, legacy, connector) = {
            let cfg = self.state.config.borrow();
            (
                cfg.frame.enabled,
                cfg.frame.style.trim().to_string(),
                cfg.frame.path.trim().to_string(),
                cfg.window.monitor.clone(),
            )
        };
        *self.frame.borrow_mut() = None;

        // 优先用内置相框库（程序目录 frame/ 下的 PNG）
        let mut path = String::new();
        if enabled && !style.is_empty() {
            let dir = crate::config::frame_dir();
            let p = dir.join(&style);
            if p.is_file() {
                path = p.to_string_lossy().into_owned();
            } else if let Some(found) = resolve_frame_name(&dir, &style) {
                // 相框库被整理/改名（例如加上 横- / 竖- 前缀）时按名字回退，
                // 避免"配置里还是旧名字 → 相框静默消失"
                crate::warn!(
                    "相框 {} 不存在，自动改用 {}（目录 {}）",
                    style,
                    found,
                    dir.display()
                );
                let _ = self.state.edit(|c| {
                    c.frame.style = found.clone();
                });
                self.state.commit();
                path = dir.join(&found).to_string_lossy().into_owned();
            } else {
                crate::warn!(
                    "内置相框不存在：{}（目录 {}）",
                    style,
                    dir.display()
                );
            }
        }
        // 兼容旧配置的绝对路径
        if path.is_empty() && enabled && !legacy.is_empty() {
            let p = crate::config::expand_user(&legacy);
            if p.is_file() {
                path = p.to_string_lossy().into_owned();
            }
        }
        if path.is_empty() {
            if let Some(w) = self.state.window() {
                w.view.set_frame_aspect(0.0);
                w.view.set_frame_texture_with_hole(None, None);
                w.view.set_frame_slices(None);
                w.set_frame_mask(None);
            }
            return;
        }
        let scale = crate::window::target_monitor(&connector)
            .as_ref()
            .map(crate::window::monitor_scale)
            .unwrap_or(1.0);
        match crate::frame::FrameRenderer::new(std::path::Path::new(&path), scale) {
            Some(r) => {
                if let Some(w) = self.state.window() {
                    w.view.set_frame_aspect(r.aspect());
                }
                *self.frame.borrow_mut() = Some(Rc::new(r));
                self.refresh_frame();
                Self::sync_frame_index_sig();
            }
            None => {
                crate::warn!("相框加载失败：{path}");
                if let Some(w) = self.state.window() {
                    w.view.set_frame_texture_with_hole(None, None);
                        w.set_frame_mask(None);
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
        let (fw, fh) = self
            .state
            .window()
            .map(|win| win.view.frame_size())
            .unwrap_or((1, 1));
        m.set_rect(crate::hypr::Rect {
            x,
            y,
            w: fw,
            h: fh,
        });
    }

    /// 组件尺寸变化后重新生成相框纹理
    /// 按**素材方向**决定相框类别：横版（宽>高）/ 竖版 / 方版。
    fn media_category(w: i32, h: i32) -> &'static str {
        // 允许 ±15% 的容差判为"方"（避免 4:3 与 5:4 之间来回跳）
        let r = w as f64 / h.max(1) as f64;
        if r > 1.15 {
            "横"
        } else if r < 0.87 {
            "竖"
        } else {
            "方"
        }
    }

    /// 相框库里属于某类别的文件名。库的命名约定：`横-*` / `竖-*`，其余（如 `大头贴-*`）算"方"。
    fn frames_in_category(cat: &str) -> Vec<String> {
        crate::config::list_frame_styles()
            .into_iter()
            .filter(|n| {
                let c = if n.starts_with("横-") {
                    "横"
                } else if n.starts_with("竖-") {
                    "竖"
                } else {
                    "方"
                };
                c == cat
            })
            .collect()
    }

    /// 为当前素材挑一个相框：
    /// - `auto_style` 开：按素材方向在对应类别里**随机**挑（每次换素材都可能不同）
    /// - `auto_style` 关：保持原行为 —— 只在 style 为空时按方向选一次，之后不再改
    fn auto_pick_frame_style(self: &Rc<Self>, w: i32, h: i32) {
        if w <= 0 || h <= 0 {
            return;
        }
        let (enabled, auto, style_empty) = {
            let cfg = self.state.config.borrow();
            (
                cfg.frame.enabled,
                cfg.frame.auto_style,
                cfg.frame.style.trim().is_empty(),
            )
        };
        if !enabled {
            return;
        }
        if !auto && !style_empty {
            return; // 用户已手动选过样式，不干预
        }
        let cat = Self::media_category(w, h);
        let pool = Self::frames_in_category(cat);
        let Some(pick) = (if pool.is_empty() {
            None
        } else if auto {
            // 随机推荐：同一类别内随机（用 rand，避免每次都是第一个）
            use rand::seq::IndexedRandom;
            pool.choose(&mut rand::rng()).cloned()
        } else {
            pool.first().cloned()
        }) else {
            crate::debug!("相框库没有「{cat}」类相框，保持当前样式");
            return;
        };
        // auto 模式下每次都换；非 auto 模式只在 style 为空时设一次
        let cur = self.state.config.borrow().frame.style.clone();
        if auto && cur == pick {
            return; // 恰好还是同一个，省一次重载
        }
        self.state.update(|c| c.frame.style = pick.clone());
        crate::info!(
            "{}：{cat}版素材 → 相框 {pick}",
            if auto { "自适应随机推荐" } else { "按首个素材方向自动选择" }
        );
        self.load_frame();
        self.refresh_frame();
    }

    pub fn refresh_frame(&self) {
        let renderer = self.frame.borrow().clone();
        if let Some(r) = renderer {
            if let Some(w) = self.state.window() {
                let (fw, fh) = w.view.frame_size();
                let fit_mode = self.state.config.borrow().frame.fit.clone();
                // cover：按用户要求保留相框原始比例并裁切素材（老行为）
                let slices = if fit_mode == "cover" { None } else { r.slices() };
                crate::debug!(
                    "相框刷新：fit={fit_mode}，九宫格={}，相框尺寸 {fw}x{fh}",
                    if slices.is_some() { "启用" } else { "未启用" }
                );
                match slices {
                    // 智能自适应：九宫格（四角不变形、四边拉伸、相框可横可竖）
                    Some(sl) => {
                        w.view.set_frame_texture_with_hole(None, r.inner_hole());
                        w.set_frame_mask(None);
                        w.view.set_frame_slices(Some(sl));
                    }
                    // 回退：整图等比缩放 + 洪泛遮罩
                    None => {
                        w.view.set_frame_slices(None);
                        w.view.set_frame_texture_with_hole(r.texture_for(fw, fh), r.inner_hole());
                        w.set_frame_mask(r.mask_surface_for(fw, fh));
                    }
                }
            }
        }
    }

    fn apply_image(self: &Rc<Self>, tex: gdk::Texture, size: (i32, i32), caption: &str) {
        let Some(window) = self.state.window() else {
            return;
        };
        // 组件尺寸**固定为最大框**，媒体在框内居中绘制 →
        // 切换媒体时 layer surface 尺寸恒定（不再重建、不再闪黑/残影）
        let (mw, mh) = {
            let cfg = self.state.config.borrow();
            (cfg.display.max_width, cfg.display.max_height)
        };
        let _ = size;
        // 首次拿到素材尺寸 → 按方向自动选默认相框（style 为空时）
        self.auto_pick_frame_style(size.0, size.1);
        window.view.set_box(mw, mh);
        window.view.set_image(Some(tex), caption);
        self.refresh_frame();
        self.refresh_visibility_rect();
        window.update_hud(&self.state, caption);
    }

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

    /// "默认位置 / 边距"立即生效：按停靠方式重算位置。
    ///
    /// 注意：画布是"上限"，可见相框在画布内居中，
    /// 所以要按**相框矩形**贴靠，而不是按画布贴靠（否则会看起来偏内一大截）。
    pub fn apply_anchor(self: &Rc<Self>) {
        let Some(window) = self.state.window() else {
            return;
        };
        let screen = window.screen_bounds(&self.state);
        let (anchor, margin) = {
            let cfg = self.state.config.borrow();
            (cfg.window.default_anchor.clone(), cfg.window.margin)
        };
        // 相框尺寸由素材决定；位置直接就是相框左上角
        let (fw, fh) = window.view.frame_size();
        let (x, y) = crate::geometry::anchor_pos(&anchor, screen, fw, fh, margin);
        self.state.edit(|c| {
            c.window.x = x;
            c.window.y = y;
            c.window.placed = true;
        });
        window.set_position(x, y);
        self.refresh_visibility_rect();
        self.state.commit();
        if let Some(w2) = self.state.window() {
            w2.update_hud(&self.state, "");
        }
        crate::info!("位置已调整到 ({x},{y})（相框 {fw}x{fh}，停靠 {anchor}，边距 {margin}）");
    }

    /// 桌面显示开关（设置页切换时调用）
    pub fn apply_desktop_visible(self: &Rc<Self>) {
        let on = self.state.config.borrow().frame.desktop_enabled;
        if let Some(w) = self.state.window() {
            w.set_desktop_visible(on);
        }
        crate::debug!("桌面显示相框 = {on}");
    }

    /// 把配置里的媒体内缩比例同步给绘制控件
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
        // 关键：尺寸上限变了，之前缓存的纹理是**旧尺寸**，不清缓存会看起来"设置无效"
        self.images.clear_cache();
        // 重新按新的尺寸上限计算当前媒体的显示尺寸
        self.show_current();
        self.load_frame();
        self.refresh_visibility_rect();
        crate::info!("设置已应用：最大 {}x{}", max_w, max_h);
    }

    /// 重新扫描（设置里改了目录时调用）
    /// 媒体目录变了：重建媒体源 + 重新扫描 + 立刻显示新目录的第一项。
    /// （只 `rescan()` 不够：媒体源是启动时创建的，不重建就会一直扫旧目录）
    pub fn reload_source(self: &Rc<Self>) {
        let source_cfg = { self.state.config.borrow().source.clone() };
        match create_source(&source_cfg) {
            Ok(src) => {
                crate::info!("媒体目录已切换：{}", source_cfg.path);
                self.lib.set_source(src);
            }
            Err(e) => crate::warn!("媒体目录无法使用：{e}"),
        }
        self.rescan();
    }

    pub fn rescan(self: &Rc<Self>) {
        let this = self.clone();
        self.lib.on_scanned(Box::new(move || {
            this.after_scan();
        }));
        self.lib.scan();
    }
}


/// 相框名回退：配置里的名字找不到时，按"去掉/补上 横-、竖- 前缀"再试，
/// 再不行就用库里第一个 PNG（保证相框不会静默消失）。
fn resolve_frame_name(dir: &std::path::Path, style: &str) -> Option<String> {
    let name = style.trim();
    if name.is_empty() {
        return None;
    }
    let list = || -> Vec<String> { crate::config::list_frame_styles() };
    // 1) 原名 + 横-/竖- 前缀（木纹.png → 横-木纹.png / 竖-木纹.png）
    for prefix in ["横-", "竖-"] {
        let cand = format!("{prefix}{name}");
        if dir.join(&cand).is_file() {
            return Some(cand);
        }
    }
    // 2) 去掉已有前缀（木纹.png 已在库里，但配置写成 横-木纹.png 的变体）
    for prefix in ["横-", "竖-"] {
        if let Some(stripped) = name.strip_prefix(prefix) {
            if dir.join(stripped).is_file() {
                return Some(stripped.to_string());
            }
        }
    }
    // 3) 库里第一个（按名称排序）
    list().into_iter().next()
}
