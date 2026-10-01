//! Omaframe — 智能自适应 PNG 相框（Omarchy / Wayland / Hyprland / GTK4）

mod app;
mod config;
mod controls;
mod frame;
mod frame_model;
mod geometry;
mod hypr;
mod log;
mod media;
mod player;
mod settings;
mod slideshow;
mod tray;
mod window;

use std::process::ExitCode;

/// 用系统默认程序打开 URL（设置页的"项目主页"链接）。
/// Wayland 下不能直接 exec 浏览器（会报错），要用 GtkUriLauncher / xdg-open。
pub fn open_url(url: &str) {
    // 直接用 xdg-open 交给系统默认浏览器打开。
    // 不用 gtk::UriLauncher：它的回调是异步的、会活过本函数，
    // 在同步函数里既难借用 url，又容易误触发"失败回退"导致打开两次。
    // spawn 立即返回，不阻塞 UI。
    match std::process::Command::new("xdg-open").arg(url).spawn() {
        Ok(_) => crate::debug!("已用 xdg-open 打开 {url}"),
        Err(e) => crate::warn!("xdg-open 打开 {url} 失败：{e}"),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match app::run(&args) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("omaframe: {e}");
            ExitCode::from(1)
        }
    }
}
