//! Room state as a pure function of timestamped events.
//!
//! A deck's playhead is modelled analytically: while playing,
//! `position(t) = anchor_pos + (t - anchor_time) * rate`. Every event
//! re-anchors at its own timestamp. So given the same events, any machine
//! can compute exactly where every deck is at any shared time, including
//! times in the past (that is how the delayed broadcast lane works) and
//! including events that arrive late over the network.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

pub const DECKS: usize = 2;
pub const HOT_CUES: usize = 8;

/// Content hash (BLAKE3) of a track file. Two peers with the same file have
/// the same id, whatever they named it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub [u8; 32]);

impl TrackId {
    pub fn of_bytes(b: &[u8]) -> Self {
        TrackId(*blake3::hash(b).as_bytes())
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, o) in out.iter_mut().enumerate() {
            *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(TrackId(out))
    }

    pub fn short(&self) -> String {
        self.to_hex()[..8].to_string()
    }
}

impl fmt::Debug for TrackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TrackId({})", self.short())
    }
}

impl fmt::Display for TrackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.short())
    }
}

impl Serialize for TrackId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for TrackId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        TrackId::from_hex(&s).ok_or_else(|| serde::de::Error::custom("bad track id"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeckState {
    pub track: Option<TrackId>,
    /// Track length in seconds (from the loader's decode, so identical everywhere).
    pub duration: f64,
    pub playing: bool,
    /// Shared time (us) at which `anchor_pos` was true.
    pub anchor_time: i64,
    /// Track position in seconds at `anchor_time`.
    pub anchor_pos: f64,
    /// Playback speed, 1.0 = original. (No key lock yet: pitch follows.)
    pub tempo: f64,
    /// Temporary speed offset while a pitch-bend button is held (e.g. -0.04).
    pub bend: f64,
    pub cue_point: f64,
    /// True while CUE is held from the cue point (CDJ-style preview).
    pub cue_preview: bool,
    pub hot_cues: [Option<f64>; HOT_CUES],
    pub volume: f32,
    /// [low, mid, high]; 0.5 = unity, 0 = kill.
    pub eq: [f32; 3],
    /// Channel trim (gain); 0.5 = unity.
    pub trim: f32,
    /// One-knob filter: 0.5 = off, below = low-pass, above = high-pass.
    pub filter: f32,
    /// True while the DJ's hand is on the jog platter (vinyl mode): the
    /// record is held, and jog movement moves the playhead directly.
    pub scratching: bool,
    /// Bumped on every discontinuity (load, play, pause, jump). The renderer
    /// hard-syncs when it changes and smoothly chases otherwise.
    pub gen: u64,
    /// Peer id of whoever last touched this deck.
    pub last_touch: u64,
}

impl Default for DeckState {
    fn default() -> Self {
        Self {
            track: None,
            duration: 0.0,
            playing: false,
            anchor_time: 0,
            anchor_pos: 0.0,
            tempo: 1.0,
            bend: 0.0,
            cue_point: 0.0,
            cue_preview: false,
            hot_cues: [None; HOT_CUES],
            volume: 1.0,
            eq: [0.5; 3],
            trim: 0.5,
            filter: 0.5,
            scratching: false,
            gen: 0,
            last_touch: 0,
        }
    }
}

impl DeckState {
    pub fn rate(&self) -> f64 {
        self.tempo * (1.0 + self.bend)
    }

    /// Track position (seconds) at shared time `t`.
    pub fn position_at(&self, t: i64) -> f64 {
        if self.track.is_none() {
            return 0.0;
        }
        let p = if self.playing && !self.scratching {
            self.anchor_pos + (t - self.anchor_time) as f64 * 1e-6 * self.rate()
        } else {
            self.anchor_pos
        };
        p.clamp(0.0, self.duration.max(0.0))
    }

    fn reanchor(&mut self, t: i64) {
        self.anchor_pos = self.position_at(t);
        self.anchor_time = t;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoomState {
    pub decks: [DeckState; DECKS],
    /// 0 = full deck A, 1 = full deck B.
    pub crossfader: f32,
    pub xf_touch: u64,
}

impl Default for RoomState {
    fn default() -> Self {
        Self {
            decks: [DeckState::default(); DECKS],
            crossfader: 0.5,
            xf_touch: 0,
        }
    }
}

/// Everything a DJ can do to the shared room. Deterministic: applying the
/// same actions in the same order gives bit-identical state on any machine.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "a")]
pub enum Action {
    Load {
        deck: u8,
        track: TrackId,
        duration: f64,
        cue: f64,
    },
    Eject {
        deck: u8,
    },
    Play {
        deck: u8,
    },
    Pause {
        deck: u8,
    },
    CueDown {
        deck: u8,
    },
    CueUp {
        deck: u8,
    },
    /// Jump to the hot cue and play; if the slot is empty, set it here.
    HotCue {
        deck: u8,
        slot: u8,
    },
    ClearHotCue {
        deck: u8,
        slot: u8,
    },
    Seek {
        deck: u8,
        pos: f64,
    },
    Tempo {
        deck: u8,
        tempo: f64,
    },
    Bend {
        deck: u8,
        bend: f64,
    },
    /// Shift the playhead by `delta` seconds (jog wheel). The renderer turns
    /// small shifts into a smooth speed change rather than a jump.
    Nudge {
        deck: u8,
        delta: f64,
    },
    Volume {
        deck: u8,
        v: f32,
    },
    Eq {
        deck: u8,
        band: u8,
        v: f32,
    },
    Trim {
        deck: u8,
        v: f32,
    },
    Filter {
        deck: u8,
        v: f32,
    },
    /// Hand on / off the jog platter (vinyl mode).
    JogTouch {
        deck: u8,
        down: bool,
    },
    Crossfader {
        v: f32,
    },
}

impl Action {
    pub fn deck(&self) -> Option<usize> {
        use Action::*;
        match *self {
            Load { deck, .. }
            | Eject { deck }
            | Play { deck }
            | Pause { deck }
            | CueDown { deck }
            | CueUp { deck }
            | HotCue { deck, .. }
            | ClearHotCue { deck, .. }
            | Seek { deck, .. }
            | Tempo { deck, .. }
            | Bend { deck, .. }
            | Nudge { deck, .. }
            | Volume { deck, .. }
            | Eq { deck, .. }
            | Trim { deck, .. }
            | Filter { deck, .. }
            | JogTouch { deck, .. } => Some(deck as usize),
            Crossfader { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Shared time (us) at which the action happened.
    pub time: i64,
    /// Peer id of the DJ who did it.
    pub origin: u64,
    /// Per-origin sequence number; (origin, seq) identifies an event.
    pub seq: u64,
    pub action: Action,
}

impl Event {
    /// Total order used everywhere, so ties resolve identically on every peer.
    pub fn key(&self) -> (i64, u64, u64) {
        (self.time, self.origin, self.seq)
    }
}

impl RoomState {
    pub fn apply(&mut self, ev: &Event) {
        let t = ev.time;
        if let Action::Crossfader { v } = ev.action {
            self.crossfader = v.clamp(0.0, 1.0);
            self.xf_touch = ev.origin;
            return;
        }
        let Some(deck) = ev.action.deck() else { return };
        let Some(d) = self.decks.get_mut(deck) else {
            return;
        };
        d.last_touch = ev.origin;
        match ev.action {
            Action::Load {
                track,
                duration,
                cue,
                ..
            } => {
                d.track = Some(track);
                d.duration = duration.max(0.0);
                d.playing = false;
                d.cue_preview = false;
                d.anchor_time = t;
                d.anchor_pos = cue.clamp(0.0, d.duration);
                d.cue_point = d.anchor_pos;
                d.hot_cues = [None; HOT_CUES];
                d.bend = 0.0;
                d.scratching = false;
                d.gen += 1;
            }
            Action::Eject { .. } => {
                d.track = None;
                d.duration = 0.0;
                d.playing = false;
                d.cue_preview = false;
                d.anchor_time = t;
                d.anchor_pos = 0.0;
                d.cue_point = 0.0;
                d.hot_cues = [None; HOT_CUES];
                d.gen += 1;
            }
            Action::Play { .. } => {
                if d.track.is_some() {
                    if !d.playing {
                        d.reanchor(t);
                        d.playing = true;
                        d.gen += 1;
                    }
                    d.cue_preview = false;
                }
            }
            Action::Pause { .. } => {
                if d.playing {
                    d.reanchor(t);
                    d.playing = false;
                    d.gen += 1;
                }
                d.cue_preview = false;
            }
            Action::CueDown { .. } => {
                if d.track.is_none() {
                    return;
                }
                if d.playing && !d.cue_preview {
                    // Playing: back to the cue point and stop.
                    d.playing = false;
                    d.anchor_pos = d.cue_point;
                    d.anchor_time = t;
                    d.gen += 1;
                } else if !d.playing {
                    let pos = d.position_at(t);
                    if (pos - d.cue_point).abs() < 0.01 {
                        // Paused on the cue point: preview while held.
                        d.playing = true;
                        d.cue_preview = true;
                        d.anchor_pos = d.cue_point;
                        d.anchor_time = t;
                        d.gen += 1;
                    } else {
                        // Paused elsewhere: set a new cue point here.
                        d.cue_point = pos;
                        d.anchor_pos = pos;
                        d.anchor_time = t;
                    }
                }
            }
            Action::CueUp { .. } => {
                if d.cue_preview {
                    d.cue_preview = false;
                    d.playing = false;
                    d.anchor_pos = d.cue_point;
                    d.anchor_time = t;
                    d.gen += 1;
                }
            }
            Action::HotCue { slot, .. } => {
                let s = slot as usize;
                if s >= HOT_CUES || d.track.is_none() {
                    return;
                }
                match d.hot_cues[s] {
                    Some(p) => {
                        d.anchor_pos = p;
                        d.anchor_time = t;
                        d.playing = true;
                        d.cue_preview = false;
                        d.gen += 1;
                    }
                    None => d.hot_cues[s] = Some(d.position_at(t)),
                }
            }
            Action::ClearHotCue { slot, .. } => {
                if let Some(h) = d.hot_cues.get_mut(slot as usize) {
                    *h = None;
                }
            }
            Action::Seek { pos, .. } => {
                if d.track.is_some() {
                    d.anchor_pos = pos.clamp(0.0, d.duration);
                    d.anchor_time = t;
                    d.gen += 1;
                }
            }
            Action::Tempo { tempo, .. } => {
                d.reanchor(t);
                d.tempo = tempo.clamp(0.5, 1.5);
            }
            Action::Bend { bend, .. } => {
                d.reanchor(t);
                d.bend = bend.clamp(-0.5, 0.5);
            }
            Action::Nudge { delta, .. } => {
                if d.track.is_some() {
                    d.reanchor(t);
                    d.anchor_pos = (d.anchor_pos + delta).clamp(0.0, d.duration);
                }
            }
            Action::Volume { v, .. } => d.volume = v.clamp(0.0, 1.0),
            Action::Trim { v, .. } => d.trim = v.clamp(0.0, 1.0),
            Action::Filter { v, .. } => d.filter = v.clamp(0.0, 1.0),
            Action::JogTouch { down, .. } => {
                if d.track.is_some() && d.scratching != down {
                    // Re-anchor so the playhead is continuous either way.
                    d.reanchor(t);
                    d.scratching = down;
                }
            }
            Action::Eq { band, v, .. } => {
                if let Some(e) = d.eq.get_mut(band as usize) {
                    *e = v.clamp(0.0, 1.0);
                }
            }
            Action::Crossfader { .. } => {}
        }
    }

    /// Hash of every field, used to check two booths agree.
    pub fn digest(&self) -> u64 {
        let mut h = Fnv::new();
        for d in &self.decks {
            match d.track {
                Some(id) => h.bytes(&id.0),
                None => h.u64(0),
            }
            h.f64(d.duration);
            h.u64(d.playing as u64);
            h.u64(d.anchor_time as u64);
            h.f64(d.anchor_pos);
            h.f64(d.tempo);
            h.f64(d.bend);
            h.f64(d.cue_point);
            h.u64(d.cue_preview as u64);
            for c in &d.hot_cues {
                h.f64(c.unwrap_or(-1.0));
            }
            h.u64(d.volume.to_bits() as u64);
            for e in &d.eq {
                h.u64(e.to_bits() as u64);
            }
            h.u64(d.trim.to_bits() as u64);
            h.u64(d.filter.to_bits() as u64);
            h.u64(d.scratching as u64);
            h.u64(d.gen);
        }
        h.u64(self.crossfader.to_bits() as u64);
        h.0
    }
}

struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= x as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes())
    }
    fn f64(&mut self, v: f64) {
        self.u64(v.to_bits())
    }
}

/// A base state plus the recent events after it, kept sorted.
///
/// `state_at(t)` folds the events up to `t` onto the base. Old events are
/// periodically folded into the base (`compact`) so the list stays short.
/// A late event is simply inserted at its timestamp, and every later query
/// reflects it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Timeline {
    base_time: i64,
    base: RoomState,
    events: Vec<Event>,
}

impl Timeline {
    pub fn new(base_time: i64) -> Self {
        Self {
            base_time,
            base: RoomState::default(),
            events: Vec::new(),
        }
    }

    pub fn base_time(&self) -> i64 {
        self.base_time
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Insert an event; returns false if we already had it. Events older than
    /// the base are clamped to the base time (they arrived very late).
    pub fn insert(&mut self, mut ev: Event) -> bool {
        if ev.time < self.base_time {
            log::warn!(
                "event arrived {} ms too late; applying it now",
                (self.base_time - ev.time) / 1000
            );
            ev.time = self.base_time;
        }
        if self
            .events
            .iter()
            .any(|e| e.origin == ev.origin && e.seq == ev.seq)
        {
            return false;
        }
        let idx = self.events.partition_point(|e| e.key() <= ev.key());
        self.events.insert(idx, ev);
        true
    }

    pub fn state_at(&self, t: i64) -> RoomState {
        self.cursor(t).0
    }

    pub fn latest(&self) -> RoomState {
        self.state_at(i64::MAX)
    }

    /// State at `t` plus the index of the first event after `t`, for
    /// incremental advancing with [`Timeline::advance`].
    pub fn cursor(&self, t: i64) -> (RoomState, usize) {
        let mut s = self.base;
        let mut i = 0;
        self.advance(&mut s, &mut i, t);
        (s, i)
    }

    pub fn advance(&self, s: &mut RoomState, i: &mut usize, t: i64) {
        while *i < self.events.len() && self.events[*i].time <= t {
            s.apply(&self.events[*i]);
            *i += 1;
        }
    }

    /// Fold every event before `before` into the base.
    pub fn compact(&mut self, before: i64) {
        if before <= self.base_time {
            return;
        }
        let n = self.events.partition_point(|e| e.time < before);
        for e in self.events.drain(..n) {
            self.base.apply(&e);
        }
        self.base_time = before;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(time: i64, origin: u64, seq: u64, action: Action) -> Event {
        Event {
            time,
            origin,
            seq,
            action,
        }
    }

    fn tid(n: u8) -> TrackId {
        TrackId([n; 32])
    }

    #[test]
    fn track_id_hex_roundtrip() {
        let id = TrackId::of_bytes(b"hello");
        assert_eq!(TrackId::from_hex(&id.to_hex()), Some(id));
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<TrackId>(&json).unwrap(), id);
    }

    #[test]
    fn playhead_is_analytic() {
        let mut tl = Timeline::new(0);
        tl.insert(ev(
            1_000_000,
            1,
            1,
            Action::Load {
                deck: 0,
                track: tid(1),
                duration: 300.0,
                cue: 0.0,
            },
        ));
        tl.insert(ev(2_000_000, 1, 2, Action::Play { deck: 0 }));
        tl.insert(ev(
            4_000_000,
            1,
            3,
            Action::Tempo {
                deck: 0,
                tempo: 1.1,
            },
        ));
        let s = tl.state_at(6_000_000);
        // 2 s at 1.0 then 2 s at 1.1
        assert!((s.decks[0].position_at(6_000_000) - 4.2).abs() < 1e-9);
    }

    #[test]
    fn late_events_converge_regardless_of_arrival_order() {
        let events = vec![
            ev(
                1_000,
                1,
                1,
                Action::Load {
                    deck: 0,
                    track: tid(1),
                    duration: 200.0,
                    cue: 0.0,
                },
            ),
            ev(
                2_000,
                2,
                1,
                Action::Load {
                    deck: 1,
                    track: tid(2),
                    duration: 180.0,
                    cue: 5.0,
                },
            ),
            ev(500_000, 1, 2, Action::Play { deck: 0 }),
            ev(700_000, 2, 2, Action::Crossfader { v: 0.2 }),
            ev(900_000, 2, 3, Action::Play { deck: 1 }),
            ev(
                900_000,
                1,
                3,
                Action::Tempo {
                    deck: 0,
                    tempo: 1.02,
                },
            ),
            ev(1_300_000, 1, 4, Action::HotCue { deck: 0, slot: 0 }),
            ev(
                1_700_000,
                2,
                4,
                Action::Nudge {
                    deck: 1,
                    delta: 0.003,
                },
            ),
            ev(2_000_000, 1, 5, Action::HotCue { deck: 0, slot: 0 }),
        ];
        let mut a = Timeline::new(0);
        for e in &events {
            a.insert(*e);
        }
        let mut b = Timeline::new(0);
        let mut shuffled = events.clone();
        fastrand::Rng::with_seed(3).shuffle(&mut shuffled);
        for e in &shuffled {
            b.insert(*e);
            b.insert(*e); // duplicates are ignored
        }
        for t in [0, 600_000, 1_000_000, 1_800_000, 5_000_000] {
            assert_eq!(
                a.state_at(t).digest(),
                b.state_at(t).digest(),
                "diverged at {t}"
            );
        }
        // Compacting doesn't change answers after the base.
        let mut c = a.clone();
        c.compact(1_000_000);
        assert_eq!(c.state_at(3_000_000), a.state_at(3_000_000));
    }

    #[test]
    fn cdj_cue_behaviour() {
        let mut s = RoomState::default();
        let mut seq = 0;
        let mut go = |s: &mut RoomState, t: i64, a: Action| {
            seq += 1;
            s.apply(&ev(t, 1, seq, a));
        };
        go(
            &mut s,
            0,
            Action::Load {
                deck: 0,
                track: tid(1),
                duration: 100.0,
                cue: 0.0,
            },
        );
        go(&mut s, 1_000_000, Action::Play { deck: 0 });
        // CUE while playing: back to cue point, paused.
        go(&mut s, 3_000_000, Action::CueDown { deck: 0 });
        assert!(!s.decks[0].playing);
        assert_eq!(s.decks[0].position_at(3_000_000), 0.0);
        go(&mut s, 3_100_000, Action::CueUp { deck: 0 });
        // CUE held on the cue point: preview, release returns.
        go(&mut s, 4_000_000, Action::CueDown { deck: 0 });
        assert!(s.decks[0].playing && s.decks[0].cue_preview);
        go(&mut s, 5_000_000, Action::CueUp { deck: 0 });
        assert!(!s.decks[0].playing);
        assert_eq!(s.decks[0].position_at(5_000_000), 0.0);
        // Seek away while paused, then CUE sets a new cue point.
        go(&mut s, 6_000_000, Action::Seek { deck: 0, pos: 42.0 });
        go(&mut s, 6_100_000, Action::CueDown { deck: 0 });
        assert_eq!(s.decks[0].cue_point, 42.0);
    }

    #[test]
    fn scratching_holds_the_record_and_moves_with_the_jog() {
        let mut s = RoomState::default();
        s.apply(&ev(
            0,
            1,
            1,
            Action::Load {
                deck: 0,
                track: tid(3),
                duration: 100.0,
                cue: 0.0,
            },
        ));
        s.apply(&ev(0, 1, 2, Action::Play { deck: 0 }));
        s.apply(&ev(
            10_000_000,
            1,
            3,
            Action::JogTouch {
                deck: 0,
                down: true,
            },
        ));
        // Hand on the platter: the record stops where it was.
        assert!((s.decks[0].position_at(12_000_000) - 10.0).abs() < 1e-9);
        s.apply(&ev(
            12_000_000,
            1,
            4,
            Action::Nudge {
                deck: 0,
                delta: -0.5,
            },
        ));
        assert!((s.decks[0].position_at(12_500_000) - 9.5).abs() < 1e-9);
        // Let go: it plays on from there.
        s.apply(&ev(
            13_000_000,
            1,
            5,
            Action::JogTouch {
                deck: 0,
                down: false,
            },
        ));
        assert!((s.decks[0].position_at(14_000_000) - 10.5).abs() < 1e-9);
    }

    #[test]
    fn hot_cue_sets_then_jumps() {
        let mut s = RoomState::default();
        s.apply(&ev(
            0,
            1,
            1,
            Action::Load {
                deck: 1,
                track: tid(9),
                duration: 100.0,
                cue: 0.0,
            },
        ));
        s.apply(&ev(0, 1, 2, Action::Play { deck: 1 }));
        s.apply(&ev(10_000_000, 1, 3, Action::HotCue { deck: 1, slot: 2 }));
        assert_eq!(s.decks[1].hot_cues[2], Some(10.0));
        s.apply(&ev(20_000_000, 1, 4, Action::HotCue { deck: 1, slot: 2 }));
        assert!((s.decks[1].position_at(21_000_000) - 11.0).abs() < 1e-9);
    }
}
