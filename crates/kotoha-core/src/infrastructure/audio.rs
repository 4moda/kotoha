//! Audio preprocessing adapter: N-channel downmix to mono and resampling to 16 kHz `f32`.

use kotoha_protocol::{MAX_CHANNELS, MAX_SAMPLE_RATE, MIN_CHANNELS, MIN_SAMPLE_RATE};

use crate::domain::{AudioChunk, CoreError};

/// Target sample rate expected by downstream STT engines (`16000` Hz).
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Stateless audio preprocessor converting arbitrary interleaved PCM into 16 kHz mono `f32`.
#[derive(Debug, Default, Clone, Copy)]
pub struct AudioPreprocessor;

impl AudioPreprocessor {
    /// Downmixes interleaved N-channel PCM (`channels` in `1..=8`) to mono and resamples
    /// (`sample_rate` in `8000..=192000`) to `16000` Hz `f32`.
    ///
    /// - When `sample_rate == 16000` and `channels == 1`, returns an exact clone fast-path.
    /// - Multi-channel frames are averaged across channels (`sum / channels`).
    /// - Resampling uses linear interpolation over normalized frame positions, preserving
    ///   duration (`output_frames = round(input_frames * 16000 / sample_rate)`).
    pub fn process_chunk(chunk: &AudioChunk) -> Result<Vec<f32>, CoreError> {
        Self::process_interleaved(&chunk.samples, chunk.sample_rate, chunk.channels)
    }

    /// Processes a raw interleaved slice with explicit `sample_rate` and `channels`.
    pub fn process_interleaved(
        samples: &[f32],
        sample_rate: u32,
        channels: u16,
    ) -> Result<Vec<f32>, CoreError> {
        if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate) {
            return Err(CoreError::InvalidAudio(format!(
                "sample rate {sample_rate} out of range {MIN_SAMPLE_RATE}..={MAX_SAMPLE_RATE}"
            )));
        }
        if !(MIN_CHANNELS..=MAX_CHANNELS).contains(&channels) {
            return Err(CoreError::InvalidAudio(format!(
                "channel count {channels} out of range {MIN_CHANNELS}..={MAX_CHANNELS}"
            )));
        }

        let ch = usize::from(channels);
        if samples.len() % ch != 0 {
            return Err(CoreError::InvalidAudio(format!(
                "sample count {} is not divisible by channels {ch}",
                samples.len()
            )));
        }

        if samples.is_empty() {
            return Ok(Vec::new());
        }

        // Fast-path when input is already 16 kHz mono.
        if sample_rate == TARGET_SAMPLE_RATE && channels == 1 {
            return Ok(samples.to_vec());
        }

        let mono = if channels == 1 {
            samples.to_vec()
        } else {
            downmix_to_mono(samples, ch)
        };

        if sample_rate == TARGET_SAMPLE_RATE {
            return Ok(mono);
        }

        Ok(resample_linear(&mono, sample_rate, TARGET_SAMPLE_RATE))
    }

    /// Computes Root Mean Square (RMS) amplitude for a mono or interleaved PCM buffer.
    #[must_use]
    pub fn compute_rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum_sq: f64 = samples
            .iter()
            .map(|&s| {
                let v = f64::from(s);
                v * v
            })
            .sum();
        #[allow(clippy::cast_precision_loss)]
        let mean_sq = sum_sq / (samples.len() as f64);
        #[allow(clippy::cast_possible_truncation)]
        let rms = mean_sq.sqrt() as f32;
        rms
    }
}

fn downmix_to_mono(samples: &[f32], channels: usize) -> Vec<f32> {
    let frame_count = samples.len() / channels;
    let mut mono = Vec::with_capacity(frame_count);
    #[allow(clippy::cast_precision_loss)]
    let inv_ch = 1.0_f32 / (channels as f32);
    for frame in samples.chunks_exact(channels) {
        let sum: f32 = frame.iter().copied().sum();
        mono.push((sum * inv_ch).clamp(-1.0, 1.0));
    }
    mono
}

fn resample_linear(mono: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    let src_len = mono.len();
    if src_len == 0 {
        return Vec::new();
    }
    if src_len == 1 {
        return vec![mono[0]];
    }

    #[allow(clippy::cast_precision_loss)]
    let ratio = f64::from(dst_rate) / f64::from(src_rate);
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let dst_len = ((src_len as f64) * ratio).round().max(1.0) as usize;

    let mut out = Vec::with_capacity(dst_len);
    let step = f64::from(src_rate) / f64::from(dst_rate);
    let max_idx = src_len - 1;

    for i in 0..dst_len {
        #[allow(clippy::cast_precision_loss)]
        let src_pos = (i as f64) * step;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let idx = (src_pos.floor() as usize).min(max_idx);
        #[allow(clippy::cast_precision_loss)]
        let frac = (src_pos - (idx as f64)).clamp(0.0, 1.0);
        let s0 = f64::from(mono[idx]);
        let s1 = f64::from(mono[(idx + 1).min(max_idx)]);
        #[allow(clippy::cast_possible_truncation)]
        let interpolated = (s1 - s0).mul_add(frac, s0) as f32;
        out.push(interpolated.clamp(-1.0, 1.0));
    }

    out
}
