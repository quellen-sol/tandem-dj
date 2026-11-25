//! Rendering the room to audio.
//!
//! A [`Lane`] renders the timeline as heard at `shared time - delay`. The
//! live lane (delay 0) feeds your speakers and headphones; the broadcast lane
//! (delay ~0.5 s) feeds a virtual cable for OBS. By the time the broadcast
//! lane reaches a moment, both DJs' events for it have arrived, so the stream
//! gets every cut exactly where it happened.
//!
//! Each deck has a read head ([`Player`]) that *chases* the analytic playhead
//! from the model: discontinuities (play, cue, hot cue, load) make it jump
//! with a 2 ms crossfade; small differences (jog nudges, sound-card drift,
//! clock corrections) are absorbed by speeding up or slowing down slightly.
//!
//! Loops are followed by the read head itself: it reads the track as if it
//! went round the loop (see [`looped_frame`]), so going round is seamless,
//! even through the time-stretcher, and needs no jump.

use crate::audio::TrackAudio;
use crate::fx::FxChain;
use crate::meter::{Ballistics, LevelMeters, Levels};
use crate::model::{DeckState, RoomState, Timeline, TrackId, XfCurve, XfSide, DECKS};
use crate::stretch::Stretcher;
use serde::{Deserialize, Deserializer, Serialize};
use std::sync::Arc;

/// Settings that belong to one booth and are never shared: your headphones
/// are yours.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalParams {
    /// Headphone cue per deck.
    #[serde(deserialize_with = "pfl_any_length")]
    pub pfl: [bool; DECKS],
    /// Headphone mix: 0 = cue only, 1 = master only.
    pub cue_mix: f32,
    pub hp_level: f32,
    pub master_level: f32,
    pub bcast_level: f32,
    /// Sync-test metronome on the shared clock (120 BPM).
    pub click: bool,
}

impl Default for LocalParams {
    fn default() -> Self {
        Self {
            pfl: [false; DECKS],
            cue_mix: 0.0,
            hp_level: 0.8,
            master_level: 0.8,
            bcast_level: 0.8,
            click: false,
        }
    }
}

/// Headphone cues from a settings file saved with any number of decks
/// (v0.8 and earlier had two): missing decks are off, extra ones dropped.
/// A strict array would fail to load, and with it every other setting.
fn pfl_any_length<'de, D: Deserializer<'de>>(d: D) -> Result<[bool; DECKS], D::Error> {
    let v = Vec::<bool>::deserialize(d)?;
    Ok(std::array::from_fn(|i| v.get(i).copied().unwrap_or(false)))
}

/// Everything an audio callback needs, swapped in atomically by the session.
pub struct AudioSnapshot {
    pub timeline: Timeline,
    pub tracks: Vec<Arc<TrackAudio>>,
    pub local: LocalParams,
}

impl AudioSnapshot {
    pub fn empty() -> Self {
        Self {
            timeline: Timeline::new(0),
            tracks: Vec::new(),
            local: LocalParams::default(),
        }
    }

    pub fn track(&self, id: Option<TrackId>) -> Option<&TrackAudio> {
        let id = id?;
        self.tracks.iter().find(|t| t.id == id).map(|t| t.as_ref())
    }
}

/// Maps a sound card's callbacks onto shared time.
///
/// Callbacks arrive with jitter, but the card consumes samples at a steady
/// rate. We predict each callback's start time from the previous one and
/// nudge the prediction gently toward what the clock says, which also tracks
/// the card's crystal drifting against the room clock.
pub struct StreamClock {
    next: Option<f64>,
    sample_rate: f64,
}

impl StreamClock {
    pub fn new(sample_rate: f64) -> Self {
        Self {
            next: None,
            sample_rate,
        }
    }

    /// `measured`: shared time at which this buffer's first frame will be heard.
    pub fn frame0(&mut self, measured: i64, frames: usize) -> i64 {
        let m = measured as f64;
        let t = match self.next {
            Some(exp) if (m - exp).abs() < 20_000.0 => exp + (m - exp) * 0.02,
            _ => m, // first callback, glitch, or the clock offset just jumped
        };
        self.next = Some(t + frames as f64 * 1e6 / self.sample_rate);
        t.round() as i64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneKind {
    Live,
    Broadcast,
}

const SUB: usize = 64;
const XFADE: u32 = 96;
const HARD_SYNC_S: f64 = 0.050;
const CHASE_TAU_S: f64 = 0.15;
const CHASE_MAX: f64 = 0.10;
/// While scratching, the read head trails the hand by about this much.
const SCRATCH_TAU_S: f64 = 0.012;
const SCRATCH_MAX_SPEED: f64 = 8.0;
const CLICK_PERIOD_US: i64 = 500_000;
const CLICK_LEN_US: i64 = 15_000;

/// Frames of audio fed to the stretcher before playback starts.
const SEEK_FRAMES: usize = 8192;
const HEAD_BUF_FRAMES: usize = 32_768;

/// Frames before a loop's out point that fade into the frames just before
/// its in point, so going round the loop never clicks (about 2 ms).
const LOOP_BLEND: i64 = 96;

/// A loop, in source frames.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LoopSpan {
    start: f64,
    end: f64,
}

impl LoopSpan {
    /// The loop deck `st` is going round, at `sr` frames per second.
    fn of(st: &DeckState, sr: f64) -> Option<Self> {
        st.active_loop().map(|(a, b)| LoopSpan {
            start: a * sr,
            end: b * sr,
        })
    }

    fn len(&self) -> f64 {
        self.end - self.start
    }

    /// The loop in whole frames: `[start, end)`.
    fn frames(&self) -> (i64, i64) {
        let s = self.start.round() as i64;
        (s, (self.end.round() as i64).max(s + 1))
    }

    /// A whole-frame position, gone round the loop if at or past its end.
    fn wrap_frames(&self, pos: f64) -> f64 {
        let (s, e) = self.frames();
        if pos >= e as f64 {
            s as f64 + (pos - s as f64).rem_euclid((e - s) as f64)
        } else {
            pos
        }
    }
}

/// Error between where the model and the read head are, going round the
/// loop the shorter way (a head that has just gone round is not a loop
/// length behind).
fn fold(err: f64, lp: Option<LoopSpan>) -> f64 {
    match lp {
        Some(l) if l.len() > 0.0 => err - (err / l.len()).round() * l.len(),
        _ => err,
    }
}

/// Track frame `idx` as heard going round `lp`: past the out point it
/// carries on from the in point, and the last [`LOOP_BLEND`] frames before
/// the out point fade into the frames just before the in point, so the
/// jump back joins up seamlessly. Deterministic, so both booths hear the
/// same loop.
#[inline]
fn looped_frame(a: &TrackAudio, idx: i64, lp: Option<LoopSpan>) -> [f32; 2] {
    let Some(lp) = lp else {
        return a.frame(idx as isize);
    };
    let (s, e) = lp.frames();
    let len = e - s;
    let i = if idx >= e {
        s + (idx - s).rem_euclid(len)
    } else {
        idx
    };
    let x = a.frame(i as isize);
    let blend = LOOP_BLEND.min(len / 4);
    if blend > 0 && i >= e - blend && i < e {
        let w = (i - (e - blend) + 1) as f32 / (blend + 1) as f32;
        let y = a.frame((i - len) as isize);
        [x[0] + (y[0] - x[0]) * w, x[1] + (y[1] - x[1]) * w]
    } else {
        x
    }
}

/// Copy track frames `[start, start + n)` as interleaved f32, going round
/// `lp` (silence outside the track).
fn fill(audio: &TrackAudio, start: i64, buf: &mut [f32], lp: Option<LoopSpan>) {
    for (i, c) in buf.chunks_exact_mut(2).enumerate() {
        let f = looped_frame(audio, start + i as i64, lp);
        c[0] = f[0];
        c[1] = f[1];
    }
}

/// One read head: vinyl-style (interpolated; pitch follows speed) or
/// through the time-stretcher (key lock / key shift).
struct Head {
    track: Option<TrackId>,
    playing: bool,
    stretched: bool,
    /// Vinyl: read position. Stretched: next input frame to feed.
    pos: f64,
    /// The loop this head goes round.
    lp: Option<LoopSpan>,
    /// Source frames consumed per output frame.
    step: f64,
    carry: f64,
    stretch: Stretcher,
    inbuf: Vec<f32>,
    outbuf: Vec<f32>,
}

impl Head {
    fn new(sr: f64) -> Self {
        Self {
            track: None,
            playing: false,
            stretched: false,
            pos: 0.0,
            lp: None,
            step: 0.0,
            carry: 0.0,
            stretch: Stretcher::new(sr as f32),
            inbuf: vec![0.0; HEAD_BUF_FRAMES * 2],
            outbuf: vec![0.0; HEAD_BUF_FRAMES * 2],
        }
    }

    /// Track position (source frames) of the audio coming out right now.
    fn heard(&self) -> f64 {
        if self.stretched {
            self.pos
                - self.stretch.input_latency() as f64
                - self.step * self.stretch.output_latency() as f64
        } else {
            self.pos
        }
    }

    /// Start (or restart) at `target` source frames.
    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        track: Option<TrackId>,
        target: f64,
        playing: bool,
        stretched: bool,
        step: f64,
        transpose: f32,
        audio: Option<&TrackAudio>,
    ) {
        self.track = track;
        self.playing = playing;
        self.stretched = stretched;
        self.step = step;
        self.carry = 0.0;
        self.pos = target;
        let (true, Some(a)) = (stretched && playing, audio) else {
            return;
        };
        // Prime the stretcher with the audio leading up to the target, then
        // run it for one output-latency and discard that, so the very first
        // sample heard is the target itself (a cue point's kick isn't lost).
        self.stretch.reset();
        self.stretch.set_transpose(transpose);
        let feed = (target + self.stretch.input_latency() as f64).round();
        fill(
            a,
            feed as i64 - SEEK_FRAMES as i64,
            &mut self.inbuf[..SEEK_FRAMES * 2],
            self.lp,
        );
        self.stretch.seek(&mut self.inbuf[..SEEK_FRAMES * 2], step);
        let out_lat = self.stretch.output_latency().min(HEAD_BUF_FRAMES / 2);
        let pre_in = ((out_lat as f64 * step).round() as usize).min(HEAD_BUF_FRAMES);
        fill(a, feed as i64, &mut self.inbuf[..pre_in * 2], self.lp);
        self.stretch.process(
            &mut self.inbuf[..pre_in * 2],
            &mut self.outbuf[..out_lat * 2],
        );
        self.pos = feed + pre_in as f64;
        if let Some(l) = self.lp {
            self.pos = l.wrap_frames(self.pos);
        }
    }

