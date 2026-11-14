//! BPM and beatgrid detection, from the audio alone (tags are not used).
//!
//! 1. **Onset envelope.** The track is split into low (kick), mid and high
//!    (clap/hat) bands. For each ~6 ms hop we take the band energies and
//!    keep only increases ("spectral flux"), weighted toward the kick.
//! 2. **Tempo.** Over the middle ~100 s, a comb of beat ticks is scored for
//!    every tempo from 68 to 190 BPM, together with the metrical levels
//!    around it (bars, half bars, eighths, sixteenths): a real beat is
//!    supported at all of them. The strongest tempo family (equal up to
//!    octaves) wins, except when its rival is a dotted or triplet pulse 4:3
//!    or 3:2 away: then the fine grid decides which tempo it subdivides
//!    (see `choose_tempo`). A gentle preference for dance tempos picks the
//!    octave.
//! 3. **Fit.** A comb slid across the whole section refines the BPM to
//!    about 0.01 (what sync needs) and finds the phase.
//! 4. **Tempo changes.** 16 s windows across the track look for a long,
//!    confident stretch at an unrelated tempo (not half/double time or a
//!    4:3 feel). Each becomes its own grid section, switching on the beat
//!    where the two grids fit best. Gradual drifts are not followed.

use crate::audio::TrackAudio;
pub use crate::grid::BeatGrid;
use crate::grid::Marker;

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

