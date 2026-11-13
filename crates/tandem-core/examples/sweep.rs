use tandem_core::{analysis, audio, TrackId};
fn main() {
    let p = std::env::args().nth(1).unwrap();
    let a = audio::decode_file(std::path::Path::new(&p), TrackId([0; 32])).unwrap();
    println!("duration {:.1}s", a.duration());
    let s = analysis::sweep(&a, 68.0, 190.0, 0.05);
    // local maxima, top 12 by raw score
    let mut peaks: Vec<_> = (1..s.len() - 1)
        .filter(|&i| s[i].1 >= s[i - 1].1 && s[i].1 >= s[i + 1].1)
        .map(|i| s[i])
        .collect();
    peaks.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    for (b, raw, wp) in peaks.iter().take(12) {
        println!("{b:7.2} bpm  pulse {raw:5.2}  with prior {wp:5.2}");
    }
}
