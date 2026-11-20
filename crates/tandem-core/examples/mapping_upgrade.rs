//! Dry run: what upgrading a saved mapping would add or change. Reads, never writes.
fn main() {
    let path = std::env::args().nth(1).expect("mapping.json");
    let mut m: tandem_core::mapping::Mapping =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let preset = tandem_core::mapping::builtin_preset(&m.device).expect("no built-in preset");
    let before = m.bindings.clone();
    let meters_before = m.meters.len();
    let changed = m.upgrade_from(&preset);
    println!(
        "{}: {} bindings, would add or change {changed}; ticks/turn kept: {:?}",
        m.device,
        before.len(),
        m.jog_ticks_per_rev
    );
    for (old, new) in before.iter().zip(&m.bindings) {
        if old.target != new.target {
            println!(
                "  ~ {}: {} -> {}",
                new.trigger.label(),
                old.target.label(),
                new.target.label()
            );
        }
    }
    for b in &m.bindings[before.len()..] {
        println!("  + {} -> {}", b.trigger.label(), b.target.label());
    }
    for l in &m.meters[meters_before..] {
        println!(
            "  + level meter: CC {:#04x} · ch {} -> deck {}",
            l.cc,
            l.channel + 1,
            tandem_core::model::deck_letter(l.deck as usize)
        );
    }
}
