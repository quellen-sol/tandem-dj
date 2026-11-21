//! Effects after a deck's fader and the crossfader: echo, reverb and phaser,
//! in a bank of three slots per deck that run in series (see
//! [`FxBank`](crate::model::FxBank)).
//!
//! Everything an effect does comes from the room state at the block being
//! rendered: the bank, the deck's BPM, its beatgrid and when the slot was
//! switched on. So every lane (each booth's speakers and the broadcast)
//! works the effect out the same way, whenever it started playing.
//!
//! Audio-thread rules: every buffer is allocated in [`FxChain::new`], and
//! nothing here allocates, locks or does unbounded work. Values in feedback
//! paths are flushed to zero once they're far below hearing, so they never
//! go denormal (which can make the CPU crawl). An effect that is off and
//! whose tail has died goes idle: it forgets its buffers, so old audio can
//! never come back, and costs nothing. With every effect idle, a deck's
//! signal goes through untouched, bit for bit.

use crate::model::{DeckState, FxKind, FX_SLOTS};

/// The most frames [`FxChain::run_block`] takes at once (it splits longer
/// blocks): the render lane's sub-block.
pub const FX_BLOCK: usize = 64;

/// Switching an effect on or off fades its input over this long, so it
/// never clicks. Switched off, it keeps running until its tail has died.
const FADE_S: f64 = 0.010;
/// Depth is smoothed over about this long, like the faders.
const SMOOTH_S: f64 = 0.010;
/// A tail this quiet is over (−120 dBFS).
const QUIET: f32 = 1e-6;
/// A value this small in a feedback path is flushed to zero. Denormals
/// start at about 1e-38.
const TINY: f32 = 1e-20;
/// Tempo for effects on a deck with no beatgrid (as for beat loops).
const NO_GRID_BPM: f64 = 120.0;

/// The longest echo. Longer settings halve until they fit.
const ECHO_MAX_S: f64 = 2.0;
/// A new echo time (a tempo change, another BEATS) crossfades in over this
/// long: no pitch glide, no click.
const ECHO_XFADE_S: f64 = 0.030;
/// Feedback at depth 0 and at full depth.
const ECHO_FEEDBACK: (f32, f32) = (0.35, 0.7);
/// The echoes' level at full depth (the dry sound stays at unity).
const ECHO_WET: f32 = 0.6;
/// Each time round, the repeats lose a little top and bottom.
const ECHO_LOWPASS_HZ: f64 = 7_000.0;
const ECHO_HIGHPASS_HZ: f64 = 90.0;

/// Freeverb's comb and all-pass lengths, in samples at 44.1 kHz (scaled to
/// the sample rate), and the right channel's offset.
const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASSES: [usize; 4] = [556, 441, 341, 225];
const STEREO_SPREAD: usize = 23;
const ALLPASS_FEEDBACK: f32 = 0.5;
/// How much the combs dull the tail each time round.
const DAMP: f32 = 0.2;
/// The reverb's input is high-passed here, so it doesn't boom.
const REVERB_HIGHPASS_HZ: f64 = 200.0;
/// The decay (60 dB) lasts 2 × BEATS beats, within these limits (seconds).
const REVERB_DECAY_S: (f64, f64) = (0.3, 10.0);
/// Input gain into the combs, the tail's level at half depth, and how much
/// more it comes up by full depth (as the dry sound goes). Measured on
/// noise with a 1-beat decay: at half depth, the tail is about 8 dB under
/// the dry sound (+0.6 dB in all); at full depth, alone, about 4 dB under it.
const REVERB_IN: f32 = 0.015;
const REVERB_WET: f32 = 1.3;
const REVERB_WET_OVER: f32 = 0.6;
/// The tail's level is evened out across decay times by this much at most
/// either way: a long decay builds up more under a sustained sound.
const REVERB_LEVEL_RANGE: (f32, f32) = (0.45, 1.6);
/// The decay the reverb's level is set for (seconds).
const REVERB_REF_DECAY_S: f64 = 1.0;

/// All-pass stages, the sweep's range, and feedback at full depth.
const PHASER_STAGES: usize = 6;
const PHASER_HZ: (f64, f64) = (200.0, 4_000.0);
const PHASER_FEEDBACK: f32 = 0.3;

#[inline]
fn flush(x: f32) -> f32 {
    if x.abs() < TINY {
        0.0
    } else {
        x
    }
}

/// One-pole smoothing coefficient for a time constant of `s` seconds.
fn smoothing(s: f64, sr: f64) -> f32 {
    (1.0 - (-1.0 / (s * sr)).exp()) as f32
}

/// One-pole low-pass coefficient for a corner at `hz`.
fn one_pole(hz: f64, sr: f64) -> f32 {
    (1.0 - (-2.0 * std::f64::consts::PI * hz / sr).exp()) as f32
}

/// An effect's input level: 1 while it's on, faded either way over
/// [`FADE_S`].
#[derive(Clone, Copy, Debug)]
struct Fade {
    now: f32,
    target: f32,
    step: f32,
}

impl Fade {
    fn new(sr: f64) -> Self {
        Self {
            now: 0.0,
            target: 0.0,
            step: (1.0 / (FADE_S * sr)) as f32,
        }
    }

    #[inline]
    fn next(&mut self) -> f32 {
        if self.now < self.target {
            self.now = (self.now + self.step).min(self.target);
        } else if self.now > self.target {
            self.now = (self.now - self.step).max(self.target);
        }
        self.now
    }

    /// Off, and done fading.
    fn off(&self) -> bool {
        self.now == 0.0 && self.target == 0.0
    }
}

/// A smoothed parameter (depth).
#[derive(Clone, Copy, Debug)]
struct Smooth {
    now: f32,
    target: f32,
    k: f32,
}

impl Smooth {
    fn new(v: f32, sr: f64) -> Self {
        Self {
            now: v,
            target: v,
            k: smoothing(SMOOTH_S, sr),
        }
    }

