//! Unit tests for `kotoha-core` translator manager (`src/application/translator_manager.rs`).

#![allow(clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use kotoha_core::application::TranslatorManager;
use kotoha_core::domain::{
    ComputeDevice, CoreError, TranslationProviderConfig, TranslationRequest, TranslationResult,
    Translator, TranslatorFactory,
};
use kotoha_core::infrastructure::DefaultTranslatorFactory;

struct RecordingFactory;

struct TaggedTranslator {
    id: String,
    prefix: String,
}

#[async_trait]
impl Translator for TaggedTranslator {
    fn provider_id(&self) -> &str {
        &self.id
    }

    async fn translate(&self, req: TranslationRequest) -> Result<TranslationResult, CoreError> {
        Ok(TranslationResult {
            utterance_id: req.utterance_id,
            original_text: req.text.clone(),
            translated_text: format!("{}{}", self.prefix, req.text),
            provider_id: self.id.clone(),
            latency_ms: 0,
        })
    }
}

impl TranslatorFactory for RecordingFactory {
    fn create(&self, config: &TranslationProviderConfig) -> Result<Arc<dyn Translator>, CoreError> {
        match config {
            TranslationProviderConfig::Mock { prefix } => Ok(Arc::new(TaggedTranslator {
                id: "mock".to_owned(),
                prefix: prefix.clone(),
            })),
            TranslationProviderConfig::LocalModel { model_id, .. } => {
                Ok(Arc::new(TaggedTranslator {
                    id: format!("local:{model_id}"),
                    prefix: format!("[{model_id}] "),
                }))
            }
            TranslationProviderConfig::OpenAiCompatible { model, .. } => {
                Ok(Arc::new(TaggedTranslator {
                    id: format!("openai:{model}"),
                    prefix: format!("[{model}] "),
                }))
            }
            _ => DefaultTranslatorFactory.create(config),
        }
    }
}

#[tokio::test]
async fn translator_manager_runtime_hot_swap_and_validation() {
    let factory: Arc<dyn TranslatorFactory> = Arc::new(RecordingFactory);
    let mgr = TranslatorManager::from_config(
        TranslationProviderConfig::Mock {
            prefix: "[mock] ".to_owned(),
        },
        factory.as_ref(),
    )
    .expect("create manager");

    assert_eq!(mgr.current_provider_id().await, "mock");
    assert_eq!(mgr.available_models().len(), 4);

    // Reject unknown model_id before mutating active provider
    let err = mgr
        .switch_provider(
            TranslationProviderConfig::LocalModel {
                model_id: "non-existent-model".to_owned(),
                device: ComputeDevice::Auto,
                threads: None,
            },
            Arc::clone(&factory),
        )
        .await
        .expect_err("unknown model profile must be rejected");
    assert!(matches!(err, CoreError::UnknownModelProfile(_)));
    assert_eq!(mgr.current_provider_id().await, "mock");

    // Hot-swap to known catalog model profile
    mgr.switch_provider(
        TranslationProviderConfig::LocalModel {
            model_id: "fugumt-en-ja-int8".to_owned(),
            device: ComputeDevice::Cpu,
            threads: Some(4),
        },
        Arc::clone(&factory),
    )
    .await
    .expect("switch to fugumt");
    assert_eq!(mgr.current_provider_id().await, "local:fugumt-en-ja-int8");

    // Hot-swap to OpenAI-compatible LLM
    mgr.switch_provider(
        TranslationProviderConfig::OpenAiCompatible {
            base_url: "http://localhost:11434/v1".to_owned(),
            model: "qwen2.5:7b".to_owned(),
            timeout_ms: 5_000,
        },
        Arc::clone(&factory),
    )
    .await
    .expect("switch to local ollama");
    assert_eq!(mgr.current_provider_id().await, "openai:qwen2.5:7b");

    // Phase 1 factory reports Phase 2 providers as unsupported (no secrets involved).
    let deepl = DefaultTranslatorFactory.create(&TranslationProviderConfig::DeepL {
        use_free_endpoint: true,
        timeout_ms: 5_000,
    });
    assert!(matches!(deepl, Err(CoreError::UnsupportedInPhase1(_))));
}
