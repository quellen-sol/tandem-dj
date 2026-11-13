//! BPM and beatgrid detection.
//!
//! 1. **Onset envelope.** The track is split into low (kick), mid and high
//!    (clap/hat) bands. For each ~6 ms hop we take the band energies and
//!    keep only increases ("spectral flux"), weighted toward the kick.
//! 2. **Tempo candidates.** Autocorrelation of that envelope over 60–200
//!    BPM, with a gentle preference for dance tempos so a half-time groove
//!    isn't reported at half its BPM.
//! 3. **Fit the whole track.** For each candidate (and its double/half),
//!    a comb of beat ticks is slid across the entire track; the BPM and
//!    phase that land on the most onsets win. Using the whole track makes
//!    the BPM precise to about 0.01, which is what sync needs.
//!
//! Assumes a steady tempo, like most DJ music.

use crate::audio::TrackAudio;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BeatGrid {
    pub bpm: f64,
    /// Position (seconds) of the first beat; later beats are every 60/bpm s.
    pub first_beat: f64,
}

impl BeatGrid {
    pub fn beat_len(&self) -> f64 {
        60.0 / self.bpm
    }
}

const HOP: usize = 256;

struct Lp {
    a: f32,
    y: f32,
}

impl Lp {
    fn new(fc: f32, sr: f32) -> Self {
        Self {
            a: 1.0 - (-2.0 * std::f32::consts::PI * fc / sr).exp(),
            y: 0.0,
        }
    }
    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        self.y += self.a * (x - self.y);
        self.y
    }
}

/// Onset strength per hop, and hops per second.
fn onset_envelope(audio: &TrackAudio) -> (Vec<f32>, f64) {
    let sr = audio.sample_rate as f32;
    let n = audio.frames.len() / HOP;
    let (mut lo1, mut lo2) = (Lp::new(150.0, sr), Lp::new(150.0, sr));
    let (mut mid1, mut mid2) = (Lp::new(3000.0, sr), Lp::new(3000.0, sr));
    let mut bands = vec![[0f32; 3]; n];
    for (h, band) in bands.iter_mut().enumerate() {
        let mut e = [0f32; 3];
        for f in &audio.frames[h * HOP..(h + 1) * HOP] {
            let x = (f[0] as f32 + f[1] as f32) * (0.5 / 32768.0);
            let low = lo2.run(lo1.run(x));
            let below3k = mid2.run(mid1.run(x));
            let mid = below3k - low;
            let high = x - below3k;
            e[0] += low * low;
            e[1] += mid * mid;
            e[2] += high * high;
        }
        *band = e;
    }
    // Normalise each band by its average so loudness doesn't matter.
    let mut mean = [1e-12f32; 3];
    for b in &bands {
        for i in 0..3 {
            mean[i] += b[i];
        }
    }
    for m in &mut mean {
        *m /= n.max(1) as f32;
    }
    let weights = [1.0f32, 0.6, 0.5];
    let mut flux = vec![0f32; n];
    for h in 1..n {
        let mut s = 0.0;
        for i in 0..3 {
            let a = (1.0 + 10.0 * bands[h][i] / mean[i]).ln();
            let b = (1.0 + 10.0 * bands[h - 1][i] / mean[i]).ln();
            s += weights[i] * (a - b).max(0.0);
        }
        flux[h] = s;
    }
    // Remove the slowly varying part (~0.25 s window) so only transients remain.
    let fps = sr as f64 / HOP as f64;
    let w = (fps * 0.25) as usize;
    let mut prefix = vec![0f64; n + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i] + flux[i] as f64;
    }
    let env = (0..n)
        .map(|i| {
            let a = i.saturating_sub(w);
            let b = (i + w + 1).min(n);
            let local = (prefix[b] - prefix[a]) / (b - a) as f64;
            (flux[i] as f64 - local).max(0.0) as f32
        })
        .collect();
    (env, fps)
}

#[inline]
fn sample(env: &[f32], pos: f64) -> f32 {
    let i = pos.floor() as usize;
    if i + 1 >= env.len() {
        return 0.0;
    }
    let x = (pos - i as f64) as f32;
    env[i] * (1.0 - x) + env[i + 1] * x
}

/// Best phase (in hops) for a given beat period, and its mean onset strength.
fn comb(env: &[f32], period: f64, phase_step: f64) -> (f64, f64) {
    let beats = ((env.len() as f64 - 2.0) / period).floor() as usize;
    if beats < 8 {
        return (0.0, 0.0);
    }
    let mut best = (0.0, -1.0);
    let mut phase = 0.0;
    while phase < period {
        let mut s = 0.0f64;
        for k in 0..beats {
            s += sample(env, phase + k as f64 * period) as f64;
        }
        let s = s / beats as f64;
        if s > best.1 {
            best = (phase, s);
        }
        phase += phase_step;
    }
    best
}