    #[inline]
    fn next(&mut self) -> f32 {
        if self.now != self.target {
            self.now += (self.target - self.now) * self.k;
            if (self.now - self.target).abs() < 1e-6 {
                self.now = self.target;
            }
        }
        self.now
    }
}

/// What an effect needs for one block, from the room.
#[derive(Clone, Copy, Debug)]
struct Params {
    /// It's the slot's effect, and the slot is on.
    fed: bool,
    depth: f32,
    /// BEATS at the deck's tempo, in seconds.
    beats_s: f64,
    /// The phaser's sweep (0..1) at the block's start and end.
    sweep: (f64, f64),
    /// Frames in the block.
    n: usize,
    /// The lane's first block: start where the room is, without fading.
    prime: bool,
}

/// Echo: the sound repeats every BEATS, each repeat a little quieter and
/// duller. Depth sets the echoes' level (the dry sound stays at unity) and
/// how long they go on: feedback rises from 0.35 to 0.7.
struct Echo {
    /// The delay line, a frame per sample, written at `pos`.
    buf: Vec<[f32; 2]>,
    pos: usize,
    /// Frames written since it was last cleared: older ones are silence.
    /// That makes clearing free, however long the line.
    written: usize,
    /// The echo time in frames, the one being crossfaded to (the same when
    /// there's no crossfade), how far that has got, and the room's.
    delay: usize,
    next: usize,
    xf: f32,
    xf_step: f32,
    target: usize,
    /// The repeats' filters: a low-pass, and the low end a high-pass takes off.
    lp: [f32; 2],
    lo: [f32; 2],
    lp_k: f32,
    hp_k: f32,
    fade: Fade,
    depth: Smooth,
    /// Frames in a row whose writes were all quiet.
    quiet: usize,
    idle: bool,
    sr: f64,
}

impl Echo {
    fn new(sr: f64) -> Self {
        let len = (ECHO_MAX_S * sr).ceil() as usize + 1;
        Self {
            buf: vec![[0.0; 2]; len],
            pos: 0,
            written: 0,
            delay: 1,
            next: 1,
            xf: 0.0,
            xf_step: (1.0 / (ECHO_XFADE_S * sr)) as f32,
            target: 1,
            lp: [0.0; 2],
            lo: [0.0; 2],
            lp_k: one_pole(ECHO_LOWPASS_HZ, sr),
            hp_k: one_pole(ECHO_HIGHPASS_HZ, sr),
            fade: Fade::new(sr),
            depth: Smooth::new(0.5, sr),
            quiet: 0,
            idle: true,
            sr,
        }
    }

    /// The echo time in frames for `beats_s`, halved until it fits.
    fn frames(&self, beats_s: f64) -> usize {
        let max = (self.buf.len() - 1) as f64;
        let mut d = (beats_s * self.sr).round().max(1.0);
        while d > max {
            d = (d / 2.0).round();
        }
        d as usize
    }

    fn prepare(&mut self, p: &Params) {
        self.target = self.frames(p.beats_s);
        if p.fed && self.idle {
            self.idle = false;
            self.depth.now = p.depth;
        }
        if self.written == 0 {
            // Nothing to crossfade from yet.
            (self.delay, self.next, self.xf) = (self.target, self.target, 0.0);
        }
        self.fade.target = if p.fed { 1.0 } else { 0.0 };
        self.depth.target = p.depth;
        if p.prime {
            self.fade.now = self.fade.target;
            self.depth.now = p.depth;
        }
    }

    /// The frame written `d` frames ago (silence if it wasn't).
    #[inline]
    fn tap(&self, d: usize) -> [f32; 2] {
        if d > self.written {
            return [0.0; 2];
        }
        let i = if self.pos >= d {
            self.pos - d
        } else {
            self.pos + self.buf.len() - d
        };
        self.buf[i]
    }

    #[inline]
    fn run(&mut self, x: [f32; 2]) -> [f32; 2] {
        let f = self.fade.next();
        let depth = self.depth.next();
        let mut e = self.tap(self.delay);
        if self.next != self.delay {
            let b = self.tap(self.next);
            let k = self.xf;
            e = [e[0] + (b[0] - e[0]) * k, e[1] + (b[1] - e[1]) * k];
            self.xf += self.xf_step;
            if self.xf >= 1.0 {
                (self.delay, self.xf) = (self.next, 0.0);
            }
        } else if self.target != self.delay {
            self.next = self.target;
        }
        let fb = ECHO_FEEDBACK.0 + (ECHO_FEEDBACK.1 - ECHO_FEEDBACK.0) * depth;
        let mut w = [0.0; 2];
        for c in 0..2 {
            self.lp[c] = flush(self.lp[c] + (e[c] - self.lp[c]) * self.lp_k);
            self.lo[c] = flush(self.lo[c] + (self.lp[c] - self.lo[c]) * self.hp_k);
            w[c] = flush(x[c] * f + (self.lp[c] - self.lo[c]) * fb);
        }
        let len = self.buf.len();
        self.buf[self.pos] = w;
        self.pos = if self.pos + 1 == len { 0 } else { self.pos + 1 };
        self.written = (self.written + 1).min(len);
        if w[0].abs() < QUIET && w[1].abs() < QUIET {
            self.quiet += 1;
        } else {
            self.quiet = 0;
        }
        let g = ECHO_WET * depth;
        [x[0] + e[0] * g, x[1] + e[1] * g]
    }

    fn run_block(&mut self, buf: &mut [[f32; 2]]) {
        for x in buf {
            *x = self.run(*x);
        }
    }

    /// Off, and everything the echo can still play is silent: go idle.
    fn settle(&mut self) {
        if !self.idle && self.fade.off() && self.quiet > self.delay.max(self.next) {
            self.idle = true;
            self.written = 0;
            self.quiet = 0;
            self.lp = [0.0; 2];
            self.lo = [0.0; 2];
            (self.delay, self.next, self.xf) = (self.target, self.target, 0.0);
        }
    }
}

