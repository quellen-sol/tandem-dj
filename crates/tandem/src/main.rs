// Release builds are a windowed app (no console window).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod midi;
mod output;
mod selftest;
mod ui_broadcast;
mod ui_broadcaster;
mod ui_deck_check;
mod ui_decks;
mod ui_dialogs;
mod ui_library;
mod ui_meters;
mod ui_wizard;

use eframe::egui;
use tandem_core::config::Paths;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--self-test") {
        attach_console();
        std::process::exit(selftest::run());
    }
    if let Some(i) = args.iter().position(|a| a == "--midi-monitor") {
        attach_console();
        let secs = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(60);
        std::process::exit(midi::monitor(secs));
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
        demo: args.iter().any(|a| a == "--demo" || a == "--demo4"),
        demo4: args.iter().any(|a| a == "--demo4"),
        host_port: arg_after("--dev-host").and_then(|p| p.to_string_lossy().parse().ok()),
        join: arg_after("--dev-join").map(|p| p.to_string_lossy().to_string()),
        join_broadcaster: args.iter().any(|a| a == "--dev-broadcaster"),
        late_ms: arg_after("--dev-late").and_then(|p| p.to_string_lossy().parse().ok()),
        cut_frame: arg_after("--dev-cut").and_then(|p| p.to_string_lossy().parse().ok()),
        search: arg_after("--dev-search").map(|p| p.to_string_lossy().to_string()),
        broadcast_view: args.iter().any(|a| a == "--broadcast-view"),
        roll: args.iter().any(|a| a == "--dev-roll"),
        window_size: arg_after("--window-size").and_then(|p| parse_size(&p.to_string_lossy())),
        zoom: arg_after("--zoom").and_then(|p| p.to_string_lossy().parse().ok()),
        settings: arg_after("--dev-settings").map(|p| p.to_string_lossy().to_string()),
        wizard: arg_after("--dev-wizard").map(|p| p.to_string_lossy().to_string()),
        deck_check: arg_after("--dev-deck-check").map(|p| p.to_string_lossy().to_string()),
        eject: arg_after("--dev-eject").map(|p| p.to_string_lossy().to_string()),
        drag: arg_after("--dev-drag").and_then(|p| parse_drag(&p.to_string_lossy())),
        crate_demo: args.iter().any(|a| a == "--dev-crate"),
        crate_step: 0,
        fitted: false,
        shared: false,
        demo_started: false,
        frame: 0,
    };
    let _ = paths.ensure();
    tandem_core::logging::init(Some(&paths.log_file), cfg!(debug_assertions));
    log::info!("Tandem {} starting", env!("CARGO_PKG_VERSION"));
    std::panic::set_hook(Box::new(|info| {
        log::error!("panic: {info}");
    }));

    // Opens at 1440 × 930 where that fits; on a smaller screen the app
    // maximises its window on the first frame (see `TandemApp::fit_window`).
    let size = dev.window_size.unwrap_or([1440.0, 930.0]);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Tandem")
            .with_inner_size(size)
            .with_min_inner_size([
                app::MIN_WINDOW[0].min(size[0]),
                app::MIN_WINDOW[1].min(size[1]),
            ]),
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

/// "1280x720" → [1280, 720].
fn parse_size(s: &str) -> Option<[f32; 2]> {
    let (w, h) = s.split_once(['x', 'X'])?;
    Some([w.trim().parse().ok()?, h.trim().parse().ok()?])
}

/// "300,200,120,700!" → [300, 200, 120, 700], and let go there.
fn parse_drag(s: &str) -> Option<([f32; 4], bool)> {
    let points = s.trim_end_matches('!').split(',');
    let v: Vec<f32> = points.filter_map(|x| x.trim().parse().ok()).collect();
    Some((v.try_into().ok()?, s.ends_with('!')))
}

/// Let `tandem.exe --self-test` print into the terminal it was run from.
fn attach_console() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
