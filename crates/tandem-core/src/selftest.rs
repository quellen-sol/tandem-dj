//! End-to-end loopback test: a host and a joining DJ in one process, talking
//! over real TCP, with the joiner's clock deliberately an hour off.
//!
//! Checks that the clock syncs, that a track the joiner doesn't have is
//! transferred and verified, that both booths' room state stays identical,
//! that both booths render bit-identical audio for the same moment, and that
//! a share can be offered, accepted and downloaded.

use crate::config::Paths;
use crate::library::Library;
use crate::model::Action;
use crate::render::{Lane, LaneKind};
use crate::session::{Session, SessionHandle, TrackStatus};
use crate::time::SharedClock;
use anyhow::{bail, ensure};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn wait_for(
    what: &str,
    timeout: Duration,
    mut f: impl FnMut() -> bool,
) -> anyhow::Result<Duration> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return Ok(start.elapsed());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    bail!("timed out waiting for {what}")
}

fn session(root: &std::path::Path, name: &str, skew_us: i64) -> anyhow::Result<SessionHandle> {
    let paths = Paths::under(&root.join(name));
    paths.ensure()?;
    let lib = Arc::new(Mutex::new(Library::new(
        paths.shared_dir.clone(),
        paths.library_cache.clone(),
    )));
    Ok(Session::create(
        name.into(),
        paths,
        lib,
        Arc::new(SharedClock::with_skew(skew_us)),
    ))
}

/// Run the loopback test, reporting progress through `say`.
pub fn loopback(say: &mut dyn FnMut(&str)) -> anyhow::Result<()> {
    let root = std::env::temp_dir().join(format!("tandem-selftest-{}", fastrand::u64(..)));
    std::fs::create_dir_all(&root)?;
    let result = run(&root, say);
    let _ = std::fs::remove_dir_all(&root);
    result
}

fn run(root: &std::path::Path, say: &mut dyn FnMut(&str)) -> anyhow::Result<()> {
    let music = root.join("alice-music");
    std::fs::create_dir_all(&music)?;
    let wav = music.join("Test Artist - Test Tone.wav");
    crate::audio::write_wav(&wav, 44_100, &crate::audio::test_signal(44_100, 6.0, 440.0))?;

    let alice = session(root, "Alice", 0)?;
    let bob = session(root, "Bob", 3_600_000_000)?; // an hour off

    let port = crate::net::host(&alice, 0, "TEST42")?;
    say(&format!("host listening on 127.0.0.1:{port}"));
    crate::net::join(&bob, &format!("127.0.0.1:{port}"), "test42")?;
    let skew = {
        let (a, b) = (alice.lock().clock(), bob.lock().clock());
        (b.now() - a.now()).abs()
    };
    say(&format!(
        "clock synced: booths differ by {:.2} ms (Bob's clock was an hour off)",
        skew as f64 / 1000.0
    ));
    ensure!(skew < 5_000, "clock offset too large: {skew} us");

    alice.lock().load_path(0, wav.clone());
    let id = {
        let mut id = None;
        wait_for("Alice to load the track", Duration::from_secs(20), || {
            id = alice.lock().latest().decks[0].track;
            id.is_some()
        })?;
        id.unwrap()
    };
    let took = wait_for("Bob to receive the track", Duration::from_secs(30), || {
        bob.lock().track_status(id) == Some(TrackStatus::Ready)
    })?;
    say(&format!(
        "track transferred, verified and decoded on the other booth in {:.2} s",
        took.as_secs_f64()
    ));

    alice.lock().act(Action::Play { deck: 0 });
    std::thread::sleep(Duration::from_millis(100));
    bob.lock().act(Action::Tempo {
        deck: 0,
        tempo: 1.02,
    });
    bob.lock().act(Action::Volume { deck: 0, v: 0.7 });
    alice.lock().act(Action::Crossfader { v: 0.3 });
    std::thread::sleep(Duration::from_millis(400));

    let t = alice.lock().clock().now() - 100_000;
    let (da, db) = (
        alice.lock().state_at(t).digest(),
        bob.lock().state_at(t).digest(),
    );
    ensure!(da == db, "room state differs between booths");
    say("room state identical on both booths");

    let snaps = (
        alice.lock().snapshot_handle().load_full(),
        bob.lock().snapshot_handle().load_full(),
    );
    let n = 4096;
    let render = |snap: &crate::render::AudioSnapshot| {
        let mut lane = Lane::new(48_000.0, 0, LaneKind::Live);
        let (mut m, mut c) = (vec![0.0f32; n * 2], vec![0.0f32; n * 2]);
        lane.render(snap, t, &mut m, &mut c);
        m
    };
    let (ma, mb) = (render(&snaps.0), render(&snaps.1));
    ensure!(ma.iter().any(|v| v.abs() > 0.01), "rendered silence");
    let diff = ma
        .iter()
        .zip(&mb)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    ensure!(
        diff < 1e-6,
        "booths render different audio (max diff {diff})"
    );
    say("both booths render identical audio for the same moment");

    // Sharing: Alice offers her folder, Bob accepts.
    alice
        .lock()
        .offer_share("Alice's test crate".into(), vec![wav.clone()]);
    wait_for("the share offer to arrive", Duration::from_secs(10), || {
        !bob.lock().view().incoming.is_empty()
    })?;
    let share_id = bob.lock().view().incoming[0].share_id;
    bob.lock().respond_share(share_id, true);
    let mut dest = None;
    wait_for(
        "the shared track to download",
        Duration::from_secs(30),
        || {
            let v = bob.lock().view();
            let s = &v.incoming[0];
            dest = Some(s.dir.clone());
            s.received + s.already_had == 1
        },
    )?;
    let dir = dest.unwrap();
    let got = std::fs::read_dir(&dir)?
        .flatten()
        .filter(|e| e.path().extension().map(|x| x == "wav").unwrap_or(false))
        .count();
    ensure!(got == 1, "shared file missing from {}", dir.display());
    ensure!(
        std::fs::read_to_string(&bob.lock().paths.consent_log)?.contains("share-accepted"),
        "consent wasn't recorded"
    );
    say("share offered, accepted (consent logged), downloaded and verified");

    bob.lock().leave();
    alice.lock().leave();
    Ok(())
}
