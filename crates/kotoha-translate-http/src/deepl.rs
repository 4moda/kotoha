//! Official `DeepL` API (Free / Pro) translator adapter with glossary placeholder guarding.

use std::time::Instant;

use async_trait::async_trait;
use kotoha_core::domain::{
    CoreError, GlossaryPlaceholderGuard, LanguageCode, Secret, TranslationRequest,
    TranslationResult, Translator,
};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::client::{
    build_http_client, decode_json_response, elapsed_ms, insert_sensitive_header,
    map_reqwest_error, validate_http_url, validate_sensitive_header_value, RedirectMode,
    DEFAULT_HTTP_TIMEOUT_MS,
};

/// Official `DeepL` API Free endpoint URL.
pub const DEEPL_FREE_ENDPOINT: &str = "https://api-free.deepl.com/v2/translate";

/// Official `DeepL` API Pro endpoint URL.
pub const DEEPL_PRO_ENDPOINT: &str = "https://api.deepl.com/v2/translate";

/// Configuration for [`DeepLTranslator`].
#[derive(Debug, Clone)]
pub struct DeepLConfig {
    /// `DeepL` API authentication key.
    pub api_key: Secret,
    /// When `true` uses Free endpoint (`api-free.deepl.com`), else Pro (`api.deepl.com`).
    pub use_free_endpoint: bool,
    /// Optional custom endpoint override (e.g. local mock server URL for testing).
    pub base_url: Option<String>,
    /// HTTP timeout in milliseconds (`100..=120_000`).
    pub timeout_ms: u64,
}

impl DeepLConfig {
    /// Creates a [`DeepLConfig`] with [`DEFAULT_HTTP_TIMEOUT_MS`] and default Free/Pro endpoint.
    #[must_use]
    pub fn new(api_key: impl Into<Secret>, use_free_endpoint: bool) -> Self {
        Self {
            api_key: api_key.into(),
            use_free_endpoint,
            base_url: None,
            timeout_ms: DEFAULT_HTTP_TIMEOUT_MS,
        }
    }
}

/// [`Translator`] adapter for the official `DeepL` v2 JSON API.
#[derive(Clone)]
pub struct DeepLTranslator {
    client: Client,
    endpoint_url: String,
    auth_header: HeaderValue,
}

impl std::fmt::Debug for DeepLTranslator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeepLTranslator")
            .field("endpoint_url", &self.endpoint_url)
            .field("auth_header", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Serialize)]
struct DeepLRequest<'a> {
    text: [&'a str; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    source_lang: Option<String>,
    target_lang: String,
}

#[derive(Debug, Deserialize)]
struct DeepLResponse {
    translations: Vec<DeepLTranslationItem>,
}

#[derive(Debug, Deserialize)]
struct DeepLTranslationItem {
    text: String,
}

impl DeepLTranslator {
    /// Provider identifier returned by [`Translator::provider_id`].
    pub const PROVIDER_ID: &'static str = "deepl";

    /// Constructs a validated [`DeepLTranslator`] with redirects disabled (`Policy::none`).
    pub fn new(config: &DeepLConfig) -> Result<Self, CoreError> {
        let api_key = config.api_key.expose().trim();
        if api_key.is_empty() {
            return Err(CoreError::NotConfigured(
                "DeepL requires a non-empty api_key".to_owned(),
            ));
        }

        let auth_header =
            validate_sensitive_header_value("DeepL.api_key", &format!("DeepL-Auth-Key {api_key}"))?;

        let endpoint_url = match config.base_url {
            Some(ref custom) if !custom.trim().is_empty() => {
                let normalized = validate_http_url("DeepL.base_url", custom, true)?;
                if normalized.ends_with("/v2/translate") {
                    normalized
                } else {
                    format!("{normalized}/v2/translate")
                }
            }
            _ => {
                if config.use_free_endpoint {
                    DEEPL_FREE_ENDPOINT.to_owned()
                } else {
                    DEEPL_PRO_ENDPOINT.to_owned()
                }
            }
        };

        let client = build_http_client(config.timeout_ms, RedirectMode::None)?;
        Ok(Self {
            client,
            endpoint_url,
            auth_header,
        })
    }

    /// Returns the resolved `DeepL` translate endpoint URL.
    #[must_use]
    pub fn endpoint_url(&self) -> &str {
        &self.endpoint_url
    }