fn prior(bpm: f64) -> f64 {
    // Log-normal preference centred on 126 BPM, about an octave wide.
    let x = (bpm / 126.0).log2() / 0.55;
    (-0.5 * x * x).exp()
}

/// Debug helper: onset envelope and a comb scorer over it.
#[doc(hidden)]
pub fn debug_scorer(audio: &TrackAudio) -> impl Fn(f64) -> f64 {
    let (env, fps) = onset_envelope(audio);
    let mean = env.iter().map(|&v| v as f64).sum::<f64>() / env.len().max(1) as f64;
    move |bpm: f64| comb(&env, 60.0 * fps / bpm, 1.0).1 / mean
}

/// Debug helper: whole-track comb score for every tempo (no prior).
#[doc(hidden)]
pub fn sweep(audio: &TrackAudio, from: f64, to: f64, step: f64) -> Vec<(f64, f64, f64)> {
    let (env, fps) = onset_envelope(audio);
    let mean = env.iter().map(|&v| v as f64).sum::<f64>() / env.len().max(1) as f64;
    let mut out = Vec::new();
    let mut b = from;
    while b <= to {
        let (_, s) = comb(&env, 60.0 * fps / b, 2.0);
        out.push((b, s / mean, s / mean * prior(b)));
        b += step;
    }
    out
}

