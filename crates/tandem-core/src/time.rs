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
}

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
        }
    }

    /// This machine's own clock (not shared).
    pub fn local(&self) -> i64 {
        local_us() + self.skew_us
    }

    pub fn offset(&self) -> i64 {
        self.offset_us.load(Ordering::Relaxed)
    }

    pub fn set_offset(&self, v: i64) {
        self.offset_us.store(v, Ordering::Relaxed)
    }

    /// Current shared (room) time in microseconds.
    pub fn now(&self) -> i64 {
        self.local() + self.offset()
    }
}
