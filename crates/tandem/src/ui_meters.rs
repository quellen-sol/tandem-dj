//! Level meters on screen: LED-style bars in dBFS, with a peak hold.
//!
//! The levels come from the live lane (`tandem_core::meter`). The main
//! window shows them as they are; the broadcast window shows them as they
//! were a broadcast delay ago, in step with the feed.

use eframe::egui::{pos2, Align2, Color32, FontId, Painter, Rect};
use std::collections::VecDeque;
use tandem_core::meter::{to_db, Levels, FALL_DB_PER_S};
use tandem_core::model::DECKS;

/// The level (dBFS) at which each light comes on, bottom to top. Closer
/// together near the top, where gain matching happens.
const SEGMENTS_DB: [f32; 16] = [
    -40.0, -34.0, -28.0, -24.0, -20.0, -17.0, -14.0, -12.0, -10.0, -8.0, -6.0, -4.5, -3.0, -2.0,
    -1.0, 0.0,
];
/// Marks on the master meter's scale.
const SCALE_DB: [f32; 4] = [0.0, -6.0, -12.0, -24.0];
/// A peak stays lit this long, then falls like the meter.
const HOLD_US: i64 = 1_000_000;

const GREEN: Color32 = Color32::from_rgb(70, 205, 100);
const AMBER: Color32 = Color32::from_rgb(250, 180, 50);
const RED: Color32 = Color32::from_rgb(240, 70, 60);

/// Red over full scale, amber in the top 9 dB, green below.
fn color(db: f32) -> Color32 {
    if db >= 0.0 {
        RED
    } else if db >= -9.0 {
        AMBER
    } else {
        GREEN
    }
}

#[derive(Clone, Copy, Debug)]
struct Meter {
    db: f32,
    hold_db: f32,
    hold_at: i64,
}

impl Default for Meter {
    fn default() -> Self {
        Self {
            db: -120.0,
            hold_db: -120.0,
            hold_at: 0,
        }
    }
}

/// What one window's meters show: each level and its held peak. Channels
/// first, then master left and right.
#[derive(Clone, Copy, Debug, Default)]
pub struct MeterView {
    meters: [Meter; DECKS + 2],
    last: i64,
}

impl MeterView {
    /// Take new levels at local time `t` (`time::local_us()`).
    pub fn update(&mut self, levels: &Levels, t: i64) {
        let dt = (t - self.last).clamp(0, 200_000) as f32 * 1e-6;
        self.last = t;
        let all = levels.channels.iter().chain(levels.master.iter());
        for (m, &level) in self.meters.iter_mut().zip(all) {
            m.db = to_db(level);
            if m.db >= m.hold_db {
                m.hold_db = m.db;
                m.hold_at = t;
            } else if t - m.hold_at > HOLD_US {
                m.hold_db = (m.hold_db - FALL_DB_PER_S * dt).max(m.db);
            }
        }
    }

    /// Draw channel `d`'s meter (before its fader).
    pub fn channel(&self, p: &Painter, rect: Rect, d: usize) {
        bar(p, rect, self.meters[d.min(DECKS - 1)]);
    }

    /// Draw the stereo master meter: two bars side by side.
    pub fn master(&self, p: &Painter, rect: Rect) {
        let gap = 2.0;
        let w = (rect.width() - gap) / 2.0;
        for c in 0..2 {
            let x = rect.left() + c as f32 * (w + gap);
            let r = Rect::from_min_max(pos2(x, rect.top()), pos2(x + w, rect.bottom()));
            bar(p, r, self.meters[DECKS + c]);
        }
    }
}

/// A vertical LED bar filling `rect`.
fn bar(p: &Painter, rect: Rect, m: Meter) {
    let n = SEGMENTS_DB.len();
    for (i, &from) in SEGMENTS_DB.iter().enumerate() {
        let lit = m.db >= from;
        let next = SEGMENTS_DB.get(i + 1).copied().unwrap_or(f32::INFINITY);
        let held = m.hold_db >= from && m.hold_db < next;
        let col = color(from);
        let fill = if lit || held {
            col
        } else {
            col.gamma_multiply(0.15)
        };
        p.rect_filled(segment(rect, i, n), 1.0, fill);
    }
}

