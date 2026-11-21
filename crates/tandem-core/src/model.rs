//! Room state as a pure function of timestamped events.
//!
//! A deck's playhead is modelled analytically: while playing,
//! `position(t) = anchor_pos + (t - anchor_time) * rate`. Every event
//! re-anchors at its own timestamp. So given the same events, any machine
//! can compute exactly where every deck is at any shared time, including
//! times in the past (that is how the delayed broadcast lane works) and
//! including events that arrive late over the network.

use crate::grid::BeatGrid;
use crate::spin;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Decks A–D. A and C sit on the crossfader's left, B and D on its right.
pub const DECKS: usize = 4;
pub const HOT_CUES: usize = 8;
/// Beat loop sizes (in beats) on the pads and buttons: ¼ to 32.
pub const BEAT_LOOPS: [f64; 8] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];
/// Loop roll sizes (in beats) on the roll pads, short to long: 1/16 to ½
/// on the top row and 1 to 8 on the bottom, as on Pioneer's roll pads.
pub const ROLLS: [f64; 8] = [0.0625, 0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0];
/// Shortest loop (seconds): LOOP OUT closer than this to LOOP IN does
/// nothing, and halving stops here.
pub const MIN_LOOP: f64 = 0.01;
/// Longest beat loop, in beats.
const MAX_LOOP_BEATS: f64 = 512.0;
/// Beat length (seconds) for beat loops on a track with no beatgrid (120 BPM).
const NO_GRID_BEAT: f64 = 0.5;

/// The length (in beats) of a quantized loop whose LOOP OUT was pressed
/// `beats` after its in point: whichever of ¼ beat, ½ beat, 1 beat, ½ bar,
/// 1 bar, 2 bars, 4 bars … 128 bars is nearest. Pressing OUT about a bar
/// after IN makes a 1-bar loop.
pub fn quantized_loop_beats(beats: f64) -> f64 {
    let mut len = BEAT_LOOPS[0];
    while len < MAX_LOOP_BEATS && beats >= len * 1.5 {
        len *= 2.0;
    }
    len
}

/// "A", "B", "C", "D" for deck 0–3.
pub fn deck_letter(d: usize) -> char {
    (b'A' + d as u8) as char
}

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
    /// True while CUE or a hot cue is held from a pause (CDJ-style
    /// preview): releasing it goes back to that point, paused.
    pub cue_preview: bool,
    /// While previewing: the hot cue being held (`None` = the CUE button).
    #[serde(default)]
    pub preview_slot: Option<u8>,
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
    /// The record was let go mid-spin: its speed at `anchor_time` (track
    /// seconds per second, negative = backwards). It runs on and slows down
    /// by itself (see [`crate::spin`]). `None`: it moves at the deck's speed.
    #[serde(default)]
    pub spin: Option<f64>,
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
    /// Loop in point (seconds), set by LOOP IN or a beat loop.
    #[serde(default)]
    pub loop_in: Option<f64>,
    /// Loop out point. None while a LOOP IN waits for LOOP OUT.
    #[serde(default)]
    pub loop_out: Option<f64>,
    /// Looping: on reaching `loop_out`, playback carries on from `loop_in`.
    /// While looping, the anchor is always before `loop_out`.
    #[serde(default)]
    pub looping: bool,
    /// The loop's length in beats, for loops made from the grid (beat loops,
    /// and halving or doubling them). None for loops set by hand.
    #[serde(default)]
    pub loop_beats: Option<f64>,
    /// A loop roll is held: the playhead goes round the roll, while the
    /// track runs on underneath (see [`Roll`]).
    #[serde(default)]
    pub roll: Option<Roll>,
    /// The channel's effects, after its fader and the crossfader.
    #[serde(default)]
    pub fx: FxBank,
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
            preview_slot: None,
            hot_cues: [None; HOT_CUES],
            volume: 1.0,
            eq: [0.5; 3],
            trim: 0.5,
            filter: 0.5,
            scratching: false,
            spin: None,
            grid: None,
            synced: false,
            key: -1,
            key_shift: 0,
            key_lock: true,
            loop_in: None,
            loop_out: None,
            looping: false,
            loop_beats: None,
            roll: None,
            fx: FxBank::default(),
            gen: 0,
            last_touch: 0,
        }
    }
}

/// A loop roll (a slip loop): a beat loop that plays only while its pad is
/// held. The deck's playhead goes round it, and the track runs on
/// underneath at the deck's speed, so letting go carries on exactly where
/// the track would have been without the roll.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Roll {
    /// The size held, in beats (one of [`ROLLS`], or any size from the UI).
    pub beats: f64,
    /// The roll's loop, on the grid: `loop_in..loop_out` (seconds).
    pub loop_in: f64,
    pub loop_out: f64,
    /// Shared time (us) at which the track underneath was at `slip_pos`.
    pub slip_time: i64,
    /// Where the track underneath was at `slip_time` (seconds). It moves at
    /// the deck's play speed, and goes round the deck's ordinary loop if
    /// one is playing.
    pub slip_pos: f64,
}

impl DeckState {
    pub fn rate(&self) -> f64 {
        self.tempo * (1.0 + self.bend)
    }

    /// Track position (seconds) at shared time `t`. While looping, the
    /// playhead goes round the loop: it's still analytic, just wrapped.
    pub fn position_at(&self, t: i64) -> f64 {
        if self.track.is_none() {
            return 0.0;
        }
        let p = self.anchor_pos + self.motion(t).0;
        self.wrap(p).clamp(0.0, self.duration.max(0.0))
    }

    /// Where the track is at shared time `t`: while a loop roll is held,
    /// where it has got to underneath the roll (and where playback carries
    /// on when the roll ends); otherwise the playhead.
    pub fn slip_position_at(&self, t: i64) -> f64 {
        let Some(r) = self.roll else {
            return self.position_at(t);
        };
        if self.track.is_none() {
            return 0.0;
        }
        // Written like `motion`, so before the roll first goes round, the
        // track underneath and the playhead agree to the last bit.
        let dt = (t - r.slip_time) as f64 * 1e-6;
        let p = r.slip_pos + dt * self.rest_speed();
        wrap_in(self.ordinary_loop(), p).clamp(0.0, self.duration.max(0.0))
    }

    /// The speed the playhead moves at when nobody is holding the record:
    /// the play speed, or 0 when paused.
    fn rest_speed(&self) -> f64 {
        if self.playing {
            self.rate()
        } else {
            0.0
        }
    }

    /// From `anchor_time` to shared time `t`: how far the playhead has moved
    /// (before going round a loop), its speed at `t` (track seconds per
    /// second), and whether a record let go mid-spin is still running on.
    fn motion(&self, t: i64) -> (f64, f64, bool) {
        let dt = (t - self.anchor_time) as f64 * 1e-6;
        if self.scratching {
            return (0.0, 0.0, false);
        }
        let rest = self.rest_speed();
        match self.spin {
            Some(v) => spin::motion(v, rest, dt),
            None => (dt * rest, rest, false),
        }
    }

    /// How fast the playhead is moving at time `t`, in track seconds per
    /// second (negative = backwards).
    pub fn speed_at(&self, t: i64) -> f64 {
        self.motion(t).1
    }

    /// True while a record let go mid-spin is still running on at time `t`.
    pub fn spinning_at(&self, t: i64) -> bool {
        self.motion(t).2
    }

    /// Making sound at time `t`: the track is playing (holding CUE or a hot
    /// cue from a pause too), under the DJ's hand, rolling, or still
    /// spinning after being let go, and hasn't run off either end.
    pub fn sounding_at(&self, t: i64) -> bool {
        if self.track.is_none() {
            return false;
        }
        // A roll goes round on the grid even after the track underneath
        // has run out.
        if self.scratching || self.roll.is_some() {
            return true;
        }
        let (v, p) = (self.speed_at(t), self.position_at(t));
        (v > 0.0 && p < self.duration) || (v < 0.0 && p > 0.0)
    }

    /// The loop that's set, `(in, out)`, whether or not it's playing.
    pub fn loop_span(&self) -> Option<(f64, f64)> {
        match (self.loop_in, self.loop_out) {
            (Some(a), Some(b)) if b > a => Some((a, b)),
            _ => None,
        }
    }

    /// The loop the playhead is going round, `(in, out)`: a loop roll
    /// while one is held, otherwise the loop while looping.
    pub fn active_loop(&self) -> Option<(f64, f64)> {
        self.roll_span().or_else(|| self.ordinary_loop())
    }

    /// The loop roll being held, `(in, out)`.
    pub fn roll_span(&self) -> Option<(f64, f64)> {
        self.roll
            .map(|r| (r.loop_in, r.loop_out))
            .filter(|&(a, b)| b > a)
    }

    /// The ordinary loop while looping (under a roll, the track goes round
    /// it underneath).
    fn ordinary_loop(&self) -> Option<(f64, f64)> {
        self.loop_span().filter(|_| self.looping)
    }

    /// A position as heard while looping: at or past the loop out point, it
    /// has gone round the loop. Positions before the loop are left alone (a
    /// playhead there walks into the loop and gets caught).
    fn wrap(&self, p: f64) -> f64 {
        wrap_in(self.active_loop(), p)
    }

    /// Length of one beat (seconds) at `pos`, from the grid.
    fn beat_len_at(&self, pos: f64) -> f64 {
        self.grid
            .map(|g| 60.0 / g.bpm_at(pos))
            .unwrap_or(NO_GRID_BEAT)
    }

    /// The beat nearest `p` that's inside the track (`p` itself with no
    /// grid): where a quantized LOOP IN goes.
    fn snap_to_beat(&self, p: f64) -> f64 {
        let Some(g) = self.grid else { return p };
        let (before, after) = g.beats_around(p);
        let inside = |x: f64| (0.0..self.duration).contains(&x);
        if inside(after) && (after - p < p - before || !inside(before)) {
            after
        } else if inside(before) {
            before
        } else {
            p
        }
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
    /// times as fast (0.5 = half time). Under a loop roll it's the track
    /// underneath that counts, since that's where the deck carries on.
    fn beat_phase(&self, mult: f64, t: i64) -> f64 {
        self.grid
            .map(|g| g.phase(self.slip_position_at(t), mult))
            .unwrap_or(0.0)
    }

    /// When (shared time) the playing deck will next reach a grid marker
    /// where the tempo changes, if that's within `horizon` us of `now`.
    pub fn next_tempo_change(&self, now: i64, horizon: i64) -> Option<i64> {
        let g = self.grid.as_ref()?;
        let rate = self.rate();
        if self.track.is_none()
            || !self.playing
            || self.scratching
            || self.spinning_at(now)
            || rate <= 1e-6
        {
            return None;
        }
        let pos = self.position_at(now);
        let m = g.markers();
        let here = m[g.section(pos)].bpm;
        // Going round a loop, the deck only reaches markers inside it, and
        // the tempo can also change where it jumps back to the loop in point.
        let (limit, back_to) = match self.active_loop() {
            Some((a, b)) if pos < b => (b, Some(a)),
            _ => (f64::INFINITY, None),
        };
        let at = match m
            .iter()
            .find(|x| x.pos > pos && x.pos < limit && x.bpm != here)
        {
            Some(next) => next.pos,
            None if back_to.is_some_and(|a| g.bpm_at(a) != here) => limit,
            None => return None,
        };
        let dt = ((at - pos) / rate * 1e6).ceil() as i64;
        (dt <= horizon).then_some(now + dt)
    }

    fn reanchor(&mut self, t: i64) {
        let (_, speed, spinning) = self.motion(t);
        let slip = self.slip_position_at(t);
        self.anchor_pos = self.position_at(t);
        self.anchor_time = t;
        // A spinning record keeps its momentum through whatever changed.
        self.spin = spinning.then_some(speed);
        // So does the track running on under a roll.
        if let Some(r) = &mut self.roll {
            r.slip_pos = slip;
            r.slip_time = t;
        }
    }

    /// A loop `beats` long on the grid around position `p` (from `p`, at
    /// 120 BPM, without a grid), moved up into the track if it would start
    /// before it and cut short at its end: `(in, out, whole)`, `whole` false
    /// if the end cut it short. None if that leaves less than [`MIN_LOOP`].
    fn grid_loop(&self, p: f64, beats: f64) -> Option<(f64, f64, bool)> {
        let (mut a, mut b) = match self.grid {
            Some(g) => g.beat_loop(p, beats),
            None => (p, p + beats * NO_GRID_BEAT),
        };
        if a < 0.0 {
            // Starts before the track: move it up to the first slice that
            // starts inside it.
            let step = (b - a) * beats.min(1.0) / beats;
            let shift = (-a / step).ceil() * step;
            a += shift;
            b += shift;
        }
        let end = b.min(self.duration);
        (end - a >= MIN_LOOP).then_some((a, end, end == b))
    }

    /// Let go of a loop roll: playback carries on from where the track has
    /// got to underneath. Seamless if the roll hasn't gone round yet.
    fn end_roll(&mut self, t: i64) {
        if self.roll.is_none() {
            return;
        }
        let heard = self.position_at(t);
        let p = self.slip_position_at(t);
        self.roll = None;
        self.anchor_pos = p;
        self.anchor_time = t;
        if (p - heard).abs() > 1e-6 || self.spin.is_some() {
            self.spin = None;
            self.gen += 1;
        }
    }

    fn clear_loop(&mut self) {
        self.loop_in = None;
        self.loop_out = None;
        self.looping = false;
        self.loop_beats = None;
    }

    /// Keep a looping deck's anchor inside its loop (anything that moved it
    /// to or past the out point has gone round the loop), and the track
    /// under a roll inside the ordinary loop.
    fn settle_loop(&mut self) {
        if self.looping && self.loop_span().is_none() {
            self.looping = false;
        }
        if self.roll.is_some() && self.roll_span().is_none() {
            self.roll = None;
        }
        self.anchor_pos = wrap_in(self.active_loop(), self.anchor_pos);
        let ordinary = self.ordinary_loop();
        if let Some(r) = &mut self.roll {
            r.slip_pos = wrap_in(ordinary, r.slip_pos);
        }
    }
}

/// Position `p` gone round loop `lp` (if at or past its out point).
fn wrap_in(lp: Option<(f64, f64)>, p: f64) -> f64 {
    match lp {
        Some((a, b)) if p >= b => a + (p - a).rem_euclid(b - a),
        _ => p,
    }
}

/// How the crossfader shares the volume between the decks, like the curve
/// switch on a club mixer. Part of the room, so both booths (and the
/// broadcast) hear the same mix. Gains: [`crate::render::xfader_gain`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum XfCurve {
    /// Straight fades: both decks at half volume (-6 dB) in the middle.
    Linear,
    /// Both decks at -3 dB in the middle, so a blend of two tracks stays
    /// about as loud as one.
    #[default]
    ConstantPower,
    /// Both decks at full volume in the middle; each only fades on the far
    /// half. A deck playing on its own stays at full volume.
    FullCentre,
    /// Both decks at full volume almost all the way across: a deck cuts in
    /// within the last few percent of travel.
    Scratch,
}

impl XfCurve {
    /// Smoothest to sharpest.
    pub const ALL: [XfCurve; 4] = [
        XfCurve::Linear,
        XfCurve::ConstantPower,
        XfCurve::FullCentre,
        XfCurve::Scratch,
    ];

    pub fn label(self) -> &'static str {
        match self {
            XfCurve::Linear => "Linear",
            XfCurve::ConstantPower => "Constant power",
            XfCurve::FullCentre => "Full centre",
            XfCurve::Scratch => "Scratch",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            XfCurve::Linear => {
                "Each deck fades evenly across the whole travel. In the middle both are at half volume (−6 dB): long, gentle blends."
            }
            XfCurve::ConstantPower => {
                "Both decks at −3 dB in the middle, so a blend of two tracks stays about as loud as one. A deck playing on its own gets quieter in the middle."
            }
            XfCurve::FullCentre => {
                "Both decks at full volume in the middle; each fades out only on the far half. A deck playing on its own stays at full volume until the fader passes the middle."
            }
            XfCurve::Scratch => {
                "Both decks at full volume almost all the way across. A deck cuts in within the last 3% of travel, for cuts and scratching."
            }
        }
    }
}

/// Which side of the crossfader a channel is on, like the assign switch
/// under each channel of a club mixer. Part of the room.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum XfSide {
    /// The crossfader's left: full with the crossfader at A.
    A,
    /// Not on the crossfader: only the channel fader sets its level.
    Thru,
    /// The crossfader's right.
    B,
}

impl XfSide {
    /// Left to right, as on a mixer's switch.
    pub const ALL: [XfSide; 3] = [XfSide::A, XfSide::Thru, XfSide::B];

    /// Decks A and C start on the left, B and D on the right.
    pub fn default_for(deck: usize) -> Self {
        if deck.is_multiple_of(2) {
            XfSide::A
        } else {
            XfSide::B
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            XfSide::A => "A",
            XfSide::Thru => "THRU",
            XfSide::B => "B",
        }
    }
}

fn default_xf_assign() -> [XfSide; DECKS] {
    std::array::from_fn(XfSide::default_for)
}

