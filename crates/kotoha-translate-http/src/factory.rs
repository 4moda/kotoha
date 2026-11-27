//! Factory constructing HTTP & Mock [`Translator`] adapters from [`TranslationProviderConfig`].

use std::sync::Arc;

use kotoha_core::domain::{CoreError, TranslationProviderConfig, Translator, TranslatorFactory};
use kotoha_core::infrastructure::MockTranslator;

use crate::deepl::{DeepLConfig, DeepLTranslator};
use crate::gas_webhook::{GasWebhookConfig, GasWebhookTranslator};
use crate::google_cloud::{GoogleCloudConfig, GoogleCloudTranslator};
use crate::openai::{OpenAiCompatibleConfig, OpenAiCompatibleTranslator};

/// [`TranslatorFactory`] constructing HTTP translation adapters (`OpenAiCompatible`, `DeepL`,
/// `GoogleCloud`, `GasWebhook`) and `Mock`.
///
/// Supports optional endpoint overrides for deterministic integration testing without hitting
/// public cloud APIs.
#[derive(Debug, Default, Clone)]
pub struct HttpTranslatorFactory {
    deepl_base_url: Option<String>,
    google_cloud_base_url: Option<String>,
}

impl HttpTranslatorFactory {
    /// Creates an [`HttpTranslatorFactory`] targeting official cloud endpoints.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            deepl_base_url: None,
            google_cloud_base_url: None,
        }
    }

    /// Overrides the `DeepL` endpoint URL (useful for testing against a local mock HTTP server).
    #[must_use]
    pub fn with_deepl_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.deepl_base_url = Some(base_url.into());
        self
    }

    /// Overrides the Google Cloud Translation v2 endpoint URL (useful for local HTTP tests).
    #[must_use]
    pub fn with_google_cloud_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.google_cloud_base_url = Some(base_url.into());
        self
    }
}

impl TranslatorFactory for HttpTranslatorFactory {
    fn create(&self, config: &TranslationProviderConfig) -> Result<Arc<dyn Translator>, CoreError> {
        match config {
            TranslationProviderConfig::Mock { prefix } => {
                Ok(Arc::new(MockTranslator::new("mock", prefix.clone())))
            }
            TranslationProviderConfig::OpenAiCompatible {
                base_url,
                model,
                api_key,
                timeout_ms,
            } => {
                let translator = OpenAiCompatibleTranslator::new(&OpenAiCompatibleConfig {
                    base_url: base_url.clone(),
                    model: model.clone(),
                    api_key: api_key.clone(),
                    timeout_ms: *timeout_ms,
                })?;
                Ok(Arc::new(translator))
            }
            TranslationProviderConfig::DeepL {
                api_key,
                use_free_endpoint,
                timeout_ms,
            } => {
                let translator = DeepLTranslator::new(&DeepLConfig {
                    api_key: api_key.clone(),
                    use_free_endpoint: *use_free_endpoint,
                    base_url: self.deepl_base_url.clone(),
                    timeout_ms: *timeout_ms,
                })?;
                Ok(Arc::new(translator))
            }
            TranslationProviderConfig::GoogleCloud {
                api_key,
                project_id,
                timeout_ms,
            } => {
                let translator = GoogleCloudTranslator::new(&GoogleCloudConfig {
                    api_key: api_key.clone(),
                    project_id: project_id.clone(),
                    base_url: self.google_cloud_base_url.clone(),
                    timeout_ms: *timeout_ms,
                })?;
                Ok(Arc::new(translator))
            }
            TranslationProviderConfig::GasWebhook {
                endpoint_url,
                bearer_token,
                timeout_ms,
            } => {
                let translator = GasWebhookTranslator::new(&GasWebhookConfig {
                    endpoint_url: endpoint_url.clone(),
                    bearer_token: bearer_token.clone(),
                    timeout_ms: *timeout_ms,
                })?;
                Ok(Arc::new(translator))
            }
            TranslationProviderConfig::LocalModel { model_id, .. } => Err(CoreError::NotConfigured(
                format!(
                    "local NMT model '{model_id}' must be constructed via LocalNmtTranslatorFactory or CompositeTranslatorFactory"
                ),
            )),
        }
    }
}