/// A Freeverb comb: a delay whose output, a little dulled, feeds back.
struct Comb {
    buf: Vec<f32>,
    pos: usize,
    store: f32,
    feedback: f32,
}

impl Comb {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            pos: 0,
            store: 0.0,
            feedback: 0.0,
        }
    }

    /// Feedback for a 60 dB decay in `decay_s`.
    fn set_decay(&mut self, decay_s: f64, sr: f64) {
        let len_s = self.buf.len() as f64 / sr;
        self.feedback = 10f64.powf(-3.0 * len_s / decay_s) as f32;
    }

    /// Feed it `input`, adding what comes out to `acc`. The buffer is
    /// walked in runs up to its end, so the loop has no wrap or bounds
    /// checks. Only `store` needs flushing: with it at zero, what's written
    /// is the input itself.
    fn run_block(&mut self, input: &[f32], acc: &mut [f32]) {
        let len = self.buf.len();
        let (mut pos, mut store, fb) = (self.pos, self.store, self.feedback);
        let mut i = 0;
        while i < input.len() {
            let run = (input.len() - i).min(len - pos);
            let seg = &mut self.buf[pos..pos + run];
            for ((b, x), a) in seg
                .iter_mut()
                .zip(&input[i..i + run])
                .zip(&mut acc[i..i + run])
            {
                let out = *b;
                store = flush(out * (1.0 - DAMP) + store * DAMP);
                *b = x + store * fb;
                *a += out;
            }
            i += run;
            pos += run;
            if pos == len {
                pos = 0;
            }
        }
        (self.pos, self.store) = (pos, store);
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
        self.store = 0.0;
    }
}

