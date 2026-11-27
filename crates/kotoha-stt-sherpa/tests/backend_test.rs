//! Unit tests for `kotoha-stt-sherpa` recognizer backend (`src/backend.rs`).

#[cfg(feature = "native-sherpa")]
mod common;

#[cfg(feature = "native-sherpa")]
#[test]
fn native_sherpa_config_mapping_populates_transducer_paraformer_and_hotwords() {
    use common::{sample_paraformer_config, sample_zipformer_config};
    use kotoha_core::domain::{ComputeDevice, SttDecodingMethod};
    use kotoha_stt_sherpa::{build_native_config, HotwordsSnapshot};

    let mut cfg = sample_zipformer_config();
    cfg.device = ComputeDevice::Metal;
    let hw_snap = HotwordsSnapshot {
        generation: 1,
        decoding_method: SttDecodingMethod::ModifiedBeamSearch,
        hotwords_score: 2.25,
        phrases: vec!["M W D".to_owned()],
        formatted_text: "M W D\n".to_owned(),
        file_path: None,
    };
    let native = build_native_config(&cfg, &hw_snap);
    assert_eq!(native.feat_config.sample_rate, 16_000);
    assert_eq!(native.model_config.provider.as_deref(), Some("coreml"));
    assert_eq!(
        native.model_config.transducer.encoder.as_deref(),
        Some("/models/encoder.int8.onnx")
    );
    assert_eq!(native.model_config.modeling_unit.as_deref(), Some("bpe"));
    assert_eq!(
        native.decoding_method.as_deref(),
        Some("modified_beam_search")
    );
    assert!((native.hotwords_score - 2.25).abs() < f32::EPSILON);
    assert_eq!(native.hotwords_buf.as_deref(), Some(b"M W D\n".as_slice()));
    assert!(native.enable_endpoint);

    let paraformer = sample_paraformer_config();
    let native_p = build_native_config(&paraformer, &hw_snap);
    assert_eq!(
        native_p.model_config.paraformer.encoder.as_deref(),
        Some("/models/paraformer-encoder.onnx")
    );
    assert_eq!(native_p.decoding_method.as_deref(), Some("greedy_search"));
    assert!(native_p.hotwords_buf.is_none());
}
