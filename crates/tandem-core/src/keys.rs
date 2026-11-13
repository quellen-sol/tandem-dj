//! Musical keys: detection, Camelot notation and key matching.
//!
//! A key is stored as `0..=11` for major (C = 0, C# = 1, ...) and `12..=23`
//! for minor (Cm = 12, ...).

use crate::audio::TrackAudio;

const NAMES: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];

pub fn is_minor(key: i8) -> bool {
    key >= 12
}

/// "Am", "F#", ...
pub fn name(key: i8) -> String {
    let pc = (key % 12) as usize;
    if is_minor(key) {
        format!("{}m", NAMES[pc])
    } else {
        NAMES[pc].to_string()
    }
}

/// Camelot wheel code: 8A = A minor, 8B = C major, 9B = G major...
pub fn camelot(key: i8) -> String {
    let pc = (key % 12) as i32;
    let major_pc = if is_minor(key) { (pc + 3) % 12 } else { pc };
    let num = ((major_pc * 7) % 12 + 7) % 12 + 1;
    format!("{num}{}", if is_minor(key) { 'A' } else { 'B' })
}

/// Parse a key tag: "Am", "A minor", "C#", "Dbm", "8A", "11B"...
pub fn parse(s: &str) -> Option<i8> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    // Camelot
    let up = t.to_uppercase();
    if let Some(letter) = up.chars().last().filter(|c| *c == 'A' || *c == 'B') {
        if let Ok(num) = up[..up.len() - 1].parse::<i32>() {
            if (1..=12).contains(&num) {
                let major_pc = ((num - 8) * 7).rem_euclid(12);
                return Some(if letter == 'B' {
                    major_pc as i8
                } else {
                    ((major_pc + 9) % 12 + 12) as i8
                });
            }
        }
    }
    let mut chars = t.chars();
    let root = chars.next()?.to_ascii_uppercase();
    let mut pc: i32 = match root {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest: String = chars.collect();
    let mut rest = rest.trim().to_string();
    if rest.starts_with('#') || rest.starts_with('♯') {
        pc += 1;
        rest = rest.chars().skip(1).collect();
    } else if rest.starts_with('b') || rest.starts_with('♭') {
        pc -= 1;
        rest = rest.chars().skip(1).collect();
    }
    let rest = rest.trim().to_lowercase();
    let minor = rest.starts_with('m') && !rest.starts_with("maj");
    let pc = pc.rem_euclid(12) as i8;
    Some(if minor { pc + 12 } else { pc })
}

/// Semitones to shift `from` so it sounds in `to` or its relative
/// major/minor (same Camelot number), whichever is the smaller shift.
pub fn match_shift(from: i8, to: i8) -> i8 {
    let f = (from % 12) as i32;
    let t = (to % 12) as i32;
    let target = match (is_minor(from), is_minor(to)) {
        (a, b) if a == b => t,
        (false, true) => (t + 3) % 12, // relative major of a minor key
        (true, false) => (t + 9) % 12, // relative minor of a major key
        _ => t,
    };
    let d = (target - f).rem_euclid(12);
    (if d > 6 { d - 12 } else { d }) as i8
}

// Temperley (Kostka-Payne corpus) key profiles.
const MAJOR: [f64; 12] = [
    0.748, 0.060, 0.488, 0.082, 0.670, 0.460, 0.096, 0.715, 0.104, 0.366, 0.057, 0.400,
];
const MINOR: [f64; 12] = [
    0.712, 0.084, 0.474, 0.618, 0.049, 0.460, 0.105, 0.747, 0.404, 0.067, 0.133, 0.330,
];

fn correlate(chroma: &[f64; 12], profile: &[f64; 12], shift: usize) -> f64 {
    let mc = chroma.iter().sum::<f64>() / 12.0;
    let mp = profile.iter().sum::<f64>() / 12.0;
    let (mut num, mut dc, mut dp) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let c = chroma[(i + shift) % 12] - mc;
        let p = profile[i] - mp;
        num += c * p;
        dc += c * c;
        dp += p * p;
    }
    num / (dc.sqrt() * dp.sqrt()).max(1e-12)
}

/// Average pitch-class energy over the track.
pub fn chroma(audio: &TrackAudio) -> Option<[f64; 12]> {
    use realfft::RealFftPlanner;
    let decim = (audio.sample_rate as usize / 11_025).max(1);
    let sr = audio.sample_rate as f64 / decim as f64;
    let mono: Vec<f32> = audio
        .frames
        .chunks(decim)
        .map(|c| {
            c.iter()
                .map(|f| (f[0] as f32 + f[1] as f32) * 0.5)
                .sum::<f32>()
                / (c.len() as f32 * 32768.0)
        })
        .collect();
    const N: usize = 8192;
    const HOP: usize = 4096;
    if mono.len() < N * 4 {
        return None;
    }
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N);
    let window: Vec<f32> = (0..N)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos())
        .collect();
    // Notes C2 (MIDI 36) to B6 (95): bins within ±0.4 semitone of each.
    let notes: Vec<(usize, usize, usize)> = (36..96)
        .map(|m| {
            let f = 440.0 * 2f64.powf((m as f64 - 69.0) / 12.0);
            let lo = ((f * 2f64.powf(-0.4 / 12.0)) * N as f64 / sr).floor() as usize;
            let hi = ((f * 2f64.powf(0.4 / 12.0)) * N as f64 / sr).ceil() as usize;
            (m % 12, lo, hi.max(lo + 1).min(N / 2))
        })
        .collect();
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut total = [0f64; 12];
    let mut prev: Vec<f64> = Vec::new();
    let mut start = 0;
    let mut frames = 0;
    while start + N <= mono.len() {
        for i in 0..N {
            input[i] = mono[start + i] * window[i];
        }
        if fft.process(&mut input, &mut spectrum).is_err() {
            return None;
        }
        let mut note_e: Vec<f64> = notes
            .iter()
            .map(|&(_, lo, hi)| {
                spectrum[lo..hi]
                    .iter()
                    .map(|c| c.norm() as f64)
                    .fold(0.0, f64::max)
            })
            .collect();
        // Whiten: remove the broadband floor (drums, noise) per frame.
        let mean = note_e.iter().sum::<f64>() / note_e.len() as f64;
        for e in &mut note_e {
            *e = (*e - mean).max(0.0).sqrt();
        }
        // Count only energy that holds across consecutive frames: sustained
        // notes (bass, chords, vocals) carry the key; drum hits don't.
        for (i, &(pc, _, _)) in notes.iter().enumerate() {
            let held = if prev.is_empty() {
                note_e[i]
            } else {
                note_e[i].min(prev[i])
            };
            total[pc] += held;
        }
        prev.clone_from(&note_e);
        frames += 1;
        start += HOP;
    }
    if frames == 0 || total.iter().sum::<f64>() <= 1e-9 {
        return None;
    }
    Some(total)
}

