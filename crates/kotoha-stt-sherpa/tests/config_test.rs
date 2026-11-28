//! Unit tests for `kotoha-stt-sherpa` configuration (`src/config.rs`).

#![allow(clippy::too_many_lines)]

mod common;

use std::path::PathBuf;

use common::{sample_paraformer_config, sample_zipformer_config};
use kotoha_core::domain::{ComputeDevice, SttDecodingMethod};
use kotoha_stt_sherpa::{
    compute_device_to_provider, SherpaEndpointConfig, SherpaModelFamily, SherpaModelingUnit,
    SherpaSttConfig,
};

#[test]
fn config_validation_provider_mapping_and_decoding_method() {
    assert_eq!(compute_device_to_provider(ComputeDevice::Auto), "cpu");
    assert_eq!(compute_device_to_provider(ComputeDevice::Cpu), "cpu");
    assert_eq!(compute_device_to_provider(ComputeDevice::Cuda), "cuda");
    assert_eq!(compute_device_to_provider(ComputeDevice::Metal), "coreml");

    let mut cfg = sample_zipformer_config();
    assert!(cfg.validate().is_ok());
    assert_eq!(cfg.provider_name(), "cpu");
    assert_eq!(
        cfg.effective_decoding_method(false),
        SttDecodingMethod::GreedySearch
    );
    assert_eq!(
        cfg.effective_decoding_method(true),
        SttDecodingMethod::ModifiedBeamSearch
    );
    assert_eq!(
        SherpaSttConfig::decoding_method_str(SttDecodingMethod::ModifiedBeamSearch),
        "modified_beam_search"
    );

    cfg.device = ComputeDevice::Metal;
    assert_eq!(cfg.provider_name(), "coreml");

    let paraformer = sample_paraformer_config();
    assert!(paraformer.validate().is_ok());
    assert!(!paraformer.model.supports_hotwords());
    assert_eq!(
        paraformer.effective_decoding_method(true),
        SttDecodingMethod::GreedySearch
    );

    // Invalid numeric parameters rejected
    let mut bad = cfg.clone();
    bad.num_threads = 0;
    assert!(bad.validate().is_err());

    bad = cfg.clone();
    bad.max_active_paths = 0;
    assert!(bad.validate().is_err());

    bad = cfg.clone();
    bad.hotwords_score = 0.0;
    assert!(bad.validate().is_err());

    bad = cfg.clone();
    bad.hotwords_score = f32::NAN;
    assert!(bad.validate().is_err());

    bad = cfg.clone();
    bad.hotwords_score = f32::INFINITY;
    assert!(bad.validate().is_err());

    bad = cfg.clone();
    bad.endpoint = SherpaEndpointConfig {
        enable: true,
        rule1_min_trailing_silence: -0.1,
        ..Default::default()
    };
    assert!(bad.validate().is_err());

    // Empty path rejected
    let empty_path_cfg = SherpaSttConfig::new(SherpaModelFamily::ZipformerTransducer {
        encoder: PathBuf::from(""),
        decoder: PathBuf::from("/models/decoder.onnx"),
        joiner: PathBuf::from("/models/joiner.int8.onnx"),
        tokens: PathBuf::from("/models/tokens.txt"),
        bpe_vocab: None,
        modeling_unit: None,
    });
    assert!(empty_path_cfg.validate().is_err());

    // BPE / CjkCharPlusBpe modeling_unit requires bpe_vocab
    for unit in [SherpaModelingUnit::Bpe, SherpaModelingUnit::CjkCharPlusBpe] {
        let missing_vocab_cfg = SherpaSttConfig::new(SherpaModelFamily::ZipformerTransducer {
            encoder: PathBuf::from("/models/encoder.int8.onnx"),
            decoder: PathBuf::from("/models/decoder.onnx"),
            joiner: PathBuf::from("/models/joiner.int8.onnx"),
            tokens: PathBuf::from("/models/tokens.txt"),
            bpe_vocab: None,
            modeling_unit: Some(unit),
        });
        assert!(missing_vocab_cfg.validate().is_err());
    }

    // CjkChar modeling_unit does NOT require bpe_vocab
    let cjk_cfg = SherpaSttConfig::new(SherpaModelFamily::ZipformerTransducer {
        encoder: PathBuf::from("/models/encoder.int8.onnx"),
        decoder: PathBuf::from("/models/decoder.onnx"),
        joiner: PathBuf::from("/models/joiner.int8.onnx"),
        tokens: PathBuf::from("/models/tokens.txt"),
        bpe_vocab: None,
        modeling_unit: Some(SherpaModelingUnit::CjkChar),
    });
    assert!(cjk_cfg.validate().is_ok());

    // Non-existent files fail validate_files_exist()
    assert!(cfg.validate_files_exist().is_err());
}
