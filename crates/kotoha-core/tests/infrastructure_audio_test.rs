//! Unit tests for `kotoha-core` audio preprocessor (`src/infrastructure/audio.rs`).

#![allow(clippy::expect_used, clippy::cast_precision_loss)]

use std::f32::consts::PI;

use kotoha_core::infrastructure::{AudioPreprocessor, StreamResampler, TARGET_SAMPLE_RATE};

#[test]
fn audio_preprocessor_16k_mono_passthrough_and_48k_stereo_resample() {
    // 1. 16 kHz mono fast-path exact preservation
    let mono_16k: Vec<f32> = (0..320).map(|i| ((i as f32) * 0.01).sin()).collect();
    let out_fast = StreamResampler::new()
        .process_interleaved(&mono_16k, 16_000, 1)
        .expect("16k mono passthrough");
    assert_eq!(out_fast, mono_16k);

    // 2. 48 kHz stereo 100ms (4800 stereo frames = 9600 samples) -> 1600 mono 16k samples
    //    440 Hz sine wave with opposite-phase second channel test vs identical stereo channels
    let src_rate = 48_000u32;
    let src_frames = 4_800usize;
    let freq_hz = 440.0f32;
    let mut stereo = Vec::with_capacity(src_frames * 2);
    for i in 0..src_frames {
        let t = (i as f32) / (src_rate as f32);
        let sample = 0.6 * (2.0 * PI * freq_hz * t).sin();
        stereo.push(sample);
        stereo.push(sample);
    }

    let resampled = StreamResampler::new()
        .process_interleaved(&stereo, src_rate, 2)
        .expect("48k stereo resample");
    // The output lags by the filter latency (< 2.5 ms of audio at 48 kHz).
    assert!((1_550..=1_600).contains(&resampled.len()));
    assert!(resampled
        .iter()
        .all(|s| s.is_finite() && (-1.0..=1.0).contains(s)));

    // Verify dominant frequency via zero-crossing count (~2 * 440 Hz * 0.1 s = 88 crossings)
    let zero_crossings = resampled
        .windows(2)
        .filter(|w| (w[0] < 0.0 && w[1] >= 0.0) || (w[0] >= 0.0 && w[1] < 0.0))
        .count();
    assert!(
        (84..=92).contains(&zero_crossings),
        "expected ~88 zero crossings for 440Hz/100ms, got {zero_crossings}"
    );
    assert_eq!(TARGET_SAMPLE_RATE, 16_000);
}

#[test]
fn audio_preprocessor_rejects_invalid_inputs() {
    assert!(StreamResampler::new()
        .process_interleaved(&[0.0], 7_999, 1)
        .is_err());
    assert!(StreamResampler::new()
        .process_interleaved(&[0.0], 192_001, 1)
        .is_err());
    assert!(StreamResampler::new()
        .process_interleaved(&[0.0], 48_000, 0)
        .is_err());
    assert!(StreamResampler::new()
        .process_interleaved(&[0.0], 48_000, 9)
        .is_err());
    assert!(StreamResampler::new()
        .process_interleaved(&[0.0], 48_000, 2)
        .is_err());
}

fn sine(freq_hz: f32, rate: u32, frames: usize, amp: f32) -> Vec<f32> {
    (0..frames)
        .map(|i| amp * (2.0 * PI * freq_hz * (i as f32) / (rate as f32)).sin())
        .collect()
}

/// RMS of `samples` after dropping the filter's start-up transient.
fn steady_rms(samples: &[f32]) -> f32 {
    AudioPreprocessor::compute_rms(&samples[samples.len() / 4..])
}

fn db(ratio: f32) -> f32 {
    20.0 * ratio.max(1e-12).log10()
}

fn run_chunked(
    input: &[f32],
    rate: u32,
    channels: u16,
    chunk_frames: usize,
    r: &mut StreamResampler,
) -> Vec<f32> {
    let step = chunk_frames * usize::from(channels);
    input
        .chunks(step)
        .flat_map(|c| r.process_interleaved(c, rate, channels).expect("chunk"))
        .collect()
}

#[test]
fn resampler_attenuates_tones_above_output_nyquist() {
    // 12 kHz would alias to 4 kHz without a low-pass filter.
    for freq in [9_000.0, 12_000.0, 20_000.0] {
        let input = sine(freq, 48_000, 48_000, 0.5);
        let out = run_chunked(&input, 48_000, 1, 960, &mut StreamResampler::new());
        let atten = db(steady_rms(&out) / AudioPreprocessor::compute_rms(&input));
        assert!(atten <= -40.0, "aliasing tone not attenuated enough");
    }
}

#[test]
fn resampler_passband_loss_is_small() {
    for freq in [300.0, 1_000.0, 3_000.0, 6_000.0] {
        let input = sine(freq, 48_000, 48_000, 0.5);
        let out = run_chunked(&input, 48_000, 1, 960, &mut StreamResampler::new());
        let gain = db(steady_rms(&out) / AudioPreprocessor::compute_rms(&input));
        assert!(gain.abs() < 1.0, "passband gain deviates by 1 dB or more");
    }
}

