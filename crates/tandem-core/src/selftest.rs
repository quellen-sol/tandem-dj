//! End-to-end loopback test: a host, three joining DJs and a broadcaster in
//! one process, talking over real TCP, with the joiners' clocks deliberately
//! hours off.
//!
//! Checks that the clocks sync, that a fifth DJ is turned away, that a track
//! a booth doesn't have is transferred and verified (a joining DJ's tracks
//! reach the other DJs and the broadcaster through the host), that one
//! joining DJ's moves reach another, that every booth's room state stays
//! identical, that the booths render bit-identical audio for the same
//! moment (effects included: two DJs switch effects on, and every booth
//! hears them the same), that a broadcaster can't change the room, that a share can be
//! offered, accepted and downloaded (from one joining DJ to another, through
//! the host), that a list shared live reaches the other DJs with nothing
//! copied until a track from it is loaded, that a booth whose link drops
//! gets back in with what it did meanwhile reaching every DJ, and that a
//! track it was receiving carries on from where it got to.

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
    let dave = session(root, "Dave", 1_800_000_000)?;
    let erin = session(root, "Erin", -900_000_000)?;

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

    // Two more DJs get in, a fifth is turned away, and a broadcaster gets in.
    for h in [&dave, &erin] {
        crate::net::join(h, &addr, "TEST42", false)?;
        let skew = (h.lock().clock().now() - alice.lock().clock().now()).abs();
        ensure!(skew < 5_000, "a joining DJ's clock is off by {skew} us");
    }
    let frank = session(root, "Frank", 0)?;
    match crate::net::join(&frank, &addr, "test42", false) {
        Ok(()) => bail!("a fifth DJ got into the room"),
        Err(e) => ensure!(
            format!("{e:#}").contains("4 DJs"),
            "fifth DJ refused for the wrong reason: {e:#}"
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
                vec![
                    "Alice".to_string(),
                    "Bob".into(),
                    "Dave".into(),
                    "Erin".into(),
                ],
                vec!["Carol".to_string()],
            );
            [&alice, &bob, &carol, &dave, &erin]
                .iter()
                .all(|h| names(h) == room)
        },
    )?;
    say(&format!(
        "Dave and Erin join as DJs too, a fifth DJ is refused, and Carol joins as a broadcaster (clock off by {:.2} ms)",
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
        "the broadcaster and the other DJs to receive both tracks",
        Duration::from_secs(40),
        || {
            [&carol, &dave, &erin].iter().all(|h| {
                let h = h.lock();
                h.track_status(id) == Some(TrackStatus::Ready)
                    && h.track_status(bob_id) == Some(TrackStatus::Ready)
            })
        },
    )?;
    ensure!(
        alice.lock().track_status(bob_id) == Some(TrackStatus::Ready),
        "the host didn't get Bob's track"
    );
    say(&format!(
        "everyone has both tracks (Bob's through the host) after {:.2} s",
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
    dave.lock().act(Action::Volume { deck: 2, v: 0.6 });
    wait_for("Dave's move to reach Erin", Duration::from_secs(5), || {
        erin.lock().latest().decks[2].volume == 0.6
    })?;

    // Effects are part of the room: Erin puts a phaser on deck A, Dave an
    // echo on deck B (Bob's track), and every booth has them.
    erin.lock().act(Action::FxOn {
        deck: 0,
        slot: 2,
        on: true,
    });
    dave.lock().act(Action::FxDepth {
        deck: 1,
        slot: 0,
        v: 0.8,
    });
    dave.lock().act(Action::FxOn {
        deck: 1,
        slot: 0,
        on: true,
    });
    wait_for(
        "the effects to reach every booth",
        Duration::from_secs(5),
        || {
            [&alice, &bob, &carol, &dave, &erin].iter().all(|h| {
                let fx = h.lock().latest().decks.map(|d| d.fx);
                fx[0].slots[2].on && fx[1].slots[0].on && fx[1].slots[0].depth == 0.8
            })
        },
    )?;
    say("effects two joining DJs switch on reach every booth and the broadcaster");

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
    let everyone = [
        ("Alice", &alice),
        ("Bob", &bob),
        ("Carol", &carol),
        ("Dave", &dave),
        ("Erin", &erin),
    ];
    for (who, h) in everyone {
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
    let da = alice.lock().state_at(t).digest();
    for (who, h) in everyone {
        ensure!(
            h.lock().state_at(t).digest() == da,
            "room state differs on {who}'s booth"
        );
    }
    say("room state identical on all five booths");

    // Pings keep each end's "last heard" fresh, so a stalled link stands out.
    let settle = Duration::from_millis(2_500);
    if joined.elapsed() < settle {
        std::thread::sleep(settle - joined.elapsed());
    }
    for (who, h) in everyone {
        let links = h.lock().view().link.links;
        let quiet: Vec<Duration> = links.iter().map(|l| l.quiet_for).collect();
        ensure!(
            !quiet.is_empty() && quiet.iter().all(|q| *q < Duration::from_secs(2)),
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
    // Long enough for the echo to come round, and heard the same way.
    let n_fx = 48_000;
    let render_fx = |snap: &crate::render::AudioSnapshot, kind| {
        let mut lane = Lane::new(48_000.0, 0, kind);
        let (mut m, mut c) = (vec![0.0f32; n_fx * 2], vec![0.0f32; n_fx * 2]);
        lane.render(snap, t, &mut m, &mut c);
        m
    };
    let with = render_fx(&snaps.0, LaneKind::Live);
    let diff = max_diff(&with, &render_fx(&snaps.1, LaneKind::Live));
    ensure!(
        diff < 1e-6,
        "booths render the effects differently (max diff {diff})"
    );
    let diff = max_diff(
        &render_fx(&snaps.0, LaneKind::Broadcast),
        &render_fx(&snaps.2, LaneKind::Broadcast),
    );
    ensure!(
        diff < 1e-6,
        "the broadcaster renders the effects differently (max diff {diff})"
    );
    // And they're there: the same moment without them sounds different.
    let mut dry = snaps.0.timeline.clone();
    for (seq, (deck, slot)) in [(0u8, 2u8), (1, 0)].into_iter().enumerate() {
        dry.insert(Event {
            time: t - 1,
            origin: 0xD7,
            seq: seq as u64 + 1,
            action: Action::FxOn {
                deck,
                slot,
                on: false,
            },
        });
    }
    let dry = crate::render::AudioSnapshot {
        timeline: dry,
        tracks: snaps.0.tracks.clone(),
        local: snaps.0.local,
    };
    ensure!(
        max_diff(&with, &render_fx(&dry, LaneKind::Live)) > 0.01,
        "the effects made no difference"
    );
    let view = carol.lock().broadcast_view();
    let djs: Vec<&str> = view.djs.iter().map(|p| p.name.as_str()).collect();
    ensure!(
        djs == ["Alice", "Bob", "Dave", "Erin"],
        "broadcaster's window names {djs:?}"
    );
    say("all booths render identical audio for the same moment, effects included, the broadcaster's feed too");

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

    // Bob shares a track only he has. Dave accepts and gets it through the
    // host, which never accepted it; Erin declines.
    let bob_extra = bob_music.join("Bob - Shared Tone.wav");
    crate::audio::write_wav(
        &bob_extra,
        44_100,
        &crate::audio::test_signal(44_100, 3.0, 660.0),
    )?;
    bob.lock()
        .offer_share("Bob's test crate".into(), vec![bob_extra]);
    let offer = |h: &SessionHandle| {
        let v = h.lock().view();
        v.incoming
            .iter()
            .find(|s| s.title == "Bob's test crate")
            .map(|s| s.share_id)
    };
    wait_for(
        "Bob's offer to reach the other DJs",
        Duration::from_secs(10),
        || [&alice, &dave, &erin].iter().all(|h| offer(h).is_some()),
    )?;
    let share_id = offer(&dave).unwrap();
    dave.lock().respond_share(share_id, true);
    erin.lock().respond_share(share_id, false);
    wait_for(
        "Bob's shared track to reach Dave",
        Duration::from_secs(30),
        || {
            let v = dave.lock().view();
            v.incoming
                .iter()
                .any(|s| s.share_id == share_id && s.received == 1)
        },
    )?;
    wait_for("Bob to hear both answers", Duration::from_secs(5), || {
        let v = bob.lock().view();
        v.outgoing
            .iter()
            .any(|(title, _, replies)| title == "Bob's test crate" && replies.len() == 2)
    })?;
    say("a share from one joining DJ reaches another through the host, which passes it on without taking it");

    // Dave shares a folder live: the others see it, and a track Erin loads
    // from it comes over from Dave to every booth, through the host.
    let dave_dir = root.join("dave-live");
    std::fs::create_dir_all(&dave_dir)?;
    crate::audio::write_wav(
        &dave_dir.join("Dave - Live One.wav"),
        44_100,
        &crate::audio::test_signal(44_100, 2.0, 495.0),
    )?;
    let dave_lib = dave.lock().library();
    let live_key = crate::library::folder_key(&dave_dir);
    crate::library::scan(
        &dave_lib,
        live_key.clone(),
        crate::library::SourceKind::Folder(dave_dir.clone()),
    );
    wait_for(
        "Dave's folder to be listed",
        Duration::from_secs(10),
        || {
            dave_lib
                .lock()
                .lists
                .get(&live_key)
                .is_some_and(|l| !l.scanning)
        },
    )?;
    let listed = dave_lib.lock().lists[&live_key].tracks.clone();
    dave.lock()
        .share_live(live_key.clone(), "Dave's live picks".into(), listed);
    let live_track = |h: &SessionHandle| {
        let v = h.lock().view();
        v.room_crates
            .iter()
            .find(|c| c.name == "Dave's live picks")
            .and_then(|c| c.tracks.first().cloned())
    };
    wait_for(
        "Dave's live list to reach the other DJs",
        Duration::from_secs(10),
        || {
            [&alice, &bob, &erin]
                .iter()
                .all(|h| live_track(h).is_some())
        },
    )?;
    ensure!(
        carol.lock().view().room_crates.is_empty(),
        "a broadcaster got a list shared live"
    );
    let info = live_track(&erin).expect("checked above");
    ensure!(
        erin.lock().track_status(info.id).is_none(),
        "a list shared live sent its tracks before anyone loaded one"
    );
    erin.lock().load_room_track(3, info.clone());
    let took = wait_for(
        "the track Erin loaded to reach every booth",
        Duration::from_secs(30),
        || {
            [&alice, &bob, &carol, &dave, &erin]
                .iter()
                .all(|h| h.lock().track_status(info.id) == Some(TrackStatus::Ready))
        },
    )?;
    dave.lock().unshare_live(&live_key);
    wait_for("Dave's live list to go", Duration::from_secs(5), || {
        [&alice, &bob, &erin]
            .iter()
            .all(|h| h.lock().view().room_crates.is_empty())
    })?;
    say(&format!(
        "a list shared live reaches the other DJs, and a track loaded from it comes over from its owner to every booth in {:.2} s",
        took.as_secs_f64()
    ));

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
        v.link.linked() && v.link.rejoin.is_none()
    })?;
    wait_for("the host to see Bob back", Duration::from_secs(5), || {
        alice.lock().roster().djs.len() == 4
    })?;
    wait_for(
        "what Bob did while he was out to reach the other DJs",
        Duration::from_secs(5),
        || {
            dave.lock().latest().decks[3].volume == 0.35
                && erin.lock().latest().decks[3].volume == 0.35
        },
    )?;
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
        "a dropped link: Bob gets back in by himself after {:.1} s, and every DJ has everyone's moves from meanwhile",
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
    for h in [&bob, &carol, &dave, &erin] {
        h.lock().leave();
    }
    Ok(())
}
