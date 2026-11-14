//! Print the detected BPM, first beat and key for audio files, one
//! tab-separated line per file (in name order), analysing several at once.
//! Usage: cargo run --release --example analyze -- <file-or-folder> [max files] [threads]
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;
use tandem_core::{analysis, audio, keys, library, TrackId};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(args.get(1).expect("path"));
    let max: usize = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);
    let threads: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(6);
    let mut files: Vec<PathBuf> = if root.is_dir() {
        std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| library::is_audio(p))
            .collect()
    } else {
        vec![root]
    };
    files.sort();
    files.truncate(max);
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<String>>> = Mutex::new(vec![None; files.len()]);
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(p) = files.get(i) else { break };
                let line = analyze_one(p);
                eprintln!("{line}");
                out.lock().unwrap()[i] = Some(line);
            });
        }
    });
    for line in out.into_inner().unwrap().into_iter().flatten() {
        println!("{line}");
    }
}

fn analyze_one(p: &std::path::Path) -> String {
    let t = Instant::now();
    let a = match audio::decode_file(p, TrackId([0; 32])) {
        Ok(a) => a,
        Err(e) => return format!("{}\tdecode error {e}", name(p)),
    };
    let decoded = t.elapsed();
    let g = analysis::analyze(&a);
    let key = keys::detect(&a)
        .map(|k| format!("{}\t{}", keys::camelot(k), keys::name(k)))
        .unwrap_or_else(|| "?\t?".into());
    let grid = match g {
        Some(g) => {
            let sections: Vec<String> = g
                .markers()
                .iter()
                .map(|m| format!("{:.2}@{:.1}s", m.bpm, m.pos))
                .collect();
            format!(
                "{:.2}\t{:.3}\t{}",
                g.main_bpm(a.duration()),
                g.first_beat(),
                sections.join(" ")
            )
        }
        None => "-\t-\t-".to_string(),
    };
    format!(
        "{}\t{grid}\t{key}\t{:.1}s\t{:.2}s",
        name(p),
        a.duration(),
        (t.elapsed() - decoded).as_secs_f64()
    )
}

fn name(p: &std::path::Path) -> String {
    p.file_stem().unwrap().to_string_lossy().to_string()
}
