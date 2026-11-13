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

use crate::audio::TrackAudio;
use crate::model::{DeckState, RoomState, Timeline, TrackId, DECKS};
use crate::stretch::Stretcher;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Settings that belong to one booth and are never shared: your headphones
/// are yours.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalParams {
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

/// Copy track frames `[start, start + n)` as interleaved f32 (silence outside the track).
fn fill(audio: &TrackAudio, start: i64, buf: &mut [f32]) {
    for (i, c) in buf.chunks_exact_mut(2).enumerate() {
        let idx = start + i as i64;
        let f = if idx >= 0 && (idx as usize) < audio.frames.len() {
            audio.frames[idx as usize]
        } else {
            [0, 0]
        };
        c[0] = f[0] as f32 * (1.0 / 32768.0);
        c[1] = f[1] as f32 * (1.0 / 32768.0);
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
        );
        self.stretch.seek(&mut self.inbuf[..SEEK_FRAMES * 2], step);
        let out_lat = self.stretch.output_latency().min(HEAD_BUF_FRAMES / 2);
        let pre_in = ((out_lat as f64 * step).round() as usize).min(HEAD_BUF_FRAMES);
        fill(a, feed as i64, &mut self.inbuf[..pre_in * 2]);
        self.stretch.process(
            &mut self.inbuf[..pre_in * 2],
            &mut self.outbuf[..out_lat * 2],
        );
        self.pos = feed + pre_in as f64;
    }

    fn render(&mut self, audio: Option<&TrackAudio>, out: &mut [[f32; 2]]) {
        let (true, Some(a)) = (self.playing, audio) else {
            out.fill([0.0; 2]);
            return;
        };
        if !self.stretched {
            for o in out.iter_mut() {
                *o = a.sample(self.pos);
                self.pos += self.step;
            }
            return;
        }
        let n = out.len();
        let want = self.carry + n as f64 * self.step.max(0.0);
        let in_frames = (want.floor() as usize).min(HEAD_BUF_FRAMES);
        self.carry = want - in_frames as f64;
        fill(a, self.pos as i64, &mut self.inbuf[..in_frames * 2]);
        self.stretch
            .process(&mut self.inbuf[..in_frames * 2], &mut self.outbuf[..n * 2]);
        self.pos += in_frames as f64;
        for (i, o) in out.iter_mut().enumerate() {
            *o = [self.outbuf[i * 2], self.outbuf[i * 2 + 1]];
        }
    }
}

struct Player {
    track: Option<TrackId>,
    has_audio: bool,
    gen: u64,
    cur: Head,
    /// The previous head, faded out after a jump.
    old: Head,
    xfade: u32,
    old_buf: [[f32; 2]; SUB],
    corr: f64,
    scratching: bool,
    /// Velocity (source frames per output frame), eased while scratching.
    vel: f64,
}

impl Player {
    fn new(sr: f64) -> Self {
        Self {
            track: None,
            has_audio: false,
            gen: 0,
            cur: Head::new(sr),
            old: Head::new(sr),
            xfade: 0,
            old_buf: [[0.0; 2]; SUB],
            corr: 0.0,
            scratching: false,
            vel: 0.0,
        }
    }

