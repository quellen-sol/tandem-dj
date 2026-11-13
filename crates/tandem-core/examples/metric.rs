use tandem_core::{analysis, audio, TrackId};
fn prior(b: f64) -> f64 {
    let x = (b / 126.0).log2() / 0.55;
    (-0.5 * x * x).exp()
}
fn main() {
    let weights: [(f64, f64, &str); 4] = [
        (0.0, 0.0, "plain"),
        (0.5, 0.5, "2x+half .5"),
        (0.5, 0.25, "2x .5 half .25"),
        (1.0, 0.5, "2x 1 half .5"),
    ];
    for p in std::env::args().skip(1) {
        let a = audio::decode_file(std::path::Path::new(&p), TrackId([0; 32])).unwrap();
        let sc = analysis::debug_scorer(&a);
        let mut raw = Vec::new();
        let mut b = 68.0;
        while b <= 190.0 {
            raw.push((b, sc(b)));
            b += 0.1;
        }
        let name: String = std::path::Path::new(&p)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .chars()
            .take(40)
            .collect();
        let mut line = format!("{name:<40}");
        for (w2, wh, label) in weights {
            let mut best = (0.0, -1.0);
            for &(b, s) in &raw {
                let v = (s + w2 * sc(b * 2.0) + wh * sc(b / 2.0)) * prior(b);
                if v > best.1 {
                    best = (b, v);
                }
            }
            line += &format!(" | {label}: {:6.1}", best.0);
        }
        println!("{line}");
    }
}