    fn render(&mut self, audio: Option<&TrackAudio>, out: &mut [[f32; 2]]) {
        let (true, Some(a)) = (self.playing, audio) else {
            out.fill([0.0; 2]);
            return;
        };
        if !self.stretched {
            match self.lp {
                None => {
                    for o in out.iter_mut() {
                        *o = a.sample(self.pos);
                        self.pos += self.step;
                    }
                }
                Some(l) => {
                    for o in out.iter_mut() {
                        *o = a.sample_with(self.pos, |i| looped_frame(a, i as i64, self.lp));
                        let before = self.pos;
                        self.pos += self.step;
                        if self.pos >= l.end && before < l.end {
                            self.pos -= l.len();
                        }
                    }
                }
            }
            return;
        }
        let n = out.len();
        let want = self.carry + n as f64 * self.step.max(0.0);
        let in_frames = (want.floor() as usize).min(HEAD_BUF_FRAMES);
        self.carry = want - in_frames as f64;
        fill(
            a,
            self.pos as i64,
            &mut self.inbuf[..in_frames * 2],
            self.lp,
        );
        self.stretch
            .process(&mut self.inbuf[..in_frames * 2], &mut self.outbuf[..n * 2]);
        self.pos += in_frames as f64;
        if let Some(l) = self.lp {
            self.pos = l.wrap_frames(self.pos);
        }
        for (i, o) in out.iter_mut().enumerate() {
            *o = [self.outbuf[i * 2], self.outbuf[i * 2 + 1]];
        }
    }
}

struct Player {
    track: Option<TrackId>,
    has_audio: bool,
    gen: u64,
    /// The loop the deck is going round (source frames), as of this block.
    lp: Option<LoopSpan>,
    cur: Head,
    /// The previous head, faded out after a jump.
    old: Head,
    xfade: u32,
    old_buf: [[f32; 2]; SUB],
    corr: f64,
    scratching: bool,
    /// Velocity (source frames per output frame), eased while scratching.
    vel: f64,
    /// Hard syncs so far (for tests).
    syncs: u32,
}

impl Player {
    fn new(sr: f64) -> Self {
        Self {
            track: None,
            has_audio: false,
            gen: 0,
            lp: None,
            cur: Head::new(sr),
            old: Head::new(sr),
            xfade: 0,
            old_buf: [[0.0; 2]; SUB],
            corr: 0.0,
            scratching: false,
            vel: 0.0,
            syncs: 0,
        }
    }

    fn prepare(&mut self, st: &DeckState, ts: i64, out_sr: f64, audio: Option<&TrackAudio>) {
        let track_sr = audio.map(|a| a.sample_rate as f64).unwrap_or(out_sr);
        let model_pos = st.position_at(ts);
        let target = model_pos * track_sr;
        let has_audio = audio.is_some();
        // A record let go mid-spin plays like a scratch until it settles.
        let spinning = has_audio && st.spinning_at(ts);
        let scratching = (st.scratching || spinning) && has_audio;
        let want_playing =
            scratching || (st.playing && has_audio && model_pos < st.duration - 1e-3);
        // Key lock / key shift go through the stretcher; scratching is always vinyl.
        let stretched = !scratching && (st.key_lock || st.key_shift != 0);
        let transpose = (st.pitch_ratio() * track_sr / out_sr) as f32;
        let base_step = track_sr / out_sr * st.rate();
        let changed = st.track != self.track
            || st.gen != self.gen
            || has_audio != self.has_audio
            || (want_playing && self.cur.playing && stretched != self.cur.stretched);
        // A new, changed or ended loop. The head can take it on the fly,
        // unless it has already read audio the loop says it shouldn't:
        // a vinyl head past the new out point (e.g. LOOP OUT pressed at
        // the playhead, or arriving from the partner a little late), or a
        // stretched head, whose stretcher holds audio read ahead.
        self.lp = LoopSpan::of(st, track_sr).filter(|_| has_audio);
        let lp = self.lp;
        let loop_changed = lp != self.cur.lp;
        let loop_jump = loop_changed
            && self.cur.playing
            && !scratching
            && match lp {
                Some(l) => self.cur.stretched || self.cur.pos >= l.end,
                None => self.cur.stretched,
            };
        if !changed && !loop_jump {
            self.cur.lp = lp;
        }
        if changed || loop_jump {
            self.hard_sync(
                st,
                target,
                want_playing,
                has_audio,
                stretched,
                base_step,
                transpose,
                audio,
            );
        }

        if scratching {
            // Vinyl: the read head follows the hand (or the spinning record),
            // forwards or backwards, with eased velocity so grabbing and
            // releasing sound natural.
            let outside = lp.is_some_and(|l| self.cur.pos >= l.end);
            let err = fold(target - self.cur.pos, lp);
            if err.abs() > 2.0 * track_sr || outside {
                self.hard_sync(st, target, true, has_audio, false, 0.0, 1.0, audio);
            }
            if !self.cur.playing {
                self.old.playing = false;
                self.xfade = XFADE;
                self.cur.playing = true;
            }
            let max = SCRATCH_MAX_SPEED * track_sr / out_sr;
            let err = fold(target - self.cur.pos, lp);
            let want = (err / (SCRATCH_TAU_S * out_sr)).clamp(-max, max);
            self.vel += (want - self.vel) * 0.35;
            self.cur.step = self.vel;
            self.scratching = true;
            self.corr = 0.0;
            return;
        }
        self.scratching = false;

        if !changed {
            if want_playing {
                let err = fold(target - self.cur.heard(), lp);
                if !self.cur.playing || err.abs() > HARD_SYNC_S * track_sr {
                    self.hard_sync(
                        st,
                        target,
                        want_playing,
                        has_audio,
                        stretched,
                        base_step,
                        transpose,
                        audio,
                    );
                } else {
                    self.corr = (err / track_sr / CHASE_TAU_S).clamp(-CHASE_MAX, CHASE_MAX);
                }
            } else {
                if self.cur.playing {
                    self.hard_sync(
                        st, target, false, has_audio, stretched, base_step, transpose, audio,
                    );
                }
                self.cur.pos = target; // paused: follow scrubs/nudges directly
                self.corr = 0.0;
            }
        }
        self.cur.step = if self.cur.playing {
            base_step * (1.0 + self.corr)
        } else {
            0.0
        };
        if self.cur.stretched {
            self.cur.stretch.set_transpose(transpose);
        }
        self.vel = self.cur.step;
    }

    #[allow(clippy::too_many_arguments)]
    fn hard_sync(
        &mut self,
        st: &DeckState,
        target: f64,
        playing: bool,
        has_audio: bool,
        stretched: bool,
        step: f64,
        transpose: f32,
        audio: Option<&TrackAudio>,
    ) {
        std::mem::swap(&mut self.cur, &mut self.old);
        self.xfade = if self.old.playing || playing {
            XFADE
        } else {
            0
        };
        self.syncs += 1;
        self.cur.lp = self.lp;
        self.cur
            .start(st.track, target, playing, stretched, step, transpose, audio);
        self.track = st.track;
        self.gen = st.gen;
        self.has_audio = has_audio;
        self.corr = 0.0;
    }

    fn render(&mut self, cur: Option<&TrackAudio>, old: Option<&TrackAudio>, out: &mut [[f32; 2]]) {
        self.cur.render(cur, out);
        if self.xfade == 0 {
            return;
        }
        let n = out.len();
        self.old.render(old, &mut self.old_buf[..n]);
        for (i, o) in out.iter_mut().enumerate() {
            if self.xfade == 0 {
                break;
            }
            let w = self.xfade as f32 / XFADE as f32;
            let so = self.old_buf[i];
            *o = [o[0] * (1.0 - w) + so[0] * w, o[1] * (1.0 - w) + so[1] * w];
            self.xfade -= 1;
        }
    }
}

/// RBJ biquad, transposed direct form II, stereo.
#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: [f32; 2],
    z2: [f32; 2],
}

impl Biquad {
    fn new(lowpass: bool, fc: f64, sr: f64) -> Self {
        let mut b = Self::default();
        b.set(lowpass, fc, sr, std::f64::consts::FRAC_1_SQRT_2);
        b
    }

    /// Change the response, keeping the filter's state (no clicks).
    fn set(&mut self, lowpass: bool, fc: f64, sr: f64, q: f64) {
        let fc = fc.clamp(10.0, sr * 0.45);
        let w0 = 2.0 * std::f64::consts::PI * fc / sr;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        let (b0, b1, b2) = if lowpass {
            ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0)
        } else {
            ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0)
        };
        self.b0 = (b0 / a0) as f32;
        self.b1 = (b1 / a0) as f32;
        self.b2 = (b2 / a0) as f32;
        self.a1 = (-2.0 * cos / a0) as f32;
        self.a2 = ((1.0 - alpha) / a0) as f32;
    }

    #[inline]
    fn run(&mut self, x: [f32; 2]) -> [f32; 2] {
        let mut y = [0.0; 2];
        for c in 0..2 {
            y[c] = self.b0 * x[c] + self.z1[c];
            self.z1[c] = self.b1 * x[c] - self.a1 * y[c] + self.z2[c];
            self.z2[c] = self.b2 * x[c] - self.a2 * y[c];
        }
        y
    }
}

/// Three-band DJ isolator built from Linkwitz-Riley (4th order) crossovers,
/// so each band can be killed completely.
struct Isolator {
    lo_lp: [Biquad; 2],
    lo_hp: [Biquad; 2],
    hi_lp: [Biquad; 2],
    hi_hp: [Biquad; 2],
}

impl Isolator {
    fn new(sr: f64) -> Self {
        let lp = |f| [Biquad::new(true, f, sr), Biquad::new(true, f, sr)];
        let hp = |f| [Biquad::new(false, f, sr), Biquad::new(false, f, sr)];
        Self {
            lo_lp: lp(300.0),
            lo_hp: hp(300.0),
            hi_lp: lp(4000.0),
            hi_hp: hp(4000.0),
        }
    }

    #[inline]
    fn process(&mut self, x: [f32; 2], g: [f32; 3]) -> [f32; 2] {
        let low = cascade(&mut self.lo_lp, x);
        let rest = cascade(&mut self.lo_hp, x);
        let mid = cascade(&mut self.hi_lp, rest);
        let high = cascade(&mut self.hi_hp, rest);
        [
            low[0] * g[0] + mid[0] * g[1] + high[0] * g[2],
            low[1] * g[0] + mid[1] * g[1] + high[1] * g[2],
        ]
    }
}

