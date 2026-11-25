//! Print a built-in controller preset, to check it against the research it
//! came from: every binding in message order, the level meters, and the
//! lights Tandem drives. With `--json`, print the mapping file instead:
//! saved as the name shown, in Tandem's mappings folder (Settings → General →
//! "Settings and mappings" → Open, then `mappings`), it lets someone try the preset
//! on a Tandem that doesn't have it built in yet.
//!
//!     cargo run -q -p tandem-core --example preset_table -- "SC LIVE 4"
//!     cargo run -q -p tandem-core --example preset_table -- "SC LIVE 4" --json > SC_LIVE_4.json
use tandem_core::mapping::{builtin_preset, mapping_key, MidiEngine, MidiKind, RelMode};
use tandem_core::model::{deck_letter, RoomState};
use tandem_core::render::LocalParams;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let Some(port) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("usage: preset_table <MIDI port name> [--json]");
        std::process::exit(2);
    };
    let Some(m) = builtin_preset(port) else {
        eprintln!("no built-in preset matches the port name {port:?}");
        std::process::exit(1);
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&m).unwrap());
        return;
    }
    println!(
        "{port:?} -> {} (preset version {}), {} bindings, {} jog ticks a turn, saved as {}.json",
        m.preset.clone().unwrap_or_default(),
        m.preset_version,
        m.bindings.len(),
        m.ticks_per_rev(),
        mapping_key(port)
    );
    let status = |kind: MidiKind, ch: u8| match kind {
        MidiKind::Note => 0x90 | ch,
        MidiKind::Cc => 0xB0 | ch,
        MidiKind::PitchBend => 0xE0 | ch,
    };
    let mut rows: Vec<_> = m.bindings.iter().collect();
    rows.sort_by_key(|b| (b.trigger.channel, b.trigger.kind as u8, b.trigger.number));
    for b in rows {
        let mut notes = Vec::new();
        if b.target.is_relative() {
            notes.push(match b.rel {
                RelMode::Offset64 => "64 = still".to_string(),
                RelMode::TwosComplement => "1 = +1, 127 = -1".to_string(),
                RelMode::Absolute | RelMode::Absolute14 { .. } => b.rel.label(),
            });
        }
        if b.invert {
            notes.push("inverted".into());
        }
        if let Some((lit, unlit)) = b.light {
            notes.push(format!("light {lit} lit / {unlit} unlit"));
        }
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!("  [{}]", notes.join(", "))
        };
        println!(
            "  {:02X} {:02X}  {:<46} -> {}{notes}",
            status(b.trigger.kind, b.trigger.channel),
            b.trigger.number,
            b.label(),
            b.target.label()
        );
    }
    for l in &m.meters {
        println!(
            "  B{:X} {:02X}  level meter (sent, Pioneer values) -> deck {}",
            l.channel,
            l.cc,
            deck_letter(l.deck as usize)
        );
    }
    // What Tandem sends on connecting, before anything is loaded or pressed.
    let lights = MidiEngine::new(m, 0.08).lights(&RoomState::default(), &LocalParams::default());
    println!(
        "{} lights driven (status, note = value at rest):",
        lights.len()
    );
    let mut lights = lights;
    lights.sort();
    let line: Vec<String> = lights
        .iter()
        .map(|((s, n), v)| format!("{s:02X} {n:02X}={v}"))
        .collect();
    for chunk in line.chunks(10) {
        println!("  {}", chunk.join("  "));
    }
}
