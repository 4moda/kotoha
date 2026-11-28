//! Factory constructing HTTP & Mock [`Translator`] adapters from [`TranslationProviderConfig`].

use std::sync::Arc;

use kotoha_core::domain::{
    CoreError, SecretRef, SecretStore, TranslationProviderConfig, Translator, TranslatorFactory,
};
use kotoha_core::infrastructure::{EnvSecretStore, MockTranslator};

use crate::deepl::{DeepLConfig, DeepLTranslator};
use crate::gas_webhook::{GasWebhookConfig, GasWebhookTranslator};
use crate::google_cloud::{GoogleCloudConfig, GoogleCloudTranslator};
use crate::openai::{OpenAiCompatibleConfig, OpenAiCompatibleTranslator};

/// [`TranslatorFactory`] constructing HTTP translation adapters (`OpenAiCompatible`, `DeepL`,
/// `GoogleCloud`, `GasWebhook`) and `Mock`.
///
/// Supports optional endpoint overrides for deterministic integration testing without hitting
/// public cloud APIs.
///
/// Secrets (API keys, bearer tokens, the GAS endpoint URL) are never part of the config. Each
/// provider variant reads only its own fixed [`SecretRef`]s (never user-chosen, so one provider's
/// key cannot be sent to another provider's host); they are resolved from the configured [`SecretStore`] (default: [`EnvSecretStore`], i.e.
/// `KOTOHA_SECRET_*` environment variables) immediately before the adapter is constructed.
#[derive(Debug, Clone)]
pub struct HttpTranslatorFactory {
    deepl_base_url: Option<String>,
    google_cloud_base_url: Option<String>,
    secrets: Arc<dyn SecretStore>,
}

impl Default for HttpTranslatorFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpTranslatorFactory {
    /// Creates an [`HttpTranslatorFactory`] targeting official cloud endpoints, resolving
    /// secrets from `KOTOHA_SECRET_*` environment variables.
    #[must_use]
    pub fn new() -> Self {
        Self::with_secret_store(Arc::new(EnvSecretStore::new()))
    }

    /// Creates an [`HttpTranslatorFactory`] resolving secrets through `secrets`.
    #[must_use]
    pub fn with_secret_store(secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            deepl_base_url: None,
            google_cloud_base_url: None,
            secrets,
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
                timeout_ms,
            } => {
                let translator = OpenAiCompatibleTranslator::new(&OpenAiCompatibleConfig {
                    base_url: base_url.clone(),
                    model: model.clone(),
                    api_key: self.secrets.resolve_if_set(&SecretRef::openai_api_key())?,
                    timeout_ms: *timeout_ms,
                })?;
                Ok(Arc::new(translator))
            }
            TranslationProviderConfig::DeepL {
                use_free_endpoint,
                timeout_ms,
            } => {
                let translator = DeepLTranslator::new(&DeepLConfig {
                    api_key: self.secrets.resolve(&SecretRef::deepl_api_key())?,
                    use_free_endpoint: *use_free_endpoint,
                    base_url: self.deepl_base_url.clone(),
                    timeout_ms: *timeout_ms,
                })?;
                Ok(Arc::new(translator))
            }
            TranslationProviderConfig::GoogleCloud {
                project_id,
                timeout_ms,
            } => {
                let translator = GoogleCloudTranslator::new(&GoogleCloudConfig {
                    api_key: self.secrets.resolve(&SecretRef::google_cloud_api_key())?,
                    project_id: project_id.clone(),
                    base_url: self.google_cloud_base_url.clone(),
                    timeout_ms: *timeout_ms,
                })?;
                Ok(Arc::new(translator))
            }
            TranslationProviderConfig::GasWebhook { timeout_ms } => {
                let translator = GasWebhookTranslator::new(&GasWebhookConfig {
                    endpoint_url: self.secrets.resolve(&SecretRef::gas_endpoint_url())?,
                    bearer_token: self.secrets.resolve_if_set(&SecretRef::gas_bearer_token())?,
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
