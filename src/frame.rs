//! PNG 相框：透明 PNG 叠在媒体之上。
//!
//! - 相框是"外观"，媒体画在它下面，靠 PNG 的透明区域露出照片；
//! - 相框源图只在启动时读一次（通常几百 KB），尺寸变化时按需缩放并缓存纹理；
//! - 渲染分辨率按屏幕缩放取 2x 上限，避免在 HiDPI 下糊、4K 下浪费显存。

use gdk_pixbuf::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

pub struct FrameRenderer {
    path: PathBuf,
    /// 原始相框（未缩放）
    source: RefCell<Option<gdk_pixbuf::Pixbuf>>,
    /// 已缓存的 (逻辑尺寸, 纹理)
    cache: RefCell<Option<((i32, i32), gdk::Texture)>>,
    /// 上限倍数（HiDPI）
    scale: Cell<f64>,
}

impl FrameRenderer {
    pub fn new(path: &Path, scale: f64) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let loader = gdk_pixbuf::PixbufLoader::new();
        loader.write(&bytes).ok()?;
        loader.close().ok()?;
        let pixbuf = loader.pixbuf()?;
        crate::debug!(
            "相框 {} ({}x{})",
            path.to_string_lossy(),
            pixbuf.width(),
            pixbuf.height()
        );
        Some(Self {
            path: path.to_path_buf(),
            source: RefCell::new(Some(pixbuf)),
            cache: RefCell::new(None),
            scale: Cell::new(scale.clamp(1.0, 2.0)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 取指定逻辑尺寸下的相框纹理（带缓存）
    pub fn texture_for(&self, w: i32, h: i32) -> Option<gdk::Texture> {
        if w <= 1 || h <= 1 {
            return None;
        }
        if let Some(((cw, ch), tex)) = self.cache.borrow().as_ref() {
            if *cw == w && *ch == h {
                return Some(tex.clone());
            }
        }
        let src = self.source.borrow().clone()?;
        let scale = self.scale.get();
        let tw = ((w as f64 * scale).round() as i32).clamp(1, 8192);
        let th = ((h as f64 * scale).round() as i32).clamp(1, 8192);

        // 相框按组件尺寸铺满（内部透明区露出媒体）
        let scaled = if (tw, th) == (src.width(), src.height()) {
            src
        } else {
            src.scale_simple(tw, th, gdk_pixbuf::InterpType::Bilinear)?
        };

        let tex = gdk::Texture::for_pixbuf(&scaled);
        *self.cache.borrow_mut() = Some(((w, h), tex.clone()));
        Some(tex)
    }

    /// 相框失效（配置变了 / 文件被替换）时重载
    pub fn reload(&self) {
        *self.cache.borrow_mut() = None;
        if let Ok(bytes) = std::fs::read(&self.path) {
            let loader = gdk_pixbuf::PixbufLoader::new();
            if loader.write(&bytes).is_ok() && loader.close().is_ok() {
                if let Some(p) = loader.pixbuf() {
                    *self.source.borrow_mut() = Some(p);
                }
            }
        }
    }
}