/// Effect slots in a deck's bank. They run in series, 1 → 2 → 3.
pub const FX_SLOTS: usize = 3;
/// What a bank's BEATS steps through, in beats: 1/16 to 32, with ¾ (the
/// classic echo time, which Pioneer mixers and the DDJ-FLX6 step through).
pub const FX_BEATS: [f64; 11] = [
    0.0625, 0.125, 0.25, 0.5, 0.75, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0,
];
/// A new bank's BEATS: 1 beat.
pub const FX_BEATS_DEFAULT: u8 = 5;

/// An effect a slot can hold (see `fx.rs` for what each does).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FxKind {
    Echo,
    Reverb,
    Phaser,
}

impl FxKind {
    /// In the order a slot's effect menu lists them.
    pub const ALL: [FxKind; 3] = [FxKind::Echo, FxKind::Reverb, FxKind::Phaser];

    pub fn label(self) -> &'static str {
        match self {
            FxKind::Echo => "ECHO",
            FxKind::Reverb => "REVERB",
            FxKind::Phaser => "PHASER",
        }
    }

    /// Four letters, for narrow places.
    pub fn short(self) -> &'static str {
        match self {
            FxKind::Echo => "ECHO",
            FxKind::Reverb => "VERB",
            FxKind::Phaser => "PHSR",
        }
    }

    /// The next effect in [`FxKind::ALL`], round to the first.
    pub fn next(self) -> FxKind {
        let i = FxKind::ALL.iter().position(|&k| k == self).unwrap_or(0);
        FxKind::ALL[(i + 1) % FxKind::ALL.len()]
    }
}

/// One effect slot of a deck's bank.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FxSlot {
    pub kind: FxKind,
    pub on: bool,
    /// 0..1: what it means is the effect's own (see `fx.rs`).
    pub depth: f32,
    /// Shared time (us) the slot was last switched on or changed effect: a
    /// starting point every booth agrees on, for effects that need one (a
    /// phaser on a track with no beatgrid).
    pub since: i64,
}

/// A deck's effects: three slots in series and one BEATS value, which sets
/// each effect's time from the deck's own BPM. Part of the room, like the
/// EQ, so every booth and the broadcast hear the same effects. It belongs
/// to the channel: loading or ejecting a track leaves it alone.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FxBank {
    pub slots: [FxSlot; FX_SLOTS],
    /// An index into [`FX_BEATS`]. An index rather than a float, so it
    /// survives JSON exactly (floats without `float_roundtrip` don't).
    pub beats: u8,
}

impl Default for FxBank {
    /// Echo, reverb and phaser, all off at half depth, BEATS 1.
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|i| FxSlot {
                kind: FxKind::ALL[i % FxKind::ALL.len()],
                on: false,
                depth: 0.5,
                since: 0,
            }),
            beats: FX_BEATS_DEFAULT,
        }
    }
}

impl FxBank {
    /// The bank's BEATS, in beats.
    pub fn beats(&self) -> f64 {
        FX_BEATS[(self.beats as usize).min(FX_BEATS.len() - 1)]
    }

