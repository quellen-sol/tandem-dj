//! How much CPU key lock costs: stretch 20 s of stereo audio in 64-frame
//! blocks (as the audio engine does) and compare with real time.
use std::time::Instant;
use tandem_core::stretch::Stretcher;
fn main() {
    let sr = 44_100usize;
    let secs = 20;
    let mut s = Stretcher::new(sr as f32);
    s.set_transpose(1.0);
    let tone: Vec<f32> = (0..sr * (secs + 5) * 2)
        .map(|i| ((i as f32) * 0.013).sin() * 0.3)
        .collect();
    let mut input = vec![0f32; 256];
    let mut out = vec![0f32; 128];
    let rate = 1.06;
    let mut carry = 0.0f64;
    let mut pos = 0usize;
    let t = Instant::now();
    for _ in 0..(sr * secs / 64) {
        let want = carry + 64.0 * rate;
        let n = want.floor() as usize;
        carry = want - n as f64;
        input[..n * 2].copy_from_slice(&tone[pos * 2..(pos + n) * 2]);
        pos += n;
        s.process(&mut input[..n * 2], &mut out);
    }
    let el = t.elapsed().as_secs_f64();
    println!(
        "{secs} s of audio stretched in {:.3} s: {:.1}% of one core per key-locked deck",
        el,
        el / secs as f64 * 100.0
    );
}
