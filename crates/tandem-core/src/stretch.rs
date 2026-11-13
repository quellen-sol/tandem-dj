//! Time-stretching and pitch-shifting (Signalsmith Stretch, vendored C++).
//!
//! Used for key lock (tempo changes without pitch changes) and key shift.

use std::os::raw::c_int;

#[repr(C)]
struct Raw {
    _private: [u8; 0],
}

extern "C" {
    fn tandem_stretch_create(channels: c_int, sample_rate: f32) -> *mut Raw;
    fn tandem_stretch_destroy(h: *mut Raw);
    fn tandem_stretch_reset(h: *mut Raw);
    fn tandem_stretch_input_latency(h: *mut Raw) -> c_int;
    fn tandem_stretch_output_latency(h: *mut Raw) -> c_int;
    fn tandem_stretch_set_transpose(h: *mut Raw, factor: f32);
    fn tandem_stretch_seek(h: *mut Raw, input: *mut f32, frames: c_int, rate: f64);
    fn tandem_stretch_process(
        h: *mut Raw,
        input: *mut f32,
        in_frames: c_int,
        output: *mut f32,
        out_frames: c_int,
    );
}

/// A stereo stretcher. Buffers are interleaved stereo `f32`.
pub struct Stretcher {
    raw: *mut Raw,
    transpose: f32,
}

// The stretcher owns its state exclusively; it's only ever used by one thread at a time.
unsafe impl Send for Stretcher {}

impl Stretcher {
    pub fn new(sample_rate: f32) -> Self {
        let raw = unsafe { tandem_stretch_create(2, sample_rate) };
        assert!(!raw.is_null());
        Self {
            raw,
            transpose: 1.0,
        }
    }

    pub fn reset(&mut self) {
        unsafe { tandem_stretch_reset(self.raw) }
    }

    /// Input frames to supply ahead of the moment being processed.
    pub fn input_latency(&self) -> usize {
        unsafe { tandem_stretch_input_latency(self.raw) as usize }
    }

    /// Output frames by which the result trails the moment being processed.
    pub fn output_latency(&self) -> usize {
        unsafe { tandem_stretch_output_latency(self.raw) as usize }
    }

    /// Pitch multiplier (1.0 = unchanged).
    pub fn set_transpose(&mut self, factor: f32) {
        if (factor - self.transpose).abs() > 1e-6 {
            self.transpose = factor;
            unsafe { tandem_stretch_set_transpose(self.raw, factor) }
        }
    }

    /// Prime with the audio leading up to where playback continues.
    pub fn seek(&mut self, input: &mut [f32], rate: f64) {
        unsafe {
            tandem_stretch_seek(
                self.raw,
                input.as_mut_ptr(),
                (input.len() / 2) as c_int,
                rate,
            )
        }
    }

    /// Consume `input` (any length) and produce exactly `output.len() / 2` frames.
    pub fn process(&mut self, input: &mut [f32], output: &mut [f32]) {
        unsafe {
            tandem_stretch_process(
                self.raw,
                input.as_mut_ptr(),
                (input.len() / 2) as c_int,
                output.as_mut_ptr(),
                (output.len() / 2) as c_int,
            )
        }
    }
}

impl Drop for Stretcher {
    fn drop(&mut self) {
        unsafe { tandem_stretch_destroy(self.raw) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strongest frequency in a stereo buffer (naive DFT scan).
    fn peak_freq(buf: &[f32], sr: f32) -> f32 {
        let mono: Vec<f32> = buf.chunks(2).map(|c| c[0]).collect();
        let mut best = (0.0, 0.0);
        let mut f = 100.0;
        while f < 2000.0 {
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for (i, x) in mono.iter().enumerate() {
                let ph = 2.0 * std::f32::consts::PI * f * i as f32 / sr;
                re += x * ph.cos();
                im += x * ph.sin();
            }
            let m = re * re + im * im;
            if m > best.1 {
                best = (f, m);
            }
            f += 2.0;
        }
        best.0
    }

    #[test]
    fn stretches_time_without_changing_pitch() {
        let sr = 44_100.0;
        let tone: Vec<f32> = (0..sr as usize * 2)
            .flat_map(|i| {
                let v = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sr).sin() * 0.5;
                [v, v]
            })
            .collect();
        let mut s = Stretcher::new(sr);
        // Play 10% faster: consume 1.1 input frames per output frame.
        let n_out = 8820;
        let n_in = (n_out as f32 * 1.1) as usize;
        let mut input = tone[..n_in * 2].to_vec();
        let mut out = vec![0.0f32; n_out * 2];
        s.process(&mut input, &mut out);
        let tail = &out[(n_out / 2) * 2..];
        let f = peak_freq(tail, sr);
        assert!((f - 440.0).abs() < 6.0, "pitch moved to {f} Hz");
        // Shift up a fifth (x1.5) without changing speed.
        s.set_transpose(1.5);
        let mut input = tone[n_in * 2..(n_in * 2 + n_out * 2)].to_vec();
        s.process(&mut input, &mut out);
        let f = peak_freq(&out[(n_out / 2) * 2..], sr);
        assert!((f - 660.0).abs() < 8.0, "transposed to {f} Hz");
    }
}
