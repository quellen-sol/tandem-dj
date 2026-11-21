//! How much CPU the effects cost: render a lane at 48 kHz in 480-frame
//! callbacks (10 ms, as a sound card asks for them) with deck A playing,
//! first with every effect off, then with one echo, reverb or phaser on,
//! then with all twelve slots on, and compare with real time. Each figure
//! is the best of several runs, as a percentage of one core per lane.
//!
//! `cargo run --release -p tandem-core --example fx_bench`
use std::sync::Arc;
use std::time::Instant;
use tandem_core::audio::{test_signal, TrackAudio};
use tandem_core::grid::BeatGrid;
use tandem_core::model::{Action, Event, FxKind, Timeline, TrackId, DECKS, FX_SLOTS};
use tandem_core::render::{AudioSnapshot, Lane, LaneKind, LocalParams};

const SR: f64 = 48_000.0;
const SECS: i64 = 20;
const RUNS: usize = 7;

fn main() {
    let audio = Arc::new(TrackAudio::new(
        TrackId([7; 32]),
        44_100,
        test_signal(44_100, SECS as f64 + 5.0, 440.0),
    ));
    // Settings moved, nothing switched on: the effects aren't touched.
    let mut moved = timeline(&audio, &[]);
    for slot in 0..FX_SLOTS as u8 {
        moved.insert(event(
            100 + slot as u64,
            Action::FxDepth {
                deck: 0,
                slot,
                v: 0.9,
            },
        ));
    }
    let all: Vec<(u8, u8, FxKind)> = (0..DECKS as u8)
        .flat_map(|d| (0..FX_SLOTS as u8).map(move |s| (d, s, FxKind::ALL[s as usize])))
        .collect();
    let cases: Vec<(String, Timeline)> = [
        (
            "every effect off (the lane itself)".to_string(),
            timeline(&audio, &[]),
        ),
        ("settings moved, nothing on".to_string(), moved),
    ]
    .into_iter()
    .chain(FxKind::ALL.into_iter().map(|k| {
        (
            format!("{} on deck A", k.label()),
            timeline(&audio, &[(0, 0, k)]),
        )
    }))
    .chain(std::iter::once((
        "all twelve slots on (echo, reverb, phaser on every deck)".to_string(),
        timeline(&audio, &all),
    )))
    .collect();
    // Round by round, so every case sees the same machine; the best of each.
    time(&cases[0].1, &audio);
    let mut best = vec![f64::MAX; cases.len()];
    for _ in 0..RUNS {
        for (b, (_, tl)) in best.iter_mut().zip(&cases) {
            *b = b.min(time(tl, &audio));
        }
    }
    println!(
        "{:.2}% of one core per lane with every effect off",
        best[0] * 100.0
    );
    for ((name, _), b) in cases.iter().zip(&best).skip(1) {
        println!(
            "{name}: {:+.3}% of one core per lane",
            (b - best[0]) * 100.0
        );
    }
}

fn event(seq: u64, action: Action) -> Event {
    Event {
        time: 0,
        origin: 1,
        seq,
        action,
    }
}

/// Deck A playing the tone on a 124 BPM grid, with `on` (deck, slot,
/// effect) switched on at full depth, BEATS ¾.
fn timeline(audio: &TrackAudio, on: &[(u8, u8, FxKind)]) -> Timeline {
    let mut tl = Timeline::new(0);
    tl.insert(event(
        1,
        Action::Load {
            deck: 0,
            track: audio.id,
            duration: audio.duration(),
            cue: 0.0,
            grid: BeatGrid::new(0.0, 124.0).map(Box::new),
            key: -1,
        },
    ));
    tl.insert(event(2, Action::Play { deck: 0 }));
    let mut seq = 10;
    for &(deck, slot, kind) in on {
        for a in [
            Action::FxSelect { deck, slot, kind },
            Action::FxDepth { deck, slot, v: 1.0 },
            Action::FxBeats { deck, beats: 4 },
            Action::FxOn {
                deck,
                slot,
                on: true,
            },
        ] {
            seq += 1;
            tl.insert(event(seq, a));
        }
    }
    tl
}

/// Seconds of CPU per second of audio, for one run.
fn time(tl: &Timeline, audio: &Arc<TrackAudio>) -> f64 {
    let snap = AudioSnapshot {
        timeline: tl.clone(),
        tracks: vec![audio.clone()],
        local: LocalParams::default(),
    };
    let frames = 480;
    let (mut m, mut c) = (vec![0.0f32; frames * 2], vec![0.0f32; frames * 2]);
    let mut lane = Lane::new(SR, 0, LaneKind::Live);
    let start = Instant::now();
    for i in 0..SECS * 100 {
        lane.render(&snap, i * 10_000, &mut m, &mut c);
    }
    let took = start.elapsed().as_secs_f64();
    std::hint::black_box(&m);
    took / SECS as f64
}
