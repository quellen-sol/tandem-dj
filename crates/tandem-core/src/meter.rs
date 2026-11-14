//! Level meters for the mixer, on screen and on the controller.
//!
//! The live lane measures two things, like a DJ mixer:
//!
//! * **Each channel before its fader**: after trim, EQ and filter, so the
//!   meter shows what TRIM is for and still moves with the fader down.
//! * **The master bus**: the channels after their faders and the
//!   crossfader, before this booth's own master level. Both booths hear the
//!   same mix, so both see the same master meter, whatever their speakers
//!   are set to.
//!
//! The audio thread keeps each level as a peak with an instant rise and a
//! steady fall, and publishes it after every block. The screen and the
//! controller read it whenever they redraw, so neither needs a history.

use crate::model::DECKS;
use crate::time::local_us;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};

/// How fast a meter falls after a peak.
pub const FALL_DB_PER_S: f32 = 24.0;
/// A meter the audio hasn't updated for this long reads silent (the sound
/// card stopped or was unplugged).
const STALE_US: i64 = 250_000;

/// Peak levels, linear (1.0 = full scale).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Levels {
    /// Each channel, before its fader.
    pub channels: [f32; DECKS],
    /// The master bus, left and right.
    pub master: [f32; 2],
}

/// Where the audio thread leaves the latest [`Levels`] for anyone to read.
pub struct LevelMeters {
    channels: [AtomicU32; DECKS],
    master: [AtomicU32; 2],
    /// `time::local_us()` of the last update.
    stamp: AtomicI64,
}

impl Default for LevelMeters {
    fn default() -> Self {
        Self {
            channels: std::array::from_fn(|_| AtomicU32::new(0)),
            master: std::array::from_fn(|_| AtomicU32::new(0)),
            stamp: AtomicI64::new(i64::MIN / 2),
        }
    }
}

impl LevelMeters {
    pub fn new() -> Self {
        Self::default()
    }

    /// The latest levels; silence if the audio has stopped updating them.
    pub fn read(&self) -> Levels {
        if local_us() - self.stamp.load(Ordering::Relaxed) > STALE_US {
            return Levels::default();
        }
        let get = |a: &AtomicU32| f32::from_bits(a.load(Ordering::Relaxed));
        Levels {
            channels: std::array::from_fn(|d| get(&self.channels[d])),
            master: std::array::from_fn(|c| get(&self.master[c])),
        }
    }

    pub(crate) fn publish(&self, l: &Levels) {
        for d in 0..DECKS {
            self.channels[d].store(l.channels[d].to_bits(), Ordering::Relaxed);
        }
        for c in 0..2 {
            self.master[c].store(l.master[c].to_bits(), Ordering::Relaxed);
        }
        self.stamp.store(local_us(), Ordering::Relaxed);
    }
}

/// dBFS of a linear level, never below -120.
pub fn to_db(level: f32) -> f32 {
    20.0 * level.max(1e-6).log10()
}

/// Peak followers for every meter, run on the audio thread.
pub(crate) struct Ballistics {
    pub levels: Levels,
    /// The level a meter keeps after one frame of silence.
    fall_per_frame: f32,
}

impl Ballistics {
    pub fn new(sample_rate: f64) -> Self {
        Self {
            levels: Levels::default(),
            fall_per_frame: 10f32.powf(-FALL_DB_PER_S / 20.0 / sample_rate as f32),
        }
    }

    /// Fold in the peaks of a block of `frames` frames.
    pub fn block(&mut self, peaks: &Levels, frames: usize) {
        let fall = self.fall_per_frame.powi(frames as i32);
        let follow = |level: &mut f32, peak: f32| *level = peak.max(*level * fall);
        for d in 0..DECKS {
            follow(&mut self.levels.channels[d], peaks.channels[d]);
        }
        for c in 0..2 {
            follow(&mut self.levels.master[c], peaks.master[c]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rises_at_once_and_falls_steadily() {
        let sr = 48_000.0;
        let mut b = Ballistics::new(sr);
        let mut peaks = Levels::default();
        peaks.channels[0] = 0.5;
        b.block(&peaks, 64);
        assert_eq!(b.levels.channels[0], 0.5);
        // Half a second of silence: 12 dB down.
        let silent = Levels::default();
        for _ in 0..375 {
            b.block(&silent, 64);
        }
        let db = to_db(b.levels.channels[0]) - to_db(0.5);
        assert!((db + 12.0).abs() < 0.01, "{db}");
        assert_eq!(b.levels.channels[1], 0.0);
    }

    #[test]
    fn stale_meters_read_silent() {
        let m = LevelMeters::new();
        assert_eq!(m.read(), Levels::default());
        let l = Levels {
            channels: [0.25, 0.5],
            master: [0.75, 1.0],
        };
        m.publish(&l);
        assert_eq!(m.read(), l);
        m.stamp.store(local_us() - 2 * STALE_US, Ordering::Relaxed);
        assert_eq!(m.read(), Levels::default());
    }
}
