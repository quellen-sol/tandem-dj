//! Print the detected BPM and first beat for audio files.
//! Usage: cargo run --release --example analyze -- <file-or-folder> [max files]
use std::path::PathBuf;
use std::time::Instant;
use tandem_core::{analysis, audio, library, TrackId};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(args.get(1).expect("path"));
    let max: usize = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);
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
    for p in files.into_iter().take(max) {
        let t = Instant::now();
        let a = match audio::decode_file(&p, TrackId([0; 32])) {
            Ok(a) => a,
            Err(e) => {
                println!("{:<60} decode error {e}", name(&p));
                continue;
            }
        };
        let decoded = t.elapsed();
        let g = analysis::analyze(&a, None);
        let txt = match g {
            Some(g) => format!("{:7.2} BPM  first beat {:6.3}s", g.bpm, g.first_beat),
            None => "   no steady beat".to_string(),
        };
        println!(
            "{:<60} {txt}  (decode {:.2}s, analyze {:.2}s)",
            name(&p),
            decoded.as_secs_f64(),
            (t.elapsed() - decoded).as_secs_f64()
        );
    }
}

fn name(p: &std::path::Path) -> String {
    p.file_stem()
        .unwrap()
        .to_string_lossy()
        .chars()
        .take(58)
        .collect()
}
