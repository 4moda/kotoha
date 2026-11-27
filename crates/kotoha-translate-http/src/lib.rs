//! HTTP translation provider adapters for Kotoha (`rustls-tls`, zero OpenSSL dependency).
//!
//! Implements [`kotoha_core::domain::ports::Translator`] for:
//! - [`OpenAiCompatibleTranslator`] (`openai-compatible`): `OpenAI`, Gemini `OpenAI` endpoint, Ollama, LM Studio.
//! - [`DeepLTranslator`] (`deepl`): Official `DeepL` v2 Free / Pro REST API.
//! - [`GoogleCloudTranslator`] (`google-cloud`): Official Google Cloud Translation v2 REST API.
//! - [`GasWebhookTranslator`] (`gas-webhook`): User-deployed Google Apps Script Web App (`302 Found`).

pub mod client;
pub mod deepl;
pub mod factory;
pub mod gas_webhook;
pub mod google_cloud;
pub mod openai;

pub use client::{
    DEFAULT_HTTP_TIMEOUT_MS, DEFAULT_MAX_REDIRECTS, MAX_HTTP_RESPONSE_BYTES, MAX_TIMEOUT_MS,
    MIN_TIMEOUT_MS,
};
pub use deepl::{DeepLConfig, DeepLTranslator, DEEPL_FREE_ENDPOINT, DEEPL_PRO_ENDPOINT};
pub use factory::HttpTranslatorFactory;
pub use gas_webhook::{GasWebhookConfig, GasWebhookTranslator};
pub use google_cloud::{GoogleCloudConfig, GoogleCloudTranslator, GOOGLE_CLOUD_V2_ENDPOINT};
pub use openai::{OpenAiCompatibleConfig, OpenAiCompatibleTranslator};