/// Detect tempo and beat positions. `hint` (e.g. a BPM tag) narrows the
/// search when it's plausible.
pub fn analyze(audio: &TrackAudio, hint: Option<f64>) -> Option<BeatGrid> {
    let (env, fps) = onset_envelope(audio);
    if env.len() < (fps * 8.0) as usize {
        return None;
    }
    let mean_env: f64 = env.iter().map(|&v| v as f64).sum::<f64>() / env.len() as f64;
    if mean_env <= 1e-9 {
        return None;
    }

    let lag_of = |bpm: f64| 60.0 * fps / bpm;
    let score = |bpm: f64, step: f64| comb(&env, lag_of(bpm), step);

    // Sweep every tempo from 68 to 190 BPM over the middle ~100 s (skips
    // intros). A real beat tempo is also supported at double speed (hats,
    // eighths) and half speed (bars); a dotted-rhythm tempo (2/3 of the
    // beat, common in 2-step) isn't, so it no longer wins.
    let mid = env.len() / 2;
    let half_win = (50.0 * fps) as usize;
    let ex = &env[mid.saturating_sub(half_win)..(mid + half_win).min(env.len())];
    let raw = |bpm: f64| comb(ex, lag_of(bpm), 1.0).1;
    let mut best_bpm = 0.0;
    let mut best_val = -1.0;
    let mut best_raw = 0.0;
    let mut raws = Vec::new();
    let mut b = 68.0;
    while b <= 190.0 {
        let r = raw(b);
        raws.push(r);
        let metric = r + 0.5 * raw(b * 2.0) + 0.5 * raw(b / 2.0);
        let hint_bonus = match hint {
            Some(h) if (60.0..=200.0).contains(&h) && (b / h - 1.0).abs() < 0.01 => 1.2,
            _ => 1.0,
        };
        let v = metric * prior(b) * hint_bonus;
        if v > best_val {
            best_val = v;
            best_bpm = b;
            best_raw = r;
        }
        b += 0.1;
    }
    // Confidence: how far the winning tempo stands above a typical tempo.
    // Beatless audio (ambient, speech, noise) has no standout tempo.
    raws.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = raws[raws.len() / 2].max(1e-12);
    if std::env::var("TANDEM_DEBUG_PULSE").is_ok() {
        eprintln!(
            "tempo confidence {:.2} at {best_bpm:.1} bpm",
            best_raw / median
        );
    }
    if best_raw / median < 1.45 {
        return None;
    }

    // A BPM tag (from Serato, rekordbox...) is trusted on half/double time.
    if let Some(h) = hint.filter(|h| (60.0..=200.0).contains(h)) {
        for m in [0.5, 2.0] {
            if (best_bpm * m / h - 1.0).abs() < 0.02 {
                best_bpm *= m;
            }
        }
    }

    // Refine around the winner.
    let refine = |center: f64, span: f64, step: f64| {
        let mut best = (center, -1.0);
        let mut b = center - span;
        while b <= center + span {
            let (_, s) = score(b, 1.0);
            if s > best.1 {
                best = (b, s);
            }
            b += step;
        }
        best.0
    };
    let bpm = refine(best_bpm, 0.15, 0.01);
    let mut bpm = refine(bpm, 0.012, 0.002);

    // Most DJ tracks sit on a whole BPM: prefer it if it fits as well.
    let rounded = bpm.round();
    if (bpm - rounded).abs() < 0.05 {
        let (_, s_round) = score(rounded, 0.5);
        let (_, s_fine) = score(bpm, 0.5);
        if s_round >= s_fine * 0.995 {
            bpm = rounded;
        }
    }

    // Phase: fine comb, then sub-hop refinement.
    let period = lag_of(bpm);
    let (phase, _) = comb(&env, period, 0.25);
    let beats = ((env.len() as f64 - 2.0) / period).floor() as usize;
    let at = |p: f64| {
        (0..beats)
            .map(|k| sample(&env, p + k as f64 * period) as f64)
            .sum::<f64>()
    };
    let (a, b, c) = (at(phase - 0.25), at(phase), at(phase + 0.25));
    let d = a - 2.0 * b + c;
    let off = if d.abs() > 1e-12 {
        (0.5 * (a - c) / d).clamp(-0.5, 0.5) * 0.25
    } else {
        0.0
    };
    // The flux at hop n measures the rise from hop n-1 to n: the transient
    // starts about one hop earlier than the hop that registers it.
    let first_hop = phase + off - 0.75;
    let beat_len = 60.0 / bpm;
    let first_beat = (first_hop / fps).rem_euclid(beat_len);
    Some(BeatGrid { bpm, first_beat })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TrackId;

    fn synth(seconds: f64, f: impl Fn(f64) -> f64) -> TrackAudio {
        let sr = 44_100u32;
        let n = (seconds * sr as f64) as usize;
        let mut rng = fastrand::Rng::with_seed(9);
        let frames = (0..n)
            .map(|i| {
                let t = i as f64 / sr as f64;
                let v = (f(t) + (rng.f64() - 0.5) * 0.02).clamp(-1.0, 1.0);
                let s = (v * 30000.0) as i16;
                [s, s]
            })
            .collect();
        TrackAudio::new(TrackId([0; 32]), sr, frames)
    }

    fn kick(t: f64) -> f64 {
        if t < 0.0 || t > 0.25 {
            return 0.0;
        }
        (2.0 * std::f64::consts::PI * (50.0 + 90.0 * (-t * 25.0).exp()) * t).sin()
            * (-t * 12.0).exp()
            * 0.8
    }

    fn hat(t: f64, seed: f64) -> f64 {
        if t < 0.0 || t > 0.04 {
            return 0.0;
        }
        ((t * 91_731.0 + seed).sin() * 43_758.545).fract() * (-t * 120.0).exp() * 0.3
    }

    fn house(bpm: f64, offset: f64) -> impl Fn(f64) -> f64 {
        move |t| {
            let beat = 60.0 / bpm;
            let x = t - offset;
            if x < 0.0 {
                return 0.0;
            }
            let k = (x / beat).floor();
            let in_beat = x - k * beat;
            let off = (x - beat / 2.0).rem_euclid(beat);
            kick(in_beat) + hat(off, k) + 0.15 * (2.0 * std::f64::consts::PI * 55.0 * t).sin()
        }
    }

    #[test]
    fn four_on_the_floor_house() {
        let g = analyze(&synth(60.0, house(124.0, 0.137)), None).unwrap();
        assert!((g.bpm - 124.0).abs() < 0.02, "bpm {}", g.bpm);
        let err = (g.first_beat - 0.137).abs();
        assert!(
            err < 0.006,
            "first beat {} (err {:.1} ms)",
            g.first_beat,
            err * 1000.0
        );
    }

    #[test]
    fn non_integer_tempo_is_kept() {
        let g = analyze(&synth(90.0, house(127.6, 0.05)), None).unwrap();
        assert!((g.bpm - 127.6).abs() < 0.02, "bpm {}", g.bpm);
    }

    #[test]
    fn half_time_groove_reports_full_tempo() {
        // 140 BPM: kick on 1, snare-ish burst on 3, hats on every eighth.
        let bpm = 140.0;
        let f = move |t: f64| {
            let beat = 60.0 / bpm;
            let bar = t.rem_euclid(beat * 4.0);
            let eighth = t.rem_euclid(beat / 2.0);
            let k = (t / (beat / 2.0)).floor();
            kick(bar) + 0.8 * hat(bar - 2.0 * beat, 1.0) * 2.0 + hat(eighth, k)
        };
        let g = analyze(&synth(60.0, f), None).unwrap();
        assert!((g.bpm - 140.0).abs() < 0.05, "bpm {}", g.bpm);
    }

    #[test]
    fn hint_resolves_ambiguity() {
        let g = analyze(&synth(60.0, house(87.0, 0.2)), Some(174.0)).unwrap();
        assert!((g.bpm - 174.0).abs() < 0.05, "bpm {}", g.bpm);
    }

    #[test]
    fn silence_has_no_grid() {
        assert!(analyze(&synth(20.0, |_| 0.0), None).is_none());
    }
}
