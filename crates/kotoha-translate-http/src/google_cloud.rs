//! Official Google Cloud Translation v2 REST adapter (API key via `X-Goog-Api-Key` header).
//!
//! Per `DESIGN.md` Section 4.2, Kotoha strictly uses the official Google Cloud Translation REST API
//! and never uses unofficial web scraping endpoints such as `translate_a/single`.

use std::time::Instant;

use async_trait::async_trait;
use kotoha_core::domain::{
    CoreError, GlossaryPlaceholderGuard, TranslationRequest, TranslationResult, Translator,
};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::client::{
    build_http_client, decode_json_response, elapsed_ms, insert_sensitive_header,
    map_reqwest_error, validate_http_url, validate_sensitive_header_value, RedirectMode,
    DEFAULT_HTTP_TIMEOUT_MS,
};

/// Official Google Cloud Translation v2 REST endpoint.
pub const GOOGLE_CLOUD_V2_ENDPOINT: &str =
    "https://translation.googleapis.com/language/translate/v2";

const HEADER_GOOG_API_KEY: HeaderName = HeaderName::from_static("x-goog-api-key");
const HEADER_GOOG_USER_PROJECT: HeaderName = HeaderName::from_static("x-goog-user-project");

/// Configuration for [`GoogleCloudTranslator`].
#[derive(Clone, PartialEq, Eq)]
pub struct GoogleCloudConfig {
    /// Google Cloud API key sent exclusively in `X-Goog-Api-Key` header (never URL query string).
    pub api_key: String,
    /// Optional GCP project ID sent in `X-Goog-User-Project`.
    pub project_id: Option<String>,
    /// Optional custom endpoint override (e.g. local mock server URL for testing).
    pub base_url: Option<String>,
    /// HTTP timeout in milliseconds (`100..=120_000`).
    pub timeout_ms: u64,
}

impl std::fmt::Debug for GoogleCloudConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleCloudConfig")
            .field("api_key", &"[REDACTED]")
            .field("project_id", &self.project_id)
            .field("base_url", &self.base_url)
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

impl GoogleCloudConfig {
    /// Creates a [`GoogleCloudConfig`] with [`DEFAULT_HTTP_TIMEOUT_MS`].
    #[must_use]
    pub fn new(api_key: impl Into<String>, project_id: Option<String>) -> Self {
        Self {
            api_key: api_key.into(),
            project_id,
            base_url: None,
            timeout_ms: DEFAULT_HTTP_TIMEOUT_MS,
        }
    }
}

/// [`Translator`] adapter targeting the official Google Cloud Translation v2 REST API.
#[derive(Clone)]
pub struct GoogleCloudTranslator {
    client: Client,
    endpoint_url: String,
    api_key_header: HeaderValue,
    project_header: Option<HeaderValue>,
}

impl std::fmt::Debug for GoogleCloudTranslator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleCloudTranslator")
            .field("endpoint_url", &self.endpoint_url)
            .field("api_key_header", &"[REDACTED]")
            .field("has_project_id", &self.project_header.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Serialize)]
struct GoogleTranslateV2Request<'a> {
    q: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<&'a str>,
    target: &'a str,
    format: &'static str,
}

#[derive(Debug, Deserialize)]
struct GoogleTranslateV2Response {
    data: GoogleTranslateV2Data,
}

#[derive(Debug, Deserialize)]
struct GoogleTranslateV2Data {
    translations: Vec<GoogleTranslateV2Item>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleTranslateV2Item {
    translated_text: String,
}

impl GoogleCloudTranslator {
    /// Provider identifier returned by [`Translator::provider_id`].
    pub const PROVIDER_ID: &'static str = "google-cloud";

    /// Constructs a validated [`GoogleCloudTranslator`] with redirects disabled (`Policy::none`).
    pub fn new(config: &GoogleCloudConfig) -> Result<Self, CoreError> {
        let api_key = config.api_key.trim();
        if api_key.is_empty() {
            return Err(CoreError::NotConfigured(
                "GoogleCloud Translation requires a non-empty api_key".to_owned(),
            ));
        }

        let api_key_header = validate_sensitive_header_value("GoogleCloud.api_key", api_key)?;

        let project_header = config
            .project_id
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|project| validate_sensitive_header_value("GoogleCloud.project_id", project))
            .transpose()?;

        let endpoint_url = match config.base_url {
            Some(ref custom) if !custom.trim().is_empty() => {
                let normalized = validate_http_url("GoogleCloud.base_url", custom, true)?;
                if normalized.ends_with("/language/translate/v2") {
                    normalized
                } else {
                    format!("{normalized}/language/translate/v2")
                }
            }
            _ => GOOGLE_CLOUD_V2_ENDPOINT.to_owned(),
        };

        let client = build_http_client(config.timeout_ms, RedirectMode::None)?;
        Ok(Self {
            client,
            endpoint_url,
            api_key_header,
            project_header,
        })
    }

    /// Returns the resolved Google Cloud Translation v2 endpoint URL.
    #[must_use]
    pub fn endpoint_url(&self) -> &str {
        &self.endpoint_url
    }
}

#[async_trait]
impl Translator for GoogleCloudTranslator {
    fn provider_id(&self) -> &'static str {
        Self::PROVIDER_ID
    }

    async fn translate(&self, req: TranslationRequest) -> Result<TranslationResult, CoreError> {
        let started = Instant::now();
        if req.tgt_lang.as_str().eq_ignore_ascii_case("auto") {
            return Err(CoreError::InvalidLanguageCode(
                "target language cannot be 'auto' for Google Cloud Translation".to_owned(),
            ));
        }

        let source = if req.src_lang.as_str().eq_ignore_ascii_case("auto") {
            None
        } else {
            Some(req.src_lang.as_str())
        };

        let (protected_text, guard) =
            GlossaryPlaceholderGuard::protect(&req.text, &req.context.glossary);

        let payload = GoogleTranslateV2Request {
            q: &protected_text,
            source,
            target: req.tgt_lang.as_str(),
            format: "text",
        };

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        insert_sensitive_header(
            &mut headers,
            HEADER_GOOG_API_KEY,
            self.api_key_header.clone(),
        );
        if let Some(ref project_header) = self.project_header {
            insert_sensitive_header(
                &mut headers,
                HEADER_GOOG_USER_PROJECT,
                project_header.clone(),
            );
        }

        let response = self
            .client
            .post(&self.endpoint_url)
            .headers(headers)
            .json(&payload)
            .send()
            .await
            .map_err(|err| map_reqwest_error(Self::PROVIDER_ID, &err))?;

        let decoded: GoogleTranslateV2Response =
            decode_json_response(Self::PROVIDER_ID, response).await?;

        let raw_translated = decoded
            .data
            .translations
            .into_iter()
            .next()
            .map(|t| t.translated_text)
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
