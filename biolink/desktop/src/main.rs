#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 920.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("BioLink"),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "BioLink",
        options,
        Box::new(|cc| Ok(Box::new(app::BioApp::new(&cc.egui_ctx)))),
    )
}
