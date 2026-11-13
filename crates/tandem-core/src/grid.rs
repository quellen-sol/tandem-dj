//! Beatgrids: where the beats and bars are.
//!
//! A grid is one or more **markers**. Each marker sits on a downbeat (the
//! "1" of a bar) and starts a section with its own tempo, which runs until
//! the next marker. That is how a grid follows a track whose tempo changes
//! part-way through. Before the first marker, the beats carry on backwards
//! at the first marker's tempo.
//!
//! Bars are four beats. The first marker is bar 1; the bars before it count
//! -1, -2, … (there is no bar 0), so a DJ can park a track a bar or two
//! early, press play and wait for the 1. Every marker starts a new bar. A
//! section's last beat stretches or shrinks (by up to half a beat) to meet
//! the next marker.
//!
//! Analysis gives a one-marker grid. The edit methods ([`BeatGrid::set_marker`],
//! [`BeatGrid::adjust`], [`BeatGrid::slip`], …) each return a new grid, which
//! the deck takes with `Action::SetGrid`. They only change the section the
//! playhead is in, so fixing one part of a track never moves another.
//!
//! The type is `Copy` and its queries never allocate, so the audio threads
//! can use it.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

pub const MAX_MARKERS: usize = 32;
pub const MIN_BPM: f64 = 20.0;
pub const MAX_BPM: f64 = 400.0;
pub const BEATS_PER_BAR: i64 = 4;
/// Markers closer together than this (seconds) are merged.
const MIN_GAP: f64 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Marker {
    /// Track position (seconds) of a downbeat.
    pub pos: f64,
    /// Tempo from here to the next marker.
    pub bpm: f64,
}

impl Marker {
    pub fn beat_len(&self) -> f64 {
        60.0 / self.bpm
    }
}

/// One beat line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Beat {
    pub pos: f64,
    /// Bar number as shown: 1, 2, 3… from the first marker; -1, -2… before it.
    pub bar: i64,
    /// 1 to 4 within the bar.
    pub beat: u8,
}

#[derive(Clone, Copy)]
pub struct BeatGrid {
    len: u8,
    markers: [Marker; MAX_MARKERS],
    /// Edited by hand, rather than straight from analysis.
    pub edited: bool,
}

impl PartialEq for BeatGrid {
    fn eq(&self, other: &Self) -> bool {
        self.edited == other.edited && self.markers() == other.markers()
    }
}

impl fmt::Debug for BeatGrid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BeatGrid")
            .field("markers", &self.markers())
            .field("edited", &self.edited)
            .finish()
    }
}

/// Bar number as shown, and beat in bar, for local beat `j` of a section
/// whose first bar has index `bar_start` (index 0 is bar 1).
fn label(bar_start: i64, j: i64) -> (i64, u8) {
    let index = bar_start + j.div_euclid(BEATS_PER_BAR);
    let bar = if index >= 0 { index + 1 } else { index };
    (bar, (j.rem_euclid(BEATS_PER_BAR) + 1) as u8)
}

impl BeatGrid {
    /// A one-marker grid, as analysis finds it.
    pub fn new(first_beat: f64, bpm: f64) -> Option<Self> {
        Self::from_markers(
            &[Marker {
                pos: first_beat,
                bpm,
            }],
            false,
        )
    }

    /// A grid from any markers: invalid ones are dropped, the rest sorted,
    /// markers on top of each other merged, and at most [`MAX_MARKERS`] kept.
    /// None if no valid marker is left.
    pub fn from_markers(markers: &[Marker], edited: bool) -> Option<Self> {
        let mut v: Vec<Marker> = markers
            .iter()
            .copied()
            .filter(|m| m.pos.is_finite() && m.bpm.is_finite())
            .filter(|m| (MIN_BPM..=MAX_BPM).contains(&m.bpm))
            .collect();
        v.sort_by(|a, b| a.pos.total_cmp(&b.pos));
        let mut g = BeatGrid {
            len: 0,
            markers: [Marker {
                pos: 0.0,
                bpm: 120.0,
            }; MAX_MARKERS],
            edited,
        };
        for m in v {
            let n = g.len as usize;
            if n == MAX_MARKERS {
                break;
            }
            if n > 0 && m.pos - g.markers[n - 1].pos < MIN_GAP {
                continue;
            }
            g.markers[n] = m;
            g.len += 1;
        }
        (g.len > 0).then_some(g)
    }

