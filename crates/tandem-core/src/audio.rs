//! Decoded tracks held in memory, and decoding from files.

use crate::model::TrackId;
use anyhow::{anyhow, bail, Context};
use std::io::Write;
use std::path::Path;

/// Frames per entry in [`TrackAudio::peaks`] (about 6 ms at 44.1 kHz).
pub const PEAK_BLOCK: usize = 256;
pub const OVERVIEW_BUCKETS: usize = 1024;

/// A whole track decoded to 16-bit stereo (half the memory of f32; plenty
/// for playback). About 60 MB for a 6-minute track.
pub struct TrackAudio {
    pub id: TrackId,
    pub sample_rate: u32,
    pub frames: Vec<[i16; 2]>,
    /// Peak level (0-255) of each PEAK_BLOCK frames, for the scrolling waveform.
    pub peaks: Vec<u8>,
    /// Peak level of OVERVIEW_BUCKETS equal slices, for the overview strip.
    pub overview: Vec<u8>,
}

impl TrackAudio {
    pub fn new(id: TrackId, sample_rate: u32, frames: Vec<[i16; 2]>) -> Self {
        let peaks: Vec<u8> = frames
            .chunks(PEAK_BLOCK)
            .map(|c| {
                let m = c
                    .iter()
                    .map(|f| f[0].unsigned_abs().max(f[1].unsigned_abs()))
                    .max()
                    .unwrap_or(0);
                (m >> 7).min(255) as u8
            })
            .collect();
        let mut overview = vec![0u8; OVERVIEW_BUCKETS];
        if !peaks.is_empty() {
            for (i, o) in overview.iter_mut().enumerate() {
                let a = i * peaks.len() / OVERVIEW_BUCKETS;
                let b = ((i + 1) * peaks.len() / OVERVIEW_BUCKETS)
                    .max(a + 1)
                    .min(peaks.len());
                *o = peaks[a.min(peaks.len() - 1)..b]
                    .iter()
                    .copied()
                    .max()
                    .unwrap_or(0);
            }
        }
        Self {
            id,
            sample_rate,
            frames,
            peaks,
            overview,
        }
    }

    pub fn duration(&self) -> f64 {
        self.frames.len() as f64 / self.sample_rate as f64
    }

    /// One frame as f32 (silence outside the track).
    #[inline]
    pub(crate) fn frame(&self, i: isize) -> [f32; 2] {
        if i < 0 || i as usize >= self.frames.len() {
            return [0.0; 2];
        }
        let f = self.frames[i as usize];
        [f[0] as f32 * (1.0 / 32768.0), f[1] as f32 * (1.0 / 32768.0)]
    }

    /// Stereo sample at fractional frame `pos` (cubic Hermite interpolation).
    #[inline]
    pub fn sample(&self, pos: f64) -> [f32; 2] {
        self.sample_with(pos, |i| self.frame(i))
    }

    /// Like [`TrackAudio::sample`], reading frames through `frame` (which
    /// can remap them, e.g. round a loop).
    #[inline]
    pub(crate) fn sample_with(&self, pos: f64, frame: impl Fn(isize) -> [f32; 2]) -> [f32; 2] {
        let i = pos.floor() as isize;
        let x = (pos - i as f64) as f32;
        let (y0, y1, y2, y3) = (frame(i - 1), frame(i), frame(i + 1), frame(i + 2));
        let mut out = [0.0; 2];
        for c in 0..2 {
            let c0 = y1[c];
            let c1 = 0.5 * (y2[c] - y0[c]);
            let c2 = y0[c] - 2.5 * y1[c] + 2.0 * y2[c] - 0.5 * y3[c];
            let c3 = 0.5 * (y3[c] - y0[c]) + 1.5 * (y1[c] - y2[c]);
            out[c] = ((c3 * x + c2) * x + c1) * x + c0;
        }
        out
    }
}

#[inline]
fn to_i16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// Decode any supported file (MP3, AAC/M4A, FLAC, WAV, AIFF, OGG, ALAC) to
/// stereo. Every peer uses this same decoder, so every peer gets the same
/// samples and the same length.
pub fn decode_file(path: &Path, id: TrackId) -> anyhow::Result<TrackAudio> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::errors::Error as SymError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let fmt_opts = FormatOptions {
        enable_gapless: true,
        ..Default::default()
    };
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &fmt_opts, &MetadataOptions::default())
        .context("unrecognised audio format")?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow!("no audio track"))?;
    let track_id = track.id;
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| anyhow!("unknown sample rate"))?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("unsupported codec")?;
    let mut frames: Vec<[i16; 2]> =
        Vec::with_capacity(track.codec_params.n_frames.unwrap_or(0) as usize);

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymError::DecodeError(_)) => continue, // skip a corrupt frame
            Err(e) => return Err(e.into()),
        };
        let spec = *decoded.spec();
        let ch = spec.channels.count().max(1);
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        for f in buf.samples().chunks_exact(ch) {
            let l = f[0];
            let r = if ch > 1 { f[1] } else { f[0] };
            frames.push([to_i16(l), to_i16(r)]);
        }
    }
    if frames.is_empty() {
        bail!("no audio could be decoded");
    }
    Ok(TrackAudio::new(id, sample_rate, frames))
}

/// Write 16-bit stereo PCM WAV (used by the self-test).
pub fn write_wav(path: &Path, sample_rate: u32, frames: &[[i16; 2]]) -> std::io::Result<()> {
    let data_len = (frames.len() * 4) as u32;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&2u16.to_le_bytes())?; // channels
    f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&(sample_rate * 4).to_le_bytes())?;
    f.write_all(&4u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for fr in frames {
        f.write_all(&fr[0].to_le_bytes())?;
        f.write_all(&fr[1].to_le_bytes())?;
    }
    f.flush()
}

/// A test signal: a quiet tone with a kick-like click every half second.
pub fn test_signal(sample_rate: u32, seconds: f64, freq: f64) -> Vec<[i16; 2]> {
    let n = (sample_rate as f64 * seconds) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / sample_rate as f64;
            let beat = t % 0.5;
            let kick = if beat < 0.05 {
                (1.0 - beat / 0.05) * (2.0 * std::f64::consts::PI * 60.0 * t).sin() * 0.6
            } else {
                0.0
            };
            let tone = (2.0 * std::f64::consts::PI * freq * t).sin() * 0.2;
            let v = to_i16((kick + tone) as f32);
            [v, v]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_roundtrip_decodes_exactly() {
        let dir = std::env::temp_dir().join(format!("tandem-audio-test-{}", fastrand::u64(..)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        let frames = test_signal(44_100, 2.0, 440.0);
        write_wav(&path, 44_100, &frames).unwrap();
        let a = decode_file(&path, TrackId([0; 32])).unwrap();
        assert_eq!(a.sample_rate, 44_100);
        assert_eq!(a.frames.len(), frames.len());
        assert_eq!(a.frames[1000], frames[1000]);
        assert!((a.duration() - 2.0).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn interpolation_hits_exact_frames() {
        let frames: Vec<[i16; 2]> = (0..100)
            .map(|i| [(i * 100) as i16, -(i * 100) as i16])
            .collect();
        let a = TrackAudio::new(TrackId([0; 32]), 44_100, frames);
        let s = a.sample(10.0);
        assert!((s[0] - 1000.0 / 32768.0).abs() < 1e-6);
        let mid = a.sample(10.5);
        assert!((mid[0] - 1050.0 / 32768.0).abs() < 1e-5);
    }
}