/// Detect the key of a track.
pub fn detect(audio: &TrackAudio) -> Option<i8> {
    let c = chroma(audio)?;
    let mut best = (-1i8, f64::MIN);
    let mut second = f64::MIN;
    for pc in 0..12 {
        for (minor, profile) in [(false, &MAJOR), (true, &MINOR)] {
            let r = correlate(&c, profile, pc);
            if r > best.1 {
                second = best.1;
                best = ((pc + if minor { 12 } else { 0 }) as i8, r);
            } else if r > second {
                second = r;
            }
        }
    }
    if std::env::var("TANDEM_DEBUG_KEY").is_ok() {
        eprintln!(
            "key {} r={:.3} (next {:.3})",
            camelot(best.0),
            best.1,
            second
        );
    }
    // Too flat or ambiguous to call.
    (best.1 > 0.2).then_some(best.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TrackId;

    #[test]
    fn camelot_codes() {
        assert_eq!(camelot(0), "8B"); // C
        assert_eq!(camelot(7), "9B"); // G
        assert_eq!(camelot(2), "10B"); // D
        assert_eq!(camelot(9 + 12), "8A"); // Am
        assert_eq!(camelot(4 + 12), "9A"); // Em
        assert_eq!(camelot(8 + 12), "1A"); // G#m
        assert_eq!(camelot(11), "1B"); // B
        assert_eq!(name(1 + 12), "Dbm");
    }

    #[test]
    fn parses_tags() {
        assert_eq!(parse("Am"), Some(9 + 12));
        assert_eq!(parse("A minor"), Some(9 + 12));
        assert_eq!(parse("F#"), Some(6));
        assert_eq!(parse("Dbm"), Some(1 + 12));
        assert_eq!(parse("8A"), Some(9 + 12));
        assert_eq!(parse("10B"), Some(2));
        assert_eq!(parse("1A"), Some(8 + 12));
        for k in 0..24 {
            assert_eq!(parse(&camelot(k)), Some(k), "{}", camelot(k));
        }
    }

    #[test]
    fn key_matching() {
        assert_eq!(match_shift(9 + 12, 9 + 12), 0); // Am -> Am
        assert_eq!(match_shift(11 + 12, 9 + 12), -2); // Bm -> Am
        assert_eq!(match_shift(0, 9 + 12), 0); // C -> Am: already relative
        assert_eq!(match_shift(2, 9 + 12), -2); // D -> Am: down to C
        assert_eq!(match_shift(7 + 12, 2), 4); // Gm -> D: up to Bm
        assert_eq!(match_shift(0, 6), 6);
    }

    fn chord_track(notes: &[f64], seconds: f64) -> TrackAudio {
        let sr = 44_100u32;
        let frames = (0..(seconds * sr as f64) as usize)
            .map(|i| {
                let t = i as f64 / sr as f64;
                let mut v = 0.0;
                for (k, &m) in notes.iter().enumerate() {
                    let f = 440.0 * 2f64.powf((m - 69.0) / 12.0);
                    // A few harmonics, like a real instrument.
                    for h in 1..4 {
                        v += (2.0 * std::f64::consts::PI * f * h as f64 * t + k as f64).sin()
                            * 0.12
                            / h as f64;
                    }
                }
                let s = (v * 32767.0 * 0.5) as i16;
                [s, s]
            })
            .collect();
        TrackAudio::new(TrackId([0; 32]), sr, frames)
    }

    #[test]
    fn detects_major_and_minor() {
        // C major: C E G plus a bass C and passing D, F, A, B.
        let c_major = chord_track(
            &[48.0, 60.0, 64.0, 67.0, 72.0, 62.0, 65.0, 69.0, 71.0],
            12.0,
        );
        assert_eq!(detect(&c_major).map(camelot), Some("8B".into()));
        // A minor: A C E plus D, G, F.
        let a_minor = chord_track(&[45.0, 57.0, 60.0, 64.0, 69.0, 62.0, 67.0, 65.0], 12.0);
        assert_eq!(detect(&a_minor).map(camelot), Some("8A".into()));
        // F# minor, to check it isn't biased to C.
        let fs_minor = chord_track(&[42.0, 54.0, 57.0, 61.0, 66.0, 59.0, 64.0], 12.0);
        assert_eq!(detect(&fs_minor).map(name), Some("F#m".into()));
    }
}