    pub fn markers(&self) -> &[Marker] {
        &self.markers[..self.len as usize]
    }

    /// Position of the first marker: bar 1, beat 1.
    pub fn first_beat(&self) -> f64 {
        self.markers[0].pos
    }

    /// Index of the marker whose section contains `pos` (the first marker's,
    /// before it).
    pub fn section(&self, pos: f64) -> usize {
        self.markers()
            .partition_point(|m| m.pos <= pos)
            .saturating_sub(1)
    }

    /// The track's own tempo at `pos`.
    pub fn bpm_at(&self, pos: f64) -> f64 {
        self.markers[self.section(pos)].bpm
    }

    /// The tempo of the longest section within `[0, duration]`: what to show
    /// as "the" BPM of a track.
    pub fn main_bpm(&self, duration: f64) -> f64 {
        let m = self.markers();
        let mut best = (f64::NEG_INFINITY, m[0].bpm);
        for i in 0..m.len() {
            let start = if i == 0 { 0.0 } else { m[i].pos };
            let end = m.get(i + 1).map(|n| n.pos).unwrap_or(duration.max(start));
            if end - start > best.0 {
                best = (end - start, m[i].bpm);
            }
        }
        best.1
    }

    /// Whole beats from marker `i` to the next one.
    fn beats_in_section(&self, i: usize) -> i64 {
        let m = self.markers();
        ((m[i + 1].pos - m[i].pos) / m[i].beat_len())
            .round()
            .max(1.0) as i64
    }

    /// Bar index (0 = bar 1) at which section `i` starts.
    fn bar_start(&self, i: usize) -> i64 {
        (0..i)
            .map(|j| (self.beats_in_section(j) + BEATS_PER_BAR - 1) / BEATS_PER_BAR)
            .sum()
    }

    /// Beats since the first marker at `pos`, continuous within a section
    /// (negative before the first marker). Each section starts on a whole
    /// bar, so whole numbers are beats and multiples of 2 or 4 line up with
    /// half bars and bars.
    pub fn beat_at(&self, pos: f64) -> f64 {
        let i = self.section(pos);
        let m = self.markers[i];
        (self.bar_start(i) * BEATS_PER_BAR) as f64 + (pos - m.pos) / m.beat_len()
    }

    /// Position within the beat (0..1) at `pos`, counting beats `mult` times
    /// as fast (0.5 = half time).
    pub fn phase(&self, pos: f64, mult: f64) -> f64 {
        (self.beat_at(pos) * mult).rem_euclid(1.0)
    }

    /// The bar and beat the playhead at `pos` is in (the last beat at or
    /// before it).
    pub fn bar_beat_at(&self, pos: f64) -> (i64, u8) {
        let i = self.section(pos);
        let m = self.markers[i];
        let mut j = ((pos - m.pos) / m.beat_len() + 1e-9).floor() as i64;
        if i + 1 < self.len as usize {
            j = j.min(self.beats_in_section(i) - 1);
        }
        label(self.bar_start(i), j)
    }

    /// The first beat at or after `pos`.
    pub fn next_beat(&self, pos: f64) -> f64 {
        let i = self.section(pos);
        let m = self.markers[i];
        let j = ((pos - m.pos) / m.beat_len() - 1e-9).ceil();
        if i + 1 < self.len as usize && j as i64 >= self.beats_in_section(i) {
            self.markers[i + 1].pos
        } else {
            m.pos + j * m.beat_len()
        }
    }

    /// A loop `beats` long that starts on the grid and contains `pos`:
    /// `(start, end)`. A loop of a beat or more starts on the beat at or
    /// before `pos`. A shorter one starts on the same fraction of a beat, so
    /// a ¼-beat loop is the quarter beat the playhead is in. The length uses
    /// the tempo of the section `pos` is in.
    pub fn beat_loop(&self, pos: f64, beats: f64) -> (f64, f64) {
        let m = self.markers[self.section(pos)];
        let len = m.beat_len();
        let step = beats.min(1.0);
        let local = (pos - m.pos) / len;
        let start = m.pos + ((local + 1e-9) / step).floor() * step * len;
        (start, start + beats * len)
    }