    /// Maps Kotoha [`LanguageCode`] into `DeepL` `source_lang` codes, stripping regional/script
    /// subtags (`"en-US"` -> `"EN"`, `"pt-BR"` -> `"PT"`, `"zh-Hant"` -> `"ZH"`, `"no"` -> `"NB"`)
    /// and returning `None` for `"auto"`.
    #[must_use]
    pub fn map_source_lang(code: &LanguageCode) -> Option<String> {
        let raw = code.as_str().trim();
        if raw.eq_ignore_ascii_case("auto") {
            return None;
        }
        let lower = raw.to_ascii_lowercase().replace('_', "-");
        let base = lower.split('-').next().unwrap_or(&lower);
        let mapped = match base {
            "no" | "nb" => "NB".to_owned(),
            other => other.to_ascii_uppercase(),
        };
        Some(mapped)
    }

    /// Maps Kotoha target [`LanguageCode`] into `DeepL` `target_lang` codes (`"en"` -> `"EN-US"`,
    /// `"en-GB"` -> `"EN-GB"`, `"pt"` -> `"PT-PT"`, `"pt-BR"` -> `"PT-BR"`, `"zh"` -> `"ZH-HANS"`,
    /// `"zh-TW"`/`"zh-Hant"` -> `"ZH-HANT"`, `"no"` -> `"NB"`).
    pub fn map_target_lang(code: &LanguageCode) -> Result<String, CoreError> {
        let raw = code.as_str().trim();
        if raw.eq_ignore_ascii_case("auto") {
            return Err(CoreError::InvalidLanguageCode(
                "target language cannot be 'auto' for DeepL".to_owned(),
            ));
        }

        let lower = raw.to_ascii_lowercase().replace('_', "-");
        let mapped = match lower.as_str() {
            "en" | "en-us" => "EN-US".to_owned(),
            "en-gb" => "EN-GB".to_owned(),
            "pt" | "pt-pt" => "PT-PT".to_owned(),
            "pt-br" => "PT-BR".to_owned(),
            "zh" | "zh-cn" | "zh-sg" | "zh-hans" => "ZH-HANS".to_owned(),
            "zh-tw" | "zh-hk" | "zh-mo" | "zh-hant" => "ZH-HANT".to_owned(),
            "no" | "nb" => "NB".to_owned(),
            other if other.starts_with("en-") => "EN-US".to_owned(),
            other if other.starts_with("pt-") => "PT-PT".to_owned(),
            other if other.starts_with("zh-hant") => "ZH-HANT".to_owned(),
            other if other.starts_with("zh-") => "ZH-HANS".to_owned(),
            other => other.to_ascii_uppercase(),
        };
        Ok(mapped)
    }
}

#[async_trait]
impl Translator for DeepLTranslator {
    fn provider_id(&self) -> &'static str {
        Self::PROVIDER_ID
    }

    async fn translate(&self, req: TranslationRequest) -> Result<TranslationResult, CoreError> {
        let started = Instant::now();
        let target_lang = Self::map_target_lang(&req.tgt_lang)?;
        let source_lang = Self::map_source_lang(&req.src_lang);

        let (protected_text, guard) =
            GlossaryPlaceholderGuard::protect(&req.text, &req.context.glossary);

        let payload = DeepLRequest {
            text: [&protected_text],
            source_lang,
            target_lang,
        };

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        insert_sensitive_header(&mut headers, AUTHORIZATION, self.auth_header.clone());

        let response = self
            .client
            .post(&self.endpoint_url)
            .headers(headers)
            .json(&payload)
            .send()
            .await
            .map_err(|err| map_reqwest_error(Self::PROVIDER_ID, &err))?;

        let decoded: DeepLResponse = decode_json_response(Self::PROVIDER_ID, response).await?;
        let raw_translated = decoded
            .translations
            .into_iter()
            .next()
            .map(|t| t.text)
            .ok_or_else(|| {
                CoreError::Translation(format!(
                    "{} response contained empty translations array",
                    Self::PROVIDER_ID
                ))
            })?;

        let translated_text = guard.restore(&raw_translated);

        Ok(TranslationResult {
            utterance_id: req.utterance_id,
            original_text: req.text,
            translated_text,
            provider_id: Self::PROVIDER_ID.to_owned(),
            latency_ms: elapsed_ms(started),
        })
    }
}
