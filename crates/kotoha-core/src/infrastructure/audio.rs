//! Audio preprocessing adapter: N-channel downmix to mono and anti-aliased, stateful resampling
//! to 16 kHz `f32`.
//!
//! [`StreamResampler`] is owned by exactly one speaker worker. Its FIR filter history carries
//! across chunks, so chunk boundaries leave no discontinuity and the output equals what a single
//! call over the whole stream would produce.

use std::f64::consts::PI;
use std::sync::Arc;

use kotoha_protocol::{MAX_CHANNELS, MAX_SAMPLE_RATE, MIN_CHANNELS, MIN_SAMPLE_RATE};

use crate::domain::{AudioChunk, CoreError, Resampler};

/// Target sample rate expected by downstream STT engines (`16000` Hz).
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Centre frequency of the sinc low-pass, as a fraction of the lower sample rate (7.36 kHz at
/// 16 kHz). This is the -6 dB point, not the passband edge: measured response is flat up to about
/// 6.7 kHz and at least 80 dB down from 8 kHz.
const CUTOFF: f64 = 0.46;
/// Stopband attenuation targeted by the Kaiser window, in dB.
const STOPBAND_DB: f64 = 80.0;
/// Largest polyphase table (in `f32` coefficients) built with exact phases; beyond this the
/// fractional position is rounded to [`QUANTIZED_PHASES`] sub-sample phases.
///
/// The worst case among common rates (44.1 kHz and 22.05 kHz, about 28k coefficients) fits with
/// room to spare; rarer rates such as 32001 Hz (denominator 16000) take the quantized path
/// instead of building a multi-megabyte table per speaker.
const MAX_EXACT_TABLE: usize = 1 << 17;
const QUANTIZED_PHASES: u64 = 1024;

/// Namespace for audio helpers and the resampler factory used by the pipeline.
#[derive(Debug, Default, Clone, Copy)]
pub struct AudioPreprocessor;

impl AudioPreprocessor {
    /// Returns a factory producing one fresh [`StreamResampler`] per speaker worker.
    #[must_use]
    pub fn resampler_factory() -> Arc<dyn Fn() -> Box<dyn Resampler> + Send + Sync> {
        Arc::new(|| Box::new(StreamResampler::new()))
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

/// Active conversion for one `(sample_rate, channels)` format.
#[derive(Debug)]
struct Stage {
    sample_rate: u32,
    channels: u16,
    /// `None` for 16 kHz input (downmix only).
    fir: Option<FirResampler>,
}

/// Stateful converter from interleaved N-channel PCM at any supported rate to 16 kHz mono `f32`.
///
/// - 16 kHz mono input is passed through unchanged (fast path).
/// - Multi-channel frames are averaged (`sum / channels`).
/// - Other rates go through a Kaiser-windowed-sinc polyphase FIR low-pass evaluated only at
///   output instants (anti-aliasing and decimation in one step). The output has zero phase
///   offset; the resampler only holds back about `half` input samples (about 2 ms at 48 kHz)
///   while it waits for future input. Samples are neither repeated nor lost over a long run, so
///   the output length tracks `input_frames * 16000 / sample_rate` minus that held-back tail.
///   On flush or close the held-back tail is not emitted: it carries over to the start of the
///   next utterance (negligible for STT).
/// - The `[-1, 1]` range holds for the downmix stage only. The FIR output is not clamped and can
///   overshoot on full-scale input (up to about 1.32).
/// - A change of sample rate or channel count recreates the filter (its history is discarded).
#[derive(Debug, Default)]
pub struct StreamResampler {
    stage: Option<Stage>,
}

impl StreamResampler {
    /// Creates a resampler with no history.
    #[must_use]
    pub const fn new() -> Self {
        Self { stage: None }
    }

    /// Processes one chunk of interleaved PCM, keeping filter state for the next call.
    pub fn process_chunk(&mut self, chunk: &AudioChunk) -> Result<Vec<f32>, CoreError> {
        self.process_interleaved(&chunk.samples, chunk.sample_rate, chunk.channels)
    }

    /// Processes a raw interleaved slice with explicit `sample_rate` (`8000..=192000`) and
    /// `channels` (`1..=8`).
    pub fn process_interleaved(
        &mut self,
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
        if !samples.len().is_multiple_of(ch) {
            return Err(CoreError::InvalidAudio(format!(
                "sample count {} is not divisible by channels {ch}",
                samples.len()
            )));
        }

        // Format change mid-stream: recreate the filter.
        if !self
            .stage
            .as_ref()
            .is_some_and(|s| s.sample_rate == sample_rate && s.channels == channels)
        {
            self.stage = Some(Stage {
                sample_rate,
                channels,
                fir: (sample_rate != TARGET_SAMPLE_RATE)
                    .then(|| FirResampler::new(sample_rate, TARGET_SAMPLE_RATE)),
            });
        }

        if samples.is_empty() {
            return Ok(Vec::new());
        }

        let fir = self.stage.as_mut().and_then(|s| s.fir.as_mut());
        match (fir, channels) {
            (Some(fir), 1) => Ok(fir.process(samples)),
            (Some(fir), _) => Ok(fir.process(&downmix_to_mono(samples, ch))),
            (None, 1) => Ok(samples.to_vec()),
            (None, _) => Ok(downmix_to_mono(samples, ch)),
        }
    }
}

impl Resampler for StreamResampler {
    fn process(&mut self, chunk: &AudioChunk) -> Result<Vec<f32>, CoreError> {
        self.process_chunk(chunk)
    }
}

fn downmix_to_mono(samples: &[f32], channels: usize) -> Vec<f32> {
    let frame_count = samples.len() / channels;
    let mut mono = Vec::with_capacity(frame_count);
    #[allow(clippy::cast_precision_loss)]
    let inv_ch = 1.0_f32 / (channels as f32);
    for frame in samples.chunks_exact(channels) {
        let sum: f32 = frame.iter().copied().sum();
        mono.push(sum * inv_ch);
    }
    mono
}

const fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Zeroth-order modified Bessel function of the first kind (series expansion).
fn bessel_i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..64_u32 {
        let kf = f64::from(k);
        term *= q / (kf * kf);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

/// Rational-ratio polyphase FIR resampler (`src_rate` -> `dst_rate`) over a mono stream.
///
/// Output `n` sits at input position `n * src / dst`; the kernel is a Kaiser-windowed sinc
/// low-pass whose cutoff is [`CUTOFF`] of the lower of the two Nyquist rates.
#[derive(Debug)]
struct FirResampler {
    /// Taps per output sample.
    taps: usize,
    /// Number of tabulated phases.
    phases: u64,
    /// `phases * taps` coefficients, each phase normalized to unity DC gain.
    table: Vec<f32>,
    /// Input step per output, in units of `1/den` input samples.
    num: u64,
    den: u64,
    /// Position of the next output, in `1/den` input samples relative to `buf[0]`.
    pos: u64,
    /// Input history (starts with zeros so the first outputs see a silent past).
    buf: Vec<f32>,
}

impl FirResampler {
    fn new(src_rate: u32, dst_rate: u32) -> Self {
        let g = gcd(u64::from(src_rate), u64::from(dst_rate));
        let num = u64::from(src_rate) / g;
        let den = u64::from(dst_rate) / g;

        let narrow = (f64::from(dst_rate) / f64::from(src_rate)).min(1.0);
        // Cutoff and transition width in cycles per input sample.
        let fc = CUTOFF * narrow;
        let transition = 2.0_f64.mul_add(-CUTOFF, 1.0) * narrow;
        let n_est = (STOPBAND_DB - 7.95) / (2.285 * 2.0 * PI * transition);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let half = ((n_est / 2.0).ceil() as usize).max(4);
        let taps = half * 2;
        let beta = 0.1102 * (STOPBAND_DB - 8.7);

        let exact = den
            .checked_mul(taps as u64)
            .is_some_and(|n| n <= MAX_EXACT_TABLE as u64);
        let phases = if exact { den } else { QUANTIZED_PHASES };

        let i0_beta = bessel_i0(beta);
        #[allow(clippy::cast_precision_loss)]
        let half_f = half as f64;
        let mut table = Vec::with_capacity(taps * usize::try_from(phases).unwrap_or(1));
        for p in 0..phases {
            #[allow(clippy::cast_precision_loss)]
            let frac = p as f64 / phases as f64;
            let mut coeffs = Vec::with_capacity(taps);
            for t in 0..taps {
                // Tap `t` reads input offset `t - (half - 1)` from the integer position.
                #[allow(clippy::cast_precision_loss)]
                let x = (t as f64) - (half_f - 1.0) - frac;
                let r = x / half_f;
                let w = if r.abs() >= 1.0 {
                    0.0
                } else {
                    bessel_i0(beta * r.mul_add(-r, 1.0).sqrt()) / i0_beta
                };
                coeffs.push(2.0 * fc * sinc(2.0 * fc * x) * w);
            }
            let sum: f64 = coeffs.iter().sum();
            #[allow(clippy::cast_possible_truncation)]
            table.extend(coeffs.iter().map(|c| (c / sum) as f32));
        }

        Self {
            taps,
            phases,
            table,
            num,
            den,
            pos: 0,
            buf: vec![0.0; half - 1],
        }
    }

    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        self.buf.extend_from_slice(input);
        // Upper bound on the outputs reachable from the current buffer.
        let reach = (self.buf.len() as u64 + 1).saturating_sub(self.taps as u64) * self.den;
        let est = reach.saturating_sub(self.pos).div_ceil(self.num);
        let mut out = Vec::with_capacity(usize::try_from(est).unwrap_or(0));
        loop {
            #[allow(clippy::cast_possible_truncation)]
            let idx = (self.pos / self.den) as usize;
            if idx + self.taps > self.buf.len() {
                break;
            }
            let rem = self.pos % self.den;
            let phase = if self.phases == self.den {
                rem
            } else {
                ((rem * self.phases + self.den / 2) / self.den).min(self.phases - 1)
            };
            #[allow(clippy::cast_possible_truncation)]
            let row = (phase as usize) * self.taps;
            out.push(dot(
                &self.buf[idx..idx + self.taps],
                &self.table[row..row + self.taps],
            ));
            self.pos += self.num;
        }
        // Drop history that no later output can reach.
        #[allow(clippy::cast_possible_truncation)]
        let consumed = ((self.pos / self.den) as usize).min(self.buf.len());
        self.buf.drain(..consumed);
        self.pos -= (consumed as u64) * self.den;
        out
    }
}

/// Dot product with independent accumulators so the compiler can vectorize it.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    const LANES: usize = 8;
    let mut acc = [0.0_f32; LANES];
    let a_chunks = a.chunks_exact(LANES);
    let b_chunks = b.chunks_exact(LANES);
    let tail: f32 = a_chunks
        .remainder()
        .iter()
        .zip(b_chunks.remainder())
        .map(|(x, y)| x * y)
        .sum();
    for (ca, cb) in a_chunks.zip(b_chunks) {
        for ((s, x), y) in acc.iter_mut().zip(ca).zip(cb) {
            *s += x * y;
        }
    }
    acc.iter().sum::<f32>() + tail
}
