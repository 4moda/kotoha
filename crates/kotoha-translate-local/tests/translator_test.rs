//! Integration tests for `kotoha-translate-local` `LocalNmtTranslator` (`src/translator.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::fs;
use std::sync::{Arc, Mutex};

use common::{
    create_model_dir, sample_request, unique_temp_dir, MockLocalBackend, MockLocalLoader,
};
use kotoha_core::application::TranslatorManager;
use kotoha_core::domain::{
    default_model_catalog, ComputeDevice, CoreError, ModelProfile, TranslationProviderConfig,
    TranslatorFactory,
};
use kotoha_translate_http::HttpTranslatorFactory;
use kotoha_translate_local::{
    detect_model_family, CompositeTranslatorFactory, HardwareCapabilities, LocalNmtConfig,
    LocalNmtFamily, LocalNmtTranslator, LocalNmtTranslatorFactory, ResolvedDevice,
};

#[tokio::test]
async fn model_granular_switching_flores_prefixes_and_placeholder_guard() {
    let root = unique_temp_dir("kotoha_nmt_models");
    let _ = fs::remove_dir_all(&root);

    let fugu_sha = create_model_dir(&root, "fugumt-en-ja-int8", b"fugu-ct2-weights", "spm");
    let nllb_sha = create_model_dir(
        &root,
        "nllb-200-distilled-600m-int8",
        b"nllb-ct2-weights",
        "hf",
    );

    // Override catalog profiles with real test SHA-256 digests
    let custom_profiles: Vec<ModelProfile> = default_model_catalog()
        .into_iter()
        .map(|mut p| {
            if p.model_id == "fugumt-en-ja-int8" {
                p.sha256 = fugu_sha.clone();
            } else if p.model_id == "nllb-200-distilled-600m-int8" {
                p.sha256 = nllb_sha.clone();
            }
            p
        })
        .collect();

    assert_eq!(
        detect_model_family(&custom_profiles[0]),
        LocalNmtFamily::MarianOrOpus
    );
    assert_eq!(
        detect_model_family(&custom_profiles[2]),
        LocalNmtFamily::Nllb200
    );

    let loader = Arc::new(MockLocalLoader::new());
    let local_factory = LocalNmtTranslatorFactory::with_loader(&root, loader.clone())
        .with_custom_profiles(custom_profiles.clone())
        .with_capabilities(HardwareCapabilities {
            cuda_available: true,
            metal_available: false,
        });

    let composite: Arc<dyn TranslatorFactory> = Arc::new(CompositeTranslatorFactory::new(
        local_factory,
        HttpTranslatorFactory::new(),
    ));
    let mgr = TranslatorManager::from_config(
        TranslationProviderConfig::LocalModel {
            model_id: "fugumt-en-ja-int8".to_owned(),
            device: ComputeDevice::Auto,
            threads: Some(4),
        },
        composite.as_ref(),
    )
    .expect("initialize with fugumt");

    assert_eq!(mgr.current_provider_id().await, "local:fugumt-en-ja-int8");
    let res1 = mgr
        .translate(sample_request("Engage M W D on Ferox now", "en", "ja"))
        .await
        .expect("fugu translate");
    assert_eq!(
        res1.translated_text,
        "[fugumt-en-ja-int8] Engage MWD on フェロックス now"
    );

    // Bilingual Marian/OPUS-MT rejects unsupported pair and 'auto' (Minor 13)
    let unsupported_pair = mgr
        .translate(sample_request("Bonjour", "fr", "de"))
        .await
        .expect_err("fugu en->ja must reject fr->de");
    assert!(matches!(
        unsupported_pair,
        CoreError::InvalidLanguageCode(_)
    ));
    let auto_pair = mgr
        .translate(sample_request("Hello", "auto", "ja"))
        .await
        .expect_err("fugu en->ja must reject auto");
    assert!(matches!(auto_pair, CoreError::InvalidLanguageCode(_)));

    // Hot-swap model profile to NLLB-200 distilled 600M -> uses CUDA + Flores-200 prefix tags
    mgr.switch_provider(
        TranslationProviderConfig::LocalModel {
            model_id: "nllb-200-distilled-600m-int8".to_owned(),
            device: ComputeDevice::Auto,
            threads: Some(2),
        },
        Arc::clone(&composite),
    )
    .await
    .expect("switch to nllb-200");
    assert_eq!(
        mgr.current_provider_id().await,
        "local:nllb-200-distilled-600m-int8"
    );

    let res2 = mgr
        .translate(sample_request("Engage M W D on Ferox now", "en", "ja"))
        .await
        .expect("nllb translate");
    assert_eq!(
        res2.translated_text,
        "[nllb-200-distilled-600m-int8:eng_Latn->jpn_Jpan] Engage MWD on フェロックス now"
    );

    let calls = loader.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].model_id, "fugumt-en-ja-int8");
    assert_eq!(calls[0].device, ResolvedDevice::Cpu);
    assert_eq!(calls[0].threads, 4);
    assert_eq!(calls[0].source_prefix, None);
    assert_eq!(calls[0].target_prefix, None);
    assert_eq!(calls[0].text, "Engage __TERM_0__ on __TERM_1__ now");

    assert_eq!(calls[1].model_id, "nllb-200-distilled-600m-int8");
    assert_eq!(calls[1].device, ResolvedDevice::Cuda);
    assert_eq!(calls[1].threads, 2);
    assert_eq!(calls[1].source_prefix.as_deref(), Some("eng_Latn"));
    assert_eq!(calls[1].target_prefix.as_deref(), Some("jpn_Jpan"));

    // Verify LocalNmtTranslator direct construction rejects corrupted weights
    fs::write(
        root.join("fugumt-en-ja-int8").join("model.bin"),
        b"corrupted-bytes",
    )
    .unwrap();
    let cfg = LocalNmtConfig::new(&root, "fugumt-en-ja-int8", ComputeDevice::Cpu);
    let corrupted_err = LocalNmtTranslator::with_backend(
        &cfg,
        &custom_profiles,
        HardwareCapabilities::default(),
        Arc::new(MockLocalBackend {
            model_id: "fugumt-en-ja-int8".to_owned(),
            device: ResolvedDevice::Cpu,
            threads: 0,
            calls: Arc::new(Mutex::new(Vec::new())),
        }),
    )
    .expect_err("corrupted weights must fail SHA-256 check");
    assert!(matches!(corrupted_err, CoreError::Translation(_)));

    let _ = fs::remove_dir_all(&root);
}
