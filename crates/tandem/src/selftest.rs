//! `tandem.exe --self-test`: checks this PC's audio, MIDI and networking
//! without opening a window.

use crate::output;
use arc_swap::ArcSwap;
use std::sync::Arc;
use std::time::Duration;
use tandem_core::config::{AudioSettings, Paths};
use tandem_core::meter::LevelMeters;
use tandem_core::render::AudioSnapshot;
use tandem_core::time::SharedClock;

pub fn run() -> i32 {
    tandem_core::logging::init(None, false);
    println!("Tandem {} self-test", env!("CARGO_PKG_VERSION"));
    let mut failed = false;

    println!("\nAudio outputs:");
    let devices = output::list_devices();
    if devices.is_empty() {
        println!("  (none found)");
    }
    let default = output::default_device_name();
    for d in &devices {
        let mark = if Some(&d.name) == default.as_ref() {
            " [default]"
        } else {
            ""
        };
        println!(
            "  - {} · up to {} ch · {} Hz{mark}",
            d.name, d.max_channels, d.sample_rate
        );
    }
    if default.is_some() {
        // Open the default device and play silence for half a second.
        let snap = Arc::new(ArcSwap::from_pointee(AudioSnapshot::empty()));
        let outs = output::start(
            &AudioSettings::default(),
            snap,
            Arc::new(SharedClock::new()),
            Arc::new(LevelMeters::new()),
            false,
        );
        std::thread::sleep(Duration::from_millis(500));
        let errors = outs.errors.lock().clone();
        for s in &outs.status {
            println!("  stream: {s}");
        }
        if errors.is_empty()
            && !outs
                .status
                .iter()
                .any(|s| s.contains("not found") || s.contains("can't"))
        {
            println!("  ok: default output opened and ran for 0.5 s");
        } else {
            println!("  FAIL: audio stream errors: {errors:?}");
            failed = true;
        }
    }

    println!("\nMIDI inputs:");
    let ports = crate::midi::ports();
    if ports.is_empty() {
        println!("  (none found)");
    }
    for p in ports {
        println!("  - {p}");
    }

    if let Ok(paths) = Paths::detect() {
        let crates = tandem_core::serato::discover(paths.music_dir.as_deref());
        println!("\nSerato crates found: {}", crates.len());
        for c in crates.iter().take(10) {
            println!("  - {}", c.name);
        }
    }

    println!("\nRoom loopback test (two DJs and a broadcaster):");
    match tandem_core::selftest::loopback(&mut |m| println!("  ok: {m}")) {
        Ok(()) => {}
        Err(e) => {
            println!("  FAIL: {e:#}");
            failed = true;
        }
    }
    println!(
        "\n{}",
        if failed {
            "SELF-TEST FAILED"
        } else {
            "SELF-TEST PASSED"
        }
    );
    i32::from(failed)
}
