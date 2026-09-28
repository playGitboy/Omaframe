//! photo-frame — 轻量桌面电子相框（Omarchy / Wayland / Hyprland / GTK4）

mod app;
mod config;
mod controls;
mod geometry;
mod log;
mod media;
mod player;
mod slideshow;
mod window;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match app::run(&args) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("photo-frame: {e}");
            ExitCode::from(1)
        }
    }
}
