//! Integration tests for `kotoha-translate-local` `CompositeTranslatorFactory` (`src/factory.rs`).

#![allow(clippy::expect_used)]

mod common;

use std::fs;
use std::sync::Arc;

use common::{create_model_dir, unique_temp_dir, MockLocalLoader};
use kotoha_core::application::TranslatorManager;
use kotoha_core::domain::{
    default_model_catalog, ComputeDevice, ModelBackend, TranslationProviderConfig,
    TranslatorFactory,
};
use kotoha_translate_http::HttpTranslatorFactory;
use kotoha_translate_local::{CompositeTranslatorFactory, LocalNmtTranslatorFactory};

#[tokio::test]
async fn composite_factory_switches_across_all_six_providers_in_translator_manager() {
    let root = unique_temp_dir("kotoha_composite_all_six");
    let _ = fs::remove_dir_all(&root);
    let sha = create_model_dir(&root, "opus-mt-en-ja-int8", b"opus-weights", "hf");

    let mut custom = default_model_catalog();
    for p in &mut custom {
        if p.model_id == "opus-mt-en-ja-int8" {
            p.sha256 = sha.clone();
            p.backend = ModelBackend::CTranslate2;
        }
    }

    let loader = Arc::new(MockLocalLoader::new());
    let local_factory =
        LocalNmtTranslatorFactory::with_loader(&root, loader).with_custom_profiles(custom);
    let http_factory = HttpTranslatorFactory::new()
        .with_deepl_base_url("http://127.0.0.1:9999")
        .with_google_cloud_base_url("http://127.0.0.1:9999");
    let composite: Arc<dyn TranslatorFactory> =
        Arc::new(CompositeTranslatorFactory::new(local_factory, http_factory));

    let mgr = TranslatorManager::from_config(
        TranslationProviderConfig::Mock {
            prefix: "[mock] ".to_owned(),
        },
        composite.as_ref(),
    )
    .expect("1. Mock");
    assert_eq!(mgr.current_provider_id().await, "mock");

    mgr.switch_provider(
        TranslationProviderConfig::LocalModel {
            model_id: "opus-mt-en-ja-int8".to_owned(),
            device: ComputeDevice::Cpu,
            threads: Some(2),
        },
        Arc::clone(&composite),
    )
    .await
    .expect("2. LocalModel");
    assert_eq!(mgr.current_provider_id().await, "local:opus-mt-en-ja-int8");

    mgr.switch_provider(
        TranslationProviderConfig::OpenAiCompatible {
            base_url: "http://127.0.0.1:11434/v1".to_owned(),
            model: "qwen2.5:7b".to_owned(),
            api_key: None,
            timeout_ms: 5_000,
        },
        Arc::clone(&composite),
    )
    .await
    .expect("3. OpenAiCompatible");
    assert_eq!(mgr.current_provider_id().await, "openai-compatible");

    mgr.switch_provider(
        TranslationProviderConfig::DeepL {
            api_key: "test-deepl-key".to_owned(),
            use_free_endpoint: true,
            timeout_ms: 5_000,
        },
        Arc::clone(&composite),
    )
    .await
    .expect("4. DeepL");
    assert_eq!(mgr.current_provider_id().await, "deepl");

    mgr.switch_provider(
        TranslationProviderConfig::GoogleCloud {
            api_key: "test-gc-key".to_owned(),
            project_id: Some("proj".to_owned()),
            timeout_ms: 5_000,
        },
        Arc::clone(&composite),
    )
    .await
    .expect("5. GoogleCloud");
    assert_eq!(mgr.current_provider_id().await, "google-cloud");

    mgr.switch_provider(
        TranslationProviderConfig::GasWebhook {
            endpoint_url: "http://127.0.0.1:8080/exec".to_owned(),
            bearer_token: Some("tok".to_owned()),
            timeout_ms: 5_000,
        },
        Arc::clone(&composite),
    )
    .await
    .expect("6. GasWebhook");
    assert_eq!(mgr.current_provider_id().await, "gas-webhook");

    let _ = fs::remove_dir_all(&root);
}