#[inline]
fn cascade(f: &mut [Biquad; 2], x: [f32; 2]) -> [f32; 2] {
    let y = f[0].run(x);
    f[1].run(y)
}

/// One-knob DJ filter: a low-pass and a high-pass in series. At the centre
/// both sit at the edges of hearing; turning left sweeps the low-pass down,
/// right sweeps the high-pass up. Both always run, so crossing the centre
/// never clicks. Each sweep runs right out of the audible range, and over
/// the last stretch of travel the output fades away, so a knob turned all
/// the way is silent.
struct DjFilter {
    lp: Biquad,
    hp: Biquad,
    last: f32,
    /// Output gain (smoothed towards `gain_target`, so the fade never zippers).
    gain: f32,
    gain_target: f32,
    smooth: f32,
}

/// Low-pass cutoff at full left, and high-pass cutoff at full right (the
/// biquad clamps it just below Nyquist).
const FILTER_LP_MIN: f64 = 20.0;
const FILTER_HP_MAX: f64 = 22_000.0;
/// Knob travel (from either end) over which the filter fades to silence.
const FILTER_FADE: f32 = 0.1;

impl DjFilter {
    fn new(sr: f64) -> Self {
        let mut f = Self {
            lp: Biquad::default(),
            hp: Biquad::default(),
            last: -1.0,
            gain: 1.0,
            gain_target: 1.0,
            smooth: (1.0 - (-1.0 / (0.010 * sr)).exp()) as f32,
        };
        f.update(0.5, sr);
        f.gain = f.gain_target;
        f
    }

    fn update(&mut self, k: f32, sr: f64) {
        if (k - self.last).abs() < 1e-4 {
            return;
        }
        self.last = k;
        self.gain_target = filter_edge_gain(k);
        let k = k as f64;
        let lp_fc = if k < 0.5 {
            20_000.0 * (FILTER_LP_MIN / 20_000.0).powf((0.5 - k) / 0.5)
        } else {
            20_000.0
        };
        let hp_fc = if k > 0.5 {
            20.0 * (FILTER_HP_MAX / 20.0).powf((k - 0.5) / 0.5)
        } else {
            20.0
        };
        let q = if (k - 0.5).abs() < 0.02 {
            std::f64::consts::FRAC_1_SQRT_2
        } else {
            1.1
        };
        self.lp.set(true, lp_fc, sr, q);
        self.hp.set(false, hp_fc, sr, q);
    }

    #[inline]
    fn run(&mut self, x: [f32; 2]) -> [f32; 2] {
        self.gain += (self.gain_target - self.gain) * self.smooth;
        if (self.gain - self.gain_target).abs() < 1e-6 {
            // Land exactly (true silence at the ends, and no denormals).
            self.gain = self.gain_target;
        }
        let y = self.lp.run(x);
        let y = self.hp.run(y);
        [y[0] * self.gain, y[1] * self.gain]
    }
}

/// Filter knob (0..1) to output gain: unity except over the last
/// [`FILTER_FADE`] of travel at either end, where it eases down to silence.
fn filter_edge_gain(k: f32) -> f32 {
    let edge = k.min(1.0 - k).clamp(0.0, 0.5);
    if edge >= FILTER_FADE {
        return 1.0;
    }
    let x = edge / FILTER_FADE;
    x * x * (3.0 - 2.0 * x)
}

/// Trim knob (0..1, 0.5 = unity) to gain: silent at 0, +12 dB at 1.
pub fn trim_gain(k: f32) -> f32 {
    if k <= 0.5 {
        let x = k / 0.5;
        x * x
    } else {
        1.0 + (k - 0.5) / 0.5 * 2.98
    }
}

/// EQ knob (0..1, 0.5 = unity) to linear gain: full kill at 0, +6 dB at 1.
pub fn eq_gain(k: f32) -> f32 {
    if k <= 0.5 {
        let x = k / 0.5;
        x * x
    } else {
        1.0 + (k - 0.5) / 0.5
    }
}

/// Channel fader position to gain (roughly audio taper).
pub fn fader_gain(v: f32) -> f32 {
    v * v
}

/// On the scratch curve, a deck cuts in over this much of the travel at the
/// far end (3%, about 2 mm on a 60 mm crossfader).
const SCRATCH_CUT: f32 = 0.03;

/// Crossfader gain for a channel on `side` with the crossfader at `x`
/// (0 = full left, side A; 1 = full right, side B) on `curve`. Side B's
/// curve is side A's, mirrored. A channel set to THRU ignores the crossfader.
pub fn xfader_gain(curve: XfCurve, side: XfSide, x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    // How far the fader is from this side's end, and from the other end
    // (each exact at the ends, so a channel is fully cut there).
    let (away, left) = match side {
        XfSide::A => (x, 1.0 - x),
        XfSide::B => (1.0 - x, x),
        XfSide::Thru => return 1.0,
    };
    // Quarter-cosine fade from full (t <= 0) to silent (t >= 1).
    let fade = |t: f32| {
        if t <= 0.0 {
            1.0
        } else if t >= 1.0 {
            0.0
        } else {
            (t * std::f32::consts::FRAC_PI_2).cos()
        }
    };
    match curve {
        XfCurve::Linear => left,
        XfCurve::ConstantPower => fade(away),
        XfCurve::FullCentre => fade(1.0 - left / 0.5),
        XfCurve::Scratch => fade(1.0 - left / SCRATCH_CUT),
    }
}

/// A deck this quiet in the master or quieter (−40 dB) is as good as
/// silent: a track may be loaded over it (see
/// [`RoomState::audible_in_master`]).
pub const SILENT_GAIN: f32 = 0.01;

/// Within this much of either end of its travel (1%), the filter counts as
/// cutting a deck out. Measured on pink noise, it lets through −40 dB there
/// on the low-pass side and −45 dB on the high-pass side, and less further
/// on (−43 dB at a controller's last step before the end).
const FILTER_KILL: f32 = 0.01;

/// Roughly how loud deck `d` is in the master mix (and the broadcast),
/// relative to the track itself: trim, EQ (its loudest band), the filter,
/// the channel fader and the crossfader. The headphone cue doesn't count.
///
/// The filter counts by its fade at the ends of its travel, and as silence
/// within [`FILTER_KILL`] of either end. Its sweep alone is left out: short
/// of the fade, it still lets the bass or the hats through.
pub fn master_gain(state: &RoomState, d: usize) -> f32 {
    let st = &state.decks[d];
    let k = st.filter;
    let filter = if k <= FILTER_KILL || k >= 1.0 - FILTER_KILL {
        0.0
    } else {
        filter_edge_gain(k)
    };
    let eq = st.eq.iter().map(|&v| eq_gain(v)).fold(0.0, f32::max);
    trim_gain(st.trim)
        * eq
        * filter
        * fader_gain(st.volume)
        * xfader_gain(state.xf_curve, state.xf_assign[d], state.crossfader)
}

#[inline]
fn soft_clip(x: f32) -> f32 {
    let a = x.abs();
    if a <= 0.9 {
        x
    } else {
        x.signum() * (0.9 + 0.1 * ((a - 0.9) / 0.1).tanh())
    }
}

pub struct Lane {
    pub kind: LaneKind,
    pub delay_us: i64,
    sr: f64,
    players: [Player; DECKS],
    iso: [Isolator; DECKS],
    filt: [DjFilter; DECKS],
    /// Each deck's effects, after its fader and the crossfader.
    fx: [FxChain; DECKS],
    deck_buf: [[[f32; 2]; SUB]; DECKS],
    vol: [f32; DECKS],
    trim: [f32; DECKS],
    eqg: [[f32; 3]; DECKS],
    smooth: f32,
    primed: bool,
    /// Where this lane publishes its levels, if it's the one metered.
    meters: Option<Arc<LevelMeters>>,
    ballistics: Ballistics,
    /// The headphone output carries only the cued channels: the sound card
    /// blends in the master itself.
    cue_only: bool,
}

impl Lane {
    pub fn new(sample_rate: f64, delay_us: i64, kind: LaneKind) -> Self {
        Self {
            kind,
            delay_us,
            sr: sample_rate,
            players: std::array::from_fn(|_| Player::new(sample_rate)),
            iso: std::array::from_fn(|_| Isolator::new(sample_rate)),
            filt: std::array::from_fn(|_| DjFilter::new(sample_rate)),
            fx: std::array::from_fn(|_| FxChain::new(sample_rate)),
            deck_buf: [[[0.0; 2]; SUB]; DECKS],
            vol: [0.0; DECKS],
            trim: [1.0; DECKS],
            eqg: [[1.0; 3]; DECKS],
            // ~10 ms parameter smoothing.
            smooth: (1.0 - (-1.0 / (0.010 * sample_rate)).exp()) as f32,
            primed: false,
            meters: None,
            ballistics: Ballistics::new(sample_rate),
            cue_only: false,
        }
    }

    /// Send only the cued channels to the headphones, ignoring the cue/master
    /// mix: for a controller that blends the master in with its own knob.
    pub fn cue_only(mut self, on: bool) -> Self {
        self.cue_only = on;
        self
    }

    /// Measure this lane's channel and master levels into `meters`.
    pub fn with_meters(mut self, meters: Arc<LevelMeters>) -> Self {
        self.meters = Some(meters);
        self
    }

