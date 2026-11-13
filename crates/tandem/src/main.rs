// Release builds are a windowed app (no console window).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod midi;
mod output;
mod selftest;
mod ui_decks;
mod ui_dialogs;
mod ui_library;

use eframe::egui;
use tandem_core::config::Paths;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--self-test") {
        attach_console();
        std::process::exit(selftest::run());
    }
    if args.iter().any(|a| a == "--version") {
        attach_console();
        println!("tandem {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // --data-dir <folder>: keep everything in one folder instead of your user
    // profile. Handy for running a second copy on one PC to test a room.
    let arg_after = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(std::path::PathBuf::from)
    };
    let paths = match arg_after("--data-dir") {
        Some(dir) => Paths::under(&dir),
        None => match Paths::detect() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Tandem couldn't find your user folders: {e}");
                std::process::exit(1);
            }
        },
    };
    let dev = app::DevOpts {
        screenshot: arg_after("--screenshot"),
        shot_frame: arg_after("--shot-frame")
            .and_then(|p| p.to_string_lossy().parse().ok())
            .unwrap_or(330),
        demo: args.iter().any(|a| a == "--demo"),
        host_port: arg_after("--dev-host").and_then(|p| p.to_string_lossy().parse().ok()),
        join: arg_after("--dev-join").map(|p| p.to_string_lossy().to_string()),
        shared: false,
        frame: 0,
    };
    let _ = paths.ensure();
    tandem_core::logging::init(Some(&paths.log_file), cfg!(debug_assertions));
    log::info!("Tandem {} starting", env!("CARGO_PKG_VERSION"));
    std::panic::set_hook(Box::new(|info| {
        log::error!("panic: {info}");
    }));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Tandem")
            .with_inner_size([1440.0, 930.0])
            .with_min_inner_size([1100.0, 700.0]),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native(
        "Tandem",
        options,
        Box::new(move |cc| Ok(Box::new(app::TandemApp::new(cc, paths, dev)))),
    ) {
        log::error!("UI failed: {e}");
    }
}

/// Let `tandem.exe --self-test` print into the terminal it was run from.
fn attach_console() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
