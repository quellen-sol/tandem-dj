//! Local monotonic time and the shared room clock.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

/// Microseconds since this process first asked for the time. Monotonic.
pub fn local_us() -> i64 {
    START.get_or_init(Instant::now).elapsed().as_micros() as i64
}

/// The room clock: this machine's monotonic clock plus an offset learned from
/// the host. The host's offset is always zero, so "shared time" is the host's
/// clock. All event timestamps are in shared time (microseconds).
#[derive(Debug)]
pub struct SharedClock {
    offset_us: AtomicI64,
    skew_us: i64,
    /// The latest [`SharedClock::stamp`] handed out.
    stamped_us: AtomicI64,
}

/// Longest [`SharedClock::stamp`] holds still after the offset steps back.
/// A new clock estimate moves it by milliseconds; a step further back than
/// this is another room's clock, taken at once.
const STAMP_HOLD_MAX_US: i64 = 500_000;

impl Default for SharedClock {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedClock {
    pub fn new() -> Self {
        Self::with_skew(0)
    }

    /// A clock whose local time is shifted by `skew_us`. Lets tests pretend
    /// that two sessions in one process live on different machines.
    pub fn with_skew(skew_us: i64) -> Self {
        Self {
            offset_us: AtomicI64::new(0),
            skew_us,
            stamped_us: AtomicI64::new(i64::MIN),
        }
    }

    /// This machine's own clock (not shared).
    pub fn local(&self) -> i64 {
        local_us() + self.skew_us
    }

    pub fn offset(&self) -> i64 {
        self.offset_us.load(Ordering::Relaxed)
    }

    /// A new estimate of the offset to the host, while in its room.
    pub fn set_offset(&self, v: i64) {
        self.offset_us.store(v, Ordering::Relaxed)
    }

    /// Take a room's clock from scratch (hosting, joining, getting back
    /// in): its offset, and stamps start over from it.
    pub fn start(&self, offset_us: i64) {
        self.offset_us.store(offset_us, Ordering::Relaxed);
        self.stamped_us.store(i64::MIN, Ordering::Relaxed);
    }

    /// Current shared (room) time in microseconds.
    pub fn now(&self) -> i64 {
        self.local() + self.offset()
    }

    /// Shared time to stamp a move this booth makes now: never earlier than
    /// the last stamp. A new clock estimate can step a joiner's offset back
    /// a few ms, and a move stamped before the one just made lands under
    /// it: the room keeps the earlier value, and soft takeover reads that
    /// as the partner moving the control and lets go of it (a crossfader
    /// cut stopped dead partway). Until the clock catches up, stamps hold
    /// still and the moves keep their order by sequence number.
    pub fn stamp(&self) -> i64 {
        let t = self.now();
        let mut last = self.stamped_us.load(Ordering::Relaxed);
        loop {
            let s = if last > t && last - t <= STAMP_HOLD_MAX_US {
                last
            } else {
                t
            };
            match self.stamped_us.compare_exchange_weak(
                last,
                s,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return s,
                Err(now_last) => last = now_last,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_hold_still_when_the_offset_steps_back() {
        let c = SharedClock::new();
        let a = c.stamp();
        // A new estimate, further back: stamps wait for the clock to catch up.
        c.set_offset(-200_000);
        let b = c.stamp();
        assert!(b >= a && c.now() < a, "a {a}, b {b}, now {}", c.now());
        assert!(c.stamp() >= b);
        // Further back than any estimate moves: another room's clock.
        c.set_offset(-60_000_000);
        assert!(c.stamp() < a - 50_000_000);
    }

    #[test]
    fn a_new_room_clock_starts_stamps_over() {
        let c = SharedClock::new();
        c.set_offset(200_000);
        let a = c.stamp();
        c.start(0);
        assert!(c.stamp() < a - 100_000);
    }
}