/// Gaussian smoothing, `sigma` in hops.
fn smooth(x: &[f32], sigma: f64) -> Vec<f32> {
    let r = (sigma * 3.0).ceil() as isize;
    let k: Vec<f32> = (-r..=r)
        .map(|i| (-0.5 * (i as f64 / sigma).powi(2)).exp() as f32)
        .collect();
    let norm: f32 = k.iter().sum();
    let n = x.len() as isize;
    (0..n)
        .map(|i| {
            let mut s = 0.0;
            for (j, w) in k.iter().enumerate() {
                let idx = (i + j as isize - r).clamp(0, n - 1);
                s += w * x[idx as usize];
            }
            s / norm
        })
        .collect()
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

/// The beat is looked for in this range.
const MIN_TEMPO: f64 = 68.0;
const MAX_TEMPO: f64 = 190.0;
/// How far the winning tempo must stand above a typical one. Below this
/// there's no steady beat (ambient, speech, noise).
const MIN_CONFIDENCE: f64 = 1.3;
/// Smoothing of the envelope (in hops) for the tempo search. Sampling an
/// unsmoothed envelope between hops blurs it, so periods of a whole number
/// of hops would otherwise score a little higher than their neighbours.
const SEARCH_SMOOTHING: f64 = 1.0;
/// A tempo is judged with the metrical levels around it, weighted: bars,
/// half bars, the beat, eighths and sixteenths.
const LEVELS: [(f64, f64); 5] = [(0.25, 0.5), (0.5, 1.0), (1.0, 1.0), (2.0, 1.0), (4.0, 0.5)];

/// Position within the octave (0..1): tempos an octave apart share it.
fn octave_pos(bpm: f64) -> f64 {
    bpm.log2().rem_euclid(1.0)
}

/// `b` over `a`, folded into one octave: 1 ≤ ratio < 2.
fn folded_ratio(a: f64, b: f64) -> f64 {
    2f64.powf((octave_pos(b) - octave_pos(a)).rem_euclid(1.0))
}

/// Equal up to octaves, within `tol` (relative).
fn same_family(a: f64, b: f64, tol: f64) -> bool {
    let r = folded_ratio(a, b);
    (r - 1.0).abs() < tol || (r / 2.0 - 1.0).abs() < tol
}

/// The beat tempo of a stretch of smoothed onset envelope, and how clearly
/// it stands out.
fn choose_tempo(ex: &[f32], fps: f64) -> (f64, f64) {
    let mean = (ex.iter().map(|&v| v as f64).sum::<f64>() / ex.len().max(1) as f64).max(1e-12);
    let raw = |bpm: f64| comb(ex, 60.0 * fps / bpm, 1.0).1 / mean;
    let metric = |b: f64| LEVELS.iter().map(|(m, w)| w * raw(b * m)).sum::<f64>();

    // Every tempo in range, 0.1 BPM apart.
    let mut sweep = Vec::new();
    let mut b = MIN_TEMPO;
    while b <= MAX_TEMPO {
        sweep.push((b, metric(b)));
        b += 0.1;
    }
    let mut peaks: Vec<(f64, f64)> = (1..sweep.len() - 1)
        .filter(|&i| sweep[i].1 >= sweep[i - 1].1 && sweep[i].1 >= sweep[i + 1].1)
        .map(|i| sweep[i])
        .collect();
    peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
    let top = peaks.first().copied().unwrap_or(sweep[0]);

    // Family (tempos equal up to octaves): the strongest wins, except in the
    // classic confusion between a beat and a dotted or triplet pulse 4:3
    // (or 3:2) away. There the fine grid decides: the slower tempo's
    // sixteenths are the faster one's triplets, so if the faster tempo's
    // eighths are stronger than that, the music is in the faster tempo,
    // and otherwise in the slower one.
    let mut family = top.0;
    if let Some(&(rival, rival_m)) = peaks.iter().find(|p| !same_family(p.0, top.0, 0.007)) {
        if rival_m >= 0.6 * top.1 {
            let r = folded_ratio(top.0, rival);
            let pair = if (r / (4.0 / 3.0) - 1.0).abs() < 0.015 {
                Some((top.0, top.0 * r))
            } else if (r / 1.5 - 1.0).abs() < 0.015 {
                Some((top.0 * r / 2.0, top.0))
            } else {
                None
            };
            if let Some((slow, fast)) = pair {
                // Peaks are narrow over a long stretch: look a little either side.
                let near = |b: f64| {
                    (-4..=4)
                        .map(|i| raw(b * (1.0 + i as f64 * 0.001)))
                        .fold(0.0, f64::max)
                };
                family = if near(4.0 * slow) > near(2.0 * fast) {
                    slow
                } else {
                    fast
                };
            }
        }
    }

    // Octave: the family member in range that the prior likes best.
    let mut best = (family, -1.0);
    for k in -2..=2 {
        let target = family * 2f64.powi(k);
        let Some(&(b, m)) = sweep
            .iter()
            .filter(|s| (s.0 / target - 1.0).abs() <= 0.01)
            .max_by(|x, y| x.1.total_cmp(&y.1))
        else {
            continue;
        };
        if m * prior(b) > best.1 {
            best = (b, m * prior(b));
        }
    }

    // Confidence: how far the winning family stands above a typical tempo.
    let mut ms: Vec<f64> = sweep.iter().map(|s| s.1).collect();
    ms.sort_by(|a, b| a.total_cmp(b));
    let median = ms[ms.len() / 2].max(1e-12);
    (best.0, top.1 / median)
}

/// Tempo and the first beat (track seconds, at or after `start`) of the
/// stretch `env[start..end]`, starting from a tempo guess.
fn fit(env: &[f32], fps: f64, start: usize, end: usize, guess: f64) -> (f64, f64) {
    let span = &env[start..end];
    let lag_of = |bpm: f64| 60.0 * fps / bpm;
    let score = |bpm: f64, step: f64| comb(span, lag_of(bpm), step).1;
    let refine = |center: f64, width: f64, step: f64| {
        let mut best = (center, -1.0);
        let mut b = center - width;
        while b <= center + width {
            let s = score(b, 1.0);
            if s > best.1 {
                best = (b, s);
            }
            b += step;
        }
        best.0
    };
    let bpm = refine(guess, 0.15, 0.01);
    let mut bpm = refine(bpm, 0.012, 0.002);

    // Most DJ tracks sit on a whole BPM: prefer it if it fits as well.
    let rounded = bpm.round();
    if (bpm - rounded).abs() < 0.05 && score(rounded, 0.5) >= score(bpm, 0.5) * 0.995 {
        bpm = rounded;
    }

    // Phase: fine comb, then sub-hop refinement.
    let period = lag_of(bpm);
    let (phase, _) = comb(span, period, 0.25);
    let beats = ((span.len() as f64 - 2.0) / period).floor() as usize;
    let at = |p: f64| {
        (0..beats)
            .map(|k| sample(span, p + k as f64 * period) as f64)
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
    let first_hop = start as f64 + phase + off - 0.75;
    (bpm, first_hop / fps)
}

/// Window length and step (seconds) for finding tempo changes.
const SECTION_WIN: f64 = 16.0;
const SECTION_STEP: f64 = 4.0;
/// A window's beat must stand out this clearly to start a section at
/// another tempo, and this clearly to carry one on.
const SECTION_CONFIDENCE: f64 = 1.8;
const SECTION_HOLD: f64 = 1.4;
/// A different tempo must hold this long (seconds) to start a section.
const SECTION_MIN: f64 = 32.0;

/// Same tempo up to octaves, or a 4:3 or 3:2 feel of it (half-time,
/// dotted and triplet grooves): not a tempo change.
fn related(main: f64, b: f64) -> bool {
    let r = folded_ratio(main, b);
    [1.0, 4.0 / 3.0, 1.5, 2.0]
        .iter()
        .any(|x| (r / x - 1.0).abs() < 0.025)
}

/// Stretches of the track whose beat is clearly at another tempo than
/// `main`: (start hop, end hop, tempo family). Only steady, confident
/// changes count: a breakdown, a half-time or dotted feel, or a gradual
/// drift doesn't.
fn tempo_changes(env: &[f32], fps: f64, main: f64) -> Vec<(usize, usize, f64)> {
    let win = (SECTION_WIN * fps) as usize;
    let step = (SECTION_STEP * fps) as usize;
    if env.len() < win + step * 8 {
        return Vec::new();
    }
    // Comb strength on a log tempo grid with exact octaves (70 steps each),
    // from 30 BPM up, so each tempo's half and double are on the grid too.
    const PER_OCTAVE: i32 = 70;
    let tempo = |i: i32| 30.0 * 2f64.powf(i as f64 / PER_OCTAVE as f64);
    // The beat is looked for from 60 to ~200 BPM.
    let (lo, hi) = (PER_OCTAVE, 2 * PER_OCTAVE + 52);
    // Per window: its tempo, how clearly, and whether it is related to `main`.
    let mut found: Vec<(f64, f64, bool)> = Vec::new();
    let mut start = 0;
    while start + win <= env.len() {
        let w = &env[start..start + win];
        let mean = (w.iter().map(|&v| v as f64).sum::<f64>() / win as f64).max(1e-12);
        let c: Vec<f64> = (0..=hi + PER_OCTAVE)
            .map(|i| comb(w, 60.0 * fps / tempo(i), 1.0).1 / mean)
            .collect();
        let m: Vec<f64> = (lo..=hi)
            .map(|i| {
                let i = i as usize;
                let o = PER_OCTAVE as usize;
                c[i] + 0.5 * (c[i + o] + c[i - o])
            })
            .collect();
        let (k, best) = m
            .iter()
            .copied()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        let mut sorted = m.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let conf = best / sorted[sorted.len() / 2].max(1e-12);
        let b = tempo(lo + k as i32);
        found.push((b, conf, related(main, b)));
        start += step;
    }

    // Runs at another tempo: a clear window starts one, and it carries on
    // through windows that still hear that tempo, if less clearly (one odd
    // window allowed).
    let starts = |w: (f64, f64, bool)| w.1 >= SECTION_CONFIDENCE && !w.2;
    let holds = |w: (f64, f64, bool), b: f64| w.1 >= SECTION_HOLD && same_family(w.0, b, 0.025);
    let mut out = Vec::new();
    let mut i = 0;
    while i < found.len() {
        if !starts(found[i]) {
            i += 1;
            continue;
        }
        let b = found[i].0;
        let (mut j, mut clear, mut k) = (i, 1, i + 1);
        while k < found.len() {
            if holds(found[k], b) {
                j = k;
                clear += starts(found[k]) as usize;
                k += 1;
            } else if k + 1 < found.len() && holds(found[k + 1], b) {
                k += 1;
            } else {
                break;
            }
        }
        // From the middle of the first window to the middle of the last.
        let s = i * step + win / 2 - step / 2;
        let e = (j * step + win / 2 + step / 2).min(env.len());
        if clear >= 4 && (e - s) as f64 / fps >= SECTION_MIN {
            // Check with the full tempo choice over the whole stretch: a
            // busy pattern (a breakbeat's 5:4 pulse, say) can win a short
            // window without being the beat.
            let (bpm, conf) = choose_tempo(&env[s..e], fps);
            if std::env::var("TANDEM_DEBUG_SECTIONS").is_ok() {
                eprintln!(
                    "  section {:.0}-{:.0}s: windows say {b:.1}, whole stretch {bpm:.1} (conf {conf:.2})",
                    s as f64 / fps,
                    e as f64 / fps
                );
            }
            if !related(main, bpm) && conf >= MIN_CONFIDENCE {
                out.push((s, e, bpm));
            }
        }
        i = j + 1;
    }
    out
}

/// Where to switch from `prev` to `next` near `around` (hops): the beat of
/// the new grid where the old grid's last beats and the new grid's first
/// beats together land on the most onsets.
fn switch_point(env: &[f32], fps: f64, around: usize, prev: (f64, f64), next: (f64, f64)) -> f64 {
    let at = |t: f64| sample(env, t * fps) as f64;
    let (pb, pf) = prev;
    let (nb, nf) = next;
    let (pl, nl) = (60.0 / pb, 60.0 / nb);
    let center = around as f64 / fps;
    let k0 = ((center - 8.0 - nf) / nl).ceil() as i64;
    let k1 = ((center + 8.0 - nf) / nl).floor() as i64;
    let mut best = (center, f64::MIN);
    for k in k0..=k1 {
        let p = nf + k as f64 * nl;
        // The old grid's eight beats before p, and the new grid's eight from p.
        let last_old = pf + ((p - pf) / pl).ceil() * pl - pl;
        let s: f64 = (0..8).map(|i| at(last_old - i as f64 * pl)).sum::<f64>()
            + (0..8).map(|i| at(p + i as f64 * nl)).sum::<f64>();
        if s > best.1 {
            best = (p, s);
        }
    }
    best.0
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

/// Detect tempo and beat positions from the audio, including big, steady
/// tempo changes part-way through (each starts a new grid marker).
pub fn analyze(audio: &TrackAudio) -> Option<BeatGrid> {
    let (env, fps) = onset_envelope(audio);
    if env.len() < (fps * 8.0) as usize {
        return None;
    }
    let mean_env: f64 = env.iter().map(|&v| v as f64).sum::<f64>() / env.len() as f64;
    if mean_env <= 1e-9 {
        return None;
    }
    let smoothed = smooth(&env, SEARCH_SMOOTHING);

    // The main tempo, from the middle ~100 s (skips intros and outros).
    let mid = env.len() / 2;
    let half_win = (50.0 * fps) as usize;
    let ex = &smoothed[mid.saturating_sub(half_win)..(mid + half_win).min(env.len())];
    let (main, confidence) = choose_tempo(ex, fps);
    if std::env::var("TANDEM_DEBUG_PULSE").is_ok() {
        eprintln!("tempo confidence {confidence:.2} at {main:.1} bpm");
    }
    if confidence < MIN_CONFIDENCE {
        return None;
    }

    // Sections: the main tempo, with any stretches at another tempo.
    let changes = tempo_changes(&smoothed, fps, main);
    let mut sections: Vec<(usize, usize, f64)> = Vec::new();
    let mut at = 0;
    for &(s, e, family) in &changes {
        if s > at {
            sections.push((at, s, main));
        }
        sections.push((s, e, family));
        at = e;
    }
    if at < env.len() {
        sections.push((at, env.len(), main));
    }
    // A short leftover at the main tempo joins its neighbour.
    let min_len = (SECTION_MIN * fps) as usize;
    let mut i = 0;
    while sections.len() > 1 && i < sections.len() {
        let (s, e, _) = sections[i];
        if e - s < min_len {
            if i > 0 {
                sections[i - 1].1 = e;
            } else {
                sections[1].0 = s;
            }
            sections.remove(i);
        } else {
            i += 1;
        }
    }

    // Fit each section on its own, then switch grids on the best beat
    // near each boundary.
    let fits: Vec<(f64, f64)> = sections
        .iter()
        .map(|&(s, e, b)| fit(&env, fps, s, e, b))
        .collect();
    let mut markers = vec![Marker {
        pos: fits[0].1.rem_euclid(60.0 / fits[0].0),
        bpm: fits[0].0,
    }];
    for i in 1..sections.len() {
        let pos = switch_point(&env, fps, sections[i].0, fits[i - 1], fits[i]);
        markers.push(Marker {
            pos,
            bpm: fits[i].0,
        });
    }
    if std::env::var("TANDEM_DEBUG_PULSE").is_ok() && markers.len() > 1 {
        eprintln!("tempo sections {markers:?}");
    }
    BeatGrid::from_markers(&markers, false)
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
        let g = analyze(&synth(60.0, house(124.0, 0.137))).unwrap();
        assert!(
            (g.bpm_at(0.0) - 124.0).abs() < 0.02,
            "bpm {}",
            g.bpm_at(0.0)
        );
        let err = (g.first_beat() - 0.137).abs();
        assert!(
            err < 0.006,
            "first beat {} (err {:.1} ms)",
            g.first_beat(),
            err * 1000.0
        );
    }

    #[test]
    fn non_integer_tempo_is_kept() {
        let g = analyze(&synth(90.0, house(127.6, 0.05))).unwrap();
        assert!(
            (g.bpm_at(0.0) - 127.6).abs() < 0.02,
            "bpm {}",
            g.bpm_at(0.0)
        );
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
        let g = analyze(&synth(60.0, f)).unwrap();
        assert!(
            (g.bpm_at(0.0) - 140.0).abs() < 0.05,
            "bpm {}",
            g.bpm_at(0.0)
        );
    }

    #[test]
    fn silence_has_no_grid() {
        assert!(analyze(&synth(20.0, |_| 0.0)).is_none());
    }

    #[test]
    fn noise_has_no_grid() {
        for (seed, seconds) in [(1u64, 30.0), (2, 60.0), (3, 120.0), (4, 200.0)] {
            // Loud, slightly darkened noise.
            let mut rng = fastrand::Rng::with_seed(seed);
            let mut lp = 0.0;
            let frames = (0..(seconds * 44_100.0) as usize)
                .map(|_| {
                    lp += 0.3 * ((rng.f64() - 0.5) - lp);
                    let s = (lp * 2.0 * 30000.0) as i16;
                    [s, s]
                })
                .collect();
            let noise = TrackAudio::new(TrackId([0; 32]), 44_100, frames);
            assert!(analyze(&noise).is_none(), "seed {seed}");
        }
    }

    /// Same tempo up to octaves (within 1%).
    fn same_family(a: f64, b: f64) -> bool {
        let r = (a / b).log2();
        (r - r.round()).abs() < 0.015
    }

    /// One drum hit on each listed beat of every 4-beat bar.
    fn pattern(
        bpm: f64,
        kicks: &'static [f64],
        snares: &'static [f64],
        hats: &'static [f64],
    ) -> impl Fn(f64) -> f64 {
        move |t| {
            let beat = 60.0 / bpm;
            let bar_len = 4.0 * beat;
            let bar = (t / bar_len).floor();
            let x = t - bar * bar_len;
            let hit = |list: &[f64], f: &dyn Fn(f64) -> f64| -> f64 {
                list.iter().map(|&b| f(x - b * beat)).sum::<f64>()
            };
            hit(kicks, &kick)
                + hit(snares, &|d| hat(d, bar + 7.0) * 2.5)
                + hit(hats, &|d| hat(d, bar) * 0.6)
        }
    }

    #[test]
    fn dotted_groove_keeps_the_slow_beat() {
        // 87 BPM R&B: tresillo kicks (3+3+2 sixteenths), backbeat, 16th hats.
        // The dotted kicks pulse at 4/3 of the tempo (116); the beat is 87.
        const KICKS: [f64; 6] = [0.0, 0.75, 1.5, 2.0, 2.75, 3.5];
        const SNARES: [f64; 2] = [1.0, 3.0];
        const HATS: [f64; 16] = [
            0.0, 0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5, 2.75, 3.0, 3.25, 3.5, 3.75,
        ];
        let g = analyze(&synth(60.0, pattern(87.0, &KICKS, &SNARES, &HATS))).unwrap();
        assert!(same_family(g.bpm_at(0.0), 87.0), "bpm {}", g.bpm_at(0.0));
    }

    #[test]
    fn two_step_garage_keeps_the_fast_beat() {
        // 135 BPM 2-step: kicks on 1 and the "and" of 2, claps on 2 and 4,
        // off-beat hats. The kicks pulse every 1.5 beats (90 BPM); the beat
        // is 135.
        const KICKS: [f64; 3] = [0.0, 1.5, 3.0];
        const SNARES: [f64; 2] = [1.0, 3.0];
        const HATS: [f64; 4] = [0.5, 1.5, 2.5, 3.5];
        let g = analyze(&synth(60.0, pattern(135.0, &KICKS, &SNARES, &HATS))).unwrap();
        assert!(same_family(g.bpm_at(0.0), 135.0), "bpm {}", g.bpm_at(0.0));
    }

    #[test]
    fn tempo_change_starts_a_new_section() {
        // 70 s of house at 90 BPM, then 70 s at 140.
        let (slow, fast) = (house(90.0, 0.1), house(140.0, 70.0));
        let g = analyze(&synth(
            140.0,
            move |t| if t < 70.0 { slow(t) } else { fast(t) },
        ))
        .unwrap();
        let m = g.markers();
        assert_eq!(m.len(), 2, "markers {m:?}");
        assert!(same_family(m[0].bpm, 90.0), "markers {m:?}");
        assert!(same_family(m[1].bpm, 140.0), "markers {m:?}");
        assert!((m[1].pos - 70.0).abs() < 1.0, "markers {m:?}");
    }

    #[test]
    fn steady_track_has_one_section() {
        let g = analyze(&synth(150.0, house(126.0, 0.3))).unwrap();
        assert_eq!(g.markers().len(), 1, "markers {:?}", g.markers());
    }
}
