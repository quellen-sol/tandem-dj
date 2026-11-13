//! Room state as a pure function of timestamped events.
//!
//! A deck's playhead is modelled analytically: while playing,
//! `position(t) = anchor_pos + (t - anchor_time) * rate`. Every event
//! re-anchors at its own timestamp. So given the same events, any machine
//! can compute exactly where every deck is at any shared time, including
//! times in the past (that is how the delayed broadcast lane works) and
//! including events that arrive late over the network.

use crate::grid::BeatGrid;
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
    /// Where the track's beats and bars are (None: no steady beat found).
    pub grid: Option<BeatGrid>,
    /// Tempo-locked to the other deck (SYNC on).
    pub synced: bool,
    /// Detected key: 0-11 major (C = 0), 12-23 minor; -1 unknown.
    pub key: i8,
    /// Key shift in semitones (pitch only, tempo unchanged).
    pub key_shift: i8,
    /// Key lock: tempo changes don't change pitch.
    pub key_lock: bool,
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
            grid: None,
            synced: false,
            key: -1,
            key_shift: 0,
            key_lock: true,
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

    /// How much higher (as a frequency ratio) the deck sounds than the
    /// original recording.
    pub fn pitch_ratio(&self) -> f64 {
        let shift = 2f64.powf(self.key_shift as f64 / 12.0);
        if self.key_lock {
            shift
        } else {
            shift * self.rate()
        }
    }

    /// The key the deck is sounding in right now (after key shift and, with
    /// key lock off, tempo), rounded to the nearest semitone.
    pub fn sounding_key(&self) -> Option<i8> {
        if self.key < 0 {
            return None;
        }
        let semis = (12.0 * self.pitch_ratio().log2()).round() as i32;
        let pc = (self.key as i32 % 12 + semis).rem_euclid(12);
        Some((pc + if self.key >= 12 { 12 } else { 0 }) as i8)
    }

    /// The track's own BPM where the playhead is at time `t` (from the grid).
    pub fn bpm_at(&self, t: i64) -> Option<f64> {
        let g = self.grid.as_ref()?;
        self.track.map(|_| g.bpm_at(self.position_at(t)))
    }

    /// The deck's BPM at time `t`, as the tempo fader sets it (ignores nudges).
    pub fn effective_bpm_at(&self, t: i64) -> Option<f64> {
        self.bpm_at(t).map(|b| b * self.tempo)
    }

    /// Position within the beat (0..1) at time `t`, counting beats `mult`
    /// times as fast (0.5 = half time).
    fn beat_phase(&self, mult: f64, t: i64) -> f64 {
        self.grid
            .map(|g| g.phase(self.position_at(t), mult))
            .unwrap_or(0.0)
    }

    /// When (shared time) the playing deck will next reach a grid marker
    /// where the tempo changes, if that's within `horizon` us of `now`.
    pub fn next_tempo_change(&self, now: i64, horizon: i64) -> Option<i64> {
        let g = self.grid.as_ref()?;
        let rate = self.rate();
        if self.track.is_none() || !self.playing || self.scratching || rate <= 1e-6 {
            return None;
        }
        let pos = self.position_at(now);
        let m = g.markers();
        let here = m[g.section(pos)].bpm;
        let next = m.iter().find(|x| x.pos > pos && x.bpm != here)?;
        let dt = ((next.pos - pos) / rate * 1e6).ceil() as i64;
        (dt <= horizon).then_some(now + dt)
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
///
/// Not `Copy`: beatgrids are boxed so the common, tiny events stay small.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "a")]
pub enum Action {
    Load {
        deck: u8,
        track: TrackId,
        duration: f64,
        cue: f64,
        /// The loader's beatgrid, so every booth uses the same one.
        #[serde(default)]
        grid: Option<Box<BeatGrid>>,
        /// Detected key (see [`DeckState::key`]), -1 unknown.
        #[serde(default = "no_key")]
        key: i8,
    },
    /// Replace the beatgrid of `track` (ignored if the deck has moved on to
    /// another track). None: no beat.
    SetGrid {
        deck: u8,
        track: TrackId,
        grid: Option<Box<BeatGrid>>,
    },
    /// The deck has just reached a grid marker where the tempo changes: if
    /// it (or the other deck) is synced, re-match the tempos so the beats
    /// stay locked. Scheduled by a booth for the exact moment.
    Relock {
        deck: u8,
    },
    /// Key lock on/off.
    KeyLock {
        deck: u8,
        on: bool,
    },
    /// Shift the key by this many semitones from the original.
    KeyShift {
        deck: u8,
        semitones: i8,
    },
    /// SYNC on/off: match the other deck's tempo and beat phase, and keep
    /// both decks' tempos locked together while on.
    Sync {
        deck: u8,
        on: bool,
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
            | SetGrid { deck, .. }
            | Relock { deck }
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
            | JogTouch { deck, .. }
            | Sync { deck, .. }
            | KeyLock { deck, .. }
            | KeyShift { deck, .. } => Some(deck as usize),
            Crossfader { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
        if deck >= DECKS {
            return;
        }
        if let Action::Sync { on, .. } = ev.action {
            self.decks[deck].last_touch = ev.origin;
            self.decks[deck].synced = on && self.sync_deck(deck, t, true);
            return;
        }
        if let Action::Relock { .. } = ev.action {
            // Automatic, so it doesn't count as anyone touching the deck.
            self.relock(deck, t);
            return;
        }
        let was_playing = self.decks[deck].playing;
        // Only a load needs the whole previous deck (and DeckState is big).
        let before = matches!(ev.action, Action::Load { .. }).then(|| self.decks[deck]);
        let jumps = matches!(
            ev.action,
            Action::Seek { .. }
                | Action::HotCue { .. }
                | Action::CueDown { .. }
                | Action::CueUp { .. }
                | Action::Nudge { .. }
                | Action::JogTouch { .. }
        );
        let bpm_before = if jumps {
            self.decks[deck].bpm_at(t)
        } else {
            None
        };
        self.apply_deck(deck, ev);
        match ev.action {
            Action::Load { .. } if was_playing => {
                if let Some(before) = before {
                    self.start_on_beat(deck, t, before)
                }
            }
            Action::Tempo { .. } => self.follow_tempo(deck, t),
            Action::Play { .. } if !was_playing && self.decks[deck].synced => {
                // Starting a synced deck: start it on the beat.
                self.sync_deck(deck, t, false);
            }
            // The deck's own BPM may have changed under a sync lock.
            Action::SetGrid { .. } => self.relock(deck, t),
            _ if jumps && self.decks[deck].bpm_at(t) != bpm_before => self.relock(deck, t),
            _ => {}
        }
    }

    /// A track loaded onto a deck that was playing starts playing at once,
    /// placed so its beats land on the beat of what's playing: the other
    /// deck if it's playing, otherwise the track it replaced. It starts on
    /// its first beat (or the first one it can reach without going before
    /// 0:00). A synced deck stays synced.
    fn start_on_beat(&mut self, x: usize, t: i64, prev: DeckState) {
        let other = self.decks[(x + 1) % DECKS];
        let d = &mut self.decks[x];
        if d.track.is_none() {
            return;
        }
        d.playing = true;
        d.anchor_time = t;
        let Some(g) = d.grid else {
            d.anchor_pos = 0.0;
            return;
        };
        let reference = if other.playing && other.grid.is_some() && other.track.is_some() {
            Some(other)
        } else if prev.grid.is_some() && prev.track.is_some() {
            Some(prev)
        } else {
            None
        };
        // Start on the first marker (bar 1), or the first beat after it we
        // can reach without going before 0:00.
        let first = g.first_beat().max(0.0);
        d.anchor_pos = match reference.and_then(|r| r.bpm_at(t).map(|b| (r, b))) {
            None => g.next_beat(first),
            Some((r, r_bpm)) => {
                // Seconds until the reference's next beat.
                let phase = r.beat_phase(1.0, t);
                let wait = if phase < 1e-9 {
                    0.0
                } else {
                    (1.0 - phase) * 60.0 / (r_bpm * r.rate())
                };
                let lead = wait * d.rate();
                g.next_beat(first.max(lead)) - lead
            }
        };
        if prev.synced && other.grid.is_some() && other.track.is_some() {
            self.decks[x].synced = self.sync_deck(x, t, true);
        }
    }

    /// Match deck `x` to the other deck: tempo (if `match_tempo`) and beat
    /// phase. Uses half/double time when that's the smaller change (e.g. an
    /// 87 BPM track against 174). Returns false if either deck lacks a grid.
    fn sync_deck(&mut self, x: usize, t: i64, match_tempo: bool) -> bool {
        let y = (x + 1) % DECKS;
        let (Some(bx), Some(by)) = (self.decks[x].bpm_at(t), self.decks[y].bpm_at(t)) else {
            return false;
        };
        let phase_y = self.decks[y].beat_phase(1.0, t);
        let target = by * self.decks[y].tempo;
        let k = [1.0, 0.5, 2.0]
            .into_iter()
            .min_by(|a, b| {
                let ca = (target / (bx * a)).ln().abs();
                let cb = (target / (bx * b)).ln().abs();
                ca.partial_cmp(&cb).unwrap()
            })
            .unwrap_or(1.0);
        let beat_bpm = bx * k;
        let d = &mut self.decks[x];
        d.reanchor(t);
        if match_tempo {
            d.tempo = (target / beat_bpm).clamp(0.5, 1.5);
        }
        let mut diff = phase_y - d.beat_phase(k, t);
        if diff >= 0.5 {
            diff -= 1.0;
        } else if diff < -0.5 {
            diff += 1.0;
        }
        let shift = diff * 60.0 / beat_bpm;
        if shift.abs() > 0.0005 {
            d.anchor_pos = (d.anchor_pos + shift).clamp(0.0, d.duration);
            d.gen += 1;
        }
        true
    }

    /// While either deck is synced, a tempo change on one carries the other.
    fn follow_tempo(&mut self, moved: usize, t: i64) {
        let other = (moved + 1) % DECKS;
        if self.decks[moved].synced || self.decks[other].synced {
            self.lock_tempo(other, t);
        }
    }

    /// Deck `c`'s own BPM just changed (its grid was edited, or it reached
    /// or jumped to a section with another tempo). If it's synced, it keeps
    /// matching the other deck; if the other deck is synced, that follows it.
    fn relock(&mut self, c: usize, t: i64) {
        let other = (c + 1) % DECKS;
        if self.decks[c].synced {
            self.lock_tempo(c, t);
        } else if self.decks[other].synced {
            self.lock_tempo(other, t);
        }
    }

    /// Set deck `x`'s tempo so its BPM matches the other deck's, keeping
    /// whatever half/double relation is the smallest change. Beat phase is
    /// left alone.
    fn lock_tempo(&mut self, x: usize, t: i64) {
        let y = (x + 1) % DECKS;
        let (Some(bx), Some(target)) = (self.decks[x].bpm_at(t), self.decks[y].effective_bpm_at(t))
        else {
            return;
        };
        let tempo = self.decks[x].tempo;
        let k = [1.0, 0.5, 2.0]
            .into_iter()
            .min_by(|a, b| {
                let ca = (bx * a * tempo / target).ln().abs();
                let cb = (bx * b * tempo / target).ln().abs();
                ca.partial_cmp(&cb).unwrap()
            })
            .unwrap_or(1.0);
        let d = &mut self.decks[x];
        d.reanchor(t);
        d.tempo = (target / (bx * k)).clamp(0.5, 1.5);
    }

    fn apply_deck(&mut self, deck: usize, ev: &Event) {
        let t = ev.time;
        let d = &mut self.decks[deck];
        d.last_touch = ev.origin;
        match ev.action {
            Action::Load {
                track,
                duration,
                cue,
                ref grid,
                key,
                ..
            } => {
                d.key = if (0..24).contains(&key) { key } else { -1 };
                d.key_shift = 0;
                d.track = Some(track);
                d.grid = grid.as_deref().copied();
                d.synced = false;
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
                d.grid = None;
                d.synced = false;
                d.gen += 1;
            }
            Action::SetGrid {
                track, ref grid, ..
            } => {
                if d.track == Some(track) {
                    d.grid = grid.as_deref().copied();
                }
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
            Action::KeyLock { on, .. } => {
                d.reanchor(t);
                d.key_lock = on;
            }
            Action::KeyShift { semitones, .. } => d.key_shift = semitones.clamp(-12, 12),
            Action::Crossfader { .. } | Action::Sync { .. } | Action::Relock { .. } => {}
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
            match &d.grid {
                Some(g) => {
                    h.u64(g.markers().len() as u64);
                    for m in g.markers() {
                        h.f64(m.pos);
                        h.f64(m.bpm);
                    }
                    h.u64(g.edited as u64);
                }
                None => h.u64(0),
            }
            h.u64(d.synced as u64);
            h.u64(d.key as u64);
            h.u64(d.key_shift as u64);
            h.u64(d.key_lock as u64);
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

fn no_key() -> i8 {
    -1
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
                grid: None,
                key: -1,
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
                    grid: None,
                    key: -1,
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
                    grid: None,
                    key: -1,
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
            a.insert(e.clone());
        }
        let mut b = Timeline::new(0);
        let mut shuffled = events.clone();
        fastrand::Rng::with_seed(3).shuffle(&mut shuffled);
        for e in &shuffled {
            b.insert(e.clone());
            b.insert(e.clone()); // duplicates are ignored
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
                grid: None,
                key: -1,
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
                grid: None,
                key: -1,
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

    fn grid_load(deck: u8, n: u8, bpm: f64, first_beat: f64) -> Action {
        Action::Load {
            deck,
            track: tid(n),
            duration: 300.0,
            cue: 0.0,
            grid: BeatGrid::new(first_beat, bpm).map(Box::new),
            key: -1,
        }
    }

    /// Phase difference (in beats, -0.5..0.5) between the decks' beats at t.
    fn beat_offset(s: &RoomState, t: i64, beat_mult_b: f64) -> f64 {
        let (a, b) = (&s.decks[0], &s.decks[1]);
        let pa = a.beat_phase(1.0, t);
        let pb = b.beat_phase(beat_mult_b, t);
        let mut d = pa - pb;
        if d >= 0.5 {
            d -= 1.0
        } else if d < -0.5 {
            d += 1.0
        }
        d
    }

    #[test]
    fn sync_matches_tempo_and_beats_and_stays_locked() {
        let mut tl = Timeline::new(0);
        let mut seq = 0;
        let mut go = |tl: &mut Timeline, t: i64, a: Action| {
            seq += 1;
            tl.insert(ev(t, 1, seq, a));
        };
        go(&mut tl, 0, grid_load(0, 1, 128.0, 0.10));
        go(&mut tl, 0, grid_load(1, 2, 124.0, 0.37));
        go(&mut tl, 0, Action::Play { deck: 0 });
        go(&mut tl, 3_123_000, Action::Play { deck: 1 });
        go(&mut tl, 5_000_000, Action::Sync { deck: 1, on: true });
        let s = tl.state_at(5_000_000);
        assert!(s.decks[1].synced);
        assert!((s.decks[1].effective_bpm_at(0).unwrap() - 128.0).abs() < 1e-9);
        for t in [5_000_000, 65_000_000] {
            assert!(
                beat_offset(&tl.state_at(t), t, 1.0).abs() < 1e-6,
                "beats drift at {t}"
            );
        }
        // Moving deck A's tempo carries deck B; beats stay aligned.
        go(
            &mut tl,
            70_000_000,
            Action::Tempo {
                deck: 0,
                tempo: 1.03,
            },
        );
        let s = tl.state_at(100_000_000);
        assert!((s.decks[1].effective_bpm_at(0).unwrap() - 128.0 * 1.03).abs() < 1e-9);
        assert!(beat_offset(&s, 100_000_000, 1.0).abs() < 1e-6);
        // SYNC off: B keeps its tempo but no longer follows.
        go(&mut tl, 101_000_000, Action::Sync { deck: 1, on: false });
        go(
            &mut tl,
            102_000_000,
            Action::Tempo {
                deck: 0,
                tempo: 1.0,
            },
        );
        let s = tl.state_at(103_000_000);
        assert!((s.decks[1].effective_bpm_at(0).unwrap() - 128.0 * 1.03).abs() < 1e-9);
    }

    #[test]
    fn sync_uses_double_time_when_closer() {
        let mut s = RoomState::default();
        s.apply(&ev(0, 1, 1, grid_load(0, 1, 174.0, 0.0)));
        s.apply(&ev(0, 1, 2, grid_load(1, 2, 87.5, 0.2)));
        s.apply(&ev(0, 1, 3, Action::Play { deck: 0 }));
        s.apply(&ev(0, 1, 4, Action::Play { deck: 1 }));
        s.apply(&ev(1_000_000, 1, 5, Action::Sync { deck: 1, on: true }));
        // 87.5 -> 87 (half of 174), not 174.
        assert!((s.decks[1].tempo - 87.0 / 87.5).abs() < 1e-9);
        assert!(beat_offset(&s, 1_000_000, 2.0).abs() < 1e-6);
    }

    #[test]
    fn synced_deck_starts_on_the_beat() {
        let mut s = RoomState::default();
        s.apply(&ev(0, 1, 1, grid_load(0, 1, 120.0, 0.0)));
        s.apply(&ev(0, 1, 2, grid_load(1, 2, 120.0, 0.0)));
        s.apply(&ev(0, 1, 3, Action::Play { deck: 0 }));
        s.apply(&ev(0, 1, 4, Action::Sync { deck: 1, on: true }));
        // B is paused somewhere off the beat, then started at a random moment.
        s.apply(&ev(
            1_000_000,
            1,
            5,
            Action::Seek {
                deck: 1,
                pos: 30.13,
            },
        ));
        s.apply(&ev(7_777_000, 1, 6, Action::Play { deck: 1 }));
        assert!(beat_offset(&s, 9_000_000, 1.0).abs() < 1e-6);
    }

    #[test]
    fn sync_without_a_grid_does_nothing() {
        let mut s = RoomState::default();
        s.apply(&ev(0, 1, 1, grid_load(0, 1, 120.0, 0.0)));
        s.apply(&ev(0, 1, 2, grid_load(1, 2, 0.0, 0.0)));
        s.apply(&ev(0, 1, 3, Action::Sync { deck: 1, on: true }));
        assert!(!s.decks[1].synced);
        assert_eq!(s.decks[1].tempo, 1.0);
    }

    #[test]
    fn loading_onto_a_playing_deck_starts_on_the_beat() {
        let mut s = RoomState::default();
        // Deck B plays at 124; deck A is playing something else.
        s.apply(&ev(0, 1, 1, grid_load(1, 2, 124.0, 0.31)));
        s.apply(&ev(0, 1, 2, Action::Play { deck: 1 }));
        s.apply(&ev(0, 1, 3, grid_load(0, 1, 124.0, 0.05)));
        s.apply(&ev(0, 1, 4, Action::Play { deck: 0 }));
        // Load a new 124 BPM track onto A mid-play.
        s.apply(&ev(9_876_543, 1, 5, grid_load(0, 3, 124.0, 1.2)));
        let a = s.decks[0];
        assert!(a.playing, "keeps playing");
        assert_eq!(a.track, Some(tid(3)));
        // Its beats line up with deck B's.
        assert!(beat_offset(&s, 12_000_000, 1.0).abs() < 1e-6);
        // It starts just before its first beat (no earlier than one beat before).
        let p = a.position_at(9_876_543);
        assert!(p <= 1.2 && p > 1.2 - 60.0 / 124.0, "started at {p}");
    }

    #[test]
    fn loading_onto_a_playing_deck_follows_the_replaced_track() {
        let mut s = RoomState::default();
        s.apply(&ev(0, 1, 1, grid_load(0, 1, 120.0, 0.0)));
        s.apply(&ev(0, 1, 2, Action::Play { deck: 0 }));
        // Other deck idle: the new track lands on the old track's beat.
        let t = 3_210_000;
        let old_phase = ((s.decks[0].position_at(t) - 0.0) * 2.0).rem_euclid(1.0);
        s.apply(&ev(t, 1, 3, grid_load(0, 2, 120.0, 0.7)));
        let d = s.decks[0];
        let wait = (1.0 - old_phase) * 0.5;
        assert!(
            (d.position_at(t) + wait - 0.7).abs() < 1e-9
                || (d.position_at(t) + wait - 1.2).abs() < 1e-9
        );
    }

    #[test]
    fn key_shift_and_lock() {
        let mut s = RoomState::default();
        s.apply(&ev(
            0,
            1,
            1,
            Action::Load {
                deck: 0,
                track: tid(1),
                duration: 100.0,
                cue: 0.0,
                grid: BeatGrid::new(0.0, 120.0).map(Box::new),
                key: 9 + 12,
            },
        ));
        assert!(s.decks[0].key_lock, "key lock is on by default");
        s.apply(&ev(
            0,
            1,
            2,
            Action::Tempo {
                deck: 0,
                tempo: 1.06,
            },
        ));
        assert_eq!(
            s.decks[0].sounding_key(),
            Some(9 + 12),
            "tempo doesn't move the key"
        );
        s.apply(&ev(
            0,
            1,
            3,
            Action::KeyShift {
                deck: 0,
                semitones: -2,
            },
        ));
        assert_eq!(s.decks[0].sounding_key(), Some(7 + 12));
        s.apply(&ev(
            0,
            1,
            4,
            Action::KeyShift {
                deck: 0,
                semitones: 0,
            },
        ));
        s.apply(&ev(0, 1, 5, Action::KeyLock { deck: 0, on: false }));
        // +6% ≈ +1 semitone.
        assert_eq!(s.decks[0].sounding_key(), Some(10 + 12));
    }

    fn two_tempo_grid() -> BeatGrid {
        // 120 BPM for 40 beats (20 s), then 125 BPM.
        BeatGrid::from_markers(
            &[
                crate::grid::Marker {
                    pos: 0.0,
                    bpm: 120.0,
                },
                crate::grid::Marker {
                    pos: 20.0,
                    bpm: 125.0,
                },
            ],
            true,
        )
        .unwrap()
    }

    fn set_grid(deck: u8, n: u8, g: BeatGrid) -> Action {
        Action::SetGrid {
            deck,
            track: tid(n),
            grid: Some(Box::new(g)),
        }
    }

    #[test]
    fn set_grid_only_applies_to_the_track_it_was_made_for() {
        let mut s = RoomState::default();
        s.apply(&ev(0, 1, 1, grid_load(0, 1, 120.0, 0.0)));
        s.apply(&ev(1, 2, 1, set_grid(0, 1, two_tempo_grid())));
        assert_eq!(s.decks[0].grid, Some(two_tempo_grid()));
        assert_eq!(s.decks[0].bpm_at(1), Some(120.0));
        // An edit that arrives after another track was loaded is dropped.
        s.apply(&ev(2, 1, 2, grid_load(0, 2, 128.0, 0.0)));
        s.apply(&ev(3, 2, 2, set_grid(0, 1, two_tempo_grid())));
        assert_eq!(s.decks[0].bpm_at(3), Some(128.0));
    }

    #[test]
    fn synced_deck_stays_locked_through_a_tempo_change() {
        let mut tl = Timeline::new(0);
        let mut seq = 0;
        let mut go = |tl: &mut Timeline, t: i64, a: Action| {
            seq += 1;
            tl.insert(ev(t, 1, seq, a));
        };
        go(&mut tl, 0, grid_load(0, 1, 128.0, 0.0));
        go(&mut tl, 0, grid_load(1, 2, 120.0, 0.0));
        go(&mut tl, 0, set_grid(1, 2, two_tempo_grid()));
        go(&mut tl, 0, Action::Play { deck: 0 });
        go(&mut tl, 0, Action::Play { deck: 1 });
        go(&mut tl, 1_000_000, Action::Sync { deck: 1, on: true });
        let s = tl.state_at(1_000_000);
        assert!((s.decks[1].tempo - 128.0 / 120.0).abs() < 1e-9);
        // B reaches its 125 BPM section (20 s into the track) at about 18.75 s.
        let horizon = 400_000;
        let early = tl.state_at(18_000_000).decks[1].next_tempo_change(18_000_000, horizon);
        assert_eq!(early, None, "not within the horizon yet");
        let t = 18_500_000;
        let tc = tl.state_at(t).decks[1]
            .next_tempo_change(t, horizon)
            .unwrap();
        let at = tl.state_at(tc).decks[1].position_at(tc);
        assert!((at - 20.0).abs() < 2e-6, "reaches the marker at {at}");
        go(&mut tl, tc + 1_000, Action::Relock { deck: 1 });
        let later = 60_000_000;
        let s = tl.state_at(later);
        assert!((s.decks[1].effective_bpm_at(later).unwrap() - 128.0).abs() < 1e-9);
        assert!((s.decks[1].tempo - 128.0 / 125.0).abs() < 1e-9);
        assert!(
            beat_offset(&s, later, 1.0).abs() < 1e-3,
            "beats drifted by {}",
            beat_offset(&s, later, 1.0)
        );
        assert_eq!(s.decks[1].last_touch, 1, "a relock isn't a touch");
    }

    #[test]
    fn jumping_into_another_tempo_section_relocks_at_once() {
        let mut s = RoomState::default();
        s.apply(&ev(0, 1, 1, grid_load(0, 1, 128.0, 0.0)));
        s.apply(&ev(0, 1, 2, grid_load(1, 2, 120.0, 0.0)));
        s.apply(&ev(0, 1, 3, set_grid(1, 2, two_tempo_grid())));
        s.apply(&ev(0, 1, 4, Action::Sync { deck: 1, on: true }));
        s.apply(&ev(1_000_000, 1, 5, Action::Seek { deck: 1, pos: 50.0 }));
        assert!((s.decks[1].tempo - 128.0 / 125.0).abs() < 1e-9);
        s.apply(&ev(2_000_000, 1, 6, Action::Seek { deck: 1, pos: 5.0 }));
        assert!((s.decks[1].tempo - 128.0 / 120.0).abs() < 1e-9);
    }

    #[test]
    fn editing_the_master_grid_carries_a_synced_deck() {
        let mut s = RoomState::default();
        s.apply(&ev(0, 1, 1, grid_load(0, 1, 124.0, 0.0)));
        s.apply(&ev(0, 1, 2, grid_load(1, 2, 120.0, 0.0)));
        s.apply(&ev(0, 1, 3, Action::Sync { deck: 1, on: true }));
        assert!((s.decks[1].effective_bpm_at(0).unwrap() - 124.0).abs() < 1e-9);
        // Deck A's grid was a little slow: fix it, and synced B follows.
        let fixed = s.decks[0].grid.unwrap().adjust(0, 0.1);
        s.apply(&ev(1_000_000, 1, 4, set_grid(0, 1, fixed)));
        assert!((s.decks[1].effective_bpm_at(1_000_000).unwrap() - 124.1).abs() < 1e-9);
    }

    #[test]
    fn grid_edits_survive_the_wire() {
        let e = ev(5, 1, 1, set_grid(0, 1, two_tempo_grid()));
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), e);
        // Small events stay small: grids are boxed.
        assert!(
            std::mem::size_of::<Event>() <= 112,
            "{}",
            std::mem::size_of::<Event>()
        );
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
                grid: None,
                key: -1,
            },
        ));
        s.apply(&ev(0, 1, 2, Action::Play { deck: 1 }));
        s.apply(&ev(10_000_000, 1, 3, Action::HotCue { deck: 1, slot: 2 }));
        assert_eq!(s.decks[1].hot_cues[2], Some(10.0));
        s.apply(&ev(20_000_000, 1, 4, Action::HotCue { deck: 1, slot: 2 }));
        assert!((s.decks[1].position_at(21_000_000) - 11.0).abs() < 1e-9);
    }
}
