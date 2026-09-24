mod app;
mod audio;
mod peaks;
mod save;
mod waveform;

use dioxus::desktop::{Config, LogicalSize, WindowBuilder};

fn main() {
    let window = WindowBuilder::new()
        .with_title("QuickSample")
        .with_inner_size(LogicalSize::new(1000.0, 520.0))
        .with_min_inner_size(LogicalSize::new(640.0, 360.0));
    let config = Config::new()
        .with_window(window)
        .with_menu(None)
        .with_disable_context_menu(true)
        .with_background_color((26, 26, 25, 255));
    dioxus::LaunchBuilder::desktop()
        .with_cfg(config)
        .launch(app::App);
}