/// A Freeverb all-pass: diffuses the combs' echoes into a smooth tail.
struct Allpass {
    buf: Vec<f32>,
    pos: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            pos: 0,
        }
    }

    /// Pass `io` through, in place.
    fn run_block(&mut self, io: &mut [f32]) {
        let len = self.buf.len();
        let mut pos = self.pos;
        let mut i = 0;
        while i < io.len() {
            let run = (io.len() - i).min(len - pos);
            let seg = &mut self.buf[pos..pos + run];
            for (b, x) in seg.iter_mut().zip(&mut io[i..i + run]) {
                let out = *b - *x;
                *b = flush(*x + *b * ALLPASS_FEEDBACK);
                *x = out;
            }
            i += run;
            pos += run;
            if pos == len {
                pos = 0;
            }
        }
        self.pos = pos;
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

/// A middling comb's power gain under a steady sound, for a decay (60 dB)
/// of `decay_s`.
fn comb_power(decay_s: f64) -> f64 {
    let len_s = 1366.0 / 44_100.0;
    let g = 10f64.powf(-3.0 * len_s / decay_s);
    1.0 / (1.0 - g * g)
}

/// Reverb (Freeverb's eight combs and four all-passes per channel, no
/// randomness). Up to half depth it adds reverb under the dry sound; above,
/// the dry sound fades out, to reverb alone at full depth. BEATS sets the
/// decay: 60 dB in 2 × BEATS beats.
struct Reverb {
    combs: [[Comb; COMBS.len()]; 2],
    allpasses: [[Allpass; ALLPASSES.len()]; 2],
    /// The input's high-pass (the low end it takes off).
    lo: f32,
    hp_k: f32,
    decay_s: f64,
    /// The tail's level for this decay.
    level: f32,
    fade: Fade,
    depth: Smooth,
    quiet: usize,
    /// Quiet frames in a row it takes to know the tail is over: long
    /// enough for anything left in the combs and all-passes to come out.
    quiet_for: usize,
    idle: bool,
    sr: f64,
}

impl Reverb {
    fn new(sr: f64) -> Self {
        let scale = |n: usize| ((n as f64) * sr / 44_100.0).round() as usize;
        let combs = std::array::from_fn(|c| {
            std::array::from_fn(|i| Comb::new(scale(COMBS[i] + c * STEREO_SPREAD)))
        });
        let allpasses = std::array::from_fn(|c| {
            std::array::from_fn(|i| Allpass::new(scale(ALLPASSES[i] + c * STEREO_SPREAD)))
        });
        let longest = scale(COMBS[COMBS.len() - 1] + STEREO_SPREAD);
        let diffusion: usize = ALLPASSES.iter().map(|&n| scale(n + STEREO_SPREAD)).sum();
        let mut r = Self {
            combs,
            allpasses,
            lo: 0.0,
            hp_k: one_pole(REVERB_HIGHPASS_HZ, sr),
            decay_s: 0.0,
            level: 1.0,
            fade: Fade::new(sr),
            depth: Smooth::new(0.5, sr),
            quiet: 0,
            quiet_for: 2 * (longest + diffusion),
            idle: true,
            sr,
        };
        r.set_decay(REVERB_REF_DECAY_S);
        r
    }

    fn set_decay(&mut self, decay_s: f64) {
        let decay_s = decay_s.clamp(REVERB_DECAY_S.0, REVERB_DECAY_S.1);
        if (decay_s - self.decay_s).abs() <= self.decay_s * 1e-3 {
            return;
        }
        self.decay_s = decay_s;
        let sr = self.sr;
        let mut power = 0.0;
        for comb in self.combs.iter_mut().flatten() {
            comb.set_decay(decay_s, sr);
            power += 1.0 / (1.0 - (comb.feedback as f64).powi(2));
        }
        // A comb's level under a steady sound grows with its decay: even
        // that out (within limits), so BEATS doesn't swing the level.
        let combs = (2 * COMBS.len()) as f64;
        let level = (comb_power(REVERB_REF_DECAY_S) / (power / combs)).sqrt() as f32;
        self.level = level.clamp(REVERB_LEVEL_RANGE.0, REVERB_LEVEL_RANGE.1);
    }

    fn prepare(&mut self, p: &Params) {
        self.set_decay(2.0 * p.beats_s);
        if p.fed && self.idle {
            self.idle = false;
            self.depth.now = p.depth;
        }
        self.fade.target = if p.fed { 1.0 } else { 0.0 };
        self.depth.target = p.depth;
        if p.prime {
            self.fade.now = self.fade.target;
            self.depth.now = p.depth;
        }
    }

    /// A block (at most [`FX_BLOCK`] frames) through the reverb, in place:
    /// the input first, then each comb and all-pass over the whole block.
    fn run_block(&mut self, buf: &mut [[f32; 2]]) {
        let n = buf.len().min(FX_BLOCK);
        let mut input = [0.0f32; FX_BLOCK];
        let mut gains = [(1.0f32, 0.0f32); FX_BLOCK];
        for ((x, inp), g) in buf.iter().zip(&mut input).zip(&mut gains) {
            let f = self.fade.next();
            let depth = self.depth.next();
            let m = (x[0] + x[1]) * 0.5 * f;
            self.lo = flush(self.lo + (m - self.lo) * self.hp_k);
            *inp = (m - self.lo) * REVERB_IN;
            // Up to half depth the tail comes up under the dry sound.
            // Above, the dry sound fades out (only while it's on), and the
            // tail comes up a little more, so full depth isn't much quieter.
            let over = (2.0 * depth - 1.0).max(0.0);
            let wet = (2.0 * depth).min(1.0) + REVERB_WET_OVER * over;
            *g = (1.0 - f * over, REVERB_WET * self.level * wet);
        }
        let mut loud = false;
        for c in 0..2 {
            let mut acc = [0.0f32; FX_BLOCK];
            for comb in &mut self.combs[c] {
                comb.run_block(&input[..n], &mut acc[..n]);
            }
            for ap in &mut self.allpasses[c] {
                ap.run_block(&mut acc[..n]);
            }
            for ((x, a), (dry, wet)) in buf.iter_mut().zip(&acc[..n]).zip(&gains) {
                loud |= a.abs() >= QUIET;
                x[c] = x[c] * dry + a * wet;
            }
        }
        self.quiet = if loud { 0 } else { self.quiet + n };
    }

    fn settle(&mut self) {
        if !self.idle && self.fade.off() && self.quiet > self.quiet_for {
            self.idle = true;
            self.quiet = 0;
            self.lo = 0.0;
            for comb in self.combs.iter_mut().flatten() {
                comb.clear();
            }
            for ap in self.allpasses.iter_mut().flatten() {
                ap.clear();
            }
        }
    }
}

/// Phaser: six all-pass stages swept between 200 Hz and 4 kHz, once every
/// BEATS, mixed with the dry sound to make moving notches. Depth is how
/// deep (0 is the dry sound exactly), with a little feedback for bite. The
/// sweep follows the beatgrid, so it's locked to the music. No tail.
struct Phaser {
    /// Each stage's state, per channel.
    z: [[f32; PHASER_STAGES]; 2],
    /// The chain's last output, fed back.
    last: [f32; 2],
    /// The stages' coefficient, and how much it moves per frame this block.
    coef: f32,
    coef_step: f32,
    fade: Fade,
    depth: Smooth,
    idle: bool,
    sr: f64,
}

impl Phaser {
    fn new(sr: f64) -> Self {
        Self {
            z: [[0.0; PHASER_STAGES]; 2],
            last: [0.0; 2],
            coef: 0.0,
            coef_step: 0.0,
            fade: Fade::new(sr),
            depth: Smooth::new(0.5, sr),
            idle: true,
            sr,
        }
    }

    /// The all-pass coefficient where the sweep is at `phase` (0..1): from
    /// the bottom of the range up and back, evenly in pitch.
    fn coef_at(&self, phase: f64) -> f32 {
        let up = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * phase).cos();
        let hz = PHASER_HZ.0 * (PHASER_HZ.1 / PHASER_HZ.0).powf(up);
        let t = (std::f64::consts::PI * hz / self.sr).tan();
        ((t - 1.0) / (t + 1.0)) as f32
    }

    fn prepare(&mut self, p: &Params) {
        let woke = p.fed && self.idle;
        if woke {
            self.idle = false;
            self.depth.now = p.depth;
        }
        if woke || p.prime {
            self.coef = self.coef_at(p.sweep.0);
        }
        self.coef_step = (self.coef_at(p.sweep.1) - self.coef) / p.n.max(1) as f32;
        self.fade.target = if p.fed { 1.0 } else { 0.0 };
        self.depth.target = p.depth;
        if p.prime {
            self.fade.now = self.fade.target;
            self.depth.now = p.depth;
        }
    }

    #[inline]
    fn run(&mut self, x: [f32; 2]) -> [f32; 2] {
        let f = self.fade.next();
        let depth = self.depth.next();
        let a = self.coef;
        self.coef += self.coef_step;
        let k = PHASER_FEEDBACK * depth;
        let mix = 0.5 * depth * f;
        let mut y = [0.0; 2];
        for c in 0..2 {
            let mut s = x[c] + self.last[c] * k;
            for z in &mut self.z[c] {
                let o = a * s + *z;
                *z = s - a * o;
                s = o;
            }
            self.last[c] = s;
            y[c] = x[c] + (s - x[c]) * mix;
        }
        y
    }

    fn run_block(&mut self, buf: &mut [[f32; 2]]) {
        for x in buf {
            *x = self.run(*x);
        }
    }

    fn settle(&mut self) {
        // Flushed once a block: a state can't get from here to a denormal
        // in one.
        for v in self.z.iter_mut().flatten().chain(self.last.iter_mut()) {
            if v.abs() < 1e-15 {
                *v = 0.0;
            }
        }
        if !self.idle && self.fade.off() {
            self.idle = true;
            self.z = [[0.0; PHASER_STAGES]; 2];
            self.last = [0.0; 2];
        }
    }
}

/// One slot: an instance of each effect. Changing the slot's effect feeds
/// another one, and the old one rings out.
struct Slot {
    echo: Echo,
    reverb: Reverb,
    phaser: Phaser,
}

