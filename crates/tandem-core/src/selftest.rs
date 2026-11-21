//! End-to-end loopback test: a host, a joining DJ and a broadcaster in one
//! process, talking over real TCP, with the joiners' clocks deliberately
//! hours off.
//!
//! Checks that the clocks sync, that a track a booth doesn't have is
//! transferred and verified (the joining DJ's tracks reach the broadcaster
//! through the host), that every booth's room state stays identical, that
//! the booths render bit-identical audio for the same moment, that a
//! broadcaster can't change the room, that a share can be offered,
//! accepted and downloaded, that a booth whose link drops gets back in, and
//! that a track it was receiving carries on from where it got to.

use crate::config::Paths;
use crate::library::Library;
use crate::model::{Action, Event};
use crate::protocol::{read_msg, write_msg, ConnKind, Msg, PeerInfo};
use crate::render::{Lane, LaneKind};
use crate::session::{Session, SessionHandle, TrackStatus, TransferState};
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

    let bob_music = root.join("bob-music");
    std::fs::create_dir_all(&bob_music)?;
    let bob_wav = bob_music.join("Bob - Low Tone.wav");
    crate::audio::write_wav(
        &bob_wav,
        44_100,
        &crate::audio::test_signal(44_100, 5.0, 330.0),
    )?;

    let alice = session(root, "Alice", 0)?;
    let bob = session(root, "Bob", 3_600_000_000)?; // an hour off
    let carol = session(root, "Carol", -7_200_000_000)?; // two hours the other way

    let port = crate::net::host(&alice, 0, "TEST42")?;
    let addr = format!("127.0.0.1:{port}");
    say(&format!("host listening on {addr}"));
    crate::net::join(&bob, &addr, "test42", false)?;
    let joined = Instant::now();
    let skew = {
        let (a, b) = (alice.lock().clock(), bob.lock().clock());
        (b.now() - a.now()).abs()
    };
    say(&format!(
        "clock synced: booths differ by {:.2} ms (Bob's clock was an hour off)",
        skew as f64 / 1000.0
    ));
    ensure!(skew < 5_000, "clock offset too large: {skew} us");

    // A third DJ is turned away; a broadcaster gets in.
    let dave = session(root, "Dave", 0)?;
    match crate::net::join(&dave, &addr, "test42", false) {
        Ok(()) => bail!("a third DJ got into the room"),
        Err(e) => ensure!(
            format!("{e:#}").contains("two DJs"),
            "third DJ refused for the wrong reason: {e:#}"
        ),
    }
    crate::net::join(&carol, &addr, "test42", true)?;
    let skew = {
        let (a, c) = (alice.lock().clock(), carol.lock().clock());
        (c.now() - a.now()).abs()
    };
    ensure!(
        skew < 5_000,
        "broadcaster's clock offset too large: {skew} us"
    );
    wait_for(
        "everyone to see the broadcaster",
        Duration::from_secs(5),
        || {
            let names = |h: &SessionHandle| {
                let r = h.lock().roster();
                (
                    r.djs.into_iter().map(|p| p.name).collect::<Vec<_>>(),
                    r.broadcasters
                        .into_iter()
                        .map(|p| p.name)
                        .collect::<Vec<_>>(),
                )
            };
            let room = (
                vec!["Alice".to_string(), "Bob".into()],
                vec!["Carol".to_string()],
            );
            names(&alice) == room && names(&bob) == room && names(&carol) == room
        },
    )?;
    say(&format!(
        "a third DJ is refused; Carol joins as a broadcaster (clock off by {:.2} ms)",
        skew as f64 / 1000.0
    ));

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

    // Bob's own track goes Bob -> Alice -> Carol.
    bob.lock().load_path(1, bob_wav.clone());
    let bob_id = {
        let mut id = None;
        wait_for("Bob to load his track", Duration::from_secs(20), || {
            id = bob.lock().latest().decks[1].track;
            id.is_some()
        })?;
        id.unwrap()
    };
    let took = wait_for(
        "the broadcaster to receive both tracks",
        Duration::from_secs(40),
        || {
            let c = carol.lock();
            c.track_status(id) == Some(TrackStatus::Ready)
                && c.track_status(bob_id) == Some(TrackStatus::Ready)
        },
    )?;
    ensure!(
        alice.lock().track_status(bob_id) == Some(TrackStatus::Ready),
        "the host didn't get Bob's track"
    );
    say(&format!(
        "the broadcaster has both DJs' tracks (Bob's through the host) after {:.2} s",
        took.as_secs_f64()
    ));

    alice.lock().act(Action::Play { deck: 0 });
    std::thread::sleep(Duration::from_millis(100));
    bob.lock().act(Action::Tempo {
        deck: 0,
        tempo: 1.02,
    });
    bob.lock().act(Action::Volume { deck: 0, v: 0.7 });
    bob.lock().act(Action::Play { deck: 1 });
    alice.lock().act(Action::Crossfader { v: 0.3 });

    // The broadcaster can't change anything: not through its own session,
    // and not by sending the host an event directly.
    let carol_id = carol.lock().me.id;
    carol.lock().act(Action::Crossfader { v: 0.99 });
    let mallory = PeerInfo {
        id: 0xBAD,
        name: "Mallory".into(),
    };
    let mut raw = crate::net::connect(
        crate::net::resolve(&addr)?,
        ConnKind::Control,
        "TEST42",
        &mallory,
        0,
        true,
    )?;
    ensure!(
        matches!(read_msg(&mut raw)?.0, Msg::Welcome { .. }),
        "no welcome for a broadcaster"
    );
    let now = alice.lock().now();
    write_msg(
        &mut raw,
        &Msg::Event {
            ev: Event {
                time: now,
                origin: mallory.id,
                seq: 1,
                action: Action::Crossfader { v: 0.77 },
            },
        },
        &[],
    )?;
    std::thread::sleep(Duration::from_millis(400));
    let _ = raw.shutdown(std::net::Shutdown::Both);
    for (who, h) in [("Alice", &alice), ("Bob", &bob), ("Carol", &carol)] {
        let s = h.lock();
        ensure!(
            s.latest().crossfader == 0.3,
            "a broadcaster moved the crossfader on {who}'s booth"
        );
        ensure!(
            !s.timeline()
                .events()
                .iter()
                .any(|e| e.origin == mallory.id || e.origin == carol_id),
            "a broadcaster's event reached {who}'s timeline"
        );
    }
    say("a broadcaster's moves are ignored, by its own booth and by the host");

    let t = alice.lock().clock().now() - 100_000;
    let (da, db, dc) = (
        alice.lock().state_at(t).digest(),
        bob.lock().state_at(t).digest(),
        carol.lock().state_at(t).digest(),
    );
    ensure!(da == db, "room state differs between booths");
    ensure!(da == dc, "room state differs on the broadcaster");
    say("room state identical on all three booths");

    // Pings keep each end's "last heard" fresh, so a stalled link stands out.
    let settle = Duration::from_millis(2_500);
    if joined.elapsed() < settle {
        std::thread::sleep(settle - joined.elapsed());
    }
    for (who, h) in [("Alice", &alice), ("Bob", &bob), ("Carol", &carol)] {
        let quiet = h.lock().view().link.quiet_for;
        ensure!(
            matches!(quiet, Some(q) if q < Duration::from_secs(2)),
            "{who} hasn't heard from the other end lately: {quiet:?}"
        );
    }
    say("each end keeps hearing the other (pings keep the link fresh)");
    let late = carol.lock().view().link.late_us;
    match late {
        Some(us) if us < 250_000 => say(&format!(
            "the DJs' moves reach the broadcaster {:.1} ms after they happen, at most",
            us as f64 / 1000.0
        )),
        other => bail!("broadcaster's lateness: {other:?}"),
    }

    let snaps = (
        alice.lock().snapshot_handle().load_full(),
        bob.lock().snapshot_handle().load_full(),
        carol.lock().snapshot_handle().load_full(),
    );
    let n = 4096;
    let render = |snap: &crate::render::AudioSnapshot, kind| {
        let mut lane = Lane::new(48_000.0, 0, kind);
        let (mut m, mut c) = (vec![0.0f32; n * 2], vec![0.0f32; n * 2]);
        lane.render(snap, t, &mut m, &mut c);
        m
    };
    let max_diff = |a: &[f32], b: &[f32]| {
        a.iter()
            .zip(b)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max)
    };
    let (ma, mb) = (
        render(&snaps.0, LaneKind::Live),
        render(&snaps.1, LaneKind::Live),
    );
    ensure!(ma.iter().any(|v| v.abs() > 0.01), "rendered silence");
    let diff = max_diff(&ma, &mb);
    ensure!(
        diff < 1e-6,
        "booths render different audio (max diff {diff})"
    );
    let (ba, bc) = (
        render(&snaps.0, LaneKind::Broadcast),
        render(&snaps.2, LaneKind::Broadcast),
    );
    let diff = max_diff(&ba, &bc);
    ensure!(
        diff < 1e-6,
        "the broadcaster renders different audio (max diff {diff})"
    );
    let view = carol.lock().broadcast_view();
    let djs: Vec<&str> = view.djs.iter().map(|p| p.name.as_str()).collect();
    ensure!(
        djs == ["Alice", "Bob"],
        "broadcaster's window names {djs:?}"
    );
    say("all booths render identical audio for the same moment, the broadcaster's feed too");

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

    // A dropped link: Bob gets back in by himself, and the room has both
    // DJs' moves from while he was out.
    bob.lock().cut_link();
    wait_for(
        "Bob to notice his link dropped",
        Duration::from_secs(5),
        || bob.lock().view().link.rejoin.is_some(),
    )?;
    alice.lock().act(Action::Crossfader { v: 0.6 });
    bob.lock().act(Action::Volume { deck: 3, v: 0.35 });
    let took = wait_for("Bob to get back in", Duration::from_secs(15), || {
        let v = bob.lock().view();
        v.link.peer.is_some() && v.link.rejoin.is_none()
    })?;
    wait_for("the host to see Bob back", Duration::from_secs(5), || {
        alice.lock().roster().djs.len() == 2
    })?;
    let t = alice.lock().clock().now() - 100_000;
    ensure!(
        alice.lock().state_at(t).digest() == bob.lock().state_at(t).digest(),
        "room state differs after Bob got back in"
    );
    ensure!(
        bob.lock().latest().crossfader == 0.6,
        "Bob missed what happened while he was out"
    );
    ensure!(
        alice.lock().latest().decks[3].volume == 0.35,
        "what Bob did while he was out didn't reach the room"
    );
    say(&format!(
        "a dropped link: Bob gets back in by himself after {:.1} s, with both DJs' moves from meanwhile",
        took.as_secs_f64()
    ));

    // A track on its way when the link drops carries on from where it got
    // to, and the deck that waited for it plays with the room.
    let long = music.join("Test Artist - Long Tone.wav");
    crate::audio::write_wav(
        &long,
        44_100,
        &crate::audio::test_signal(44_100, 12.0, 520.0),
    )?;
    alice.lock().max_upload_bytes_s = 1_200_000.0;
    alice.lock().load_path(2, long);
    let long_id = {
        let mut id = None;
        wait_for(
            "Alice to load the long track",
            Duration::from_secs(20),
            || {
                id = alice.lock().latest().decks[2].track;
                id.is_some()
            },
        )?;
        id.unwrap()
    };
    alice.lock().act(Action::Play { deck: 2 });
    let download = |h: &SessionHandle| {
        let v = h.lock().view();
        v.transfers
            .into_iter()
            .find(|t| t.id == long_id && !t.upload)
    };
    wait_for("Bob to be part-way", Duration::from_secs(15), || {
        download(&bob).is_some_and(|t| t.total > 0 && t.done * 4 >= t.total && t.done < t.total)
    })?;
    bob.lock().cut_link();
    wait_for("Bob's download to pause", Duration::from_secs(5), || {
        download(&bob).is_some_and(|t| t.state == TransferState::Paused)
    })?;
    wait_for(
        "the long track to reach Bob and Carol",
        Duration::from_secs(30),
        || {
            bob.lock().track_status(long_id) == Some(TrackStatus::Ready)
                && carol.lock().track_status(long_id) == Some(TrackStatus::Ready)
        },
    )?;
    let t = download(&bob).ok_or_else(|| anyhow::anyhow!("Bob's download is gone"))?;
    ensure!(
        t.from > 0,
        "Bob's download started over after the link dropped"
    );
    let t_now = alice.lock().clock().now() - 100_000;
    let (sa, sb) = (
        alice.lock().snapshot_handle().load_full(),
        bob.lock().snapshot_handle().load_full(),
    );
    let render_at = |snap: &crate::render::AudioSnapshot| {
        let mut lane = Lane::new(48_000.0, 0, LaneKind::Live);
        let (mut m, mut c) = (vec![0.0f32; n * 2], vec![0.0f32; n * 2]);
        lane.render(snap, t_now, &mut m, &mut c);
        m
    };
    let (ma, mb) = (render_at(&sa), render_at(&sb));
    ensure!(ma.iter().any(|v| v.abs() > 0.01), "rendered silence");
    let diff = max_diff(&ma, &mb);
    ensure!(
        diff < 1e-6,
        "Bob's booth plays the resumed track differently (max diff {diff})"
    );
    say(&format!(
        "a track cut off at {:.0}% carries on from there once Bob's back, and his deck plays with the room",
        t.from as f64 * 100.0 / t.total as f64
    ));

    // A broadcaster that loses the room stops playing it.
    alice.lock().leave();
    wait_for(
        "the broadcaster to notice the host left",
        Duration::from_secs(10),
        || {
            let c = carol.lock();
            !c.is_broadcaster() && c.latest().decks.iter().all(|d| d.track.is_none())
        },
    )?;
    say("when the host leaves, the broadcaster's decks empty out");
    bob.lock().leave();
    carol.lock().leave();
    Ok(())
}