    /// Every beat in `[from, to]`, in order.
    pub fn beats_in(&self, from: f64, to: f64, mut f: impl FnMut(Beat)) {
        let n = self.len as usize;
        let mut bar_start = 0;
        for i in 0..n {
            let m = self.markers[i];
            let len = m.beat_len();
            let lo = ((from - m.pos) / len - 1e-9).ceil() as i64;
            let lo = if i == 0 { lo } else { lo.max(0) };
            let mut hi = ((to - m.pos) / len + 1e-9).floor() as i64;
            let beats = (i + 1 < n).then(|| self.beats_in_section(i));
            if let Some(b) = beats {
                hi = hi.min(b - 1);
            }
            // A window is a few seconds; the cap only guards against nonsense input.
            if hi >= lo && hi - lo <= 100_000 {
                for j in lo..=hi {
                    let (bar, beat) = label(bar_start, j);
                    f(Beat {
                        pos: m.pos + j as f64 * len,
                        bar,
                        beat,
                    });
                }
            }
            if let Some(b) = beats {
                bar_start += (b + BEATS_PER_BAR - 1) / BEATS_PER_BAR;
            }
        }
    }

    // ------------------------------------------------------------ edits

    /// SET: a marker (a downbeat) at `pos`.
    ///
    /// On a grid nobody has edited yet, this moves the whole grid so that
    /// `pos` becomes its first marker (bar 1). After that, it moves the
    /// marker within half a beat of `pos` if there is one, or adds a new
    /// marker (with the tempo the grid already had there). None when the
    /// grid already has [`MAX_MARKERS`] markers.
    pub fn set_marker(&self, pos: f64) -> Option<Self> {
        let bpm = self.bpm_at(pos);
        if !self.edited {
            return Self::from_markers(&[Marker { pos, bpm }], true);
        }
        let mut v = self.markers().to_vec();
        let half = 30.0 / bpm;
        let near = (0..v.len())
            .filter(|&k| (v[k].pos - pos).abs() < half)
            .min_by(|&a, &b| (v[a].pos - pos).abs().total_cmp(&(v[b].pos - pos).abs()));
        match near {
            Some(k) => v[k].pos = pos,
            None if v.len() >= MAX_MARKERS => return None,
            None => v.push(Marker { pos, bpm }),
        }
        Self::from_markers(&v, true)
    }

    /// Remove marker `i`. None if it's the only one.
    pub fn remove_marker(&self, i: usize) -> Option<Self> {
        if self.len <= 1 || i >= self.len as usize {
            return None;
        }
        let mut v = self.markers().to_vec();
        v.remove(i);
        Self::from_markers(&v, true)
    }

    /// ADJUST: change section `i`'s tempo by `delta` BPM. Its marker stays
    /// put and the beats after it spread out or close up.
    pub fn adjust(&self, i: usize, delta: f64) -> Self {
        self.set_bpm(i, self.markers[i.min(self.len as usize - 1)].bpm + delta)
    }

    /// Multiply section `i`'s tempo (2.0 or 0.5 for double or half time).
    pub fn scale(&self, i: usize, factor: f64) -> Self {
        self.set_bpm(i, self.markers[i.min(self.len as usize - 1)].bpm * factor)
    }

    fn set_bpm(&self, i: usize, bpm: f64) -> Self {
        let mut g = *self;
        g.edited = true;
        if i < g.len as usize && bpm.is_finite() {
            g.markers[i].bpm = bpm.clamp(MIN_BPM, MAX_BPM);
        }
        g
    }

    /// SLIP: slide section `i` (its marker and every beat up to the next
    /// marker) by `delta` seconds, without changing its tempo. A marker
    /// can't pass its neighbours, and the first one can go at most one beat
    /// before the start of the track.
    pub fn slip(&self, i: usize, delta: f64) -> Self {
        let mut g = *self;
        g.edited = true;
        let n = g.len as usize;
        if i >= n || !delta.is_finite() {
            return g;
        }
        let m = g.markers[i];
        let lo = if i == 0 {
            -m.beat_len()
        } else {
            g.markers[i - 1].pos + MIN_GAP
        };
        let hi = if i + 1 < n {
            g.markers[i + 1].pos - MIN_GAP
        } else {
            f64::INFINITY
        };
        g.markers[i].pos = (m.pos + delta).clamp(lo, hi.max(lo));
        g
    }
}

// ---------------------------------------------------------------- serde

/// On disk and on the wire. `bpm` and `first_beat` are the v0.2 one-marker
/// form: still written (the first marker), so older versions can read a
/// cache, and still read, so a v0.2 cache loads.
#[derive(Serialize, Deserialize)]
struct Repr {
    bpm: f64,
    first_beat: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    markers: Vec<[f64; 2]>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    edited: bool,
}