    /// Any slot switched on.
    pub fn any_on(&self) -> bool {
        self.slots.iter().any(|s| s.on)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoomState {
    pub decks: [DeckState; DECKS],
    /// 0 = full left (decks on side A), 1 = full right (side B).
    pub crossfader: f32,
    pub xf_touch: u64,
    #[serde(default)]
    pub xf_curve: XfCurve,
    /// Which side of the crossfader each channel is on.
    #[serde(default = "default_xf_assign")]
    pub xf_assign: [XfSide; DECKS],
    /// The sync master: the deck synced decks follow. None while no deck is
    /// synced. See [`RoomState::sync_target`].
    #[serde(default)]
    pub master: Option<u8>,
}

impl Default for RoomState {
    fn default() -> Self {
        Self {
            decks: [DeckState::default(); DECKS],
            crossfader: 0.5,
            xf_touch: 0,
            xf_curve: XfCurve::default(),
            xf_assign: default_xf_assign(),
            master: None,
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
    /// Put a track on the deck, paused at `cue`. Dropped if the deck can be
    /// heard in the master at that moment (see
    /// [`RoomState::audible_in_master`]): nobody loads over music the room
    /// is hearing.
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
    /// it's synced (or the master synced decks follow), re-match the tempos
    /// so the beats stay locked. Scheduled by a booth for the exact moment.
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
    /// SYNC on/off: match the sync master's tempo and beat phase (see
    /// [`RoomState::sync_target`]), and keep the synced decks' tempos
    /// locked to the master's while on.
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
    /// Pressed while paused, it plays only while held (see [`Action::HotCueUp`]).
    HotCue {
        deck: u8,
        slot: u8,
    },
    /// Hot cue released. Ends a preview started from a pause: back to the
    /// hot cue, paused.
    HotCueUp {
        deck: u8,
        slot: u8,
    },
    ClearHotCue {
        deck: u8,
        slot: u8,
    },
    /// LOOP IN: a loop starts at the playhead, and LOOP OUT ends it. While
    /// looping, it moves the start of the loop to the playhead instead.
    /// `quantize` (the pressing DJ's setting): on the nearest beat instead,
    /// on a track with a beatgrid.
    LoopIn {
        deck: u8,
        #[serde(default)]
        quantize: bool,
    },
    /// LOOP OUT: the loop ends at the playhead, and playback goes back to
    /// the loop in point and loops. While looping, it moves the end here.
    /// `quantize`: on a track with a beatgrid, the loop is the length from
    /// [`quantized_loop_beats`] instead (1 bar, ½ bar, 2 bars …), and a
    /// playhead already past the end has gone round the loop.
    LoopOut {
        deck: u8,
        #[serde(default)]
        quantize: bool,
    },
    /// RELOOP/EXIT: while looping, leave the loop (playback carries on
    /// through it). Otherwise jump back to the loop in point and loop again.
    Reloop {
        deck: u8,
    },
    /// A loop `beats` long, on the grid around the playhead. While a loop
    /// of the same size plays, it leaves the loop instead. A different size
    /// resizes the playing loop from its in point.
    BeatLoop {
        deck: u8,
        beats: f64,
    },
    /// Halve the loop, keeping its in point.
    LoopHalve {
        deck: u8,
    },
    /// Double the loop, keeping its in point.
    LoopDouble {
        deck: u8,
    },
    /// A loop roll pad pressed (`on`) or let go. While held, `beats` beats
    /// loop on the grid from the beat the playhead is in (like a beat loop),
    /// and the track runs on underneath; letting go carries on where the
    /// track has got to. Another size while one is held switches to it,
    /// from the same in point, staying in step with the track underneath.
    /// Only on a playing deck. Letting go of a size that isn't the one
    /// rolling does nothing.
    Roll {
        deck: u8,
        beats: f64,
        on: bool,
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
    /// Put the channel on a side of the crossfader, or through it.
    XfAssign {
        deck: u8,
        side: XfSide,
    },
    /// Hand on / off the jog platter (vinyl mode). On letting go, `spin` is
    /// how fast the hand was moving the record (track seconds per second,
    /// negative = backwards): fast enough, and the record spins on by itself.
    JogTouch {
        deck: u8,
        down: bool,
        #[serde(default)]
        spin: f32,
    },
    Crossfader {
        v: f32,
    },
    /// The crossfader's curve, for the whole room.
    CrossfaderCurve {
        curve: XfCurve,
    },
    /// Switch an effect slot on or off. A value, not a toggle, so late and
    /// reordered moves come out the same everywhere. Switched off, the
    /// effect's tail rings out.
    FxOn {
        deck: u8,
        slot: u8,
        on: bool,
    },
    /// An effect slot's depth (0..1).
    FxDepth {
        deck: u8,
        slot: u8,
        v: f32,
    },
    /// The effect in a slot. A slot that's on switches effect at once, and
    /// the old one rings out.
    FxSelect {
        deck: u8,
        slot: u8,
        kind: FxKind,
    },
    /// The bank's BEATS: an index into [`FX_BEATS`].
    FxBeats {
        deck: u8,
        beats: u8,
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
            | HotCueUp { deck, .. }
            | ClearHotCue { deck, .. }
            | LoopIn { deck, .. }
            | LoopOut { deck, .. }
            | Reloop { deck }
            | BeatLoop { deck, .. }
            | LoopHalve { deck }
            | LoopDouble { deck }
            | Roll { deck, .. }
            | Seek { deck, .. }
            | Tempo { deck, .. }
            | Bend { deck, .. }
            | Nudge { deck, .. }
            | Volume { deck, .. }
            | Eq { deck, .. }
            | Trim { deck, .. }
            | Filter { deck, .. }
            | XfAssign { deck, .. }
            | JogTouch { deck, .. }
            | Sync { deck, .. }
            | KeyLock { deck, .. }
            | KeyShift { deck, .. }
            | FxOn { deck, .. }
            | FxDepth { deck, .. }
            | FxSelect { deck, .. }
            | FxBeats { deck, .. } => Some(deck as usize),
            Crossfader { .. } | CrossfaderCurve { .. } => None,
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
        self.apply_event(ev);
        for d in &mut self.decks {
            d.settle_loop();
        }
        self.settle_master();
    }

    /// The decks other than `x`, in the order `x` looks at them: its partner
    /// across the crossfader first (A and B, C and D), then the rest.
    fn others(x: usize) -> impl Iterator<Item = usize> {
        let partner = x ^ 1;
        std::iter::once(partner)
            .filter(|&p| p < DECKS)
            .chain((0..DECKS).filter(move |&d| d != x && d != partner))
    }

    /// The sync master, if there is one and it has a track with a beatgrid.
    pub fn master_deck(&self) -> Option<usize> {
        self.master
            .map(usize::from)
            .filter(|&m| m < DECKS && has_beat(&self.decks[m]))
    }

    /// The deck that deck `x` syncs to: the master, or when there's none
    /// (or `x` is the master) another deck with a beatgrid, synced and
    /// playing decks first.
    pub fn sync_target(&self, x: usize) -> Option<usize> {
        if let Some(m) = self.master_deck().filter(|&m| m != x) {
            return Some(m);
        }
        let rank = |y: usize| {
            let d = &self.decks[y];
            match (d.synced, d.playing) {
                (true, true) => 0,
                (true, false) => 1,
                (false, true) => 2,
                (false, false) => 3,
            }
        };
        Self::others(x)
            .filter(|&y| has_beat(&self.decks[y]))
            .min_by_key(|&y| rank(y))
    }

    /// SYNC can lock deck `x`: it and some other deck have beatgrids.
    pub fn can_sync(&self, x: usize) -> bool {
        has_beat(&self.decks[x]) && self.sync_target(x).is_some()
    }

    /// In the sync group: synced, or the master that synced decks follow.
    /// A tempo change on any deck in the group carries the others.
    pub fn in_sync_group(&self, x: usize) -> bool {
        self.decks[x].synced || self.master == Some(x as u8)
    }

    /// The deck `x` is being mixed against, for comparing keys: the sync
    /// master, otherwise a deck that's playing (its partner across the
    /// crossfader first), otherwise any deck with a track.
    pub fn mix_partner(&self, x: usize) -> Option<usize> {
        let d = &self.decks;
        self.master_deck()
            .filter(|&m| m != x)
            .or_else(|| Self::others(x).find(|&y| d[y].track.is_some() && d[y].playing))
            .or_else(|| Self::others(x).find(|&y| d[y].track.is_some()))
    }

    /// Deck `x` is making sound at time `t` (see [`DeckState::sounding_at`])
    /// and can be heard in the master: louder there than
    /// [`render::SILENT_GAIN`](crate::render::SILENT_GAIN), −40 dB, by
    /// [`render::master_gain`](crate::render::master_gain). The channel
    /// fader at about 10% or below, the crossfader at the far end from the
    /// deck, or the filter turned all the way, each make it silent. The headphone
    /// cue doesn't count. A track can't be loaded onto a deck that's
    /// audible.
    pub fn audible_in_master(&self, x: usize, t: i64) -> bool {
        x < DECKS
            && self.decks[x].sounding_at(t)
            && crate::render::master_gain(self, x) > crate::render::SILENT_GAIN
    }

    /// Why a track can't be loaded onto deck `x` at time `t`, to tell the
    /// DJ, or None if it can be.
    pub fn load_refusal(&self, x: usize, t: i64) -> Option<String> {
        self.audible_in_master(x, t).then(|| {
            format!(
                "Deck {} is playing: pause it or pull its fader down to load onto it.",
                deck_letter(x)
            )
        })
    }

    /// Keep the sync master sensible after every event. There's none while
    /// no deck is synced. A master that isn't playing (or has lost its track
    /// or grid) hands over to a synced deck that is, so the deck you're
    /// hearing leads and a stopped deck can be prepared without dragging it.
    /// Holding CUE or a hot cue from a pause is only a preview: it neither
    /// keeps nor takes the master.
    fn settle_master(&mut self) {
        if !self.decks.iter().any(|d| d.synced) {
            self.master = None;
            return;
        }
        let live = |d: &DeckState| d.playing && !d.cue_preview;
        let current = self.master_deck();
        if current.is_some_and(|m| live(&self.decks[m])) {
            return;
        }
        let synced = |d: &DeckState| d.synced && has_beat(d);
        let playing = (0..DECKS).find(|&d| synced(&self.decks[d]) && live(&self.decks[d]));
        self.master = playing
            .or(current)
            .or_else(|| (0..DECKS).find(|&d| synced(&self.decks[d])))
            .map(|d| d as u8);
    }

    fn apply_event(&mut self, ev: &Event) {
        let t = ev.time;
        if let Action::Crossfader { v } = ev.action {
            self.crossfader = v.clamp(0.0, 1.0);
            self.xf_touch = ev.origin;
            return;
        }
        if let Action::CrossfaderCurve { curve } = ev.action {
            self.xf_curve = curve;
            return;
        }
        let Some(deck) = ev.action.deck() else { return };
        if deck >= DECKS {
            return;
        }
        if matches!(ev.action, Action::Load { .. }) && self.audible_in_master(deck, t) {
            // Never over music the room can hear, whichever booth loaded it
            // and however it raced the faders: the deck carries on as it was.
            return;
        }
        if let Action::XfAssign { side, .. } = ev.action {
            self.decks[deck].last_touch = ev.origin;
            self.xf_assign[deck] = side;
            return;
        }
        if let Action::Sync { on, .. } = ev.action {
            self.decks[deck].last_touch = ev.origin;
            let target = if on { self.sync_target(deck) } else { None };
            let synced = target.is_some_and(|y| self.sync_deck(deck, y, t, true));
            self.decks[deck].synced = synced;
            if synced && self.master_deck().is_none() {
                // The first deck synced to: the others follow it from now on.
                self.master = target.map(|y| y as u8);
            }
            return;
        }
        if let Action::Relock { .. } = ev.action {
            // Automatic, so it doesn't count as anyone touching the deck.
            self.relock(deck, t);
            return;
        }
        if matches!(ev.action, Action::Load { .. } | Action::Eject { .. })
            && self.master == Some(deck as u8)
        {
            // A new track on the master: the synced decks no longer follow
            // it (a synced deck takes over, see `settle_master`).
            self.master = None;
        }
        let was_playing = self.decks[deck].playing;
        let was_looping = self.decks[deck].looping;
        // A jump, PAUSE or any loop control lets go of a loop roll first.
        let lets_go = lets_go_of_roll(&ev.action, &self.decks[deck]);
        // Moves the playhead to somewhere chosen (a cue, a seek): if that's
        // outside a playing loop, the loop ends.
        let lands = matches!(
            ev.action,
            Action::Seek { .. }
                | Action::HotCue { .. }
                | Action::HotCueUp { .. }
                | Action::CueDown { .. }
                | Action::CueUp { .. }
        );
        let jumps = lands
            || lets_go
            || matches!(
                ev.action,
                Action::Nudge { .. }
                    | Action::JogTouch { .. }
                    | Action::LoopOut { .. }
                    | Action::Reloop { .. }
                    | Action::BeatLoop { .. }
                    | Action::LoopHalve { .. }
                    | Action::Roll { .. }
            );
        let bpm_before = if jumps {
            self.decks[deck].bpm_at(t)
        } else {
            None
        };
        if lets_go {
            self.decks[deck].end_roll(t);
        }
        self.apply_deck(deck, ev);
        if lands {
            let d = &mut self.decks[deck];
            if let Some((a, b)) = d.active_loop() {
                if d.anchor_pos < a || d.anchor_pos >= b {
                    d.looping = false;
                }
            }
        }
        // Settle the loop now, so the BPM check below sees where it lands.
        self.decks[deck].settle_loop();
        match ev.action {
            Action::Tempo { .. } => self.follow_tempo(deck, t),
            Action::Play { .. } if !was_playing && self.decks[deck].synced => {
                // Starting a synced deck: start it on the beat.
                self.rephase(deck, t);
            }
            // Leaving a loop shorter than a beat (or not on the beat) can
            // leave a synced deck between beats: put it back on the beat.
            Action::Reloop { .. } | Action::BeatLoop { .. }
                if was_looping
                    && !self.decks[deck].looping
                    && self.decks[deck].synced
                    && self.decks[deck].playing =>
            {
                self.rephase(deck, t);
            }
            // The deck's own BPM may have changed under a sync lock.
            Action::SetGrid { .. } => self.relock(deck, t),
            _ if jumps && self.decks[deck].bpm_at(t) != bpm_before => self.relock(deck, t),
            _ => {}
        }
    }

    /// Put synced deck `x` back on the beat of the deck it follows.
    fn rephase(&mut self, x: usize, t: i64) {
        if let Some(y) = self.sync_target(x) {
            self.sync_deck(x, y, t, false);
        }
    }

    /// Match deck `x` to deck `y`: tempo (if `match_tempo`) and beat phase.
    /// Uses half/double time when that's the smaller change (e.g. an 87 BPM
    /// track against 174). Returns false if either deck lacks a grid.
    fn sync_deck(&mut self, x: usize, y: usize, t: i64, match_tempo: bool) -> bool {
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
            // Under a roll, the phase is the track's underneath: both move.
            if let Some(r) = &mut d.roll {
                r.slip_pos = (r.slip_pos + shift).clamp(0.0, d.duration);
            }
            d.gen += 1;
        }
        true
    }

    /// A tempo change on a deck in the sync group carries the rest of the
    /// group: the master and every synced deck.
    fn follow_tempo(&mut self, moved: usize, t: i64) {
        if !self.in_sync_group(moved) {
            return;
        }
        for d in 0..DECKS {
            if d != moved && self.in_sync_group(d) {
                self.lock_tempo(d, moved, t);
            }
        }
    }

    /// Deck `c`'s own BPM just changed (its grid was edited, or it reached
    /// or jumped to a section with another tempo). The synced decks follow
    /// it if it's the master; if it's synced, it keeps matching the master.
    fn relock(&mut self, c: usize, t: i64) {
        if self.master_deck() == Some(c) {
            for d in 0..DECKS {
                if d != c && self.decks[d].synced {
                    self.lock_tempo(d, c, t);
                }
            }
        } else if self.decks[c].synced {
            if let Some(y) = self.sync_target(c) {
                self.lock_tempo(c, y, t);
            }
        }
    }

    /// Set deck `x`'s tempo so its BPM matches deck `y`'s, keeping whatever
    /// half/double relation is the smallest change. Beat phase is left alone.
    fn lock_tempo(&mut self, x: usize, y: usize, t: i64) {
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
                d.preview_slot = None;
                d.anchor_time = t;
                d.spin = None;
                d.anchor_pos = cue.clamp(0.0, d.duration);
                d.cue_point = d.anchor_pos;
                d.hot_cues = [None; HOT_CUES];
                d.clear_loop();
                // Already let go (see `lets_go_of_roll`).
                d.roll = None;
                d.bend = 0.0;
                d.scratching = false;
                d.gen += 1;
            }
            Action::Eject { .. } => {
                d.track = None;
                d.duration = 0.0;
                d.playing = false;
                d.cue_preview = false;
                d.preview_slot = None;
                d.anchor_time = t;
                d.spin = None;
                d.anchor_pos = 0.0;
                d.cue_point = 0.0;
                d.hot_cues = [None; HOT_CUES];
                d.clear_loop();
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
                    // Play during a preview: keep playing after the release.
                    d.cue_preview = false;
                    d.preview_slot = None;
                }
            }
            Action::Pause { .. } => {
                if d.playing {
                    d.reanchor(t);
                    d.playing = false;
                    d.gen += 1;
                }
                d.cue_preview = false;
                d.preview_slot = None;
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
                    d.spin = None;
                    d.gen += 1;
                } else if !d.playing {
                    let pos = d.position_at(t);
                    if (pos - d.cue_point).abs() < 0.01 {
                        // Paused on the cue point: preview while held.
                        d.playing = true;
                        d.cue_preview = true;
                        d.anchor_pos = d.cue_point;
                        d.anchor_time = t;
                        d.spin = None;
                        d.gen += 1;
                    } else {
                        // Paused elsewhere: set a new cue point here.
                        d.cue_point = pos;
                        d.anchor_pos = pos;
                        d.anchor_time = t;
                        d.spin = None;
                    }
                }
            }
            Action::CueUp { .. } => {
                if d.cue_preview && d.preview_slot.is_none() {
                    d.cue_preview = false;
                    d.playing = false;
                    d.anchor_pos = d.cue_point;
                    d.anchor_time = t;
                    d.spin = None;
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
                        // Paused (or only previewing): play while held.
                        // Playing: jump and keep playing.
                        let preview = !d.playing || d.cue_preview;
                        d.anchor_pos = p;
                        d.anchor_time = t;
                        d.spin = None;
                        d.playing = true;
                        d.cue_preview = preview;
                        d.preview_slot = preview.then_some(slot);
                        d.gen += 1;
                    }
                    None => d.hot_cues[s] = Some(d.position_at(t)),
                }
            }
            Action::HotCueUp { slot, .. } => {
                if d.cue_preview && d.preview_slot == Some(slot) {
                    let held = d.hot_cues.get(slot as usize).copied().flatten();
                    let back = held.unwrap_or(d.position_at(t));
                    d.cue_preview = false;
                    d.preview_slot = None;
                    d.playing = false;
                    d.anchor_pos = back;
                    d.anchor_time = t;
                    d.spin = None;
                    d.gen += 1;
                }
            }
            Action::ClearHotCue { slot, .. } => {
                if let Some(h) = d.hot_cues.get_mut(slot as usize) {
                    *h = None;
                }
            }
            Action::LoopIn { quantize, .. } => {
                if d.track.is_none() {
                    return;
                }
                let p = d.position_at(t);
                // Quantized, it can land a little after the playhead, which
                // then walks into the loop.
                let a = if quantize { d.snap_to_beat(p) } else { p };
                match d.active_loop() {
                    Some((_, b)) => {
                        if a < b - MIN_LOOP {
                            d.reanchor(t);
                            d.loop_in = Some(a);
                            d.loop_beats = None;
                        }
                    }
                    None => {
                        d.loop_in = Some(a);
                        d.loop_out = None;
                        d.looping = false;
                        d.loop_beats = None;
                    }
                }
            }
            Action::LoopOut { quantize, .. } => {
                let (Some(a), Some(_)) = (d.loop_in, d.track) else {
                    return;
                };
                let p = d.position_at(t);
                if p < a + MIN_LOOP {
                    return;
                }
                let (end, beats) = match d.grid.filter(|_| quantize) {
                    Some(_) => {
                        // The nearest musical length, timed with the tempo
                        // at the in point (as beat loops are).
                        let len = d.beat_len_at(a);
                        let n = quantized_loop_beats((p - a) / len);
                        let want = a + n * len;
                        let end = want.min(d.duration);
                        (end, (end == want).then_some(n))
                    }
                    None => (p, None),
                };
                if end - a < MIN_LOOP {
                    return;
                }
                // A playhead at or past the new out point has gone round:
                // settling the loop takes it back to the in point (plus how
                // far past it was, so a quantized loop stays on the beat).
                // No `gen` bump: the renderer follows loops itself and wraps
                // seamlessly.
                d.reanchor(t);
                d.loop_out = Some(end);
                d.looping = true;
                d.loop_beats = beats;
            }
            Action::Reloop { .. } => {
                if d.track.is_none() {
                    return;
                }
                if d.looping {
                    d.reanchor(t);
                    d.looping = false;
                } else if let Some((a, _)) = d.loop_span() {
                    d.anchor_pos = a;
                    d.anchor_time = t;
                    d.spin = None;
                    d.looping = true;
                    d.gen += 1;
                }
            }
            Action::BeatLoop { beats, .. } => {
                if d.track.is_none() || !(beats > 0.0 && beats <= MAX_LOOP_BEATS) {
                    return;
                }
                let same = d.loop_beats.is_some_and(|b| (b - beats).abs() < 1e-9);
                match d.active_loop() {
                    Some(_) if same => {
                        d.reanchor(t);
                        d.looping = false;
                    }
                    Some((a, _)) => {
                        let want = a + beats * d.beat_len_at(a);
                        let end = want.min(d.duration);
                        if end - a >= MIN_LOOP {
                            d.reanchor(t);
                            d.loop_out = Some(end);
                            d.loop_beats = (end == want).then_some(beats);
                        }
                    }
                    None => {
                        if let Some((a, end, whole)) = d.grid_loop(d.position_at(t), beats) {
                            d.reanchor(t);
                            d.loop_in = Some(a);
                            d.loop_out = Some(end);
                            d.looping = true;
                            d.loop_beats = whole.then_some(beats);
                        }
                    }
                }
            }
            Action::Roll { beats, on, .. } => {
                let same = d.roll.is_some_and(|r| (r.beats - beats).abs() < 1e-9);
                if !on {
                    if same {
                        d.end_roll(t);
                    }
                    return;
                }
                if same
                    || d.track.is_none()
                    || !d.playing
                    || !(beats > 0.0 && beats <= MAX_LOOP_BEATS)
                {
                    return;
                }
                // A new roll starts on the grid at the playhead; another
                // size from the held roll's in point.
                let from = d.roll.map_or_else(|| d.position_at(t), |r| r.loop_in);
                let Some((a, b, _)) = d.grid_loop(from, beats) else {
                    return;
                };
                d.reanchor(t);
                let heard = d.anchor_pos;
                let (slip_pos, now) = match d.roll {
                    // Switching: the same point in the roll as the track
                    // underneath, so it stays in step with it.
                    Some(r) => (r.slip_pos, a + (r.slip_pos - a).rem_euclid(b - a)),
                    None => (heard, heard),
                };
                d.roll = Some(Roll {
                    beats,
                    loop_in: a,
                    loop_out: b,
                    slip_time: t,
                    slip_pos,
                });
                if (now - heard).abs() > 1e-6 {
                    d.anchor_pos = now;
                    d.spin = None;
                    d.gen += 1;
                }
            }
            Action::LoopHalve { .. } | Action::LoopDouble { .. } => {
                let Some((a, b)) = d.loop_span() else { return };
                let double = matches!(ev.action, Action::LoopDouble { .. });
                let factor = if double { 2.0 } else { 0.5 };
                let want = a + (b - a) * factor;
                let end = want.min(d.duration);
                if end - a < MIN_LOOP || end == b {
                    return;
                }
                d.reanchor(t);
                d.loop_out = Some(end);
                d.loop_beats = d.loop_beats.map(|x| x * factor).filter(|_| end == want);
            }
            Action::Seek { pos, .. } => {
                if d.track.is_some() {
                    d.anchor_pos = pos.clamp(0.0, d.duration);
                    d.anchor_time = t;
                    d.spin = None;
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
            // Under a roll, only what's heard moves: the track runs on.
            Action::Nudge { delta, .. } => {
                if d.track.is_some() {
                    d.reanchor(t);
                    d.anchor_pos = (d.anchor_pos + delta).clamp(0.0, d.duration);
                }
            }
            Action::Volume { v, .. } => d.volume = v.clamp(0.0, 1.0),
            Action::Trim { v, .. } => d.trim = v.clamp(0.0, 1.0),
            Action::Filter { v, .. } => d.filter = v.clamp(0.0, 1.0),
            Action::JogTouch { down, spin, .. } => {
                if d.track.is_some() && d.scratching != down {
                    // Re-anchor so the playhead is continuous either way.
                    // Touching a spinning record catches it.
                    d.reanchor(t);
                    d.scratching = down;
                    d.spin = None;
                    if !down {
                        // Let go mid-spin: the record runs on by itself.
                        d.spin = spin::let_go(spin as f64, d.rest_speed(), d.anchor_pos);
                    }
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
            // The effects aren't the track's: none of them moves the
            // playhead or lets go of a roll.
            Action::FxOn { slot, on, .. } => {
                if let Some(s) = d.fx.slots.get_mut(slot as usize) {
                    if on && !s.on {
                        s.since = t;
                    }
                    s.on = on;
                }
            }
            Action::FxDepth { slot, v, .. } => {
                if let Some(s) = d.fx.slots.get_mut(slot as usize) {
                    // NaN would stick (clamp passes it through).
                    s.depth = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
                }
            }
            Action::FxSelect { slot, kind, .. } => {
                if let Some(s) = d.fx.slots.get_mut(slot as usize) {
                    if s.kind != kind {
                        s.kind = kind;
                        s.since = t;
                    }
                }
            }
            Action::FxBeats { beats, .. } => {
                d.fx.beats = beats.min(FX_BEATS.len() as u8 - 1);
            }
            Action::Crossfader { .. }
            | Action::CrossfaderCurve { .. }
            | Action::XfAssign { .. }
            | Action::Sync { .. }
            | Action::Relock { .. } => {}
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
            h.u64(d.preview_slot.map_or(u64::MAX, u64::from));
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
            match d.spin {
                Some(v) => {
                    h.u64(1);
                    h.f64(v);
                }
                None => h.u64(0),
            }
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
            h.f64(d.loop_in.unwrap_or(-1.0));
            h.f64(d.loop_out.unwrap_or(-1.0));
            h.u64(d.looping as u64);
            h.f64(d.loop_beats.unwrap_or(-1.0));
            match d.roll {
                Some(r) => {
                    h.u64(1);
                    h.f64(r.beats);
                    h.f64(r.loop_in);
                    h.f64(r.loop_out);
                    h.u64(r.slip_time as u64);
                    h.f64(r.slip_pos);
                }
                None => h.u64(0),
            }
            for s in &d.fx.slots {
                h.u64(s.kind as u64);
                h.u64(s.on as u64);
                h.u64(s.depth.to_bits() as u64);
                h.u64(s.since as u64);
            }
            h.u64(d.fx.beats as u64);
            h.u64(d.gen);
        }
        h.u64(self.crossfader.to_bits() as u64);
        h.u64(self.xf_curve as u64);
        for side in &self.xf_assign {
            h.u64(*side as u64);
        }
        h.u64(self.master.map_or(u64::MAX, u64::from));
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

/// Whether `action` lets go of a loop roll held on deck `d` before it
/// applies: anything that jumps (a seek, a hot cue, CUE, the end of a
/// preview, loading), PAUSE, and every loop control. What's left (tempo,
/// bend, sync, scratching, the mixer) carries on under the roll.
fn lets_go_of_roll(action: &Action, d: &DeckState) -> bool {
    if d.roll.is_none() {
        return false;
    }
    match *action {
        Action::Load { .. }
        | Action::Eject { .. }
        | Action::Pause { .. }
        | Action::CueDown { .. }
        | Action::Seek { .. }
        | Action::LoopIn { .. }
        | Action::LoopOut { .. }
        | Action::Reloop { .. }
        | Action::BeatLoop { .. }
        | Action::LoopHalve { .. }
        | Action::LoopDouble { .. } => true,
        // Jumping to a hot cue; setting one doesn't jump.
        Action::HotCue { slot, .. } => d.hot_cues.get(slot as usize).is_some_and(|h| h.is_some()),
        // Ending a preview from a pause jumps back.
        Action::CueUp { .. } => d.cue_preview && d.preview_slot.is_none(),
        Action::HotCueUp { slot, .. } => d.cue_preview && d.preview_slot == Some(slot),
        _ => false,
    }
}

/// Has a track with a beatgrid, so it can sync or be synced to.
fn has_beat(d: &DeckState) -> bool {
    d.track.is_some() && d.grid.is_some()
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
        if self.has(ev.origin, ev.seq) {
            return false;
        }
        self.insert_new(ev);
        true
    }

    /// Insert an event this booth has just made. Its sequence number is new,
    /// so there's no looking for it among the others, which matters on a
    /// timeline that keeps minutes of history (see `Session`'s held room).
    pub fn insert_new(&mut self, ev: Event) {
        let idx = self.events.partition_point(|e| e.key() <= ev.key());
        self.events.insert(idx, ev);
    }

    /// Put many events in at once, each at its own time, as `insert` would
    /// one by one, but without clamping: one older than the base may already
    /// be folded into it, so it's left out. So is one already here. Returns
    /// the events that went in.
    pub fn merge(&mut self, events: Vec<Event>) -> Vec<Event> {
        let mut have: std::collections::HashSet<(u64, u64)> =
            self.events.iter().map(|e| (e.origin, e.seq)).collect();
        let added: Vec<Event> = events
            .into_iter()
            .filter(|e| e.time >= self.base_time && have.insert((e.origin, e.seq)))
            .collect();
        if !added.is_empty() {
            self.events.extend(added.iter().cloned());
            // Keys are unique, so this is the order `insert` gives.
            self.events.sort_by_key(Event::key);
        }
        added
    }

    /// Is the event (origin, seq) among the recent ones?
    pub fn has(&self, origin: u64, seq: u64) -> bool {
        self.events
            .iter()
            .any(|e| e.origin == origin && e.seq == seq)
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
    fn crossfader_curve_is_room_state() {
        let mut tl = Timeline::new(0);
        tl.insert(ev(1_000, 1, 1, Action::Crossfader { v: 0.3 }));
        let before = tl.latest();
        assert_eq!(before.xf_curve, XfCurve::ConstantPower);
        tl.insert(ev(
            2_000,
            2,
            1,
            Action::CrossfaderCurve {
                curve: XfCurve::FullCentre,
            },
        ));
        let after = tl.latest();
        assert_eq!(after.xf_curve, XfCurve::FullCentre);
        assert_eq!(tl.state_at(1_500).xf_curve, XfCurve::ConstantPower);
        // The fader itself, and who last moved it, are untouched.
        assert_eq!((after.crossfader, after.xf_touch), (0.3, 1));
        assert_ne!(before.digest(), after.digest());
        // Sent over the wire as part of a timeline.
        let json = serde_json::to_string(&tl).unwrap();
        let back: Timeline = serde_json::from_str(&json).unwrap();
        assert_eq!(back.latest(), after);
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

    /// Merging a booth's moves in one go puts each at its own time, exactly
    /// as inserting them one by one would, skips any already there, and
    /// leaves out any older than the base (the timeline may already have
    /// folded them in, and applying one twice would move a deck twice).
    #[test]
    fn merging_moves_places_them_like_inserting_them() {
        let mut room = Timeline::new(1_000_000);
        room.insert(ev(
            1_500_000,
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
        ));
        room.insert(ev(2_000_000, 1, 2, Action::Crossfader { v: 0.9 }));
        room.insert(ev(
            2_600_000,
            2,
            1,
            Action::Nudge {
                deck: 0,
                delta: 0.5,
            },
        ));
        room.insert(ev(4_000_000, 1, 3, Action::Volume { deck: 0, v: 0.2 }));
        let moves = vec![
            // Already folded into the base: left out.
            ev(
                900_000,
                2,
                0,
                Action::Nudge {
                    deck: 0,
                    delta: 9.0,
                },
            ),
            ev(1_800_000, 2, 2, Action::Play { deck: 0 }),
            ev(
                2_600_000,
                2,
                1,
                Action::Nudge {
                    deck: 0,
                    delta: 0.5,
                },
            ), // already here
            ev(3_000_000, 2, 3, Action::Crossfader { v: 0.1 }),
            ev(3_500_000, 2, 4, Action::Volume { deck: 0, v: 0.7 }),
        ];
        let mut one_by_one = room.clone();
        for m in moves.iter().skip(1) {
            one_by_one.insert(m.clone());
        }
        let added = room.merge(moves);
        assert_eq!(
            added.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [2, 3, 4],
            "only new moves at or after the base"
        );
        assert_eq!(room.events(), one_by_one.events());
        let s = room.latest();
        // The later move on each control wins, whoever made it.
        assert_eq!(s.crossfader, 0.1);
        assert_eq!(s.decks[0].volume, 0.2);
        // Played from 1.8 s, nudged once.
        assert!(s.decks[0].playing);
        assert!((s.decks[0].position_at(5_000_000) - 3.7).abs() < 1e-9);
        assert!(room.merge(Vec::new()).is_empty());
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
                spin: 0.0,
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
                spin: 0.0,
            },
        ));
        assert!((s.decks[0].position_at(14_000_000) - 10.5).abs() < 1e-9);
    }

    /// A room with deck A playing (or paused at) 20 s of a 100 s track, a
    /// hand on its platter at 10 s, let go at 10 s at `spin` x normal speed.
    fn let_go_at(spin: f32, playing: bool) -> RoomState {
        let mut s = RoomState::default();
        let load = Action::Load {
            deck: 0,
            track: tid(3),
            duration: 100.0,
            cue: 20.0,
            grid: None,
            key: -1,
        };
        s.apply(&ev(0, 1, 1, load));
        if playing {
            s.apply(&ev(0, 1, 2, Action::Seek { deck: 0, pos: 10.0 }));
            s.apply(&ev(0, 1, 3, Action::Play { deck: 0 }));
        }
        let touch = |down, spin| Action::JogTouch {
            deck: 0,
            down,
            spin,
        };
        s.apply(&ev(10_000_000, 1, 4, touch(true, 0.0)));
        s.apply(&ev(10_000_000, 1, 5, touch(false, spin)));
        s
    }

    #[test]
    fn a_backspin_let_go_runs_on_then_the_deck_plays_on() {
        let s = let_go_at(-5.5, true);
        let d = &s.decks[0];
        assert!((d.position_at(10_000_000) - 20.0).abs() < 1e-9);
        // It carries on backwards at the hand's speed, slowing down...
        assert!((d.speed_at(10_000_000) + 5.5).abs() < 1e-9);
        assert!(d.spinning_at(11_000_000) && d.speed_at(11_000_000) < 0.0);
        assert!(d.position_at(11_000_000) < 20.0 - 2.0);
        // ...then the deck plays on at its own speed.
        assert!(!d.spinning_at(12_000_000));
        assert_eq!(d.speed_at(12_000_000), 1.0);
        let p = d.position_at(12_000_000);
        assert!((d.position_at(13_000_000) - p - 1.0).abs() < 1e-9);
        assert_eq!(d.next_tempo_change(11_000_000, 10_000_000), None);
    }

    #[test]
    fn a_paused_record_flicked_and_let_go_coasts_to_a_stop() {
        let s = let_go_at(3.0, false);
        let d = &s.decks[0];
        assert!(d.spinning_at(10_300_000));
        let a = d.position_at(10_300_000);
        assert!(a > 20.3);
        let stop = d.position_at(15_000_000);
        assert!(stop > a && !d.spinning_at(15_000_000));
        assert_eq!(d.position_at(20_000_000), stop);
        // Let go gently (or held still): it stays where the hand left it.
        let s = let_go_at(0.3, false);
        assert_eq!(s.decks[0].spin, None);
        assert_eq!(s.decks[0].position_at(15_000_000), 20.0);
    }

    #[test]
    fn a_spinning_record_keeps_its_path_through_other_changes() {
        let plain = let_go_at(-5.5, true);
        let mut s = let_go_at(-5.5, true);
        // Re-anchoring mid-spin (here: key lock off and on) changes nothing.
        let same = |s: &RoomState, times: &[i64]| {
            for &t in times {
                let (a, b) = (at(&plain, 0, t), at(s, 0, t));
                assert!((a - b).abs() < 1e-9, "at {t}: {a} vs {b}");
            }
        };
        s.apply(&ev(
            10_400_000,
            1,
            6,
            Action::KeyLock { deck: 0, on: false },
        ));
        same(&s, &[10_500_000, 10_900_000]);
        s.apply(&ev(10_900_000, 1, 7, Action::KeyLock { deck: 0, on: true }));
        same(&s, &[11_000_000, 11_500_000, 13_000_000]);
        // Pausing mid-spin: the record still runs down, then stays stopped.
        s.apply(&ev(11_000_000, 1, 8, Action::Pause { deck: 0 }));
        assert!(s.decks[0].spinning_at(11_100_000));
        assert_eq!(at(&s, 0, 14_000_000), at(&s, 0, 20_000_000));
    }

    #[test]
    fn catching_or_jumping_stops_a_spinning_record() {
        let mut s = let_go_at(4.0, false);
        let caught = at(&s, 0, 10_200_000);
        s.apply(&ev(
            10_200_000,
            1,
            6,
            Action::JogTouch {
                deck: 0,
                down: true,
                spin: 0.0,
            },
        ));
        assert_eq!(at(&s, 0, 12_000_000), caught);
        let mut s = let_go_at(4.0, false);
        s.apply(&ev(10_200_000, 1, 6, Action::Seek { deck: 0, pos: 50.0 }));
        assert_eq!(s.decks[0].spin, None);
        assert_eq!(at(&s, 0, 12_000_000), 50.0);
    }

    #[test]
    fn a_backspin_stops_at_the_start_of_the_track() {
        let mut s = RoomState::default();
        let load = Action::Load {
            deck: 0,
            track: tid(3),
            duration: 100.0,
            cue: 0.4,
            grid: None,
            key: -1,
        };
        s.apply(&ev(0, 1, 1, load));
        s.apply(&ev(0, 1, 2, Action::Play { deck: 0 }));
        let touch = |down, spin| Action::JogTouch {
            deck: 0,
            down,
            spin,
        };
        s.apply(&ev(100_000, 1, 3, touch(true, 0.0)));
        s.apply(&ev(100_000, 1, 4, touch(false, -6.0)));
        // Runs back to 0:00 and no further, then plays from there.
        let d = &s.decks[0];
        let mut lowest = f64::MAX;
        let mut t = 100_000;
        while d.spinning_at(t) {
            lowest = lowest.min(d.position_at(t));
            t += 1_000;
        }
        assert!(lowest.abs() < 1e-6, "lowest {lowest}");
        assert!(d.position_at(t + 1_000_000) > 0.9);
    }

    #[test]
    fn jog_touch_on_the_wire() {
        let a = Action::JogTouch {
            deck: 1,
            down: false,
            spin: -5.37,
        };
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<Action>(&json).unwrap(), a);
        // The release speed survives the trip exactly (it's an f32).
        let old = json.replace(",\"spin\":-5.37", "");
        assert_ne!(old, json);
        assert_eq!(
            serde_json::from_str::<Action>(&old).unwrap(),
            Action::JogTouch {
                deck: 1,
                down: false,
                spin: 0.0
            }
        );
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

    /// Phase difference (in beats, -0.5..0.5) between decks `x` and `y`.
    fn phase_gap(s: &RoomState, x: usize, y: usize, t: i64) -> f64 {
        let d = s.decks[x].beat_phase(1.0, t) - s.decks[y].beat_phase(1.0, t);
        d - d.round()
    }

    fn bpm(s: &RoomState, d: usize, t: i64) -> f64 {
        s.decks[d].effective_bpm_at(t).unwrap()
    }

    #[test]
    fn four_decks_sync_to_the_master() {
        let mut s = RoomState::default();
        let mut seq = 0;
        let mut go = |s: &mut RoomState, t: i64, a: Action| {
            seq += 1;
            s.apply(&ev(t, 1, seq, a));
        };
        for (d, bpm) in [(0, 128.0), (1, 124.0), (2, 126.0), (3, 140.0)] {
            go(&mut s, 0, grid_load(d, d + 1, bpm, 0.05 * d as f64));
        }
        go(&mut s, 0, Action::Play { deck: 0 });
        go(&mut s, 0, Action::Play { deck: 3 });
        assert_eq!(s.master, None);
        // B syncs to A, the deck across from it: A is the master.
        go(&mut s, 1_000_000, Action::Sync { deck: 1, on: true });
        assert_eq!(s.master, Some(0));
        // C syncs to the master too, not to B or the playing deck D.
        go(&mut s, 2_000_000, Action::Sync { deck: 2, on: true });
        assert_eq!(s.sync_target(2), Some(0));
        assert!((bpm(&s, 2, 2_000_000) - 128.0).abs() < 1e-9);
        go(&mut s, 2_500_000, Action::Play { deck: 2 });
        go(&mut s, 2_500_000, Action::Play { deck: 1 });
        for d in [1, 2] {
            assert!(
                phase_gap(&s, d, 0, 9_000_000).abs() < 1e-6,
                "deck {d} on A's beat"
            );
        }
        // A tempo change on any deck in the group carries the whole group,
        // and nothing else.
        go(
            &mut s,
            10_000_000,
            Action::Tempo {
                deck: 2,
                tempo: 1.0,
            },
        );
        let t = 11_000_000;
        for d in [0, 1, 2] {
            assert!((bpm(&s, d, t) - 126.0).abs() < 1e-9, "deck {d}");
        }
        assert_eq!(bpm(&s, 3, t), 140.0);
        go(
            &mut s,
            12_000_000,
            Action::Tempo {
                deck: 3,
                tempo: 0.95,
            },
        );
        assert!((bpm(&s, 0, t) - 126.0).abs() < 1e-9);
        // D joins: it syncs to the master like the rest.
        go(&mut s, 13_000_000, Action::Sync { deck: 3, on: true });
        assert!((bpm(&s, 3, 14_000_000) - 126.0).abs() < 1e-9);
        assert!(phase_gap(&s, 3, 0, 14_000_000).abs() < 1e-6);
        // Everyone off: no master.
        for deck in 1..4 {
            go(&mut s, 15_000_000, Action::Sync { deck, on: false });
        }
        assert_eq!(s.master, None);
        assert!(!s.in_sync_group(0));
    }

    #[test]
    fn the_master_hands_over_when_it_stops_or_changes_track() {
        let mut s = RoomState::default();
        let mut seq = 0;
        let mut go = |s: &mut RoomState, t: i64, a: Action| {
            seq += 1;
            s.apply(&ev(t, 1, seq, a));
        };
        go(&mut s, 0, grid_load(0, 1, 128.0, 0.0));
        go(&mut s, 0, grid_load(1, 2, 124.0, 0.0));
        go(&mut s, 0, Action::Play { deck: 0 });
        go(&mut s, 0, Action::Play { deck: 1 });
        go(&mut s, 0, Action::Sync { deck: 1, on: true });
        assert_eq!(s.master, Some(0));
        // CUE stops the master: B is what's playing, so B leads.
        go(&mut s, 1_000_000, Action::CueDown { deck: 0 });
        assert_eq!(s.master, Some(1));
        // Holding CUE on A is only a preview: it doesn't take the master back.
        go(&mut s, 1_100_000, Action::CueDown { deck: 0 });
        assert!(s.decks[0].cue_preview);
        go(&mut s, 1_200_000, Action::CueUp { deck: 0 });
        assert_eq!(s.master, Some(1));
        // A can now be prepared without dragging B.
        go(
            &mut s,
            2_000_000,
            Action::Tempo {
                deck: 0,
                tempo: 1.05,
            },
        );
        assert!((bpm(&s, 1, 2_000_000) - 128.0).abs() < 1e-9);
        // And synced to B.
        go(&mut s, 3_000_000, Action::Sync { deck: 0, on: true });
        assert_eq!(s.sync_target(0), Some(1));
        assert!((bpm(&s, 0, 3_000_000) - 128.0).abs() < 1e-9);
        // A new track on the master: the synced deck that's playing takes over.
        go(&mut s, 4_000_000, Action::Play { deck: 0 });
        go(&mut s, 5_000_000, Action::Sync { deck: 0, on: false });
        go(&mut s, 5_000_000, Action::Sync { deck: 1, on: false });
        go(&mut s, 6_000_000, Action::Sync { deck: 0, on: true });
        assert_eq!(s.master, Some(1));
        // B can't be loaded while it can be heard: pull its fader down first.
        go(&mut s, 6_500_000, Action::Volume { deck: 1, v: 0.0 });
        go(&mut s, 7_000_000, grid_load(1, 3, 90.0, 0.0));
        assert_eq!(s.decks[1].track, Some(tid(3)));
        assert_eq!(s.master, Some(0), "A is synced and playing");
        go(
            &mut s,
            8_000_000,
            Action::Tempo {
                deck: 1,
                tempo: 1.2,
            },
        );
        assert!(
            (bpm(&s, 0, 8_000_000) - 128.0).abs() < 1e-9,
            "B no longer drags A"
        );
        // The last synced deck off: no master.
        go(&mut s, 9_000_000, Action::Sync { deck: 0, on: false });
        assert_eq!(s.master, None);
    }

    #[test]
    fn a_master_tempo_change_carries_every_synced_deck() {
        let mut s = RoomState::default();
        s.apply(&ev(0, 1, 1, grid_load(0, 1, 120.0, 0.0)));
        s.apply(&ev(0, 1, 2, set_grid(0, 1, two_tempo_grid())));
        s.apply(&ev(0, 1, 3, grid_load(1, 3, 128.0, 0.0)));
        s.apply(&ev(0, 1, 4, grid_load(3, 4, 64.0, 0.0)));
        for (i, deck) in [0u8, 1, 3].into_iter().enumerate() {
            s.apply(&ev(0, 1, 5 + i as u64, Action::Play { deck }));
        }
        // B syncs to A across the crossfader, so A is the master; D follows it.
        s.apply(&ev(0, 1, 9, Action::Sync { deck: 1, on: true }));
        s.apply(&ev(0, 1, 10, Action::Sync { deck: 3, on: true }));
        assert_eq!(s.master, Some(0));
        // A reaches its 125 BPM section: B follows, and D at half time.
        s.apply(&ev(20_500_000, 1, 11, Action::Relock { deck: 0 }));
        assert!((bpm(&s, 1, 21_000_000) - 125.0).abs() < 1e-9);
        assert!((bpm(&s, 3, 21_000_000) - 62.5).abs() < 1e-9);
    }

    #[test]
    fn keys_compare_with_the_deck_being_mixed_against() {
        let mut s = RoomState::default();
        assert_eq!(s.mix_partner(0), None);
        for d in 0..4 {
            s.apply(&ev(0, 1, 1 + d as u64, grid_load(d, d + 1, 120.0, 0.0)));
        }
        // Nothing playing: the deck across the crossfader.
        assert_eq!(s.mix_partner(0), Some(1));
        assert_eq!(s.mix_partner(3), Some(2));
        // A deck that's playing.
        s.apply(&ev(0, 1, 5, Action::Play { deck: 3 }));
        assert_eq!(s.mix_partner(0), Some(3));
        // The sync master.
        s.apply(&ev(0, 1, 6, Action::Sync { deck: 1, on: true }));
        assert_eq!(s.master, Some(3), "B synced to the deck that's playing");
        assert_eq!(s.mix_partner(0), Some(3));
        // The master itself looks at what's playing, then across.
        assert_eq!(s.mix_partner(3), Some(2));
        s.apply(&ev(0, 1, 7, Action::Play { deck: 1 }));
        assert_eq!(s.mix_partner(3), Some(1));
    }

    #[test]
    fn crossfader_assign_is_room_state() {
        let mut tl = Timeline::new(0);
        assert_eq!(
            tl.latest().xf_assign,
            [XfSide::A, XfSide::B, XfSide::A, XfSide::B]
        );
        let before = tl.latest();
        tl.insert(ev(
            1_000,
            2,
            1,
            Action::XfAssign {
                deck: 2,
                side: XfSide::Thru,
            },
        ));
        let after = tl.latest();
        assert_eq!(after.xf_assign[2], XfSide::Thru);
        assert_eq!(after.decks[2].last_touch, 2);
        assert_ne!(before.digest(), after.digest());
        let json = serde_json::to_string(&tl).unwrap();
        let back: Timeline = serde_json::from_str(&json).unwrap();
        assert_eq!(back.latest(), after);
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

    /// Deck A playing at full volume, the crossfader all the way to it.
    fn a_playing() -> RoomState {
        let mut s = RoomState::default();
        s.apply(&ev(0, 1, 1, grid_load(0, 1, 120.0, 0.0)));
        s.apply(&ev(0, 1, 2, Action::Play { deck: 0 }));
        s.apply(&ev(0, 1, 3, Action::Crossfader { v: 0.0 }));
        s
    }

    #[test]
    fn a_playing_deck_is_audible_until_something_mutes_it() {
        let t = 5_000_000;
        let s = a_playing();
        assert!(s.audible_in_master(0, t));
        assert!(s
            .load_refusal(0, t)
            .unwrap()
            .starts_with("Deck A is playing"));
        assert!(!s.audible_in_master(1, t), "no track");
        assert!(!s.audible_in_master(DECKS, t));
        let with = |a: Action| {
            let mut s = s;
            s.apply(&ev(1_000_000, 1, 10, a));
            s
        };
        // Paused, or never started.
        assert!(!with(Action::Pause { deck: 0 }).audible_in_master(0, t));
        // Channel fader: about 10% is -40 dB, and below that is silent.
        for (v, audible) in [(0.0, false), (0.09, false), (0.11, true), (1.0, true)] {
            let s = with(Action::Volume { deck: 0, v });
            assert_eq!(s.audible_in_master(0, t), audible, "fader at {v}");
        }
        // Filter: silent within 1% of either end (low-pass or high-pass).
        for (v, audible) in [
            (0.0, false),
            (0.008, false),
            (0.02, true),
            (0.5, true),
            (0.98, true),
            (0.992, false),
            (1.0, false),
        ] {
            let s = with(Action::Filter { deck: 0, v });
            assert_eq!(s.audible_in_master(0, t), audible, "filter at {v}");
        }
        // Trim and EQ turned all the way down mute it too.
        assert!(!with(Action::Trim { deck: 0, v: 0.0 }).audible_in_master(0, t));
        let mut killed = s;
        for band in 0..3 {
            killed.apply(&ev(
                1_000_000,
                1,
                10 + band as u64,
                Action::Eq {
                    deck: 0,
                    band,
                    v: 0.0,
                },
            ));
        }
        assert!(!killed.audible_in_master(0, t));
        assert!(with(Action::Eq {
            deck: 0,
            band: 0,
            v: 0.0
        })
        .audible_in_master(0, t));
        // Cuts add up: the fader at 30% (-21 dB) and trim down 21 dB.
        let mut quiet = with(Action::Volume { deck: 0, v: 0.3 });
        assert!(quiet.audible_in_master(0, t));
        quiet.apply(&ev(1_000_000, 1, 11, Action::Trim { deck: 0, v: 0.15 }));
        assert!(!quiet.audible_in_master(0, t));
    }

    #[test]
    fn the_crossfader_mutes_a_deck_only_at_the_far_end() {
        let t = 5_000_000;
        for curve in XfCurve::ALL {
            for side in XfSide::ALL {
                let mut s = a_playing();
                s.apply(&ev(1_000_000, 1, 10, Action::CrossfaderCurve { curve }));
                s.apply(&ev(1_000_000, 1, 11, Action::XfAssign { deck: 0, side }));
                let at = |x: f32| {
                    let mut s = s;
                    s.apply(&ev(2_000_000, 1, 12, Action::Crossfader { v: x }));
                    s.audible_in_master(0, t)
                };
                // The end away from the deck's side cuts it; THRU never does.
                let (near, far) = match side {
                    XfSide::A => (0.0, 1.0),
                    XfSide::B => (1.0, 0.0),
                    XfSide::Thru => (0.0, 1.0),
                };
                assert!(at(near), "{curve:?} {side:?} near end");
                assert!(at(0.5), "{curve:?} {side:?} middle");
                assert_eq!(at(far), side == XfSide::Thru, "{curve:?} {side:?} far end");
                // Short of the far end, it's still heard on every curve.
                let short = near + (far - near) * 0.95;
                assert!(at(short), "{curve:?} {side:?} short of the far end");
            }
        }
    }

    #[test]
    fn a_deck_sounds_while_it_plays_previews_or_is_scratched() {
        let mut s = RoomState::default();
        let mut seq = 0;
        let mut go = |s: &mut RoomState, t: i64, a: Action| {
            seq += 1;
            s.apply(&ev(t, 1, seq, a));
        };
        go(&mut s, 0, grid_load(0, 1, 120.0, 0.0));
        assert!(!s.decks[0].sounding_at(1_000_000), "loaded, paused");
        // Holding CUE from a pause previews it.
        go(&mut s, 1_000_000, Action::CueDown { deck: 0 });
        assert!(s.audible_in_master(0, 1_500_000));
        go(&mut s, 2_000_000, Action::CueUp { deck: 0 });
        assert!(!s.audible_in_master(0, 2_500_000));
        // A hand on the platter of a paused deck: scratching.
        go(
            &mut s,
            3_000_000,
            Action::JogTouch {
                deck: 0,
                down: true,
                spin: 0.0,
            },
        );
        assert!(s.audible_in_master(0, 3_500_000));
        // Let go mid-spin: it sounds until it coasts to a stop.
        go(
            &mut s,
            4_000_000,
            Action::JogTouch {
                deck: 0,
                down: false,
                spin: 2.0,
            },
        );
        assert!(s.audible_in_master(0, 4_100_000));
        assert!(!s.audible_in_master(0, 9_000_000));
        // Played off the end of the track: silent, though still "playing".
        go(&mut s, 10_000_000, Action::Play { deck: 0 });
        assert!(s.audible_in_master(0, 10_000_000));
        assert!(s.decks[0].playing);
        assert!(!s.audible_in_master(0, 400_000_000));
        // A roll held near the end keeps going round after the track
        // underneath has run out.
        go(
            &mut s,
            400_000_000,
            Action::Seek {
                deck: 0,
                pos: 298.0,
            },
        );
        go(&mut s, 400_000_000, roll(0, 2.0, true));
        assert!(s.decks[0].roll.is_some());
        assert!(s.decks[0].slip_position_at(405_000_000) >= 300.0);
        assert!(s.audible_in_master(0, 405_000_000));
    }

    #[test]
    fn a_track_cant_be_loaded_over_a_deck_the_room_can_hear() {
        let mut s = a_playing();
        let before = s;
        s.apply(&ev(3_000_000, 2, 1, grid_load(0, 2, 124.0, 0.0)));
        assert_eq!(
            s, before,
            "dropped: nothing changes, not even who touched it"
        );
        assert_eq!(s.digest(), before.digest());
        // Paused, it can be.
        s.apply(&ev(4_000_000, 1, 3, Action::Pause { deck: 0 }));
        s.apply(&ev(5_000_000, 2, 2, grid_load(0, 2, 124.0, 0.0)));
        assert_eq!(s.decks[0].track, Some(tid(2)));
    }

    #[test]
    fn a_load_onto_a_silent_playing_deck_starts_paused_at_the_cue() {
        let mut s = RoomState::default();
        let mut seq = 0;
        let mut go = |s: &mut RoomState, t: i64, a: Action| {
            seq += 1;
            s.apply(&ev(t, 1, seq, a));
        };
        go(&mut s, 0, grid_load(0, 1, 120.0, 0.0));
        go(&mut s, 0, grid_load(1, 2, 124.0, 0.0));
        go(&mut s, 0, Action::Play { deck: 0 });
        go(&mut s, 0, Action::Play { deck: 1 });
        go(&mut s, 0, Action::Sync { deck: 1, on: true });
        go(
            &mut s,
            1_000_000,
            Action::BeatLoop {
                deck: 1,
                beats: 4.0,
            },
        );
        go(
            &mut s,
            1_500_000,
            Action::Bend {
                deck: 1,
                bend: 0.04,
            },
        );
        go(&mut s, 1_800_000, roll(1, 1.0, true));
        assert!(s.decks[1].synced && s.decks[1].looping && s.decks[1].roll.is_some());
        // B is cut by the crossfader (all the way to A), so it can be loaded.
        go(&mut s, 2_000_000, Action::Crossfader { v: 0.0 });
        assert!(s.decks[1].sounding_at(3_000_000));
        assert!(!s.audible_in_master(1, 3_000_000));
        let load = Action::Load {
            deck: 1,
            track: tid(3),
            duration: 200.0,
            cue: 12.5,
            grid: BeatGrid::new(0.2, 126.0).map(Box::new),
            key: 4,
        };
        go(&mut s, 3_000_000, load);
        let b = s.decks[1];
        assert_eq!(b.track, Some(tid(3)));
        assert!(!b.playing, "starts paused");
        assert_eq!((b.position_at(9_000_000), b.cue_point), (12.5, 12.5));
        assert!(!b.synced && !b.looping && b.loop_span().is_none() && b.roll.is_none());
        assert!(!b.cue_preview && !b.scratching && b.spin.is_none());
        assert_eq!(b.bend, 0.0);
        assert_eq!(s.master, None, "nothing synced any more");
        // A plays on untouched.
        assert!(s.decks[0].playing);
        assert!(close(at(&s, 0, 9_000_000), 9.0));
    }

    /// One booth loads onto a muted deck just as the other brings its fader
    /// up. Whichever order the events arrive in, both booths end up with
    /// the same room: the fader came first, so the load is dropped.
    #[test]
    fn a_load_racing_a_fader_comes_out_the_same_on_both_booths() {
        let mut base = Timeline::new(0);
        base.insert(ev(0, 1, 1, grid_load(0, 1, 120.0, 0.0)));
        base.insert(ev(0, 1, 2, Action::Volume { deck: 0, v: 0.0 }));
        base.insert(ev(0, 1, 3, Action::Play { deck: 0 }));
        let fader_up = ev(2_000_000, 1, 4, Action::Volume { deck: 0, v: 0.8 });
        let load = ev(2_000_100, 2, 1, grid_load(0, 2, 124.0, 0.0));
        // Booth 2 loaded first by its own reckoning; booth 1's fader arrives later.
        let mut two = base.clone();
        two.insert(load.clone());
        assert_eq!(two.latest().decks[0].track, Some(tid(2)));
        two.insert(fader_up.clone());
        let mut one = base.clone();
        one.insert(fader_up);
        one.insert(load);
        let (a, b) = (one.latest(), two.latest());
        assert_eq!(a.digest(), b.digest());
        assert_eq!(a.decks[0].track, Some(tid(1)), "the load was dropped");
        assert!(a.decks[0].playing);
        // A load just before the fader came up gets in, on both.
        let early = ev(1_999_900, 2, 1, grid_load(0, 2, 124.0, 0.0));
        let mut three = base.clone();
        three.insert(ev(2_000_000, 1, 4, Action::Volume { deck: 0, v: 0.8 }));
        three.insert(early);
        let c = three.latest().decks[0];
        assert_eq!(c.track, Some(tid(2)));
        assert!(!c.playing);
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
        // A gated hot cue held across the marker, then released: back in
        // the 120 BPM section, so back to its tempo.
        s.apply(&ev(2_000_000, 1, 7, Action::Seek { deck: 1, pos: 19.0 }));
        s.apply(&ev(2_000_000, 1, 8, Action::HotCue { deck: 1, slot: 0 }));
        s.apply(&ev(3_000_000, 1, 9, Action::HotCue { deck: 1, slot: 0 }));
        assert!(s.decks[1].cue_preview);
        s.apply(&ev(4_500_000, 1, 10, Action::Relock { deck: 1 }));
        assert!((s.decks[1].tempo - 128.0 / 125.0).abs() < 1e-9);
        s.apply(&ev(5_000_000, 1, 11, Action::HotCueUp { deck: 1, slot: 0 }));
        assert_eq!(s.decks[1].position_at(5_000_000), 19.0);
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

    fn at(s: &RoomState, deck: usize, t: i64) -> f64 {
        s.decks[deck].position_at(t)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn loop_in_and_out_go_round_and_reloop_exits_and_returns() {
        let mut s = RoomState::default();
        let mut seq = 0;
        let mut go = |s: &mut RoomState, t: i64, a: Action| {
            seq += 1;
            s.apply(&ev(t, 1, seq, a));
        };
        go(&mut s, 0, grid_load(0, 1, 120.0, 0.0));
        // LOOP OUT with no LOOP IN does nothing.
        go(&mut s, 0, loop_out(false));
        assert!(!s.decks[0].looping);
        go(&mut s, 0, Action::Play { deck: 0 });
        go(&mut s, 10_000_000, loop_in(false));
        assert!(!s.decks[0].looping, "waits for LOOP OUT");
        go(&mut s, 12_000_000, loop_out(false));
        let d = s.decks[0];
        assert_eq!(d.active_loop(), Some((10.0, 12.0)));
        assert_eq!(d.gen, s.decks[0].gen);
        // Straight back to the in point, then round and round.
        assert!(close(at(&s, 0, 12_000_000), 10.0));
        assert!(close(at(&s, 0, 13_000_000), 11.0));
        assert!(close(at(&s, 0, 14_500_000), 10.5));
        assert!(close(at(&s, 0, 101_250_000), 11.25));
        // EXIT: carries on from where it is, through the out point.
        go(&mut s, 15_000_000, Action::Reloop { deck: 0 });
        assert!(!s.decks[0].looping);
        assert!(close(at(&s, 0, 17_000_000), 13.0));
        // RELOOP: back to the in point, looping again.
        go(&mut s, 20_000_000, Action::Reloop { deck: 0 });
        assert!(s.decks[0].looping);
        assert!(close(at(&s, 0, 21_000_000), 11.0));
        assert!(close(at(&s, 0, 22_500_000), 10.5));
        // LOOP IN while looping moves the start to the playhead.
        go(&mut s, 23_000_000, loop_in(false));
        assert_eq!(s.decks[0].active_loop(), Some((11.0, 12.0)));
        assert!(close(at(&s, 0, 24_500_000), 11.5));
        // LOOP OUT while looping moves the end to the playhead.
        go(&mut s, 25_750_000, loop_out(false));
        assert_eq!(s.decks[0].active_loop(), Some((11.0, 11.75)));
        assert!(close(at(&s, 0, 26_000_000), 11.25));
        // Loading another track (fader down, so the deck can be loaded while
        // it loops) forgets the loop.
        go(&mut s, 26_500_000, Action::Volume { deck: 0, v: 0.0 });
        go(&mut s, 27_000_000, grid_load(0, 2, 120.0, 0.0));
        assert_eq!(s.decks[0].loop_span(), None);
        assert!(!s.decks[0].looping);
        assert!(!s.decks[0].playing);
    }

    fn loop_in(quantize: bool) -> Action {
        Action::LoopIn { deck: 0, quantize }
    }

    fn loop_out(quantize: bool) -> Action {
        Action::LoopOut { deck: 0, quantize }
    }

    /// Deck A playing from 0:00 at t = 0, at 120 BPM with its first beat at
    /// `first_beat`: a beat every 0.5 s, a bar every 2 s.
    fn playing_on_grid(first_beat: f64) -> RoomState {
        let mut s = RoomState::default();
        s.apply(&ev(0, 1, 1, grid_load(0, 1, 120.0, first_beat)));
        s.apply(&ev(0, 1, 2, Action::Play { deck: 0 }));
        s
    }

    fn span_is(s: &RoomState, a: f64, b: f64) -> bool {
        s.decks[0]
            .loop_span()
            .is_some_and(|(x, y)| close(x, a) && close(y, b))
    }

    #[test]
    fn quantized_loop_in_snaps_to_the_beat_and_out_to_a_bar() {
        // Beats at 0.25 s, 0.75 s, 1.25 s …
        let mut s = playing_on_grid(0.25);
        // IN at 10.1 s: the nearest beat is 10.25 s, just ahead.
        s.apply(&ev(10_100_000, 1, 10, loop_in(true)));
        assert_eq!(s.decks[0].loop_in, Some(10.25));
        assert!(!s.decks[0].looping, "waits for LOOP OUT");
        // OUT 4.3 beats later: exactly 1 bar. The playhead was past the
        // end, so it has gone round, still on the beat.
        s.apply(&ev(12_400_000, 1, 11, loop_out(true)));
        assert_eq!(s.decks[0].active_loop(), Some((10.25, 12.25)));
        assert_eq!(s.decks[0].loop_beats, Some(4.0));
        assert!(close(at(&s, 0, 12_400_000), 10.4));
        assert!(close(at(&s, 0, 14_300_000), 10.3));
        // While looping, IN moves the start to the nearest beat (2.1 beats
        // into the loop: the beat before), and the end stays.
        s.apply(&ev(13_350_000, 1, 12, loop_in(true)));
        assert_eq!(s.decks[0].active_loop(), Some((11.25, 12.25)));
        // OUT while looping, 1.4 beats in: a 1-beat loop.
        s.apply(&ev(13_950_000, 1, 13, loop_out(true)));
        assert_eq!(s.decks[0].active_loop(), Some((11.25, 11.75)));
        assert_eq!(s.decks[0].loop_beats, Some(1.0));
        assert!(close(at(&s, 0, 13_950_000), 11.45));
        // ×2 carries on in beats.
        s.apply(&ev(14_000_000, 1, 14, Action::LoopDouble { deck: 0 }));
        assert_eq!(s.decks[0].active_loop(), Some((11.25, 12.25)));
        assert_eq!(s.decks[0].loop_beats, Some(2.0));
        // IN while looping can land just ahead of the playhead, which then
        // walks into the loop.
        assert!(close(at(&s, 0, 14_150_000), 11.65));
        s.apply(&ev(14_150_000, 1, 15, loop_in(true)));
        assert_eq!(s.decks[0].active_loop(), Some((11.75, 12.25)));
        assert!(close(at(&s, 0, 14_250_000), 11.75));
        assert!(close(at(&s, 0, 14_850_000), 11.85));
    }

    #[test]
    fn quantized_loop_out_takes_the_nearest_musical_length() {
        assert_eq!(quantized_loop_beats(0.0), 0.25);
        assert_eq!(quantized_loop_beats(5.9), 4.0);
        assert_eq!(quantized_loop_beats(6.0), 8.0);
        assert_eq!(quantized_loop_beats(1e9), MAX_LOOP_BEATS);
        // OUT pressed `gap` seconds after IN (on a beat; a beat is 0.5 s).
        for (gap, beats) in [
            (0.05, 0.25),
            (0.12, 0.25),
            (0.2, 0.5),
            (0.3, 0.5),
            (0.45, 1.0),
            (0.8, 2.0),
            (1.1, 2.0),
            (1.6, 4.0),
            (1.9, 4.0),
            (2.3, 4.0),
            (2.9, 4.0),
            (3.1, 8.0),
            (7.5, 16.0),
        ] {
            let mut s = playing_on_grid(0.25);
            s.apply(&ev(10_250_000, 1, 10, loop_in(true)));
            let t = 10_250_000 + (gap * 1e6) as i64;
            s.apply(&ev(t, 1, 11, loop_out(true)));
            assert!(span_is(&s, 10.25, 10.25 + beats * 0.5), "{gap} s");
            assert_eq!(s.decks[0].loop_beats, Some(beats), "{gap} s");
        }
    }

    #[test]
    fn loops_are_exact_with_quantize_off_or_no_grid() {
        // Quantize off: exactly at the playhead, even on a grid.
        let mut s = playing_on_grid(0.25);
        s.apply(&ev(10_100_000, 1, 10, loop_in(false)));
        s.apply(&ev(12_400_000, 1, 11, loop_out(false)));
        assert!(span_is(&s, 10.1, 12.4));
        assert_eq!(s.decks[0].loop_beats, None);
        // No grid: quantize has nothing to snap to.
        let mut s = RoomState::default();
        s.apply(&ev(
            0,
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
        s.apply(&ev(0, 1, 2, Action::Play { deck: 0 }));
        s.apply(&ev(10_100_000, 1, 3, loop_in(true)));
        s.apply(&ev(12_400_000, 1, 4, loop_out(true)));
        assert!(span_is(&s, 10.1, 12.4));
        assert_eq!(s.decks[0].loop_beats, None);
        // Old peers' events (no flag) are exact.
        let old: Action = serde_json::from_str(r#"{"a":"LoopIn","deck":0}"#).unwrap();
        assert_eq!(old, loop_in(false));
    }

    #[test]
    fn quantized_loops_stay_inside_the_track() {
        // Paused at 0:00, first beat at 0.4 s: the nearest beat (-0.1 s) is
        // before the track starts, so IN takes the next one.
        let mut s = RoomState::default();
        s.apply(&ev(0, 1, 1, grid_load(0, 1, 120.0, 0.4)));
        s.apply(&ev(0, 1, 2, loop_in(true)));
        assert!(close(s.decks[0].loop_in.unwrap(), 0.4));
        // A bar that would run past the end of the track (300 s) stops there.
        s.apply(&ev(
            0,
            1,
            3,
            Action::Seek {
                deck: 0,
                pos: 298.4,
            },
        ));
        s.apply(&ev(0, 1, 4, Action::Play { deck: 0 }));
        s.apply(&ev(0, 1, 5, loop_in(true)));
        s.apply(&ev(1_500_000, 1, 6, loop_out(true)));
        assert!(span_is(&s, 298.4, 300.0));
        assert_eq!(s.decks[0].loop_beats, None);
    }

    #[test]
    fn beat_loops_toggle_resize_halve_and_double() {
        let mut s = RoomState::default();
        let mut seq = 0;
        let mut go = |s: &mut RoomState, t: i64, a: Action| {
            seq += 1;
            s.apply(&ev(t, 1, seq, a));
        };
        // 120 BPM, beats every 0.5 s from 0.
        go(&mut s, 0, grid_load(0, 1, 120.0, 0.0));
        go(&mut s, 0, Action::Play { deck: 0 });
        go(
            &mut s,
            5_300_000,
            Action::BeatLoop {
                deck: 0,
                beats: 4.0,
            },
        );
        let d = s.decks[0];
        assert_eq!(d.active_loop(), Some((5.0, 7.0)), "from the beat before");
        assert_eq!(d.loop_beats, Some(4.0));
        assert!(close(at(&s, 0, 6_900_000), 6.9));
        assert!(close(at(&s, 0, 8_000_000), 6.0));
        // Another size resizes from the in point (and wraps if past it).
        go(
            &mut s,
            8_000_000,
            Action::BeatLoop {
                deck: 0,
                beats: 1.0,
            },
        );
        assert_eq!(s.decks[0].active_loop(), Some((5.0, 5.5)));
        assert!(close(at(&s, 0, 8_000_000), 5.0));
        // Double and halve keep the in point.
        go(&mut s, 8_100_000, Action::LoopDouble { deck: 0 });
        assert_eq!(s.decks[0].active_loop(), Some((5.0, 6.0)));
        assert_eq!(s.decks[0].loop_beats, Some(2.0));
        go(&mut s, 8_700_000, Action::LoopHalve { deck: 0 });
        assert_eq!(s.decks[0].active_loop(), Some((5.0, 5.5)));
        assert_eq!(s.decks[0].loop_beats, Some(1.0));
        assert!(close(at(&s, 0, 8_700_000), 5.2), "{}", at(&s, 0, 8_700_000));
        // The same size again leaves the loop; playback carries on.
        go(
            &mut s,
            9_000_000,
            Action::BeatLoop {
                deck: 0,
                beats: 1.0,
            },
        );
        assert!(!s.decks[0].looping);
        assert!(close(at(&s, 0, 10_000_000), 6.5));
        // A quarter beat: the slice of the beat the playhead is in.
        go(
            &mut s,
            10_000_000,
            Action::BeatLoop {
                deck: 0,
                beats: 0.25,
            },
        );
        assert_eq!(s.decks[0].active_loop(), Some((6.5, 6.625)));
        // Halving stops at the shortest loop.
        for i in 0..8 {
            go(&mut s, 10_100_000 + i, Action::LoopHalve { deck: 0 });
        }
        let (a, b) = s.decks[0].active_loop().unwrap();
        assert!(b - a >= MIN_LOOP && b - a < 2.0 * MIN_LOOP, "{}", b - a);
    }

    #[test]
    fn beat_loops_without_a_grid_and_at_the_ends() {
        let mut s = RoomState::default();
        s.apply(&ev(
            0,
            1,
            1,
            Action::Load {
                deck: 0,
                track: tid(1),
                duration: 10.0,
                cue: 0.0,
                grid: None,
                key: -1,
            },
        ));
        s.apply(&ev(0, 1, 2, Action::Seek { deck: 0, pos: 3.3 }));
        s.apply(&ev(
            0,
            1,
            3,
            Action::BeatLoop {
                deck: 0,
                beats: 2.0,
            },
        ));
        // No grid: from the playhead, at 120 BPM.
        assert_eq!(s.decks[0].active_loop(), Some((3.3, 4.3)));
        // Near the end, the loop stops at the end of the track.
        s.apply(&ev(1, 1, 4, Action::Reloop { deck: 0 }));
        s.apply(&ev(1, 1, 5, Action::Seek { deck: 0, pos: 9.5 }));
        s.apply(&ev(
            1,
            1,
            6,
            Action::BeatLoop {
                deck: 0,
                beats: 4.0,
            },
        ));
        assert_eq!(s.decks[0].active_loop(), Some((9.5, 10.0)));
        assert_eq!(s.decks[0].loop_beats, None);
    }

    #[test]
    fn jumping_out_of_a_loop_leaves_it() {
        let mut s = RoomState::default();
        let mut seq = 0;
        let mut go = |s: &mut RoomState, t: i64, a: Action| {
            seq += 1;
            s.apply(&ev(t, 1, seq, a));
        };
        go(&mut s, 0, grid_load(0, 1, 120.0, 0.0));
        go(&mut s, 0, Action::Seek { deck: 0, pos: 30.0 });
        go(&mut s, 0, Action::HotCue { deck: 0, slot: 0 });
        go(&mut s, 0, Action::Seek { deck: 0, pos: 10.0 });
        go(&mut s, 0, Action::Play { deck: 0 });
        go(
            &mut s,
            0,
            Action::BeatLoop {
                deck: 0,
                beats: 8.0,
            },
        );
        assert_eq!(s.decks[0].active_loop(), Some((10.0, 14.0)));
        // A seek inside the loop keeps looping...
        go(&mut s, 1_000_000, Action::Seek { deck: 0, pos: 13.0 });
        assert!(s.decks[0].looping);
        assert!(close(at(&s, 0, 2_500_000), 10.5));
        // ...a hot cue outside it leaves the loop (RELOOP can go back).
        go(&mut s, 3_000_000, Action::HotCue { deck: 0, slot: 0 });
        assert!(!s.decks[0].looping);
        assert!(close(at(&s, 0, 8_000_000), 35.0));
        go(&mut s, 8_000_000, Action::Reloop { deck: 0 });
        assert!(close(at(&s, 0, 9_000_000), 11.0));
        // Scratching forwards past the out point goes round the loop.
        go(
            &mut s,
            9_000_000,
            Action::JogTouch {
                deck: 0,
                down: true,
                spin: 0.0,
            },
        );
        go(
            &mut s,
            9_100_000,
            Action::Nudge {
                deck: 0,
                delta: 3.5,
            },
        );
        assert!(s.decks[0].looping);
        assert!(close(at(&s, 0, 9_100_000), 10.5));
    }

    #[test]
    fn a_looping_deck_stays_in_sync() {
        let mut tl = Timeline::new(0);
        let mut seq = 0;
        let mut go = |tl: &mut Timeline, t: i64, a: Action| {
            seq += 1;
            tl.insert(ev(t, 1, seq, a));
        };
        go(&mut tl, 0, grid_load(0, 1, 128.0, 0.1));
        go(&mut tl, 0, grid_load(1, 2, 124.0, 0.37));
        go(&mut tl, 0, Action::Play { deck: 0 });
        go(&mut tl, 0, Action::Play { deck: 1 });
        go(&mut tl, 1_000_000, Action::Sync { deck: 1, on: true });
        // A 1-beat loop on the synced deck, then a ¼ beat, then out again.
        go(
            &mut tl,
            2_345_000,
            Action::BeatLoop {
                deck: 1,
                beats: 1.0,
            },
        );
        for t in [2_400_000, 5_000_000, 9_876_000] {
            assert!(beat_offset(&tl.state_at(t), t, 1.0).abs() < 1e-6, "at {t}");
        }
        // A ¼-beat loop stutters the first quarter of that beat...
        go(
            &mut tl,
            10_000_000,
            Action::BeatLoop {
                deck: 1,
                beats: 0.25,
            },
        );
        let t = 12_345_678;
        let b = tl.state_at(t).decks[1];
        assert!(b.beat_phase(1.0, t) < 0.25 + 1e-9);
        // ...and leaving it puts the synced deck back on the beat.
        go(&mut tl, 13_000_000, Action::Reloop { deck: 1 });
        let t = 20_000_000;
        assert!(beat_offset(&tl.state_at(t), t, 1.0).abs() < 1e-6);
    }

    #[test]
    fn loops_and_tempo_changes() {
        // 120 BPM up to 20 s, then 125.
        let mut s = RoomState::default();
        s.apply(&ev(0, 1, 1, grid_load(0, 2, 120.0, 0.0)));
        s.apply(&ev(0, 1, 2, set_grid(0, 2, two_tempo_grid())));
        s.apply(&ev(0, 1, 3, Action::Seek { deck: 0, pos: 16.0 }));
        s.apply(&ev(0, 1, 4, Action::Play { deck: 0 }));
        s.apply(&ev(
            0,
            1,
            5,
            Action::BeatLoop {
                deck: 0,
                beats: 4.0,
            },
        ));
        // A loop before the marker never reaches it.
        assert_eq!(s.decks[0].next_tempo_change(0, 60_000_000), None);
        // A loop across the marker reaches it, and changes back at the wrap.
        s.apply(&ev(0, 1, 6, Action::LoopDouble { deck: 0 }));
        assert_eq!(s.decks[0].active_loop(), Some((16.0, 20.0)));
        s.apply(&ev(0, 1, 7, Action::Reloop { deck: 0 }));
        s.apply(&ev(0, 1, 8, Action::Seek { deck: 0, pos: 18.0 }));
        s.apply(&ev(0, 1, 9, loop_in(false)));
        s.apply(&ev(4_000_000, 1, 10, loop_out(false)));
        assert_eq!(s.decks[0].active_loop(), Some((18.0, 22.0)));
        let t = 4_000_000;
        assert_eq!(
            s.decks[0].next_tempo_change(t, 60_000_000),
            Some(t + 2_000_000)
        );
        let t = 7_000_000; // at 21 s, in the 125 BPM section
        assert_eq!(
            s.decks[0].next_tempo_change(t, 60_000_000),
            Some(t + 1_000_000)
        );
    }

    #[test]
    fn loops_converge_regardless_of_arrival_order() {
        let mut events = vec![
            ev(0, 1, 1, grid_load(0, 1, 126.0, 0.2)),
            ev(0, 2, 1, grid_load(1, 2, 124.0, 0.1)),
            ev(100_000, 1, 2, Action::Play { deck: 0 }),
            ev(150_000, 2, 2, Action::Play { deck: 1 }),
            ev(
                900_000,
                1,
                3,
                Action::BeatLoop {
                    deck: 0,
                    beats: 2.0,
                },
            ),
            ev(
                1_500_000,
                2,
                3,
                Action::LoopIn {
                    deck: 1,
                    quantize: true,
                },
            ),
            ev(
                2_700_000,
                2,
                4,
                Action::LoopOut {
                    deck: 1,
                    quantize: true,
                },
            ),
            ev(3_100_000, 1, 4, Action::LoopHalve { deck: 0 }),
            ev(4_000_000, 2, 5, Action::Sync { deck: 1, on: true }),
            ev(5_200_000, 1, 5, Action::Reloop { deck: 0 }),
            ev(6_000_000, 2, 6, Action::LoopDouble { deck: 1 }),
        ];
        let mut a = Timeline::new(0);
        for e in &events {
            a.insert(e.clone());
        }
        fastrand::Rng::with_seed(9).shuffle(&mut events);
        let mut b = Timeline::new(0);
        for e in &events {
            b.insert(e.clone());
        }
        for t in [0, 1_000_000, 3_000_000, 5_500_000, 9_000_000] {
            assert_eq!(
                a.state_at(t).digest(),
                b.state_at(t).digest(),
                "diverged at {t}"
            );
        }
        let json = serde_json::to_string(&events[0]).unwrap();
        assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), events[0]);
    }

    #[test]
    fn four_deck_sync_converges_regardless_of_arrival_order() {
        // Two DJs on four decks: syncs, a master that stops and a new track
        // on the master, arriving in any order.
        let mut events = vec![
            ev(0, 1, 1, grid_load(0, 1, 126.0, 0.2)),
            ev(0, 2, 1, grid_load(1, 2, 124.0, 0.1)),
            ev(0, 1, 2, grid_load(2, 3, 128.0, 0.3)),
            ev(0, 2, 2, grid_load(3, 4, 87.0, 0.0)),
            ev(100_000, 1, 3, Action::Play { deck: 0 }),
            ev(150_000, 2, 3, Action::Play { deck: 3 }),
            ev(400_000, 2, 4, Action::Sync { deck: 1, on: true }),
            ev(500_000, 1, 4, Action::Sync { deck: 2, on: true }),
            ev(700_000, 2, 5, Action::Play { deck: 1 }),
            ev(900_000, 1, 5, Action::Play { deck: 2 }),
            ev(
                1_100_000,
                2,
                6,
                Action::Tempo {
                    deck: 3,
                    tempo: 1.02,
                },
            ),
            ev(
                1_300_000,
                1,
                6,
                Action::Tempo {
                    deck: 1,
                    tempo: 0.98,
                },
            ),
            ev(2_000_000, 1, 7, Action::Pause { deck: 0 }),
            ev(2_400_000, 2, 7, Action::Sync { deck: 3, on: true }),
            ev(2_500_000, 1, 8, grid_load(0, 5, 140.0, 0.0)),
            ev(
                3_000_000,
                2,
                8,
                Action::XfAssign {
                    deck: 3,
                    side: XfSide::Thru,
                },
            ),
            ev(3_500_000, 1, 9, Action::Sync { deck: 2, on: false }),
        ];
        let mut a = Timeline::new(0);
        for e in &events {
            a.insert(e.clone());
        }
        let end = a.latest();
        assert!(end.master.is_some() && end.decks[3].synced);
        // Sent whole (a resync), the master, sync and assign switches come
        // out the same, whatever the floats' last bits do on the way.
        let wire: Timeline = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        for t in [600_000, 2_200_000, 2_700_000, 9_000_000] {
            let (x, y) = (a.state_at(t), wire.state_at(t));
            assert_eq!((x.master, x.xf_assign), (y.master, y.xf_assign), "at {t}");
            for d in 0..DECKS {
                let flags = |s: &RoomState| (s.decks[d].synced, s.decks[d].playing);
                assert_eq!(flags(&x), flags(&y), "deck {d} at {t}");
            }
        }
        for seed in 0..8 {
            fastrand::Rng::with_seed(seed).shuffle(&mut events);
            let mut b = Timeline::new(0);
            for e in &events {
                b.insert(e.clone());
            }
            for t in [0, 600_000, 1_200_000, 2_200_000, 2_700_000, 9_000_000] {
                let (x, y) = (a.state_at(t), b.state_at(t));
                assert_eq!(x.master, y.master, "master at {t}");
                assert_eq!(x.digest(), y.digest(), "diverged at {t}");
            }
        }
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
        // Playing: releasing the pad changes nothing.
        s.apply(&ev(21_000_000, 1, 5, Action::HotCueUp { deck: 1, slot: 2 }));
        assert!(s.decks[1].playing && !s.decks[1].cue_preview);
        assert!((s.decks[1].position_at(22_000_000) - 12.0).abs() < 1e-9);
    }

    #[test]
    fn hot_cue_from_pause_is_gated() {
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
                track: tid(3),
                duration: 100.0,
                cue: 0.0,
                grid: None,
                key: -1,
            },
        );
        go(&mut s, 0, Action::Seek { deck: 0, pos: 30.0 });
        go(&mut s, 0, Action::HotCue { deck: 0, slot: 1 });
        go(&mut s, 0, Action::Seek { deck: 0, pos: 50.0 });
        // Paused: hold plays from the hot cue...
        go(&mut s, 1_000_000, Action::HotCue { deck: 0, slot: 1 });
        let d = s.decks[0];
        assert!(d.playing && d.cue_preview && d.preview_slot == Some(1));
        assert!((d.position_at(3_000_000) - 32.0).abs() < 1e-9);
        // ...releasing another control doesn't end it...
        go(&mut s, 2_000_000, Action::CueUp { deck: 0 });
        go(&mut s, 2_000_000, Action::HotCueUp { deck: 0, slot: 0 });
        assert!(s.decks[0].playing);
        // ...and release snaps back to the hot cue, paused.
        go(&mut s, 3_000_000, Action::HotCueUp { deck: 0, slot: 1 });
        let d = s.decks[0];
        assert!(!d.playing && !d.cue_preview && d.preview_slot.is_none());
        assert_eq!(d.position_at(9_000_000), 30.0);
        assert_eq!(d.cue_point, 0.0);
        // PLAY while holding: keeps playing after the release.
        go(&mut s, 4_000_000, Action::HotCue { deck: 0, slot: 1 });
        go(&mut s, 5_000_000, Action::Play { deck: 0 });
        go(&mut s, 6_000_000, Action::HotCueUp { deck: 0, slot: 1 });
        let d = s.decks[0];
        assert!(d.playing && !d.cue_preview);
        assert!((d.position_at(7_000_000) - 33.0).abs() < 1e-9);
    }

    fn roll(deck: u8, beats: f64, on: bool) -> Action {
        Action::Roll { deck, beats, on }
    }

    fn slip(s: &RoomState, deck: usize, t: i64) -> f64 {
        s.decks[deck].slip_position_at(t)
    }

    #[test]
    fn a_roll_loops_while_held_and_lets_go_where_the_track_has_got_to() {
        let mut s = RoomState::default();
        let mut seq = 0;
        let mut go = |s: &mut RoomState, t: i64, a: Action| {
            seq += 1;
            s.apply(&ev(t, 1, seq, a));
        };
        // 120 BPM, beats every 0.5 s from 0.
        go(&mut s, 0, grid_load(0, 1, 120.0, 0.0));
        go(&mut s, 0, Action::Play { deck: 0 });
        // A ½-beat roll: the half of the beat the playhead is in.
        go(&mut s, 5_300_000, roll(0, 0.5, true));
        let d = s.decks[0];
        assert_eq!(d.active_loop(), Some((5.25, 5.5)));
        assert_eq!(d.loop_span(), None, "not an ordinary loop");
        assert!(close(at(&s, 0, 5_450_000), 5.45));
        assert!(close(at(&s, 0, 5_600_000), 5.35));
        assert!(close(at(&s, 0, 6_900_000), 5.4));
        // Underneath, the track runs on.
        assert!(close(slip(&s, 0, 6_900_000), 6.9));
        // Letting go: on from where the track has got to.
        let gen = s.decks[0].gen;
        go(&mut s, 7_100_000, roll(0, 0.5, false));
        let d = s.decks[0];
        assert!(d.roll.is_none() && d.playing && d.gen > gen);
        assert!(close(at(&s, 0, 7_100_000), 7.1));
        assert!(close(at(&s, 0, 9_000_000), 9.0));
        // Let go before it has gone round: seamless, not even a jump.
        go(&mut s, 10_100_000, roll(0, 4.0, true));
        assert_eq!(s.decks[0].active_loop(), Some((10.0, 12.0)));
        let gen = s.decks[0].gen;
        go(&mut s, 11_500_000, roll(0, 4.0, false));
        assert_eq!(s.decks[0].gen, gen);
        assert!(close(at(&s, 0, 12_500_000), 12.5));
        // A tempo change carries the track underneath too.
        go(&mut s, 20_100_000, roll(0, 1.0, true));
        go(
            &mut s,
            20_200_000,
            Action::Tempo {
                deck: 0,
                tempo: 1.1,
            },
        );
        assert_eq!(s.decks[0].active_loop(), Some((20.0, 20.5)));
        assert!(close(at(&s, 0, 21_200_000), 20.3));
        go(&mut s, 21_200_000, roll(0, 1.0, false));
        assert!(close(at(&s, 0, 21_200_000), 21.3));
        assert!(close(at(&s, 0, 22_200_000), 22.4));
    }

    #[test]
    fn switching_roll_sizes_keeps_the_track_running_underneath() {
        let mut s = RoomState::default();
        let mut seq = 0;
        let mut go = |s: &mut RoomState, t: i64, a: Action| {
            seq += 1;
            s.apply(&ev(t, 1, seq, a));
        };
        go(&mut s, 0, grid_load(0, 1, 120.0, 0.0));
        go(&mut s, 0, Action::Play { deck: 0 });
        go(&mut s, 5_300_000, roll(0, 1.0, true));
        assert_eq!(s.decks[0].active_loop(), Some((5.0, 5.5)));
        assert!(close(at(&s, 0, 5_900_000), 5.4));
        // A shorter size: from the same in point, at the point in it the
        // track underneath is at (0.9 s after the in point, 0.15 into ½).
        go(&mut s, 5_900_000, roll(0, 0.5, true));
        assert_eq!(s.decks[0].active_loop(), Some((5.0, 5.25)));
        assert!(close(at(&s, 0, 5_900_000), 5.15));
        assert!(close(at(&s, 0, 6_050_000), 5.05));
        for t in [6_000_123, 6_040_000, 6_087_000] {
            let lag = (slip(&s, 0, t) - at(&s, 0, t)).rem_euclid(0.25);
            assert!(lag < 1e-6 || 0.25 - lag < 1e-6, "out of step at {t}: {lag}");
        }
        go(&mut s, 6_100_000, roll(0, 0.25, true));
        assert_eq!(s.decks[0].active_loop(), Some((5.0, 5.125)));
        assert!(close(at(&s, 0, 6_100_000), 5.1));
        // Pressing it again changes nothing, nor does letting go of a size
        // that isn't rolling.
        let held = s.decks[0].roll;
        go(&mut s, 6_200_000, roll(0, 0.25, true));
        go(&mut s, 6_200_000, roll(0, 1.0, false));
        assert_eq!(s.decks[0].roll, held);
        // Back up to 2 beats: from the beat the roll starts on, in step.
        go(&mut s, 6_300_000, roll(0, 2.0, true));
        assert_eq!(s.decks[0].active_loop(), Some((5.0, 6.0)));
        assert!(close(at(&s, 0, 6_300_000), 5.3));
        // Let go: the track, as if it had never rolled.
        go(&mut s, 7_000_000, roll(0, 2.0, false));
        assert!(s.decks[0].roll.is_none());
        assert!(close(at(&s, 0, 7_000_000), 7.0));
        assert!(close(at(&s, 0, 8_000_000), 8.0));
    }

    #[test]
    fn a_roll_over_a_loop_comes_back_to_the_loop() {
        let mut s = RoomState::default();
        let mut seq = 0;
        let mut go = |s: &mut RoomState, t: i64, a: Action| {
            seq += 1;
            s.apply(&ev(t, 1, seq, a));
        };
        go(&mut s, 0, grid_load(0, 1, 120.0, 0.0));
        go(&mut s, 0, Action::Play { deck: 0 });
        go(
            &mut s,
            5_300_000,
            Action::BeatLoop {
                deck: 0,
                beats: 4.0,
            },
        );
        assert!(close(at(&s, 0, 8_000_000), 6.0));
        // A 1-beat roll inside the 4-beat loop.
        go(&mut s, 8_000_000, roll(0, 1.0, true));
        let d = s.decks[0];
        assert_eq!(d.active_loop(), Some((6.0, 6.5)));
        assert_eq!(d.loop_span(), Some((5.0, 7.0)));
        assert!(d.looping && d.loop_beats == Some(4.0));
        assert!(close(at(&s, 0, 9_200_000), 6.2));
        // Underneath, the loop goes round as usual (7.2 is 5.2).
        assert!(close(slip(&s, 0, 9_200_000), 5.2));
        go(&mut s, 9_200_000, roll(0, 1.0, false));
        let d = s.decks[0];
        assert_eq!(d.active_loop(), Some((5.0, 7.0)));
        assert!(close(at(&s, 0, 9_200_000), 5.2));
        assert!(close(at(&s, 0, 10_000_000), 6.0));
        // A roll longer than the loop plays on past the loop's end, while
        // the track underneath goes round the loop.
        go(&mut s, 12_000_000, roll(0, 8.0, true));
        assert_eq!(s.decks[0].active_loop(), Some((6.0, 10.0)));
        assert!(close(at(&s, 0, 13_500_000), 7.5));
        go(&mut s, 13_500_000, roll(0, 8.0, false));
        assert!(s.decks[0].looping);
        assert!(close(at(&s, 0, 13_500_000), 5.5));
        // EXIT under a roll: back in the loop, then out of it.
        go(&mut s, 14_000_000, roll(0, 0.25, true));
        go(&mut s, 14_600_000, Action::Reloop { deck: 0 });
        let d = s.decks[0];
        assert!(d.roll.is_none() && !d.looping);
        assert!(close(at(&s, 0, 14_600_000), 6.6));
        assert!(close(at(&s, 0, 15_600_000), 7.6));
    }

    #[test]
    fn rolls_need_a_playing_deck_and_jumps_let_go_of_them() {
        let mut s = RoomState::default();
        let mut seq = 0;
        let mut go = |s: &mut RoomState, t: i64, a: Action| {
            seq += 1;
            s.apply(&ev(t, 1, seq, a));
        };
        go(&mut s, 0, grid_load(0, 1, 120.0, 0.0));
        // Paused: nothing to roll.
        go(&mut s, 0, roll(0, 1.0, true));
        assert!(s.decks[0].roll.is_none());
        go(&mut s, 0, Action::Play { deck: 0 });
        // PAUSE lets go: the deck stops where the track has got to.
        go(&mut s, 2_100_000, roll(0, 0.5, true));
        go(&mut s, 2_600_000, Action::Pause { deck: 0 });
        let d = s.decks[0];
        assert!(!d.playing && d.roll.is_none());
        assert!(close(at(&s, 0, 9_000_000), 2.6));
        go(&mut s, 3_000_000, Action::Play { deck: 0 });
        // Scratching under a roll moves what's heard; the track runs on.
        go(&mut s, 4_000_000, roll(0, 1.0, true));
        assert_eq!(s.decks[0].active_loop(), Some((3.5, 4.0)));
        go(
            &mut s,
            4_100_000,
            Action::JogTouch {
                deck: 0,
                down: true,
                spin: 0.0,
            },
        );
        go(
            &mut s,
            4_200_000,
            Action::Nudge {
                deck: 0,
                delta: -0.3,
            },
        );
        assert!(s.decks[0].roll.is_some());
        assert!(close(at(&s, 0, 4_500_000), 3.4));
        assert!(close(slip(&s, 0, 4_500_000), 4.1));
        go(
            &mut s,
            4_600_000,
            Action::JogTouch {
                deck: 0,
                down: false,
                spin: 0.0,
            },
        );
        assert!(close(at(&s, 0, 4_700_000), 3.5));
        assert!(close(slip(&s, 0, 4_700_000), 4.3));
        go(&mut s, 4_800_000, roll(0, 1.0, false));
        assert!(close(at(&s, 0, 4_800_000), 4.4));
        // Setting a hot cue under a roll keeps rolling; jumping to one lets go.
        go(&mut s, 6_000_000, roll(0, 1.0, true));
        go(&mut s, 6_100_000, Action::HotCue { deck: 0, slot: 0 });
        assert!(s.decks[0].roll.is_some());
        assert!(close(s.decks[0].hot_cues[0].unwrap(), 5.7));
        go(&mut s, 6_300_000, Action::HotCue { deck: 0, slot: 0 });
        assert!(s.decks[0].roll.is_none());
        assert!(close(at(&s, 0, 6_300_000), 5.7));
        // A loop control lets go first: LOOP IN marks where the track is.
        go(&mut s, 7_000_000, roll(0, 0.25, true));
        go(&mut s, 7_500_000, loop_in(false));
        let d = s.decks[0];
        assert!(d.roll.is_none());
        assert!(close(d.loop_in.unwrap(), 6.9));
        assert!(close(at(&s, 0, 7_500_000), 6.9));
        // Loading another track (fader down, so it can be loaded while it
        // rolls) forgets the roll.
        go(&mut s, 8_000_000, roll(0, 0.25, true));
        go(&mut s, 8_050_000, Action::Volume { deck: 0, v: 0.0 });
        go(&mut s, 8_100_000, grid_load(0, 2, 120.0, 0.0));
        assert_eq!(s.decks[0].track, Some(tid(2)));
        assert!(s.decks[0].roll.is_none());
    }

    #[test]
    fn rolls_and_quantized_loops() {
        // Beats at 0.25 s, 0.75 s … (120 BPM), playing from 0:00.
        let mut s = playing_on_grid(0.25);
        let mut seq = 10;
        let mut go = |s: &mut RoomState, t: i64, a: Action| {
            seq += 1;
            s.apply(&ev(t, 1, seq, a));
        };
        // A quantized LOOP IN under a roll lets go first, and snaps where
        // the track has got to (10.9 s: the beat at 10.75), not where the
        // roll is (10.275 s, nearest 10.25).
        go(&mut s, 10_300_000, roll(0, 0.25, true));
        assert_eq!(s.decks[0].active_loop(), Some((10.25, 10.375)));
        assert!(close(at(&s, 0, 10_900_000), 10.275));
        go(&mut s, 10_900_000, loop_in(true));
        assert!(s.decks[0].roll.is_none());
        assert_eq!(s.decks[0].loop_in, Some(10.75));
        // OUT 4.4 beats later: a quantized 1-bar loop.
        go(&mut s, 12_950_000, loop_out(true));
        assert_eq!(s.decks[0].active_loop(), Some((10.75, 12.75)));
        assert_eq!(s.decks[0].loop_beats, Some(4.0));
        // A roll over it comes back into it, on the beat.
        go(&mut s, 13_000_000, roll(0, 1.0, true));
        assert_eq!(s.decks[0].active_loop(), Some((10.75, 11.25)));
        assert!(close(at(&s, 0, 14_600_000), 11.1));
        assert!(close(slip(&s, 0, 14_600_000), 12.6));
        go(&mut s, 14_600_000, roll(0, 1.0, false));
        let d = s.decks[0];
        assert_eq!(d.active_loop(), Some((10.75, 12.75)));
        assert_eq!(d.loop_beats, Some(4.0));
        assert!(close(at(&s, 0, 15_000_000), 11.0));
        // A quantized OUT under a roll lets go first too: the track is 2.5
        // beats into the loop, so the loop becomes 2 beats, and the track
        // (past its new end) has gone round it.
        go(&mut s, 15_500_000, roll(0, 0.5, true));
        assert_eq!(s.decks[0].active_loop(), Some((11.5, 11.75)));
        go(&mut s, 16_000_000, loop_out(true));
        let d = s.decks[0];
        assert!(d.roll.is_none());
        assert_eq!(d.active_loop(), Some((10.75, 11.75)));
        assert_eq!(d.loop_beats, Some(2.0));
        assert!(close(at(&s, 0, 16_000_000), 11.0));
    }

    /// Phase difference (beats, -0.5..0.5) between what decks A and B
    /// sound like at time t (under a roll, not the track underneath).
    fn heard_offset(s: &RoomState, t: i64) -> f64 {
        let phase = |d: &DeckState| d.grid.unwrap().phase(d.position_at(t), 1.0);
        let d = phase(&s.decks[0]) - phase(&s.decks[1]);
        d - d.round()
    }

    #[test]
    fn a_rolling_synced_deck_comes_back_on_the_beat() {
        let base = [
            grid_load(0, 1, 128.0, 0.1),
            grid_load(1, 2, 124.0, 0.37),
            Action::Play { deck: 0 },
            Action::Play { deck: 1 },
        ];
        let mut tl = Timeline::new(0);
        let mut plain = Timeline::new(0);
        let mut seq = 0;
        for a in base {
            seq += 1;
            tl.insert(ev(0, 1, seq, a.clone()));
            plain.insert(ev(0, 1, seq, a));
        }
        seq += 1;
        tl.insert(ev(1_000_000, 1, seq, Action::Sync { deck: 1, on: true }));
        plain.insert(ev(1_000_000, 1, seq, Action::Sync { deck: 1, on: true }));
        let mut go = |tl: &mut Timeline, t: i64, a: Action| {
            seq += 1;
            tl.insert(ev(t, 1, seq, a));
        };
        // A whole-beat roll sounds on the beat while it plays.
        go(&mut tl, 2_345_000, roll(1, 1.0, true));
        for t in [2_400_000, 3_000_000, 3_777_000] {
            assert!(heard_offset(&tl.state_at(t), t).abs() < 1e-6, "at {t}");
        }
        // Down to 1/16 (a stutter), then let go: back on the beat, exactly
        // where the deck would be if it had never rolled.
        go(&mut tl, 4_000_000, roll(1, 0.0625, true));
        let t = 4_321_000;
        let s = tl.state_at(t);
        let (a, b) = s.decks[1].roll_span().unwrap();
        assert!(a <= at(&s, 1, t) && at(&s, 1, t) < b && b - a < 0.031);
        go(&mut tl, 5_000_000, roll(1, 0.0625, false));
        for t in [5_000_000, 9_000_000] {
            let s = tl.state_at(t);
            assert!(heard_offset(&s, t).abs() < 1e-6, "at {t}");
            assert!((at(&s, 1, t) - at(&plain.state_at(t), 1, t)).abs() < 1e-9);
        }
        // SYNC pressed under a roll lines up the track underneath.
        go(&mut tl, 10_000_000, Action::Sync { deck: 1, on: false });
        go(
            &mut tl,
            10_000_000,
            Action::Tempo {
                deck: 1,
                tempo: 1.05,
            },
        );
        go(&mut tl, 11_000_000, roll(1, 0.25, true));
        go(&mut tl, 11_500_000, Action::Sync { deck: 1, on: true });
        assert!(tl.state_at(11_600_000).decks[1].roll.is_some());
        go(&mut tl, 12_400_000, roll(1, 0.25, false));
        let t = 14_000_000;
        assert!(heard_offset(&tl.state_at(t), t).abs() < 1e-6);
    }

    #[test]
    fn rolls_converge_regardless_of_arrival_order() {
        let mut events = vec![
            ev(0, 1, 1, grid_load(0, 1, 126.0, 0.2)),
            ev(0, 2, 1, grid_load(1, 2, 124.0, 0.1)),
            ev(100_000, 1, 2, Action::Play { deck: 0 }),
            ev(150_000, 2, 2, Action::Play { deck: 1 }),
            ev(400_000, 2, 3, Action::Sync { deck: 1, on: true }),
            ev(
                900_000,
                1,
                3,
                Action::BeatLoop {
                    deck: 0,
                    beats: 4.0,
                },
            ),
            ev(1_200_000, 1, 4, roll(0, 0.5, true)),
            ev(1_300_000, 2, 4, roll(1, 1.0, true)),
            ev(1_700_000, 1, 5, roll(0, 0.125, true)),
            ev(
                1_900_000,
                2,
                5,
                Action::Tempo {
                    deck: 1,
                    tempo: 1.02,
                },
            ),
            ev(2_100_000, 1, 6, roll(0, 0.5, false)),
            ev(2_300_000, 1, 7, roll(0, 0.125, false)),
            ev(2_600_000, 2, 6, roll(1, 1.0, false)),
            ev(3_000_000, 1, 8, roll(0, 2.0, true)),
            ev(3_400_000, 1, 9, Action::Pause { deck: 0 }),
        ];
        let mut a = Timeline::new(0);
        for e in &events {
            a.insert(e.clone());
        }
        let rolls = |s: &RoomState| s.decks.map(|d| d.roll.map(|r| r.beats));
        assert_eq!(rolls(&a.state_at(1_500_000))[..2], [Some(0.5), Some(1.0)]);
        assert_eq!(rolls(&a.state_at(2_200_000))[..2], [Some(0.125), Some(1.0)]);
        assert_eq!(rolls(&a.state_at(2_400_000))[..2], [None, Some(1.0)]);
        assert_eq!(rolls(&a.state_at(9_000_000))[..2], [None, None]);
        // Sent whole (a resync), the rolls come out the same.
        let wire: Timeline = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        for t in [1_500_000, 2_200_000, 3_200_000] {
            assert_eq!(rolls(&a.state_at(t)), rolls(&wire.state_at(t)), "at {t}");
        }
        for seed in 0..8 {
            fastrand::Rng::with_seed(seed).shuffle(&mut events);
            let mut b = Timeline::new(0);
            for e in &events {
                b.insert(e.clone());
            }
            for t in [
                0, 1_250_000, 1_800_000, 2_200_000, 2_700_000, 3_200_000, 9_000_000,
            ] {
                assert_eq!(a.state_at(t).digest(), b.state_at(t).digest(), "at {t}");
            }
        }
        let e = ev(1, 2, 3, roll(1, 0.0625, true));
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""a":"Roll""#), "{json}");
        assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), e);
    }

    fn load(deck: u8, track: u8) -> Action {
        Action::Load {
            deck,
            track: tid(track),
            duration: 200.0,
            cue: 0.0,
            grid: None,
            key: -1,
        }
    }

    #[test]
    fn a_new_deck_has_echo_reverb_and_phaser_off() {
        let fx = DeckState::default().fx;
        let kinds = fx.slots.map(|s| s.kind);
        assert_eq!(kinds, [FxKind::Echo, FxKind::Reverb, FxKind::Phaser]);
        assert!(fx
            .slots
            .iter()
            .all(|s| !s.on && s.depth == 0.5 && s.since == 0));
        assert_eq!(fx.beats(), 1.0);
        assert!(!fx.any_on());
        assert_eq!(FX_BEATS[FX_BEATS_DEFAULT as usize], 1.0);
        assert_eq!(FxKind::Echo.next(), FxKind::Reverb);
        assert_eq!(FxKind::Phaser.next(), FxKind::Echo);
    }

    #[test]
    fn effects_apply_and_clamp() {
        let mut tl = Timeline::new(0);
        let fx = |a: Action| a;
        tl.insert(ev(
            1_000,
            7,
            1,
            fx(Action::FxOn {
                deck: 1,
                slot: 2,
                on: true,
            }),
        ));
        tl.insert(ev(
            2_000,
            7,
            2,
            Action::FxDepth {
                deck: 1,
                slot: 2,
                v: 1.7,
            },
        ));
        tl.insert(ev(
            3_000,
            7,
            3,
            Action::FxDepth {
                deck: 1,
                slot: 0,
                v: -0.3,
            },
        ));
        tl.insert(ev(
            4_000,
            7,
            4,
            Action::FxBeats {
                deck: 1,
                beats: 200,
            },
        ));
        tl.insert(ev(
            5_000,
            7,
            5,
            Action::FxSelect {
                deck: 1,
                slot: 1,
                kind: FxKind::Echo,
            },
        ));
        // Out of range: nothing happens (and nothing panics).
        tl.insert(ev(
            6_000,
            7,
            6,
            Action::FxOn {
                deck: 1,
                slot: 3,
                on: true,
            },
        ));
        tl.insert(ev(
            6_500,
            7,
            7,
            Action::FxOn {
                deck: 9,
                slot: 0,
                on: true,
            },
        ));
        tl.insert(ev(
            6_600,
            7,
            8,
            Action::FxDepth {
                deck: 1,
                slot: 1,
                v: f32::NAN,
            },
        ));
        let s = tl.latest();
        let b = s.decks[1].fx;
        assert!(b.slots[2].on);
        assert_eq!(b.slots[2].since, 1_000);
        assert_eq!(b.slots[2].depth, 1.0);
        assert_eq!(b.slots[0].depth, 0.0);
        assert_eq!(b.slots[1].depth, 0.0);
        assert_eq!(b.beats as usize, FX_BEATS.len() - 1);
        assert_eq!(b.beats(), 32.0);
        assert_eq!(b.slots[1].kind, FxKind::Echo);
        assert_eq!(b.slots[1].since, 5_000);
        assert!(!b.slots[0].on && !b.slots[1].on);
        assert_eq!(s.decks[1].last_touch, 7);
        // The other decks' banks are untouched.
        assert_eq!(s.decks[0].fx, FxBank::default());
        // Switching on again, or picking the same effect, keeps `since`.
        tl.insert(ev(
            7_000,
            7,
            9,
            Action::FxOn {
                deck: 1,
                slot: 2,
                on: true,
            },
        ));
        tl.insert(ev(
            7_500,
            7,
            10,
            Action::FxSelect {
                deck: 1,
                slot: 1,
                kind: FxKind::Echo,
            },
        ));
        let b = tl.latest().decks[1].fx;
        assert_eq!((b.slots[2].since, b.slots[1].since), (1_000, 5_000));
        // Off, then on: a new starting point.
        tl.insert(ev(
            8_000,
            7,
            11,
            Action::FxOn {
                deck: 1,
                slot: 2,
                on: false,
            },
        ));
        tl.insert(ev(
            9_000,
            7,
            12,
            Action::FxOn {
                deck: 1,
                slot: 2,
                on: true,
            },
        ));
        assert_eq!(tl.latest().decks[1].fx.slots[2].since, 9_000);
    }

    #[test]
    fn effects_dont_touch_the_track() {
        let mut tl = Timeline::new(0);
        tl.insert(ev(1_000, 1, 1, load(0, 1)));
        tl.insert(ev(2_000, 1, 2, Action::Play { deck: 0 }));
        tl.insert(ev(
            3_000_000,
            1,
            3,
            Action::Roll {
                deck: 0,
                beats: 1.0,
                on: true,
            },
        ));
        let before = tl.latest().decks[0];
        let moves = [
            Action::FxOn {
                deck: 0,
                slot: 0,
                on: true,
            },
            Action::FxDepth {
                deck: 0,
                slot: 0,
                v: 0.8,
            },
            Action::FxBeats { deck: 0, beats: 2 },
            Action::FxSelect {
                deck: 0,
                slot: 0,
                kind: FxKind::Phaser,
            },
        ];
        for (i, a) in moves.into_iter().enumerate() {
            tl.insert(ev(3_100_000 + i as i64 * 1_000, 2, 1 + i as u64, a));
        }
        let after = tl.latest().decks[0];
        assert_eq!(
            (after.anchor_time, after.anchor_pos, after.gen),
            (before.anchor_time, before.anchor_pos, before.gen)
        );
        assert_eq!(after.roll, before.roll);
        assert!(after.playing);
        assert_eq!(after.last_touch, 2);
    }

    #[test]
    fn loading_and_ejecting_keep_the_bank() {
        let mut tl = Timeline::new(0);
        tl.insert(ev(
            1_000,
            1,
            1,
            Action::FxOn {
                deck: 2,
                slot: 0,
                on: true,
            },
        ));
        tl.insert(ev(2_000, 1, 2, Action::FxBeats { deck: 2, beats: 7 }));
        tl.insert(ev(
            3_000,
            1,
            3,
            Action::FxDepth {
                deck: 2,
                slot: 1,
                v: 0.2,
            },
        ));
        let bank = tl.latest().decks[2].fx;
        tl.insert(ev(4_000, 1, 4, load(2, 5)));
        assert_eq!(tl.latest().decks[2].track, Some(tid(5)));
        assert_eq!(tl.latest().decks[2].fx, bank);
        tl.insert(ev(5_000, 1, 5, Action::Eject { deck: 2 }));
        assert_eq!(tl.latest().decks[2].track, None);
        assert_eq!(tl.latest().decks[2].fx, bank);
    }

    #[test]
    fn effect_moves_converge_whatever_order_they_arrive_in() {
        let mut events = vec![
            ev(1_000, 1, 1, load(0, 1)),
            ev(
                2_000,
                1,
                2,
                Action::FxOn {
                    deck: 0,
                    slot: 0,
                    on: true,
                },
            ),
            ev(
                2_500,
                2,
                1,
                Action::FxOn {
                    deck: 0,
                    slot: 0,
                    on: false,
                },
            ),
            ev(
                3_000,
                2,
                2,
                Action::FxDepth {
                    deck: 0,
                    slot: 0,
                    v: 0.25,
                },
            ),
            ev(
                3_000,
                1,
                3,
                Action::FxDepth {
                    deck: 0,
                    slot: 0,
                    v: 0.75,
                },
            ),
            ev(
                3_500,
                2,
                3,
                Action::FxSelect {
                    deck: 0,
                    slot: 1,
                    kind: FxKind::Phaser,
                },
            ),
            ev(
                4_000,
                1,
                4,
                Action::FxOn {
                    deck: 0,
                    slot: 1,
                    on: true,
                },
            ),
            ev(4_100, 2, 4, Action::FxBeats { deck: 0, beats: 3 }),
            ev(4_200, 1, 5, Action::FxBeats { deck: 0, beats: 8 }),
            ev(
                5_000,
                2,
                5,
                Action::FxOn {
                    deck: 1,
                    slot: 2,
                    on: true,
                },
            ),
        ];
        let mut a = Timeline::new(0);
        for e in &events {
            a.insert(e.clone());
        }
        let s = a.latest().decks[0].fx;
        assert!(!s.slots[0].on);
        // Same time: the higher origin is applied last.
        assert_eq!(s.slots[0].depth, 0.25);
        assert_eq!((s.slots[1].kind, s.slots[1].on), (FxKind::Phaser, true));
        assert_eq!((s.slots[1].since, s.beats), (4_000, 8));
        for seed in 0..8 {
            fastrand::Rng::with_seed(seed).shuffle(&mut events);
            let mut b = Timeline::new(0);
            for e in &events {
                b.insert(e.clone());
            }
            for t in [0, 2_200, 3_200, 4_150, 9_000] {
                assert_eq!(a.state_at(t), b.state_at(t), "at {t}");
                assert_eq!(a.state_at(t).digest(), b.state_at(t).digest(), "at {t}");
            }
        }
    }

    #[test]
    fn effects_survive_the_wire() {
        let moves = [
            Action::FxOn {
                deck: 3,
                slot: 1,
                on: true,
            },
            Action::FxDepth {
                deck: 3,
                slot: 1,
                v: 0.123_456_79,
            },
            Action::FxSelect {
                deck: 3,
                slot: 2,
                kind: FxKind::Reverb,
            },
            Action::FxBeats { deck: 3, beats: 4 },
        ];
        let mut tl = Timeline::new(0);
        for (i, a) in moves.iter().enumerate() {
            let e = ev(1_000 + i as i64, 4, i as u64 + 1, a.clone());
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), e, "{json}");
            tl.insert(e);
        }
        let json = serde_json::to_string(&tl).unwrap();
        assert!(json.contains(r#""a":"FxSelect""#) && json.contains(r#""kind":"Reverb""#));
        let back: Timeline = serde_json::from_str(&json).unwrap();
        assert_eq!(back.latest(), tl.latest());
        assert_eq!(back.latest().digest(), tl.latest().digest());
        // A room state saved before effects existed loads with the default bank.
        let mut old: serde_json::Value = serde_json::to_value(RoomState::default()).unwrap();
        for d in old["decks"].as_array_mut().unwrap() {
            d.as_object_mut().unwrap().remove("fx");
        }
        let s: RoomState = serde_json::from_value(old).unwrap();
        assert_eq!(s.decks[0].fx, FxBank::default());
    }

    #[test]
    fn the_digest_sees_every_effect_field() {
        let base = RoomState::default();
        let changes: [&dyn Fn(&mut FxBank); 5] = [
            &|b| b.slots[1].kind = FxKind::Phaser,
            &|b| b.slots[2].on = true,
            &|b| b.slots[0].depth = 0.51,
            &|b| b.slots[1].since = 12,
            &|b| b.beats = 6,
        ];
        for (i, change) in changes.iter().enumerate() {
            for d in 0..DECKS {
                let mut s = base;
                change(&mut s.decks[d].fx);
                assert_ne!(s.digest(), base.digest(), "change {i} on deck {d}");
            }
        }
    }
}