fn segment(rect: Rect, i: usize, n: usize) -> Rect {
    let gap = if rect.height() / n as f32 >= 4.0 {
        1.0
    } else {
        0.0
    };
    let h = (rect.height() - gap * (n - 1) as f32) / n as f32;
    let bottom = rect.bottom() - i as f32 * (h + gap);
    Rect::from_min_max(pos2(rect.left(), bottom - h), pos2(rect.right(), bottom))
}

/// dB marks for a meter in `meter`, right-aligned at `x`.
pub fn scale(p: &Painter, meter: Rect, x: f32, size: f32) {
    let n = SEGMENTS_DB.len();
    for db in SCALE_DB {
        let Some(i) = SEGMENTS_DB.iter().position(|&s| s == db) else {
            continue;
        };
        p.text(
            pos2(x, segment(meter, i, n).center().y),
            Align2::RIGHT_CENTER,
            format!("{db:.0}"),
            FontId::monospace(size),
            Color32::from_gray(130),
        );
    }
}

/// Recent levels, so the broadcast window can show them as late as the feed.
/// Keyed by local time, which never jumps (the room clock does on joining).
#[derive(Default)]
pub struct MeterHistory {
    samples: VecDeque<(i64, Levels)>,
}

impl MeterHistory {
    /// Record `levels` at local time `t`, keeping `keep_us` of history.
    pub fn push(&mut self, t: i64, levels: Levels, keep_us: i64) {
        self.samples.push_back((t, levels));
        while self.samples.len() > 1 && self.samples[0].0 < t - keep_us {
            self.samples.pop_front();
        }
    }

    /// The levels as they were at local time `t`.
    pub fn at(&self, t: i64) -> Levels {
        self.samples
            .iter()
            .rev()
            .find(|(s, _)| *s <= t)
            .map(|(_, l)| *l)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(db: f32) -> f32 {
        10f32.powf(db / 20.0)
    }

    #[test]
    fn peaks_hold_then_fall() {
        let mut v = MeterView::default();
        let mut l = Levels::default();
        l.channels[0] = level(-3.0);
        v.update(&l, 1_000_000);
        l.channels[0] = level(-30.0);
        v.update(&l, 1_500_000);
        let m = v.meters[0];
        assert!((m.db + 30.0).abs() < 0.01);
        assert!((m.hold_db + 3.0).abs() < 0.01, "still held");
        // After the hold, it falls at the meter's rate, 20 ms a frame.
        let mut t = 1_500_000;
        while t < 2_500_000 {
            t += 20_000;
            v.update(&l, t);
        }
        let fell = -3.0 - v.meters[0].hold_db;
        assert!((fell - FALL_DB_PER_S * 0.5).abs() < 0.6, "{fell}");
        assert!(v.meters[0].hold_db >= -30.0);
    }

    #[test]
    fn history_is_as_late_as_asked() {
        let mut h = MeterHistory::default();
        for i in 0..100 {
            let l = Levels {
                master: [i as f32 / 100.0; 2],
                ..Default::default()
            };
            h.push(i * 10_000, l, 600_000);
        }
        // Now 990 ms; 500 ms ago the master read 0.49.
        assert_eq!(h.at(490_000).master[0], 0.49);
        assert_eq!(h.at(495_000).master[0], 0.49);
        // Older than the history keeps: nothing.
        assert_eq!(h.at(100_000), Levels::default());
        assert!(h.samples.len() <= 61);
    }

    #[test]
    fn colours_by_level() {
        assert_eq!(color(0.0), RED);
        assert_eq!(color(-1.0), AMBER);
        assert_eq!(color(-8.0), AMBER);
        assert_eq!(color(-10.0), GREEN);
        assert!(SEGMENTS_DB.windows(2).all(|w| w[0] < w[1]));
        assert!(SCALE_DB.iter().all(|d| SEGMENTS_DB.contains(d)));
    }
}