#[test]
fn resampler_chunked_equals_batch() {
    let rate = 48_000;
    let input: Vec<f32> = sine(440.0, rate, 48_000, 0.3)
        .iter()
        .zip(sine(3_100.0, rate, 48_000, 0.2))
        .zip(sine(11_000.0, rate, 48_000, 0.2))
        .map(|((a, b), c)| a + b + c)
        .collect();
    let batch = StreamResampler::new()
        .process_interleaved(&input, rate, 1)
        .expect("batch");
    // 20 ms chunks, plus awkward sizes that are not multiples of the ratio.
    for chunk_frames in [960, 1, 7, 1_001] {
        let chunked = run_chunked(&input, rate, 1, chunk_frames, &mut StreamResampler::new());
        assert_eq!(chunked.len(), batch.len(), "chunked length differs");
        let max_diff = chunked
            .iter()
            .zip(&batch)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f32, f32::max);
        assert!(max_diff < 1e-5, "chunked output diverges from batch");
    }
}

#[test]
fn resampler_output_length_does_not_drift() {
    for rate in [8_000u32, 44_100, 48_000, 96_000, 192_000] {
        let seconds = 10usize;
        let chunk = (rate / 50) as usize; // 20 ms
        let mut r = StreamResampler::new();
        let silence = vec![0.0_f32; chunk];
        let mut total = 0usize;
        for _ in 0..(50 * seconds) {
            total += r
                .process_interleaved(&silence, rate, 1)
                .expect("chunk")
                .len();
        }
        let expected = 16_000 * seconds;
        // Only the constant filter latency may be missing at the end (< 20 ms of output).
        assert!(
            total <= expected,
            "produced more samples than the input spans"
        );
        assert!(expected - total < 320, "output length drifted");
    }
}

#[test]
fn resampler_44k1_tone_keeps_frequency_and_level() {
    let input = sine(1_000.0, 44_100, 44_100, 0.5);
    let out = run_chunked(&input, 44_100, 1, 882, &mut StreamResampler::new());
    let gain = db(steady_rms(&out) / AudioPreprocessor::compute_rms(&input));
    assert!(gain.abs() < 1.0);
    let crossings = out.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    assert!((998..=1_001).contains(&crossings), "pitch drifted");
}

#[test]
fn resampler_upsamples_8k_input() {
    let input = sine(1_000.0, 8_000, 8_000, 0.5);
    let out = run_chunked(&input, 8_000, 1, 160, &mut StreamResampler::new());
    assert!(out.len() > 15_000 && out.len() <= 16_000);
    let gain = db(steady_rms(&out) / AudioPreprocessor::compute_rms(&input));
    assert!(gain.abs() < 1.0);
}

#[test]
fn resampler_downmixes_stereo_to_mono_by_averaging() {
    // Opposite-phase channels cancel; a one-sided signal is halved.
    let tone = sine(500.0, 48_000, 4_800, 0.8);
    let opposite: Vec<f32> = tone.iter().flat_map(|&s| [s, -s]).collect();
    let out = StreamResampler::new()
        .process_interleaved(&opposite, 48_000, 2)
        .expect("opposite");
    assert!(
        out.iter().all(|s| s.abs() < 1e-6),
        "channels did not cancel"
    );

    let left_only: Vec<f32> = tone.iter().flat_map(|&s| [s, 0.0]).collect();
    let out = run_chunked(&left_only, 48_000, 2, 960, &mut StreamResampler::new());
    let mono_in = AudioPreprocessor::compute_rms(&tone) / 2.0;
    let gain = db(steady_rms(&out) / mono_in);
    assert!(gain.abs() < 1.0, "one-sided stereo is not halved");

    // 16 kHz stereo: averaging only, no filter.
    let out = StreamResampler::new()
        .process_interleaved(&[1.0, 0.0, 0.5, 0.5, -1.0, 1.0], 16_000, 2)
        .expect("16k stereo");
    assert_eq!(out, vec![0.5, 0.5, 0.0]);
}

#[test]
fn resampler_recreates_filter_on_format_change() {
    let mut r = StreamResampler::new();
    let a = sine(1_000.0, 48_000, 4_800, 0.5);
    r.process_interleaved(&a, 48_000, 1).expect("48k");
    // Switching to 44.1 kHz stereo must behave like a fresh resampler.
    let b: Vec<f32> = sine(1_000.0, 44_100, 4_410, 0.5)
        .iter()
        .flat_map(|&s| [s, s])
        .collect();
    let switched = r.process_interleaved(&b, 44_100, 2).expect("switch");
    let fresh = StreamResampler::new()
        .process_interleaved(&b, 44_100, 2)
        .expect("fresh");
    assert_eq!(switched, fresh);
}
