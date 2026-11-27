//! Unit tests for `kotoha-core` audio preprocessor (`src/infrastructure/audio.rs`).

#![allow(clippy::expect_used, clippy::cast_precision_loss)]

use std::f32::consts::PI;

use kotoha_core::infrastructure::{AudioPreprocessor, TARGET_SAMPLE_RATE};

#[test]
fn audio_preprocessor_16k_mono_passthrough_and_48k_stereo_resample() {
    // 1. 16 kHz mono fast-path exact preservation
    let mono_16k: Vec<f32> = (0..320).map(|i| ((i as f32) * 0.01).sin()).collect();
    let out_fast =
        AudioPreprocessor::process_interleaved(&mono_16k, 16_000, 1).expect("16k mono passthrough");
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

    let resampled =
        AudioPreprocessor::process_interleaved(&stereo, src_rate, 2).expect("48k stereo resample");
    assert_eq!(resampled.len(), 1_600);
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
    assert!(AudioPreprocessor::process_interleaved(&[0.0], 7_999, 1).is_err());
    assert!(AudioPreprocessor::process_interleaved(&[0.0], 192_001, 1).is_err());
    assert!(AudioPreprocessor::process_interleaved(&[0.0], 48_000, 0).is_err());
    assert!(AudioPreprocessor::process_interleaved(&[0.0], 48_000, 9).is_err());
    assert!(AudioPreprocessor::process_interleaved(&[0.0], 48_000, 2).is_err());
}
