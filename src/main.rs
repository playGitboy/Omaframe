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
