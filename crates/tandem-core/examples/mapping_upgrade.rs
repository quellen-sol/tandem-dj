//! Dry run: what upgrading a saved mapping would add. Reads, never writes.
fn main() {
    let path = std::env::args().nth(1).expect("mapping.json");
    let mut m: tandem_core::mapping::Mapping =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let preset = tandem_core::mapping::builtin_preset(&m.device).expect("no built-in preset");
    let before = m.bindings.len();
    let meters_before = m.meters.len();
    let added = m.upgrade_from(&preset);
    println!(
        "{}: {before} bindings, would add {added}; ticks/turn kept: {:?}",
        m.device, m.jog_ticks_per_rev
    );
    for b in &m.bindings[before..] {
        println!("  + {} -> {}", b.trigger.label(), b.target.label());
    }
    for l in &m.meters[meters_before..] {
        println!(
            "  + level meter: CC {:#04x} · ch {} -> deck {}",
            l.cc,
            l.channel + 1,
            l.deck
        );
    }
}