impl Serialize for BeatGrid {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let m = self.markers();
        Repr {
            bpm: m[0].bpm,
            first_beat: m[0].pos,
            markers: if m.len() > 1 {
                m.iter().map(|m| [m.pos, m.bpm]).collect()
            } else {
                Vec::new()
            },
            edited: self.edited,
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for BeatGrid {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let r = Repr::deserialize(d)?;
        let markers: Vec<Marker> = if r.markers.is_empty() {
            vec![Marker {
                pos: r.first_beat,
                bpm: r.bpm,
            }]
        } else {
            r.markers
                .iter()
                .map(|&[pos, bpm]| Marker { pos, bpm })
                .collect()
        };
        BeatGrid::from_markers(&markers, r.edited)
            .ok_or_else(|| serde::de::Error::custom("beatgrid has no valid markers"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(markers: &[(f64, f64)]) -> BeatGrid {
        let m: Vec<Marker> = markers
            .iter()
            .map(|&(pos, bpm)| Marker { pos, bpm })
            .collect();
        BeatGrid::from_markers(&m, true).unwrap()
    }

    fn beats(g: &BeatGrid, from: f64, to: f64) -> Vec<Beat> {
        let mut v = Vec::new();
        g.beats_in(from, to, |b| v.push(b));
        v
    }

    #[test]
    fn beat_loops_start_on_the_grid_around_the_playhead() {
        // 120 BPM from 0.25 s: beats at 0.25, 0.75, 1.25…
        let g = BeatGrid::new(0.25, 120.0).unwrap();
        let close =
            |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9;
        assert!(close(g.beat_loop(1.6, 4.0), (1.25, 3.25)));
        assert!(close(g.beat_loop(1.25, 1.0), (1.25, 1.75)));
        // Shorter than a beat: the slice of the beat the playhead is in.
        assert!(close(g.beat_loop(1.6, 0.25), (1.5, 1.625)));
        assert!(close(g.beat_loop(1.6, 0.5), (1.5, 1.75)));
        // Before the first marker the beats carry on backwards.
        assert!(close(g.beat_loop(0.1, 1.0), (-0.25, 0.25)));
        // A later section uses its own tempo.
        let g = grid(&[(0.0, 120.0), (20.0, 150.0)]);
        assert!(close(g.beat_loop(20.5, 2.0), (20.4, 21.2)));
    }

    #[test]
    fn first_marker_is_bar_one_and_earlier_bars_count_down() {
        // 120 BPM: a beat every 0.5 s, a bar every 2 s, bar 1 at 10 s.
        let g = BeatGrid::new(10.0, 120.0).unwrap();
        assert_eq!(g.bar_beat_at(10.0), (1, 1));
        assert_eq!(g.bar_beat_at(10.6), (1, 2));
        assert_eq!(g.bar_beat_at(12.0), (2, 1));
        assert_eq!(g.bar_beat_at(9.9), (-1, 4));
        assert_eq!(g.bar_beat_at(8.0), (-1, 1));
        assert_eq!(g.bar_beat_at(7.99), (-2, 4));
        let b = beats(&g, 7.0, 10.5);
        let bars: Vec<(i64, u8)> = b.iter().map(|b| (b.bar, b.beat)).collect();
        assert_eq!(
            bars,
            [
                (-2, 3),
                (-2, 4),
                (-1, 1),
                (-1, 2),
                (-1, 3),
                (-1, 4),
                (1, 1),
                (1, 2)
            ]
        );
        assert!((b[0].pos - 7.0).abs() < 1e-9);
    }

    #[test]
    fn tempo_changes_at_a_marker_and_each_marker_starts_a_bar() {
        // 120 BPM from 0 s; 128 BPM from 30.25 s, 60.5 beats in. The first
        // section gets 61 beats (its last one, at 30.0 s, is half a beat
        // long), which is 16 bars counting the short one.
        let g = grid(&[(0.0, 120.0), (30.25, 128.0)]);
        assert_eq!(g.bpm_at(29.0), 120.0);
        assert_eq!(g.bpm_at(30.25), 128.0);
        assert_eq!(g.section(-1.0), 0);
        assert_eq!(g.bar_beat_at(30.25), (17, 1));
        assert_eq!(g.bar_beat_at(30.2), (16, 1), "the short last beat");
        let b = beats(&g, 29.0, 31.0);
        let pos: Vec<f64> = b.iter().map(|b| b.pos).collect();
        assert_eq!(pos.len(), 5);
        assert!((pos[0] - 29.0).abs() < 1e-9 && (pos[2] - 30.0).abs() < 1e-9);
        assert!((pos[3] - 30.25).abs() < 1e-9);
        assert!((pos[4] - (30.25 + 60.0 / 128.0)).abs() < 1e-9);
        // Beat count continues on whole bars, so half/double-time phase lines up.
        assert!((g.beat_at(30.25) - 64.0).abs() < 1e-9);
        assert!(g.phase(30.25, 0.25).abs() < 1e-9);
        assert!((g.next_beat(30.1) - 30.25).abs() < 1e-9);
        assert!((g.next_beat(30.26) - (30.25 + 60.0 / 128.0)).abs() < 1e-9);
    }

    #[test]
    fn set_moves_a_detected_grid_then_adds_markers() {
        let auto = BeatGrid::new(0.31, 124.0).unwrap();
        assert!(!auto.edited);
        // First SET: the whole grid moves so the playhead is bar 1.
        let g = auto.set_marker(95.0).unwrap();
        assert_eq!(
            g.markers(),
            &[Marker {
                pos: 95.0,
                bpm: 124.0
            }]
        );
        assert!(g.edited);
        assert_eq!(g.bar_beat_at(95.0), (1, 1));
        // Next SET far away adds a marker with the tempo already there.
        let g = g.set_marker(140.0).unwrap();
        assert_eq!(g.markers().len(), 2);
        assert_eq!(
            g.markers()[1],
            Marker {
                pos: 140.0,
                bpm: 124.0
            }
        );
        // SET just before the start makes a new bar 1.
        let g = g.set_marker(20.0).unwrap();
        assert_eq!(g.markers()[0].pos, 20.0);
        assert_eq!(g.bar_beat_at(20.0), (1, 1));
        // SET within half a beat of a marker moves it instead.
        let g = g.set_marker(140.1).unwrap();
        assert_eq!(g.markers().len(), 3);
        assert_eq!(g.markers()[2].pos, 140.1);
    }

    #[test]
    fn adjust_and_slip_only_touch_the_section_under_the_playhead() {
        let g = grid(&[(1.0, 120.0), (61.0, 120.0), (121.0, 120.0)]);
        let i = g.section(80.0);
        assert_eq!(i, 1);
        let a = g.adjust(i, 0.1);
        assert_eq!(a.markers()[0], g.markers()[0]);
        assert_eq!(a.markers()[2], g.markers()[2]);
        assert!((a.markers()[1].bpm - 120.1).abs() < 1e-9);
        assert_eq!(a.markers()[1].pos, 61.0, "adjust keeps the marker in place");
        let s = g.slip(i, 0.005);
        assert!((s.markers()[1].pos - 61.005).abs() < 1e-12);
        assert_eq!(s.markers()[1].bpm, 120.0);
        assert_eq!(
            (s.markers()[0], s.markers()[2]),
            (g.markers()[0], g.markers()[2])
        );
        // Markers can't slip past their neighbours.
        let far = g.slip(1, 500.0);
        assert!(far.markers()[1].pos < far.markers()[2].pos);
        assert_eq!(g.scale(0, 2.0).markers()[0].bpm, 240.0);
        let r = g.remove_marker(1).unwrap();
        assert_eq!(r.markers().len(), 2);
        assert!(BeatGrid::new(0.0, 120.0)
            .unwrap()
            .remove_marker(0)
            .is_none());
    }

    #[test]
    fn serde_round_trip_and_v02_cache_format() {
        let g = grid(&[(0.5, 120.0), (30.0, 126.5)]);
        let json = serde_json::to_string(&g).unwrap();
        assert_eq!(serde_json::from_str::<BeatGrid>(&json).unwrap(), g);
        let old: BeatGrid = serde_json::from_str(r#"{"bpm":124.0,"first_beat":0.137}"#).unwrap();
        assert_eq!(old, BeatGrid::new(0.137, 124.0).unwrap());
        assert!(serde_json::from_str::<BeatGrid>(r#"{"bpm":0.0,"first_beat":0.1}"#).is_err());
    }

    #[test]
    fn main_bpm_is_the_longest_section() {
        let g = grid(&[(0.5, 120.0), (30.0, 128.0)]);
        assert_eq!(g.main_bpm(300.0), 128.0);
        assert_eq!(g.main_bpm(40.0), 120.0);
    }
}
