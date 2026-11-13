//! Clock-offset estimation (NTP-style).
//!
//! The client sends `Ping{t1}` stamped with its own clock; the host answers
//! with its clock reading; the client notes the arrival time `t2`. If the
//! network were symmetric, the host's reading happened at the midpoint of
//! `t1..t2`, so `offset = remote - (t1 + t2) / 2`. Queues and Wi-Fi make
//! trips asymmetric; the trips with the smallest round-trip time have the
//! least room for asymmetry, so we keep a window of samples and trust the
//! fastest one.

use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockSample {
    pub rtt_us: i64,
    pub offset_us: i64,
}

#[derive(Clone, Debug)]
pub struct ClockEstimator {
    samples: VecDeque<ClockSample>,
    cap: usize,
}

impl Default for ClockEstimator {
    fn default() -> Self {
        Self::new(40)
    }
}

impl ClockEstimator {
    pub fn new(cap: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(cap),
            cap: cap.max(1),
        }
    }

    /// `t1`: local send time, `remote`: remote clock when it answered,
    /// `t2`: local receive time. Returns the current best estimate.
    pub fn add(&mut self, t1: i64, remote: i64, t2: i64) -> Option<ClockSample> {
        let rtt = t2 - t1;
        if rtt < 0 {
            return self.best();
        }
        self.samples.push_back(ClockSample {
            rtt_us: rtt,
            offset_us: remote - (t1 + rtt / 2),
        });
        while self.samples.len() > self.cap {
            self.samples.pop_front();
        }
        self.best()
    }

    /// The sample with the smallest round trip.
    pub fn best(&self) -> Option<ClockSample> {
        self.samples.iter().copied().min_by_key(|s| s.rtt_us)
    }

    pub fn last_rtt(&self) -> Option<i64> {
        self.samples.back().map(|s| s.rtt_us)
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Rough accuracy indicator: how far apart the offsets of the fastest
    /// third of samples are. Small = the estimate is trustworthy.
    pub fn spread(&self) -> Option<i64> {
        if self.samples.len() < 3 {
            return None;
        }
        let mut v: Vec<ClockSample> = self.samples.iter().copied().collect();
        v.sort_by_key(|s| s.rtt_us);
        let n = (v.len() / 3).max(2);
        let lo = v[..n].iter().map(|s| s.offset_us).min()?;
        let hi = v[..n].iter().map(|s| s.offset_us).max()?;
        Some(hi - lo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate a jittery, asymmetric link and check the estimate converges.
    #[test]
    fn converges_on_jittery_link() {
        let true_offset: i64 = 3_600_000_000; // remote is an hour "ahead"
        let mut est = ClockEstimator::new(40);
        let mut rng = fastrand::Rng::with_seed(7);
        let mut local = 1_000_000i64;
        for _ in 0..40 {
            // 20 ms base one-way delay, plus 0-30 ms of queueing each way.
            let up = 20_000 + rng.i64(0..30_000);
            let down = 20_000 + rng.i64(0..30_000);
            let t1 = local;
            let remote = t1 + up + true_offset;
            let t2 = t1 + up + down;
            est.add(t1, remote, t2);
            local += 250_000;
        }
        let best = est.best().unwrap();
        let err = (best.offset_us - true_offset).abs();
        assert!(err < 5_000, "offset error {err}us too large");
        assert!(best.rtt_us < 50_000);
    }

    #[test]
    fn ignores_negative_rtt() {
        let mut est = ClockEstimator::new(4);
        assert!(est.add(10, 0, 5).is_none());
        assert!(est.add(0, 100, 10).is_some());
    }
}