    /// Render `master.len() / 2` stereo frames whose first frame is heard at
    /// shared time `t0`. Writes the main mix to `master` and the headphone
    /// mix to `cue` (both interleaved stereo, same length).
    pub fn render(&mut self, snap: &AudioSnapshot, t0: i64, master: &mut [f32], cue: &mut [f32]) {
        let frames = master.len() / 2;
        let lane_t0 = t0 - self.delay_us;
        let tl = &snap.timeline;
        let (mut state, mut idx) = tl.cursor(lane_t0);
        let local = snap.local;
        let us_per_frame = 1e6 / self.sr;
        let metering = self.meters.is_some();

        let mut f = 0;
        while f < frames {
            let n = SUB.min(frames - f);
            let ts = lane_t0 + (f as f64 * us_per_frame) as i64;
            tl.advance(&mut state, &mut idx, ts);
            self.render_block(snap, &state, ts, n);

            let targets = self.targets(&state);
            let first = !self.primed;
            if first {
                self.vol = targets.0;
                self.eqg = targets.1;
                self.trim = targets.2;
                self.primed = true;
            }
            for d in 0..DECKS {
                self.filt[d].update(state.decks[d].filter, self.sr);
                self.fx[d].prepare(&state.decks[d], ts, n, first);
            }
            let mut peaks = Levels::default();
            // Each deck after its fader, in place, and the headphone cue.
            let mut cues = [[0.0f32; 2]; SUB];
            for (i, c) in cues.iter_mut().enumerate().take(n) {
                for d in 0..DECKS {
                    self.vol[d] += (targets.0[d] - self.vol[d]) * self.smooth;
                    for b in 0..3 {
                        self.eqg[d][b] += (targets.1[d][b] - self.eqg[d][b]) * self.smooth;
                    }
                    self.trim[d] += (targets.2[d] - self.trim[d]) * self.smooth;
                    let x = self.deck_buf[d][i];
                    let x = [x[0] * self.trim[d], x[1] * self.trim[d]];
                    let s = self.iso[d].process(x, self.eqg[d]);
                    let s = self.filt[d].run(s);
                    if metering {
                        let p = &mut peaks.channels[d];
                        *p = p.max(s[0].abs()).max(s[1].abs());
                    }
                    self.deck_buf[d][i] = [s[0] * self.vol[d], s[1] * self.vol[d]];
                    if local.pfl[d] {
                        c[0] += s[0];
                        c[1] += s[1];
                    }
                }
            }
            // Then its effects. While they're all off and quiet, the
            // signal doesn't go near them.
            for d in 0..DECKS {
                if self.fx[d].busy() {
                    self.fx[d].run_block(&mut self.deck_buf[d][..n]);
                }
            }
            for (i, c) in cues.iter().enumerate().take(n) {
                let mut m = [0.0f32; 2];
                for d in 0..DECKS {
                    m[0] += self.deck_buf[d][i][0];
                    m[1] += self.deck_buf[d][i][1];
                }
                if metering {
                    peaks.master[0] = peaks.master[0].max(m[0].abs());
                    peaks.master[1] = peaks.master[1].max(m[1].abs());
                }
                let tf = ts + (i as f64 * us_per_frame) as i64;
                let click = if local.click { click_sample(tf) } else { 0.0 };
                let (out_gain, hp) = match self.kind {
                    LaneKind::Live => (local.master_level, local.hp_level),
                    LaneKind::Broadcast => (local.bcast_level, 0.0),
                };
                let o = (f + i) * 2;
                master[o] = soft_clip(m[0] * out_gain + click);
                master[o + 1] = soft_clip(m[1] * out_gain + click);
                let mix = if self.cue_only { 0.0 } else { local.cue_mix };
                cue[o] = soft_clip((c[0] * (1.0 - mix) + m[0] * mix) * hp + click);
                cue[o + 1] = soft_clip((c[1] * (1.0 - mix) + m[1] * mix) * hp + click);
            }
            if metering {
                self.ballistics.block(&peaks, n);
            }
            for chain in &mut self.fx {
                chain.settle();
            }
            f += n;
        }
        if let Some(meters) = &self.meters {
            meters.publish(&self.ballistics.levels);
        }
    }

    fn render_block(&mut self, snap: &AudioSnapshot, state: &RoomState, ts: i64, n: usize) {
        for d in 0..DECKS {
            let st = &state.decks[d];
            let p = &mut self.players[d];
            p.prepare(st, ts, self.sr, snap.track(st.track));
            let cur = snap.track(p.cur.track);
            let old = if p.xfade > 0 {
                snap.track(p.old.track)
            } else {
                None
            };
            p.render(cur, old, &mut self.deck_buf[d][..n]);
        }
    }

    #[allow(clippy::type_complexity)]
    fn targets(&self, state: &RoomState) -> ([f32; DECKS], [[f32; 3]; DECKS], [f32; DECKS]) {
        let mut vol = [0.0; DECKS];
        let mut eq = [[1.0; 3]; DECKS];
        let mut trim = [1.0; DECKS];
        for d in 0..DECKS {
            let st = &state.decks[d];
            vol[d] = fader_gain(st.volume)
                * xfader_gain(state.xf_curve, state.xf_assign[d], state.crossfader);
            trim[d] = trim_gain(st.trim);
            for b in 0..3 {
                eq[d][b] = eq_gain(st.eq[b]);
            }
        }
        (vol, eq, trim)
    }
}