impl Slot {
    fn awake(&self) -> bool {
        !(self.echo.idle && self.reverb.idle && self.phaser.idle)
    }
}

/// A deck's effects in one lane.
pub struct FxChain {
    slots: [Slot; FX_SLOTS],
    sr: f64,
    /// Some effect isn't idle: the deck's signal goes through the chain.
    busy: bool,
}

impl FxChain {
    /// Allocates everything the chain will ever need.
    pub fn new(sample_rate: f64) -> Self {
        Self {
            slots: std::array::from_fn(|_| Slot {
                echo: Echo::new(sample_rate),
                reverb: Reverb::new(sample_rate),
                phaser: Phaser::new(sample_rate),
            }),
            sr: sample_rate,
            busy: false,
        }
    }

    /// Whether the deck's signal has to go through [`FxChain::run`] this
    /// block. When it doesn't, it's exactly what it would be without effects.
    #[inline]
    pub fn busy(&self) -> bool {
        self.busy
    }

    /// Ready the chain for a block of `n` frames whose first is heard at
    /// shared time `ts`, from the deck as the room has it then. `prime`:
    /// the lane's first block, which starts where the room is (no fades).
    /// Costs nothing while every effect is off and idle.
    pub fn prepare(&mut self, st: &DeckState, ts: i64, n: usize, prime: bool) {
        let bank = &st.fx;
        if !self.busy && !bank.any_on() {
            return;
        }
        let beats = bank.beats();
        let beats_s = beats * 60.0 / deck_bpm(st, ts);
        let ts_end = ts + (n as f64 * 1e6 / self.sr) as i64;
        for (fx, slot) in self.slots.iter_mut().zip(&bank.slots) {
            let base = Params {
                fed: false,
                depth: slot.depth.clamp(0.0, 1.0),
                beats_s,
                sweep: (0.0, 0.0),
                n,
                prime,
            };
            let fed = |k: FxKind| slot.on && slot.kind == k;
            if !fx.echo.idle || fed(FxKind::Echo) {
                fx.echo.prepare(&Params {
                    fed: fed(FxKind::Echo),
                    ..base
                });
            }
            if !fx.reverb.idle || fed(FxKind::Reverb) {
                fx.reverb.prepare(&Params {
                    fed: fed(FxKind::Reverb),
                    ..base
                });
            }
            if !fx.phaser.idle || fed(FxKind::Phaser) {
                let sweep = (
                    sweep_phase(st, slot.since, ts, beats),
                    sweep_phase(st, slot.since, ts_end, beats),
                );
                fx.phaser.prepare(&Params {
                    fed: fed(FxKind::Phaser),
                    sweep,
                    ..base
                });
            }
        }
        self.busy = self.slots.iter().any(Slot::awake);
    }

    /// A block of frames through the slots, 1 → 2 → 3, in place. Only
    /// while [`busy`](Self::busy), and with blocks no longer than the one
    /// [`prepare`](Self::prepare) was told of.
    pub fn run_block(&mut self, buf: &mut [[f32; 2]]) {
        for block in buf.chunks_mut(FX_BLOCK) {
            for s in &mut self.slots {
                if !s.echo.idle {
                    s.echo.run_block(block);
                }
                if !s.reverb.idle {
                    s.reverb.run_block(block);
                }
                if !s.phaser.idle {
                    s.phaser.run_block(block);
                }
            }
        }
    }

    /// After a block: effects that are off and have rung out go idle.
    pub fn settle(&mut self) {
        if !self.busy {
            return;
        }
        for s in &mut self.slots {
            s.echo.settle();
            s.reverb.settle();
            s.phaser.settle();
        }
        self.busy = self.slots.iter().any(Slot::awake);
    }

    /// Every value the chain keeps from one frame to the next, for tests
    /// (NaNs, denormals).
    #[cfg(test)]
    fn state(&self) -> Vec<f32> {
        let mut v = Vec::new();
        for s in &self.slots {
            v.extend(s.echo.buf.iter().flatten());
            v.extend(s.echo.lp.iter().chain(&s.echo.lo));
            for c in s.reverb.combs.iter().flatten() {
                v.extend(&c.buf);
                v.push(c.store);
            }
            for a in s.reverb.allpasses.iter().flatten() {
                v.extend(&a.buf);
            }
            v.push(s.reverb.lo);
            v.extend(s.phaser.z.iter().flatten());
            v.extend(&s.phaser.last);
        }
        v
    }
}

/// The deck's BPM at `t` for timing its effects: from its beatgrid and
/// tempo, or 120 without a grid.
fn deck_bpm(st: &DeckState, t: i64) -> f64 {
    st.effective_bpm_at(t)
        .filter(|b| b.is_finite() && *b > 1.0)
        .unwrap_or(NO_GRID_BPM)
        .clamp(20.0, 999.0)
}

