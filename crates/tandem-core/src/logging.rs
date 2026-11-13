//! A small logger: writes to a file and keeps recent lines for the UI.

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;

struct Logger {
    file: Mutex<Option<std::fs::File>>,
    recent: Mutex<VecDeque<String>>,
    echo: bool,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info || m.target().starts_with("tandem")
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) || r.level() > log::Level::Debug {
            return;
        }
        // Keep third-party chatter out of the UI.
        if !r.target().starts_with("tandem") && r.level() > log::Level::Warn {
            return;
        }
        let secs = crate::time::local_us() as f64 / 1e6;
        let line = format!("[{secs:9.3}] {:<5} {}", r.level(), r.args());
        if self.echo {
            eprintln!("{line}");
        }
        if let Some(f) = self.file.lock().as_mut() {
            let _ = writeln!(f, "{line}");
        }
        if r.level() <= log::Level::Info {
            let mut q = self.recent.lock();
            q.push_back(format!("{}", r.args()));
            while q.len() > 200 {
                q.pop_front();
            }
        }
    }

    fn flush(&self) {
        if let Some(f) = self.file.lock().as_mut() {
            let _ = f.flush();
        }
    }
}

/// Install the logger (once). `echo` also prints to stderr.
pub fn init(file: Option<&Path>, echo: bool) {
    let f = file.and_then(|p| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .ok()
    });
    let logger = LOGGER.get_or_init(|| Logger {
        file: Mutex::new(f),
        recent: Mutex::new(VecDeque::new()),
        echo,
    });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
    }
}

/// The most recent `n` info-level lines, oldest first.
pub fn recent(n: usize) -> Vec<String> {
    match LOGGER.get() {
        Some(l) => {
            let q = l.recent.lock();
            q.iter().skip(q.len().saturating_sub(n)).cloned().collect()
        }
        None => Vec::new(),
    }
}
