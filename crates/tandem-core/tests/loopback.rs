#[test]
fn two_booths_over_tcp() {
    tandem_core::logging::init(None, true);
    tandem_core::selftest::loopback(&mut |m| eprintln!("  ok: {m}"))
        .expect("loopback self-test failed");
}