/// Where a phaser's sweep (one every `beats` beats) is at time `t`: from
/// the beatgrid, under a loop roll the track underneath, so it's locked to
/// the music and the same in every lane. Without a grid, from when the slot
/// was switched on (`since`), at 120 BPM.
fn sweep_phase(st: &DeckState, since: i64, t: i64, beats: f64) -> f64 {
    match st.grid.filter(|_| st.track.is_some()) {
        Some(g) => g.phase(st.slip_position_at(t), 1.0 / beats),
        None => ((t - since) as f64 * 1e-6 * NO_GRID_BPM / 60.0 / beats).rem_euclid(1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::BeatGrid;
    use crate::model::{FxBank, FxSlot, TrackId, FX_BEATS};

    /// A deck playing a track at `bpm` (beatgrid from 0 s), with `bank`.
    fn deck(bpm: f64, bank: FxBank) -> DeckState {
        DeckState {
            track: Some(TrackId([1; 32])),
            duration: 600.0,
            playing: true,
            grid: BeatGrid::new(0.0, bpm),
            fx: bank,
            ..DeckState::default()
        }
    }

    /// A bank with slot 0 holding `kind`, on, at `depth`, BEATS index `beats`.
    fn bank(kind: FxKind, depth: f32, beats: u8) -> FxBank {
        let mut b = FxBank::default();
        b.slots[0] = FxSlot {
            kind,
            on: true,
            depth,
            since: 0,
        };
        b.beats = beats;
        b
    }

    /// Run `input` through a chain at `sr` in 64-frame blocks, from shared
    /// time `t0`, with the deck as `at` has it at each block.
    fn render(
        chain: &mut FxChain,
        sr: f64,
        t0: i64,
        input: &[[f32; 2]],
        at: impl Fn(i64) -> DeckState,
    ) -> Vec<[f32; 2]> {
        let mut out = Vec::with_capacity(input.len());
        for (b, block) in input.chunks(64).enumerate() {
            let ts = t0 + ((b * 64) as f64 * 1e6 / sr) as i64;
            chain.prepare(&at(ts), ts, block.len(), b == 0);
            let mut y = block.to_vec();
            if chain.busy() {
                chain.run_block(&mut y);
            }
            out.extend(y);
            chain.settle();
        }
        out
    }

    fn impulse(frames: usize) -> Vec<[f32; 2]> {
        let mut v = vec![[0.0; 2]; frames];
        v[0] = [1.0, 1.0];
        v
    }

    fn noise(frames: usize, seed: u64) -> Vec<[f32; 2]> {
        let mut rng = fastrand::Rng::with_seed(seed);
        (0..frames)
            .map(|_| {
                let x = (rng.f32() - 0.5) * 0.5;
                [x, x]
            })
            .collect()
    }

    fn rms(x: &[[f32; 2]]) -> f64 {
        (x.iter().map(|f| (f[0] as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
    }

    /// The frame (from 0) where the loudest sample within `window` of
    /// `around` is, and its level.
    fn peak_near(out: &[[f32; 2]], around: usize, window: usize) -> (usize, f32) {
        let from = around.saturating_sub(window);
        out[from..(around + window).min(out.len())]
            .iter()
            .enumerate()
            .fold((0, 0.0f32), |best, (i, f)| {
                if f[0].abs() > best.1 {
                    (from + i, f[0].abs())
                } else {
                    best
                }
            })
    }

    fn energy(x: &[[f32; 2]]) -> f64 {
        x.iter().map(|f| (f[0] as f64).powi(2)).sum()
    }

    fn sine(frames: usize, hz: f64, sr: f64) -> Vec<[f32; 2]> {
        (0..frames)
            .map(|i| {
                let v = (i as f64 * 2.0 * std::f64::consts::PI * hz / sr).sin() as f32 * 0.5;
                [v, v]
            })
            .collect()
    }

    const RATES: [f64; 2] = [44_100.0, 48_000.0];

    #[test]
    fn an_echo_repeats_every_beats_and_dies_away() {
        for sr in RATES {
            // 1 beat at 120 BPM, ¾ beat at 128, ½ beat at 90 with the tempo up 4%.
            for (bpm, tempo, beats) in [(120.0, 1.0, 5u8), (128.0, 1.0, 4), (90.0, 1.04, 3)] {
                let mut st = deck(bpm, bank(FxKind::Echo, 0.8, beats));
                st.tempo = tempo;
                let period = FX_BEATS[beats as usize] * 60.0 / (bpm * tempo);
                let mut c = FxChain::new(sr);
                let out = render(&mut c, sr, 0, &impulse((sr * 3.0) as usize), |_| st);
                let p = (period * sr).round() as usize;
                let mut last = f64::MAX;
                for k in 1..4 {
                    let (at, level) = peak_near(&out, k * p, (sr * 0.05) as usize);
                    let late = (at as f64 - (k * p) as f64).abs() / sr;
                    assert!(
                        late < 0.001,
                        "{sr} {bpm}: repeat {k} at {at}, not {}",
                        k * p
                    );
                    assert!(level > 0.01, "repeat {k}: {level}");
                    // Each repeat quieter than the one before.
                    let e = energy(&out[k * p - p / 2..k * p + p / 2]);
                    assert!(e < last * 0.8, "repeat {k}: {e} after {last}");
                    last = e;
                }
            }
        }
    }

    #[test]
    fn an_echo_with_the_depth_down_is_the_dry_sound() {
        for kind in [FxKind::Echo, FxKind::Phaser] {
            let st = deck(120.0, bank(kind, 0.0, 5));
            let x = noise(48_000, 1);
            let mut c = FxChain::new(48_000.0);
            let out = render(&mut c, 48_000.0, 0, &x, |_| st);
            assert!(c.busy(), "{kind:?} is on");
            assert_eq!(out, x, "{kind:?}");
        }
        // Turning it down silences the echoes at once (within the 10 ms
        // smoothing), though they're still going round inside.
        let sr = 48_000.0;
        let on = deck(120.0, bank(FxKind::Echo, 1.0, 5));
        let mut down = on;
        down.fx.slots[0].depth = 0.0;
        let mut c = FxChain::new(sr);
        let out = render(&mut c, sr, 0, &impulse(48_000 * 2), |t| {
            if t < 600_000 {
                on
            } else {
                down
            }
        });
        assert!(peak_near(&out, 24_000, 100).1 > 0.3);
        let after = &out[(0.65 * sr) as usize..];
        assert!(after.iter().all(|f| f[0].abs() < 1e-3), "still echoing");
    }

    #[test]
    fn an_echos_tail_rings_out_after_switching_off() {
        let sr = 48_000.0;
        let on = deck(120.0, bank(FxKind::Echo, 0.6, 5));
        let mut off = on;
        off.fx.slots[0].on = false;
        let mut c = FxChain::new(sr);
        // Off 0.1 s after the sound, long before the first echo.
        let out = render(&mut c, sr, 0, &impulse(48_000 * 20), |t| {
            if t < 100_000 {
                on
            } else {
                off
            }
        });
        for k in 1..4 {
            assert!(
                peak_near(&out, k * 24_000, 50).1 > 0.003,
                "echo {k} missing"
            );
        }
        // Once the tail has died, the echo goes idle: exact silence.
        assert!(!c.busy());
        let quiet = out.iter().rposition(|f| f[0] != 0.0).unwrap();
        assert!(quiet < out.len() / 2, "still sounding at {quiet}");
        // And it starts from nothing next time: what it held is gone.
        let mut fresh = FxChain::new(sr);
        let x = noise(48_000 * 2, 9);
        let again = render(&mut c, sr, 30_000_000, &x, |_| on);
        let first = render(&mut fresh, sr, 30_000_000, &x, |_| on);
        assert_eq!(again, first);
    }

    #[test]
    fn a_tempo_change_moves_the_echo_without_a_click() {
        let sr = 48_000.0;
        let slow = deck(120.0, bank(FxKind::Echo, 1.0, 5));
        let mut fast = slow;
        fast.tempo = 1.05;
        // The sound stops just before the tempo moves, at 1 s, so all the
        // output after that is echoes being moved to the new time.
        let mut x = sine((sr * 3.0) as usize, 220.0, sr);
        for f in &mut x[(0.98 * sr) as usize..] {
            *f = [0.0; 2];
        }
        let at = |t: i64| if t < 1_000_000 { slow } else { fast };
        let mut c = FxChain::new(sr);
        let out = render(&mut c, sr, 0, &x, at);
        let step = |w: &[[f32; 2]]| {
            w.windows(2)
                .map(|p| (p[1][0] - p[0][0]).abs())
                .fold(0.0, f32::max)
        };
        // A sine through a delay steps at most this much per frame.
        let steady = step(&out[(0.6 * sr) as usize..(0.97 * sr) as usize]);
        let moved = step(&out[sr as usize..(1.2 * sr) as usize]);
        assert!(moved <= steady * 1.05, "click: {moved} vs {steady}");
        // A sound after the change echoes at the new time.
        let mut c = FxChain::new(sr);
        let mut x = vec![[0.0; 2]; (sr * 3.0) as usize];
        x[(1.5 * sr) as usize] = [1.0; 2];
        let out = render(&mut c, sr, 0, &x, at);
        let p = (0.5 / 1.05 * sr).round() as usize;
        let (got, _) = peak_near(&out, (1.5 * sr) as usize + p, 2_000);
        assert!(got.abs_diff((1.5 * sr) as usize + p) <= 48, "echo at {got}");
    }

    #[test]
    fn long_echoes_halve_until_they_fit() {
        let c = FxChain::new(48_000.0);
        let e = &c.slots[0].echo;
        assert_eq!(e.frames(1.5), 72_000);
        assert_eq!(e.frames(2.0), 96_000);
        // 32 beats at 120 BPM is 16 s: 2 s.
        assert_eq!(e.frames(16.0), 96_000);
        assert_eq!(e.frames(3.0), 72_000);
        assert_eq!(e.frames(0.0), 1);
    }

    /// How long (s) the reverb's tail takes to fall 60 dB, from a sound
    /// around 300 Hz – 1.5 kHz (Schroeder's backward-integrated decay,
    /// from −5 to −35 dB), and how long until it's silent after being
    /// switched off where the sound stops.
    fn reverb_decay(sr: f64, beats: u8) -> (f64, Option<f64>) {
        let on = deck(120.0, bank(FxKind::Reverb, 1.0, beats));
        let mut off = on;
        off.fx.slots[0].on = false;
        let stop = sr as usize;
        let mut x = noise((sr * 26.0) as usize, 8);
        let (k, mut lp) = (one_pole(1_500.0, sr), 0.0);
        for (i, f) in x.iter_mut().enumerate() {
            lp += (f[0] - lp) * k;
            let v = if i < stop { lp * 4.0 } else { 0.0 };
            *f = [v, v];
        }
        let mut c = FxChain::new(sr);
        let out = render(&mut c, sr, 0, &x, |t| if t < 1_000_000 { on } else { off });
        let tail = &out[stop..];
        let mut edc: Vec<f64> = tail.iter().map(|f| (f[0] as f64).powi(2)).collect();
        for i in (0..edc.len() - 1).rev() {
            edc[i] += edc[i + 1];
        }
        let db = |e: f64| 10.0 * (e / edc[0]).log10();
        let at = |lvl: f64| edc.iter().position(|&e| db(e) < lvl).unwrap() as f64 / sr;
        let rt60 = (at(-35.0) - at(-5.0)) * 2.0;
        let silent = tail
            .iter()
            .rposition(|f| f[0] != 0.0)
            .map(|i| i as f64 / sr);
        assert!(!c.busy());
        (rt60, silent)
    }

    #[test]
    fn reverb_decays_at_about_the_stated_rate_then_goes_silent() {
        for sr in RATES {
            // 2 × BEATS beats at 120 BPM, clamped to 0.3 – 10 s.
            for (beats, want) in [(1u8, 0.3), (5, 1.0), (7, 4.0), (10, 10.0)] {
                let (rt60, silent) = reverb_decay(sr, beats);
                assert!(
                    (rt60 / want - 1.0).abs() < 0.15,
                    "{sr} beats {}: {rt60:.2} s, not {want}",
                    FX_BEATS[beats as usize]
                );
                // Exact silence once the tail is 120 dB down, a little
                // more than twice the time it takes to fall 60.
                let silent = silent.unwrap();
                assert!(silent < want * 2.3 + 0.3, "{sr}: sounding {silent:.2} s");
            }
        }
    }

    #[test]
    fn phasers_started_at_different_times_agree_once_settled() {
        let sr = 48_000.0;
        let x = noise((sr * 4.0) as usize, 5);
        let mut no_grid = deck(120.0, bank(FxKind::Phaser, 0.9, 6));
        no_grid.grid = None;
        no_grid.fx.slots[0].since = 300_000;
        for st in [deck(123.0, bank(FxKind::Phaser, 0.9, 6)), no_grid] {
            let mut a = FxChain::new(sr);
            let out_a = render(&mut a, sr, 0, &x, |_| st);
            // Starting 0.7 s and 1001 frames later, out of step with the
            // first lane's blocks.
            let skip = (0.7 * sr) as usize + 1001;
            let t0 = (skip as f64 * 1e6 / sr) as i64;
            let mut b = FxChain::new(sr);
            let out_b = render(&mut b, sr, t0, &x[skip..], |_| st);
            let settle = (0.1 * sr) as usize;
            let diff = out_a[skip + settle..]
                .iter()
                .zip(&out_b[settle..])
                .map(|(p, q)| (p[0] - q[0]).abs())
                .fold(0.0, f32::max);
            assert!(diff < 1e-3, "lanes differ by {diff}");
            // And it does something: notches move through the sound.
            assert!((rms(&out_a) / rms(&x) - 1.0).abs() > 0.05);
        }
    }

    #[test]
    fn the_phasers_sweep_follows_the_beatgrid() {
        let st = deck(120.0, bank(FxKind::Phaser, 1.0, 5));
        // One sweep a beat: the same place every half second of track.
        let at = |t| sweep_phase(&st, 0, t, 1.0);
        assert!((at(250_000) - 0.5).abs() < 1e-9);
        assert!((at(1_250_000) - 0.5).abs() < 1e-9);
        // Paused, the track doesn't move, and neither does the sweep.
        let mut paused = st;
        paused.playing = false;
        assert_eq!(
            sweep_phase(&paused, 0, 0, 1.0),
            sweep_phase(&paused, 0, 9_000_000, 1.0)
        );
        // Without a grid: from when it was switched on, at 120 BPM.
        let mut free = st;
        free.grid = None;
        assert!((sweep_phase(&free, 1_000_000, 1_250_000, 1.0) - 0.5).abs() < 1e-9);
        assert!((sweep_phase(&free, 1_000_000, 1_250_000, 2.0) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn switching_effect_lets_the_old_one_ring_out() {
        let sr = 48_000.0;
        let echo = deck(120.0, bank(FxKind::Echo, 0.8, 5));
        let mut phaser = echo;
        phaser.fx.slots[0].kind = FxKind::Phaser;
        let mut c = FxChain::new(sr);
        let out = render(&mut c, sr, 0, &impulse(48_000 * 2), |t| {
            if t < 100_000 {
                echo
            } else {
                phaser
            }
        });
        assert!(peak_near(&out, 24_000, 200).1 > 0.05, "the echo was cut");
        assert!(!c.slots[0].phaser.idle && !c.slots[0].echo.idle);
    }

    /// Echo, reverb and phaser on, a burst of noise, then a minute of
    /// silence: nothing in the chain is NaN or denormal, and switched off
    /// they all go quiet.
    #[test]
    fn a_minute_of_silence_leaves_no_nan_or_denormal() {
        for sr in RATES {
            let mut st = deck(126.0, FxBank::default());
            for s in &mut st.fx.slots {
                s.on = true;
                s.depth = 1.0;
            }
            st.fx.beats = 7;
            let mut x = noise((sr * 61.0) as usize, 2);
            for f in &mut x[sr as usize..] {
                *f = [0.0; 2];
            }
            let mut c = FxChain::new(sr);
            let out = render(&mut c, sr, 0, &x, |_| st);
            assert!(out.iter().flatten().all(|v| v.is_finite()));
            let bad = c
                .state()
                .into_iter()
                .filter(|v| !v.is_finite() || v.is_subnormal())
                .count();
            assert_eq!(bad, 0, "{sr}");
            assert!(c.busy(), "on, so still running");
            let mut off = st;
            for s in &mut off.fx.slots {
                s.on = false;
            }
            // A full-depth echo four beats long takes about a minute to
            // fall 120 dB; then it's idle, and exactly silent.
            let mut t = (61.0 * 1e6) as i64;
            let second = vec![[0.0; 2]; sr as usize];
            let mut left = 0;
            while c.busy() {
                assert!(left < 120, "{sr}: still running after {left} s");
                render(&mut c, sr, t, &second, |_| off);
                (t, left) = (t + 1_000_000, left + 1);
            }
            let out = render(&mut c, sr, t, &second, |_| off);
            assert!(out.iter().flatten().all(|&v| v == 0.0));
            let bad = c
                .state()
                .into_iter()
                .filter(|v| !v.is_finite() || v.is_subnormal())
                .count();
            assert_eq!(bad, 0, "{sr}");
        }
    }

    /// Effects add energy to a mix that may already reach the master's soft
    /// clipper, so their levels stay modest: at full depth on loud noise,
    /// none raises the level by more than 2 dB.
    #[test]
    fn effects_keep_their_levels_modest() {
        let sr = 48_000.0;
        for kind in FxKind::ALL {
            for beats in [2u8, 5, 8, 10] {
                let mut c = FxChain::new(sr);
                let st = deck(124.0, bank(kind, 1.0, beats));
                let x = noise((sr * 12.0) as usize, 3);
                let out = render(&mut c, sr, 0, &x, |_| st);
                let tail = (sr * 8.0) as usize;
                let db = 20.0 * (rms(&out[tail..]) / rms(&x[tail..])).log10();
                assert!(
                    db < 2.0,
                    "{kind:?} beats {}: {db:+.1} dB",
                    FX_BEATS[beats as usize]
                );
            }
        }
    }

    #[test]
    fn an_idle_chain_costs_nothing_and_changes_nothing() {
        let mut c = FxChain::new(48_000.0);
        let st = deck(120.0, FxBank::default());
        let x = noise(4_800, 4);
        assert_eq!(render(&mut c, 48_000.0, 0, &x, |_| st), x);
        assert!(!c.busy());
        assert!(c.slots.iter().all(|s| !s.awake()));
    }
}