    fn prepare(&mut self, st: &DeckState, ts: i64, out_sr: f64, audio: Option<&TrackAudio>) {
        let track_sr = audio.map(|a| a.sample_rate as f64).unwrap_or(out_sr);
        let model_pos = st.position_at(ts);
        let target = model_pos * track_sr;
        let has_audio = audio.is_some();
        let scratching = st.scratching && has_audio;
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
        if changed {
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
            // Vinyl: the read head follows the hand, forwards or backwards,
            // with eased velocity so grabbing and releasing sound natural.
            let err = target - self.cur.pos;
            if err.abs() > 2.0 * track_sr {
                self.hard_sync(st, target, true, has_audio, false, 0.0, 1.0, audio);
            }
            if !self.cur.playing {
                self.old.playing = false;
                self.xfade = XFADE;
                self.cur.playing = true;
            }
            let max = SCRATCH_MAX_SPEED * track_sr / out_sr;
            let want = ((target - self.cur.pos) / (SCRATCH_TAU_S * out_sr)).clamp(-max, max);
            self.vel += (want - self.vel) * 0.35;
            self.cur.step = self.vel;
            self.scratching = true;
            self.corr = 0.0;
            return;
        }
        self.scratching = false;

        if !changed {
            if want_playing {
                let err = target - self.cur.heard();
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
/// never clicks.
struct DjFilter {
    lp: Biquad,
    hp: Biquad,
    last: f32,
}

impl DjFilter {
    fn new(sr: f64) -> Self {
        let mut f = Self {
            lp: Biquad::default(),
            hp: Biquad::default(),
            last: -1.0,
        };
        f.update(0.5, sr);
        f
    }

    fn update(&mut self, k: f32, sr: f64) {
        if (k - self.last).abs() < 1e-4 {
            return;
        }
        self.last = k;
        let k = k as f64;
        let lp_fc = if k < 0.5 {
            20_000.0 * (60.0f64 / 20_000.0).powf((0.5 - k) / 0.5)
        } else {
            20_000.0
        };
        let hp_fc = if k > 0.5 {
            20.0 * (8_000.0f64 / 20.0).powf((k - 0.5) / 0.5)
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
        let y = self.lp.run(x);
        self.hp.run(y)
    }
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

/// Constant-power crossfader. Deck A is on the left.
pub fn xfader_gain(deck: usize, x: f32) -> f32 {
    let a = x.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
    if deck == 0 {
        a.cos()
    } else {
        a.sin()
    }
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
    deck_buf: [[[f32; 2]; SUB]; DECKS],
    vol: [f32; DECKS],
    trim: [f32; DECKS],
    eqg: [[f32; 3]; DECKS],
    smooth: f32,
    primed: bool,
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
            deck_buf: [[[0.0; 2]; SUB]; DECKS],
            vol: [0.0; DECKS],
            trim: [1.0; DECKS],
            eqg: [[1.0; 3]; DECKS],
            // ~10 ms parameter smoothing.
            smooth: (1.0 - (-1.0 / (0.010 * sample_rate)).exp()) as f32,
            primed: false,
        }
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

        let mut f = 0;
        while f < frames {
            let n = SUB.min(frames - f);
            let ts = lane_t0 + (f as f64 * us_per_frame) as i64;
            tl.advance(&mut state, &mut idx, ts);
            self.render_block(snap, &state, ts, n);

            let targets = self.targets(&state);
            if !self.primed {
                self.vol = targets.0;
                self.eqg = targets.1;
                self.trim = targets.2;
                self.primed = true;
            }
            for d in 0..DECKS {
                self.filt[d].update(state.decks[d].filter, self.sr);
            }
            for i in 0..n {
                let mut m = [0.0f32; 2];
                let mut c = [0.0f32; 2];
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
                    m[0] += s[0] * self.vol[d];
                    m[1] += s[1] * self.vol[d];
                    if local.pfl[d] {
                        c[0] += s[0];
                        c[1] += s[1];
                    }
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
                let mix = local.cue_mix;
                cue[o] = soft_clip((c[0] * (1.0 - mix) + m[0] * mix) * hp + click);
                cue[o + 1] = soft_clip((c[1] * (1.0 - mix) + m[1] * mix) * hp + click);
            }
            f += n;
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
            vol[d] = fader_gain(st.volume) * xfader_gain(d, state.crossfader);
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

    fn track() -> Arc<TrackAudio> {
        Arc::new(TrackAudio::new(
            TrackId([1; 32]),
            44_100,
            test_signal(44_100, 10.0, 440.0),
        ))
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
