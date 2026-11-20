//! Dry run: what upgrading a saved mapping would add or change. Reads, never writes.
fn main() {
    let path = std::env::args().nth(1).expect("mapping.json");
    let mut m: tandem_core::mapping::Mapping =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let preset = tandem_core::mapping::builtin_preset(&m.device).expect("no built-in preset");
    let before = m.bindings.clone();
    if m.adopt(&preset) {
        // A mapping made before the preset existed: it becomes the preset.
        println!(
            "{}: {} bindings, would become the built-in {} mapping ({} bindings); ticks/turn: {:?}",
            m.device,
            before.len(),
            m.preset.clone().unwrap_or_default(),
            m.bindings.len(),
            m.jog_ticks_per_rev
        );
        let same = |a: &tandem_core::mapping::Binding, b: &tandem_core::mapping::Binding| {
            a.trigger == b.trigger && a.target == b.target
        };
        for b in &before {
            if !m.bindings.iter().any(|n| same(n, b)) {
                println!("  - {} -> {}", b.label(), b.target.label());
            }
        }
        for b in &m.bindings {
            match before.iter().find(|o| same(o, b)) {
                Some(o) if o == b => {}
                Some(_) => println!("  ~ {} -> {}", b.label(), b.target.label()),
                None => println!("  + {} -> {}", b.label(), b.target.label()),
            }
        }
        return;
    }
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
        println!("  + {} -> {}", b.label(), b.target.label());
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