/// Metronome on the shared clock: every booth clicks at the same instants,
/// so two laptops side by side should click together.
fn click_sample(t: i64) -> f32 {
    let phase = t.rem_euclid(CLICK_PERIOD_US);
    if phase >= CLICK_LEN_US {
        return 0.0;
    }
    let beat = t.div_euclid(CLICK_PERIOD_US);
    let freq = if beat.rem_euclid(4) == 0 {
        2000.0
    } else {
        1000.0
    };
    let env = 1.0 - phase as f32 / CLICK_LEN_US as f32;
    let x = (2.0 * std::f64::consts::PI * freq * phase as f64 * 1e-6).sin() as f32;
    x * env * 0.35
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::test_signal;
    use crate::model::{Action, Event};

    fn snap_with(tl: Timeline, audio: Arc<TrackAudio>) -> AudioSnapshot {
        AudioSnapshot {
            timeline: tl,
            tracks: vec![audio],
            local: LocalParams::default(),
        }
    }

    /// Level (RMS out / RMS in) of a 40 Hz + 1 kHz + 12 kHz mix through the
    /// DJ filter at knob `k`, once settled.
    fn filter_level(k: f32) -> f32 {
        let sr = 48_000.0;
        let mut f = DjFilter::new(sr);
        f.update(k, sr);
        let (mut out, mut inp) = (0.0f64, 0.0f64);
        for i in 0..sr as usize {
            let t = i as f64 / sr;
            let x = [40.0, 1_000.0, 12_000.0]
                .iter()
                .map(|hz| (2.0 * std::f64::consts::PI * hz * t).sin() * 0.3)
                .sum::<f64>() as f32;
            let y = f.run([x, x]);
            if i >= sr as usize / 2 {
                out += (y[0] as f64).powi(2) + (y[1] as f64).powi(2);
                inp += 2.0 * (x as f64).powi(2);
            }
        }
        (out / inp).sqrt() as f32
    }

    #[test]
    fn filter_turned_all_the_way_is_silent() {
        assert!(
            (filter_level(0.5) - 1.0).abs() < 0.1,
            "{}",
            filter_level(0.5)
        );
        assert_eq!(filter_level(0.0), 0.0);
        assert_eq!(filter_level(1.0), 0.0);
        // Short of the end, each side still lets its end of the spectrum through...
        for k in [0.15, 0.85] {
            assert!(filter_level(k) > 0.4, "k {k}: {}", filter_level(k));
        }
        // ...and the last stretch fades it out.
        assert!(filter_level(0.03) < filter_level(0.15) * 0.3);
        assert!(filter_level(0.97) < filter_level(0.85) * 0.3);
        assert_eq!(filter_edge_gain(0.5), 1.0);
        assert_eq!(filter_edge_gain(1.0 - FILTER_FADE), 1.0);
    }

    /// The load rule only calls a deck filtered out where the real filter
    /// lets through -40 dB or less, and calls it out all the way to the ends.
    #[test]
    fn the_filter_only_counts_as_silent_where_it_is() {
        let mut s = RoomState::default();
        s.crossfader = 0.0;
        let fine = (0..=30).map(|i| i as f32 / 1000.0);
        let coarse = (4..=50).map(|i| i as f32 / 100.0);
        for k in fine.chain(coarse) {
            for k in [k, 1.0 - k] {
                s.decks[0].filter = k;
                if master_gain(&s, 0) <= SILENT_GAIN {
                    assert!(filter_level(k) <= SILENT_GAIN, "k {k}: {}", filter_level(k));
                }
            }
        }
        for k in [0.0, FILTER_KILL, 1.0 - FILTER_KILL, 1.0] {
            s.decks[0].filter = k;
            assert_eq!(master_gain(&s, 0), 0.0, "k {k}");
        }
        s.decks[0].filter = 0.5;
        assert_eq!(master_gain(&s, 0), 1.0);
    }

    #[test]
    fn master_gain_follows_the_mixer() {
        let mut s = RoomState::default();
        // Constant power, crossfader in the middle: -3 dB.
        let mid = std::f32::consts::FRAC_1_SQRT_2;
        assert!((master_gain(&s, 0) - mid).abs() < 1e-6);
        s.decks[0].volume = 0.5;
        assert!((master_gain(&s, 0) - 0.25 * mid).abs() < 1e-6);
        s.decks[0].trim = 1.0;
        assert!((master_gain(&s, 0) - 0.25 * mid * trim_gain(1.0)).abs() < 1e-6);
        s.decks[0].eq = [0.0, 0.0, 1.0];
        assert!((master_gain(&s, 0) - 0.25 * mid * trim_gain(1.0) * 2.0).abs() < 1e-6);
        s.crossfader = 1.0;
        assert_eq!(master_gain(&s, 0), 0.0);
        s.xf_assign[0] = XfSide::Thru;
        assert!(master_gain(&s, 0) > 1.0);
    }

    fn track() -> Arc<TrackAudio> {
        Arc::new(TrackAudio::new(
            TrackId([1; 32]),
            44_100,
            test_signal(44_100, 10.0, 440.0),
        ))
    }

    #[test]
    fn meters_read_channels_before_the_fader_and_the_bus_after() {
        let a = track();
        let peak = a.frames.iter().map(|f| f[0].unsigned_abs()).max().unwrap() as f32 / 32768.0;
        let mut tl = Timeline::new(0);
        let mut seq = 0;
        let mut at = |tl: &mut Timeline, time: i64, action: Action| {
            seq += 1;
            tl.insert(Event {
                time,
                origin: 1,
                seq,
                action,
            });
        };
        at(
            &mut tl,
            0,
            Action::Load {
                deck: 0,
                track: a.id,
                duration: a.duration(),
                cue: 0.0,
                grid: None,
                key: -1,
            },
        );
        at(&mut tl, 0, Action::Play { deck: 0 });
        at(&mut tl, 1_000_000, Action::Volume { deck: 0, v: 0.0 });
        let snap = snap_with(tl, a);
        let meters = Arc::new(LevelMeters::new());
        let mut lane = Lane::new(48_000.0, 0, LaneKind::Live).with_meters(meters.clone());
        // The test signal kicks every half second: read just after one.
        let render = |lane: &mut Lane, from_s: f64, to_s: f64| {
            let n = ((to_s - from_s) * 48_000.0).round() as usize;
            let (mut m, mut c) = (vec![0.0; n * 2], vec![0.0; n * 2]);
            lane.render(&snap, (from_s * 1e6) as i64, &mut m, &mut c);
        };
        render(&mut lane, 0.0, 0.52);
        let l = meters.read();
        assert!(
            (l.channels[0] - peak).abs() < 0.05 * peak,
            "{l:?} vs {peak}"
        );
        assert_eq!(l.channels[1], 0.0);
        // Fader up, crossfader in the middle.
        let bus = peak * xfader_gain(XfCurve::ConstantPower, XfSide::A, 0.5);
        assert!((l.master[0] - bus).abs() < 0.05 * bus, "{l:?} vs {bus}");
        // Fader down: the channel still reads, the bus falls away.
        render(&mut lane, 0.52, 2.52);
        let l = meters.read();
        assert!((l.channels[0] - peak).abs() < 0.05 * peak, "{l:?}");
        assert!(l.master[0] < 0.05 * bus, "{l:?}");
    }

    /// Deck A cued with its fader down and the phones mix turned all the way
    /// to master: the headphone peak with and without `cue_only`.
    fn phones_peak(cue_only: bool) -> f32 {
        let a = track();
        let mut tl = Timeline::new(0);
        for (seq, action) in [
            Action::Load {
                deck: 0,
                track: a.id,
                duration: a.duration(),
                cue: 0.0,
                grid: None,
                key: -1,
            },
            Action::Play { deck: 0 },
            Action::Volume { deck: 0, v: 0.0 },
        ]
        .into_iter()
        .enumerate()
        {
            tl.insert(Event {
                time: 0,
                origin: 1,
                seq: seq as u64 + 1,
                action,
            });
        }
        let mut snap = snap_with(tl, a);
        snap.local.pfl[0] = true;
        snap.local.cue_mix = 1.0;
        let mut lane = Lane::new(48_000.0, 0, LaneKind::Live).cue_only(cue_only);
        let n = 48_000;
        let (mut m, mut c) = (vec![0.0; n * 2], vec![0.0; n * 2]);
        lane.render(&snap, 0, &mut m, &mut c);
        assert!(
            m.iter().all(|x| x.abs() < 1e-3),
            "fader down: master is silent"
        );
        c.iter().fold(0.0, |p, x| p.max(x.abs()))
    }

    #[test]
    fn cue_only_phones_ignore_the_mix() {
        // Mixed in software, "all master" leaves nothing of the cued deck...
        assert!(phones_peak(false) < 1e-3);
        // ...but a controller blends the master in itself, so its headphone
        // channels always carry the cued deck.
        assert!(phones_peak(true) > 0.1, "{}", phones_peak(true));
    }

    #[test]
    fn crossfader_curves() {
        use XfCurve::*;
        // Side A is deck 0 here, side B deck 1.
        let g = |c, d: usize, x| xfader_gain(c, [XfSide::A, XfSide::B][d], x);
        for c in XfCurve::ALL {
            // Each end is one deck alone, at full volume.
            assert_eq!((g(c, 0, 0.0), g(c, 1, 0.0)), (1.0, 0.0), "{c:?}");
            assert_eq!((g(c, 0, 1.0), g(c, 1, 1.0)), (0.0, 1.0), "{c:?}");
            // Deck B mirrors deck A, and a deck never gets louder as the
            // fader moves away from it.
            let mut last = 1.0;
            for i in 0..=1000 {
                let x = i as f32 / 1000.0;
                let a = g(c, 0, x);
                assert!((a - g(c, 1, 1.0 - x)).abs() < 1e-5, "{c:?} at {x}");
                assert!(a <= last, "{c:?} rises at {x}");
                last = a;
            }
        }
        let mid = |c| g(c, 0, 0.5);
        assert!((mid(Linear) - 0.5).abs() < 1e-6);
        assert!((mid(ConstantPower) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert_eq!(mid(FullCentre), 1.0);
        assert_eq!(mid(Scratch), 1.0);
        for i in 0..=100 {
            let x = i as f32 / 100.0;
            // Linear: the gains add up to one; constant power: their squares do.
            assert!((g(Linear, 0, x) + g(Linear, 1, x) - 1.0).abs() < 1e-6);
            let p = g(ConstantPower, 0, x).powi(2) + g(ConstantPower, 1, x).powi(2);
            assert!((p - 1.0).abs() < 1e-5, "{x}: {p}");
            // Full centre: the near deck is always at full volume.
            assert_eq!(g(FullCentre, 0, x).max(g(FullCentre, 1, x)), 1.0);
        }
        // Full centre fades over the far half; scratch over the last 3%.
        assert!((g(FullCentre, 0, 0.75) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert_eq!(g(Scratch, 0, 0.96), 1.0);
        assert_eq!(g(Scratch, 1, 0.04), 1.0);
        assert!((g(Scratch, 0, 0.985) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
    }

    /// RMS out of a lane with deck A playing alone, the crossfader at `x`
    /// on `curve`.
    fn lone_deck_level(curve: XfCurve, x: f32) -> f32 {
        deck_level(0, None, curve, x)
    }

    /// RMS out of a lane with `deck` playing alone (moved to `side` of the
    /// crossfader, if given), the crossfader at `x` on `curve`.
    fn deck_level(deck: u8, side: Option<XfSide>, curve: XfCurve, x: f32) -> f32 {
        let a = track();
        let mut tl = Timeline::new(0);
        let mut actions = vec![
            Action::Load {
                deck,
                track: a.id,
                duration: a.duration(),
                cue: 0.0,
                grid: None,
                key: -1,
            },
            Action::CrossfaderCurve { curve },
            Action::Crossfader { v: x },
            Action::Play { deck },
        ];
        if let Some(side) = side {
            actions.push(Action::XfAssign { deck, side });
        }
        for (i, action) in actions.into_iter().enumerate() {
            tl.insert(Event {
                time: 0,
                origin: 1,
                seq: i as u64 + 1,
                action,
            });
        }
        let snap = snap_with(tl, a);
        let mut lane = Lane::new(44_100.0, 0, LaneKind::Live);
        let n = 44_100;
        let (mut m, mut c) = (vec![0.0; n * 2], vec![0.0; n * 2]);
        lane.render(&snap, 500_000, &mut m, &mut c);
        (m.iter().map(|v| v * v).sum::<f32>() / m.len() as f32).sqrt()
    }

    #[test]
    fn every_deck_plays_on_its_crossfader_side() {
        let full = deck_level(0, None, XfCurve::Linear, 0.0);
        assert!(full > 0.05, "silence");
        let close = |a: f32, b: f32| (a - b).abs() < 0.02 * full;
        // A and C start on the left, B and D on the right.
        for deck in 0..DECKS as u8 {
            let (near, far) = if deck % 2 == 0 {
                (0.0, 1.0)
            } else {
                (1.0, 0.0)
            };
            let level = |x| deck_level(deck, None, XfCurve::Linear, x);
            assert!(close(level(near), full), "deck {deck} at its own end");
            assert!(level(far) < 1e-4, "deck {deck} at the far end");
            assert!(close(level(0.5), 0.5 * full), "deck {deck} in the middle");
        }
        // Moved across, deck C follows the right side; through, it ignores
        // the crossfader.
        assert!(deck_level(2, Some(XfSide::B), XfCurve::Linear, 0.0) < 1e-4);
        assert!(close(
            deck_level(2, Some(XfSide::B), XfCurve::Linear, 1.0),
            full
        ));
        for x in [0.0, 0.5, 1.0] {
            let thru = deck_level(2, Some(XfSide::Thru), XfCurve::Scratch, x);
            assert!(close(thru, full), "THRU at {x}: {thru} vs {full}");
        }
    }

    #[test]
    fn old_settings_keep_their_headphone_cues() {
        let p: LocalParams =
            serde_json::from_str(r#"{"pfl": [true, false], "cue_mix": 0.25}"#).unwrap();
        assert_eq!(p.pfl, [true, false, false, false]);
        assert_eq!(p.cue_mix, 0.25);
        let back: LocalParams = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn full_centre_keeps_a_lone_deck_at_full_volume() {
        let full = lone_deck_level(XfCurve::ConstantPower, 0.0);
        assert!(full > 0.05, "silence");
        for (curve, at_centre) in [
            (XfCurve::Linear, 0.5),
            (XfCurve::ConstantPower, std::f32::consts::FRAC_1_SQRT_2),
            (XfCurve::FullCentre, 1.0),
            (XfCurve::Scratch, 1.0),
        ] {
            assert!(
                (lone_deck_level(curve, 0.0) - full).abs() < 1e-4,
                "{curve:?}"
            );
            let r = lone_deck_level(curve, 0.5) / full;
            assert!((r - at_centre).abs() < 0.01, "{curve:?}: {r}");
        }
    }

    #[test]
    fn two_lanes_rendering_same_time_are_identical() {
        let a = track();
        let mut tl = Timeline::new(0);
        tl.insert(Event {
            time: 0,
            origin: 1,
            seq: 1,
            action: Action::Load {
                deck: 0,
                track: a.id,
                duration: a.duration(),
                cue: 0.0,
                grid: None,
                key: -1,
            },
        });
        tl.insert(Event {
            time: 100_000,
            origin: 1,
            seq: 2,
            action: Action::Play { deck: 0 },
        });
        let snap = snap_with(tl, a);
        let mut l1 = Lane::new(48_000.0, 0, LaneKind::Live);
        let mut l2 = Lane::new(48_000.0, 500_000, LaneKind::Live);
        let n = 4800;
        let (mut m1, mut c1) = (vec![0.0; n * 2], vec![0.0; n * 2]);
        let (mut m2, mut c2) = (vec![0.0; n * 2], vec![0.0; n * 2]);
        l1.render(&snap, 1_000_000, &mut m1, &mut c1);
        // The delayed lane asked for t+0.5 s renders the same moment.
        l2.render(&snap, 1_500_000, &mut m2, &mut c2);
        assert!(m1.iter().any(|v| v.abs() > 0.01), "silence");
        let max_diff = m1
            .iter()
            .zip(&m2)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f32::max);
        assert!(max_diff < 1e-6, "lanes differ by {max_diff}");
    }

    /// A deck ejected while it runs in the headphones (its fader down) is
    /// quiet at once, without a click, with key lock and without; a lane
    /// behind the room still plays it up to the moment it went.
    #[test]
    fn an_ejected_deck_goes_quiet_without_a_click() {
        for key_lock in [true, false] {
            let a = track();
            let mut tl = Timeline::new(0);
            for (i, (time, action)) in [
                (
                    0,
                    Action::Load {
                        deck: 0,
                        track: a.id,
                        duration: a.duration(),
                        cue: 0.0,
                        grid: None,
                        key: -1,
                    },
                ),
                (
                    0,
                    Action::KeyLock {
                        deck: 0,
                        on: key_lock,
                    },
                ),
                (0, Action::Volume { deck: 0, v: 0.0 }),
                (100_000, Action::Play { deck: 0 }),
                (900_000, Action::Eject { deck: 0 }),
            ]
            .into_iter()
            .enumerate()
            {
                tl.insert(fx_ev(time, 1 + i as u64, action));
            }
            let mut snap = snap_with(tl, a);
            snap.local.pfl[0] = true;
            snap.local.cue_mix = 0.0;
            let phones = |delay_us: i64| -> Vec<f32> {
                let mut lane = Lane::new(48_000.0, delay_us, LaneKind::Live);
                let n = 480;
                let (mut m, mut c) = (vec![0.0; n * 2], vec![0.0; n * 2]);
                let mut out = Vec::new();
                for i in 0..120i64 {
                    lane.render(&snap, delay_us + i * 10_000, &mut m, &mut c);
                    out.extend(c.iter().step_by(2));
                }
                out
            };
            let live = phones(0);
            let at = |ms: usize| ms * 48;
            let peak =
                |r: std::ops::Range<usize>| live[r].iter().fold(0.0f32, |m, v| m.max(v.abs()));
            let step = |r: std::ops::Range<usize>| {
                live[r]
                    .windows(2)
                    .map(|w| (w[1] - w[0]).abs())
                    .fold(0.0, f32::max)
            };
            assert!(peak(at(600)..at(890)) > 0.1, "key lock {key_lock}: silence");
            let (before, after) = (step(at(600)..at(890)), step(at(890)..at(960)));
            assert!(
                after < before * 2.0,
                "key lock {key_lock}: a click, {after} after {before}"
            );
            assert!(peak(at(960)..at(1200)) < 1e-3, "key lock {key_lock}");
            let late = phones(500_000);
            let diff = live
                .iter()
                .zip(&late)
                .map(|(x, y)| (x - y).abs())
                .fold(0.0, f32::max);
            assert!(diff < 1e-6, "key lock {key_lock}: lanes differ by {diff}");
        }
    }

    /// Render `secs` of the master from shared time `t0` through a fresh
    /// lane at 48 kHz, in 480-frame callbacks.
    fn master(snap: &AudioSnapshot, delay_us: i64, t0: i64, secs: f64) -> Vec<f32> {
        let mut lane = Lane::new(48_000.0, delay_us, LaneKind::Live);
        let n = 480;
        let mut out = Vec::new();
        let (mut m, mut c) = (vec![0.0; n * 2], vec![0.0; n * 2]);
        for i in 0..(secs * 100.0) as i64 {
            lane.render(snap, t0 + i * 10_000, &mut m, &mut c);
            out.extend_from_slice(&m);
        }
        out
    }

    fn fx_ev(time: i64, seq: u64, action: Action) -> Event {
        Event {
            time,
            origin: 2,
            seq,
            action,
        }
    }

    /// Deck A playing a 30 s tone from the start, on a 120 BPM grid.
    fn playing_tone() -> (Timeline, Arc<TrackAudio>) {
        let a = Arc::new(TrackAudio::new(
            TrackId([3; 32]),
            44_100,
            test_signal(44_100, 30.0, 440.0),
        ));
        let mut tl = Timeline::new(0);
        tl.insert(Event {
            time: 0,
            origin: 1,
            seq: 1,
            action: Action::Load {
                deck: 0,
                track: a.id,
                duration: a.duration(),
                cue: 0.0,
                grid: crate::grid::BeatGrid::new(0.0, 120.0).map(Box::new),
                key: -1,
            },
        });
        tl.insert(Event {
            time: 0,
            origin: 1,
            seq: 2,
            action: Action::Play { deck: 0 },
        });
        (tl, a)
    }

    #[test]
    fn effects_left_off_leave_the_audio_bit_identical() {
        let (plain, a) = playing_tone();
        // Every effect setting changed, but nothing switched on.
        let mut moved = plain.clone();
        let moves = [
            Action::FxDepth {
                deck: 0,
                slot: 0,
                v: 0.9,
            },
            Action::FxBeats { deck: 0, beats: 2 },
            Action::FxSelect {
                deck: 0,
                slot: 1,
                kind: crate::model::FxKind::Phaser,
            },
        ];
        for (i, m) in moves.into_iter().enumerate() {
            moved.insert(fx_ev(100_000 * (i as i64 + 1), i as u64 + 1, m));
        }
        let want = master(&snap_with(plain.clone(), a.clone()), 0, 0, 1.0);
        assert!(want.iter().any(|v| v.abs() > 0.1));
        assert_eq!(master(&snap_with(moved, a.clone()), 0, 0, 1.0), want);
        // An echo used and switched off: once its tail has died, the deck
        // is exactly what it would be without it.
        let mut used = plain.clone();
        used.insert(fx_ev(
            100_000,
            1,
            Action::FxOn {
                deck: 0,
                slot: 0,
                on: true,
            },
        ));
        used.insert(fx_ev(
            300_000,
            2,
            Action::FxOn {
                deck: 0,
                slot: 0,
                on: false,
            },
        ));
        let full = master(&snap_with(plain, a.clone()), 0, 0, 22.0);
        let after = master(&snap_with(used, a), 0, 0, 22.0);
        assert_ne!(after[48_000..96_000], full[48_000..96_000], "no echo");
        let tail = 20 * 48_000 * 2;
        assert_eq!(after[tail..], full[tail..]);
    }

    #[test]
    fn an_echo_rings_on_after_the_fader_closes() {
        // A click at 0.2 s on deck A, echoing every beat (0.5 s).
        let mut frames = vec![[0i16; 2]; 44_100 * 4];
        for f in &mut frames[8_820..8_830] {
            *f = [20_000; 2];
        }
        let a = Arc::new(TrackAudio::new(TrackId([4; 32]), 44_100, frames));
        let mut tl = Timeline::new(0);
        let mut seq = 0;
        let mut at = |time: i64, action: Action| {
            seq += 1;
            tl.insert(Event {
                time,
                origin: 1,
                seq,
                action,
            });
        };
        at(
            0,
            Action::Load {
                deck: 0,
                track: a.id,
                duration: a.duration(),
                cue: 0.0,
                grid: crate::grid::BeatGrid::new(0.0, 120.0).map(Box::new),
                key: -1,
            },
        );
        at(0, Action::Play { deck: 0 });
        at(
            0,
            Action::FxOn {
                deck: 0,
                slot: 0,
                on: true,
            },
        );
        // The fader closes, then the crossfader cuts the deck too, and the
        // echo is switched off: the tail rings on.
        at(300_000, Action::Volume { deck: 0, v: 0.0 });
        at(350_000, Action::Crossfader { v: 1.0 });
        at(
            400_000,
            Action::FxOn {
                deck: 0,
                slot: 0,
                on: false,
            },
        );
        let out = master(&snap_with(tl, a), 0, 0, 1.5);
        let peak = |from: f64, to: f64| {
            out[(from * 96_000.0) as usize..(to * 96_000.0) as usize]
                .iter()
                .fold(0.0f32, |m, v| m.max(v.abs()))
        };
        let click = peak(0.19, 0.25);
        assert!(click > 0.1, "{click}");
        assert!(
            peak(0.69, 0.75) > click * 0.1,
            "first echo {}",
            peak(0.69, 0.75)
        );
        assert!(
            peak(1.19, 1.25) > click * 0.03,
            "second echo {}",
            peak(1.19, 1.25)
        );
        // Between the echoes, nothing: the deck itself is shut.
        assert!(peak(0.8, 1.15) < click * 0.01);
    }

    #[test]
    fn two_lanes_hear_the_same_effects() {
        let (mut tl, a) = playing_tone();
        for (slot, seq) in [(0u8, 1u64), (1, 2), (2, 3)] {
            tl.insert(fx_ev(
                200_000,
                seq,
                Action::FxOn {
                    deck: 0,
                    slot,
                    on: true,
                },
            ));
        }
        tl.insert(fx_ev(
            600_000,
            4,
            Action::Tempo {
                deck: 0,
                tempo: 1.03,
            },
        ));
        tl.insert(fx_ev(
            900_000,
            5,
            Action::FxOn {
                deck: 0,
                slot: 0,
                on: false,
            },
        ));
        let snap = snap_with(tl, a);
        // The live lane and a broadcast lane half a second behind it.
        let live = master(&snap, 0, 0, 2.0);
        let late = master(&snap, 500_000, 500_000, 2.0);
        assert_eq!(live, late);
        let (plain, a) = playing_tone();
        assert_ne!(live, master(&snap_with(plain, a), 0, 0, 2.0));
    }

    #[test]
    fn chases_small_offsets_without_jumping() {
        let a = track();
        let mut tl = Timeline::new(0);
        tl.insert(Event {
            time: 0,
            origin: 1,
            seq: 1,
            action: Action::Load {
                deck: 0,
                track: a.id,
                duration: a.duration(),
                cue: 0.0,
                grid: None,
                key: -1,
            },
        });
        tl.insert(Event {
            time: 0,
            origin: 1,
            seq: 2,
            action: Action::Play { deck: 0 },
        });
        let mut snap = snap_with(tl.clone(), a.clone());
        let mut lane = Lane::new(48_000.0, 0, LaneKind::Live);
        let (mut m, mut c) = (vec![0.0; 960], vec![0.0; 960]);
        let mut t = 0;
        for _ in 0..10 {
            lane.render(&snap, t, &mut m, &mut c);
            t += 10_000;
        }
        let gen = lane.players[0].gen;
        // A 10 ms nudge: chased smoothly, no hard sync.
        tl.insert(Event {
            time: t,
            origin: 1,
            seq: 3,
            action: Action::Nudge {
                deck: 0,
                delta: 0.010,
            },
        });
        snap = snap_with(tl, a.clone());
        for _ in 0..100 {
            lane.render(&snap, t, &mut m, &mut c);
            t += 10_000;
        }
        assert_eq!(lane.players[0].gen, gen);
        let model = snap.timeline.state_at(t).decks[0].position_at(t) * 44_100.0;
        let err_ms = (model - lane.players[0].cur.heard()).abs() / 44.1;
        assert!(err_ms < 1.0, "still {err_ms} ms off after 1 s");
    }

    #[test]
    fn scratch_follows_the_hand_backwards() {
        let a = track();
        let mut tl = Timeline::new(0);
        let mut seq = 0;
        let mut push = |tl: &mut Timeline, t: i64, action: Action| {
            seq += 1;
            tl.insert(Event {
                time: t,
                origin: 1,
                seq,
                action,
            });
        };
        push(
            &mut tl,
            0,
            Action::Load {
                deck: 0,
                track: a.id,
                duration: a.duration(),
                cue: 0.0,
                grid: None,
                key: -1,
            },
        );
        push(&mut tl, 0, Action::Seek { deck: 0, pos: 5.0 });
        push(&mut tl, 0, Action::Play { deck: 0 });
        push(
            &mut tl,
            1_000_000,
            Action::JogTouch {
                deck: 0,
                down: true,
                spin: 0.0,
            },
        );
        // Drag the record back 0.3 s over 100 ms.
        for i in 0..20 {
            push(
                &mut tl,
                1_000_000 + i * 5_000,
                Action::Nudge {
                    deck: 0,
                    delta: -0.015,
                },
            );
        }
        let snap = snap_with(tl, a.clone());
        let mut lane = Lane::new(48_000.0, 0, LaneKind::Live);
        let (mut m, mut c) = (vec![0.0; 960], vec![0.0; 960]);
        let mut t = 900_000;
        let mut min_step = f64::MAX;
        while t < 1_300_000 {
            lane.render(&snap, t, &mut m, &mut c);
            min_step = min_step.min(lane.players[0].cur.step);
            t += 10_000;
        }
        assert!(
            min_step < -0.5,
            "never played backwards (min step {min_step})"
        );
        let pos_s = lane.players[0].cur.pos / 44_100.0;
        assert!(
            (pos_s - 5.7).abs() < 0.01,
            "ended at {pos_s} s, expected 5.7"
        );
    }

    #[test]
    fn a_record_let_go_mid_backspin_plays_on_then_settles() {
        let a = track();
        let mut tl = Timeline::new(0);
        let mut seq = 0;
        let mut push = |tl: &mut Timeline, t: i64, action: Action| {
            seq += 1;
            tl.insert(Event {
                time: t,
                origin: 1,
                seq,
                action,
            });
        };
        push(
            &mut tl,
            0,
            Action::Load {
                deck: 0,
                track: a.id,
                duration: a.duration(),
                cue: 0.0,
                grid: None,
                key: -1,
            },
        );
        push(&mut tl, 0, Action::Seek { deck: 0, pos: 6.0 });
        push(&mut tl, 0, Action::Play { deck: 0 });
        let touch = |down, spin| Action::JogTouch {
            deck: 0,
            down,
            spin,
        };
        push(&mut tl, 1_000_000, touch(true, 0.0));
        // Spin it back at 5.5x for 100 ms, and let go.
        for i in 0..20 {
            push(
                &mut tl,
                1_000_000 + i * 5_000,
                Action::Nudge {
                    deck: 0,
                    delta: -0.0275,
                },
            );
        }
        push(&mut tl, 1_100_000, touch(false, -5.5));
        let snap = snap_with(tl, a.clone());
        let mut lane = Lane::new(48_000.0, 0, LaneKind::Live);
        let (mut m, mut c) = (vec![0.0; 960], vec![0.0; 960]);
        let mut t = 900_000;
        let mut syncs = None;
        let mut steps = Vec::new();
        while t < 4_000_000 {
            lane.render(&snap, t, &mut m, &mut c);
            t += 10_000;
            let p = &lane.players[0];
            if t == 1_100_000 {
                syncs = Some(p.syncs);
            }
            if t > 1_100_000 {
                steps.push(p.cur.step);
            }

            let st = snap.timeline.state_at(t).decks[0];
            if t > 1_100_000 && st.spinning_at(t) {
                // While it runs on, the head plays it through: no jumps.
                assert_eq!(Some(p.syncs), syncs, "jumped at {t}");
                assert!(p.cur.playing);
            }
        }
        let speed = 44_100.0 / 48_000.0;
        // Straight after letting go it's still going back at about the
        // hand's speed, and it slows down gradually...
        assert!(
            steps[..5].iter().all(|&s| s < -4.0 * speed),
            "{:?}",
            &steps[..5]
        );
        assert!(steps[50] < 0.0 && steps[50] > steps[5]);
        let jerk = steps
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f64::max);
        assert!(jerk < 0.6, "speed jumped by {jerk} in 10 ms");
        // ...then plays on at normal speed, where the model says.
        assert!((steps.last().unwrap() - speed).abs() < 0.01);
        let model = snap.timeline.state_at(t).decks[0].position_at(t) * 44_100.0;
        let err_ms = (model - lane.players[0].cur.heard()).abs() / 44.1;
        assert!(err_ms < 2.0, "{err_ms} ms off");
    }

    /// Render deck A alone (crossfader hard left) for `secs` from shared time 0.
    fn render_deck(a: &Arc<TrackAudio>, key_lock: bool, tempo: f64, secs: f64) -> Vec<f32> {
        let mut tl = Timeline::new(-1_000_000);
        let mut seq = 0;
        let mut go = |tl: &mut Timeline, t: i64, action: Action| {
            seq += 1;
            tl.insert(Event {
                time: t,
                origin: 1,
                seq,
                action,
            });
        };
        go(
            &mut tl,
            -1_000_000,
            Action::Load {
                deck: 0,
                track: a.id,
                duration: a.duration(),
                cue: 0.0,
                grid: None,
                key: -1,
            },
        );
        go(&mut tl, -1_000_000, Action::Crossfader { v: 0.0 });
        go(
            &mut tl,
            -1_000_000,
            Action::KeyLock {
                deck: 0,
                on: key_lock,
            },
        );
        go(&mut tl, -1_000_000, Action::Tempo { deck: 0, tempo });
        go(&mut tl, 0, Action::Play { deck: 0 });
        let snap = snap_with(tl, a.clone());
        let mut lane = Lane::new(44_100.0, 0, LaneKind::Live);
        let block = 441;
        let mut out = Vec::new();
        let (mut m, mut c) = (vec![0.0; block * 2], vec![0.0; block * 2]);
        let mut t = 0i64;
        while (out.len() / 2) < (secs * 44_100.0) as usize {
            lane.render(&snap, t, &mut m, &mut c);
            out.extend(m.iter().step_by(2));
            out.extend(std::iter::repeat(0.0).take(0));
            t += 10_000;
        }
        out
    }

    /// Times (s) where the signal first jumps above `thresh` after quiet.
    fn onsets(x: &[f32], thresh: f32) -> Vec<f64> {
        let mut v = Vec::new();
        let mut quiet = usize::MAX / 2;
        for (i, s) in x.iter().enumerate() {
            if s.abs() > thresh {
                if quiet > 2000 {
                    v.push(i as f64 / 44_100.0);
                }
                quiet = 0;
            } else {
                quiet += 1;
            }
        }
        v
    }

    #[test]
    fn key_lock_keeps_kicks_on_time() {
        // Short clicks every 0.5 s.
        let frames: Vec<[i16; 2]> = (0..44_100 * 8)
            .map(|i| {
                let ph = i % 22_050;
                let v = if ph < 220 {
                    ((ph as f64 * 0.3).sin() * 20_000.0) as i16
                } else {
                    0
                };
                [v, v]
            })
            .collect();
        let a = Arc::new(TrackAudio::new(TrackId([5; 32]), 44_100, frames));
        for tempo in [1.0, 1.06] {
            let vinyl = onsets(&render_deck(&a, false, tempo, 4.0), 0.05);
            let locked = onsets(&render_deck(&a, true, tempo, 4.0), 0.05);
            assert!(vinyl.len() >= 6, "vinyl onsets {vinyl:?}");
            // The very first click (the cue point) must not be swallowed.
            assert!(
                locked[0] < 0.005,
                "first click came out at {:.1} ms",
                locked[0] * 1000.0
            );
            for (v, l) in vinyl.iter().zip(&locked) {
                let err_ms = (v - l).abs() * 1000.0;
                assert!(
                    err_ms < 4.0,
                    "tempo {tempo}: click at {v:.4}s vinyl vs {l:.4}s locked ({err_ms:.1} ms)"
                );
            }
        }
    }

    /// A track that arrives while its deck is playing (downloaded late, or
    /// after the link dropped) comes in where the room is, as if it had been
    /// there all along: on the live lane and on the broadcast lane, half a
    /// second behind. From a moment after it arrives (once the mixer's
    /// filters, which had silence, settle) it plays what a lane that had the
    /// track from the start plays, to within the model's microsecond. Key
    /// lock can't match sample for sample, since the stretcher's output
    /// depends on where it started, so there it's the timing: within a
    /// millisecond of the same track without key lock.
    #[test]
    fn a_track_that_arrives_late_comes_in_where_the_room_is() {
        let a = track();
        let block = 480; // 10 ms
        let arrives = 150; // 1.5 s into the track
                           // The left channel from 0.2 s after the track arrives to 3 s.
        let play = |kind: LaneKind, delay: i64, key_lock: bool, tempo: f64, arrives: usize| {
            let mut tl = Timeline::new(-1_000_000);
            for (seq, action) in [
                Action::Load {
                    deck: 0,
                    track: a.id,
                    duration: a.duration(),
                    cue: 0.0,
                    grid: None,
                    key: -1,
                },
                Action::Crossfader { v: 0.0 },
                Action::KeyLock {
                    deck: 0,
                    on: key_lock,
                },
                Action::Tempo { deck: 0, tempo },
            ]
            .into_iter()
            .enumerate()
            {
                tl.insert(Event {
                    time: -1_000_000,
                    origin: 1,
                    seq: seq as u64 + 1,
                    action,
                });
            }
            tl.insert(Event {
                time: 0,
                origin: 1,
                seq: 9,
                action: Action::Play { deck: 0 },
            });
            let with = snap_with(tl.clone(), a.clone());
            let without = AudioSnapshot {
                timeline: tl,
                tracks: Vec::new(),
                local: LocalParams::default(),
            };
            let mut lane = Lane::new(48_000.0, delay, kind);
            let (mut m, mut c) = (vec![0.0; block * 2], vec![0.0; block * 2]);
            let mut out: Vec<f32> = Vec::new();
            for k in 0..300 {
                let snap = if k < arrives { &without } else { &with };
                lane.render(snap, k as i64 * 10_000 + delay, &mut m, &mut c);
                if k < arrives {
                    assert!(m.iter().all(|v| *v == 0.0), "sound before the track");
                } else if k == arrives && arrives > 0 {
                    // It comes in straight away, after a 2 ms fade-in.
                    assert!(
                        m[200..].iter().any(|v| v.abs() > 0.01),
                        "{kind:?}, key lock {key_lock}: silent after the track arrived"
                    );
                }
                if k >= 170 {
                    out.extend(m.iter().step_by(2));
                }
            }
            out
        };
        // How many samples `x` lags `r`, and how well they match then.
        let lag = |r: &[f32], x: &[f32]| {
            let span = 2000..x.len() - 2000;
            let corr = |lag: i64| -> f64 {
                span.clone()
                    .map(|i| r[(i as i64 + lag) as usize] as f64 * x[i] as f64)
                    .sum()
            };
            let best = (-300..=300)
                .max_by(|a, b| corr(*a).total_cmp(&corr(*b)))
                .unwrap();
            let energy: f64 = span.clone().map(|i| (x[i] as f64).powi(2)).sum();
            (best, corr(best) / energy)
        };
        for (kind, delay) in [(LaneKind::Live, 0), (LaneKind::Broadcast, 500_000)] {
            let along = play(kind, delay, false, 1.04, 0);
            let late = play(kind, delay, false, 1.04, arrives);
            let worst = along
                .iter()
                .zip(&late)
                .map(|(x, y)| (x - y).abs())
                .fold(0.0f32, f32::max);
            // A microsecond is 0.05 frames, at most about 1e-3 here.
            assert!(worst < 1e-3, "{kind:?}: off by up to {worst}");

            let vinyl = play(kind, delay, false, 1.0, 0);
            let locked = play(kind, delay, true, 1.0, arrives);
            let (off, fit) = lag(&vinyl, &locked);
            assert!(fit > 0.6, "{kind:?}, key lock: doesn't match ({fit})");
            assert!(
                off.abs() <= 48,
                "{kind:?}, key lock: {off} samples out ({:.2} ms)",
                off as f64 / 48.0
            );
        }
    }

    #[test]
    fn key_lock_keeps_pitch() {
        let frames: Vec<[i16; 2]> = (0..44_100 * 4)
            .map(|i| {
                let v = ((2.0 * std::f64::consts::PI * 440.0 * i as f64 / 44_100.0).sin()
                    * 12_000.0) as i16;
                [v, v]
            })
            .collect();
        let a = Arc::new(TrackAudio::new(TrackId([6; 32]), 44_100, frames));
        let zero_crossings_hz = |x: &[f32]| {
            let seg = &x[22_050..66_150];
            let z = seg.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
            z as f64 / (seg.len() as f64 / 44_100.0)
        };
        let locked = zero_crossings_hz(&render_deck(&a, true, 1.08, 2.0));
        let vinyl = zero_crossings_hz(&render_deck(&a, false, 1.08, 2.0));
        assert!((locked - 440.0).abs() < 5.0, "key-locked pitch {locked} Hz");
        assert!((vinyl - 475.2).abs() < 5.0, "vinyl pitch {vinyl} Hz");
    }

    /// Deck A alone, playing from 0 at shared time 0, with `extra` events.
    /// Renders `secs` in 10 ms blocks and checks after each block (once
    /// settled) that the read head is where the model says, going round any
    /// loop. Returns the left channel and the player's hard-sync count.
    fn render_loop(
        a: &Arc<TrackAudio>,
        key_lock: bool,
        extra: &[(i64, Action)],
        secs: f64,
    ) -> (Vec<f32>, u32) {
        let mut tl = Timeline::new(-1_000_000);
        let mut seq = 0;
        let mut go = |tl: &mut Timeline, t: i64, action: Action| {
            seq += 1;
            tl.insert(Event {
                time: t,
                origin: 1,
                seq,
                action,
            });
        };
        go(
            &mut tl,
            -1_000_000,
            Action::Load {
                deck: 0,
                track: a.id,
                duration: a.duration(),
                cue: 0.0,
                grid: crate::grid::BeatGrid::new(0.0, 120.0).map(Box::new),
                key: -1,
            },
        );
        go(&mut tl, -1_000_000, Action::Crossfader { v: 0.0 });
        go(
            &mut tl,
            -1_000_000,
            Action::KeyLock {
                deck: 0,
                on: key_lock,
            },
        );
        go(&mut tl, 0, Action::Play { deck: 0 });
        for (t, action) in extra {
            go(&mut tl, *t, action.clone());
        }
        let snap = snap_with(tl, a.clone());
        let sr = 44_100.0;
        let mut lane = Lane::new(sr, 0, LaneKind::Live);
        let block = 441;
        let (mut m, mut c) = (vec![0.0; block * 2], vec![0.0; block * 2]);
        let mut out = Vec::new();
        let mut t = 0i64;
        while t < (secs * 1e6) as i64 {
            lane.render(&snap, t, &mut m, &mut c);
            out.extend(m.iter().step_by(2));
            t += 10_000;
            if t > 300_000 {
                // Events at exactly `t` reach the head in the next block.
                let st = snap.timeline.state_at(t - 1).decks[0];
                let p = &lane.players[0];
                let err = fold(
                    st.position_at(t) * sr - p.cur.heard(),
                    LoopSpan::of(&st, sr),
                );
                assert!(
                    err.abs() / sr < 0.002,
                    "{:.1} ms off at {t}",
                    err / sr * 1e3
                );
            }
        }
        (out, lane.players[0].syncs)
    }

    /// A sine that's a quarter cycle in at the loop's in point (1.0 s) and
    /// at -45° at its out point (1.5 s), so a plain jump back would click.
    fn sine_track() -> Arc<TrackAudio> {
        let frames: Vec<[i16; 2]> = (0..44_100 * 12)
            .map(|i| {
                let v = ((2.0 * std::f64::consts::PI * 207.25 * i as f64 / 44_100.0).sin()
                    * 16_000.0) as i16;
                [v, v]
            })
            .collect();
        Arc::new(TrackAudio::new(TrackId([7; 32]), 44_100, frames))
    }

    /// Biggest jump between neighbouring samples, ignoring the first `skip`.
    fn max_step(x: &[f32], skip: usize) -> f32 {
        x[skip..]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn loops_go_round_seamlessly() {
        let a = sine_track();
        // The steepest the sine itself ever gets, sample to sample.
        let natural = 2.0 * std::f32::consts::PI * 207.25 / 44_100.0 * 16_000.0 / 32_768.0;
        for key_lock in [false, true] {
            // A 1-beat loop (0.5 s, from the beat at 1.0 s), set while playing:
            // the out point is ahead, so the head goes round on its own.
            let (out, syncs) = render_loop(
                &a,
                key_lock,
                &[(
                    1_234_000,
                    Action::BeatLoop {
                        deck: 0,
                        beats: 1.0,
                    },
                )],
                4.0,
            );
            let limit = if key_lock { 1 + 1 } else { 1 };
            assert!(syncs <= limit, "key lock {key_lock}: {syncs} hard syncs");
            if !key_lock {
                let step = max_step(&out, 4410);
                assert!(
                    step < natural * 1.6,
                    "click going round: {step} vs {natural}"
                );
            }
        }
    }

    #[test]
    fn loop_out_at_the_playhead_and_exit() {
        let a = sine_track();
        for key_lock in [false, true] {
            render_loop(
                &a,
                key_lock,
                &[
                    (
                        1_000_000,
                        Action::LoopIn {
                            deck: 0,
                            quantize: false,
                        },
                    ),
                    (
                        1_730_000,
                        Action::LoopOut {
                            deck: 0,
                            quantize: false,
                        },
                    ),
                    (3_100_000, Action::LoopHalve { deck: 0 }),
                    (4_000_000, Action::Reloop { deck: 0 }),
                    (5_000_000, Action::Reloop { deck: 0 }),
                    (
                        5_500_000,
                        Action::BeatLoop {
                            deck: 0,
                            beats: 0.25,
                        },
                    ),
                    (6_500_000, Action::Reloop { deck: 0 }),
                ],
                7.5,
            );
        }
    }

    #[test]
    fn quantized_loop_out_pressed_late_goes_round() {
        // 120 BPM from 0. IN at 1.1 s snaps to 1.0; OUT 4.4 beats after
        // that makes a 4-beat loop that ends at 3.0 s, behind the playhead,
        // which has gone round to 1.2 s. The head follows it there.
        let a = sine_track();
        for key_lock in [false, true] {
            render_loop(
                &a,
                key_lock,
                &[
                    (
                        1_100_000,
                        Action::LoopIn {
                            deck: 0,
                            quantize: true,
                        },
                    ),
                    (
                        3_200_000,
                        Action::LoopOut {
                            deck: 0,
                            quantize: true,
                        },
                    ),
                    (6_000_000, Action::Reloop { deck: 0 }),
                ],
                7.0,
            );
        }
    }

    #[test]
    fn loop_rolls_go_round_and_let_go_onto_the_track() {
        let a = sine_track();
        let roll = |beats, on| Action::Roll { deck: 0, beats, on };
        let natural = 2.0 * std::f32::consts::PI * 207.25 / 44_100.0 * 16_000.0 / 32_768.0;
        let (plain, _) = render_loop(&a, false, &[], 4.5);
        for key_lock in [false, true] {
            let (out, syncs) = render_loop(
                &a,
                key_lock,
                &[
                    // Let go before it has gone round: nothing to hear.
                    (1_234_000, roll(1.0, true)),
                    (1_400_000, roll(1.0, false)),
                    // ½ beat, then ¼ from the same start (in step: no jump),
                    // then let go onto the track.
                    (2_100_000, roll(0.5, true)),
                    (2_600_000, roll(0.25, true)),
                    (3_300_000, roll(0.25, false)),
                ],
                4.5,
            );
            if key_lock {
                continue;
            }
            // Playing, and letting go: the only hard syncs.
            assert!(syncs <= 2, "{syncs} hard syncs");
            let at = |s: f64| (s * 44_100.0) as usize;
            let step = max_step(&out[..at(3.29)], 4410);
            assert!(step < natural * 1.6, "click while rolling: {step}");
            // Before the first roll went round, and after letting go, it's
            // the track as if it had never rolled.
            let same = |from: f64, to: f64| {
                (at(from)..at(to))
                    .map(|i| (out[i] - plain[i]).abs())
                    .fold(0.0, f32::max)
            };
            assert!(same(0.5, 2.1) < 2e-3, "{}", same(0.5, 2.1));
            assert!(same(3.4, 4.5) < 2e-3, "{}", same(3.4, 4.5));
            assert!(same(2.4, 3.2) > 0.1, "rolling sounds different");
        }
    }

    #[test]
    fn stream_clock_smooths_jitter() {
        let mut sc = StreamClock::new(48_000.0);
        let mut t_true = 1_000_000.0f64;
        let mut rng = fastrand::Rng::with_seed(1);
        let mut last = 0;
        for i in 0..500 {
            let jitter = rng.i64(-2000..2000);
            let got = sc.frame0(t_true as i64 + jitter, 480);
            if i > 200 {
                assert!(
                    (got - t_true as i64).abs() < 600,
                    "err {}",
                    got - t_true as i64
                );
            }
            last = got;
            t_true += 10_000.0;
        }
        assert!(last > 0);
    }
}
