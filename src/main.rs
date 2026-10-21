#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audible;
mod book;
mod chapters;
mod convert;
mod encode;
mod faststart;
mod ffmpeg;
mod mp4;
mod naming;
mod nfo;
mod picture;
mod session;
mod session_ui;
mod settings;
mod source;
mod task;
mod text;
mod theme;

fn main() -> eframe::Result {
    let initial_dir = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/folio.png"))
        .expect("valid icon");
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Folio")
            .with_icon(icon)
            .with_inner_size([1320.0, 880.0])
            .with_min_inner_size([980.0, 620.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "Folio",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc, initial_dir)))),
    )
}
