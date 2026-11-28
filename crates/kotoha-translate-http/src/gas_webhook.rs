//! Google Apps Script (`LanguageApp.translate()`) / custom HTTP webhook translator adapter.

use std::time::Instant;

use async_trait::async_trait;
use kotoha_core::domain::{
    CoreError, GlossaryPlaceholderGuard, Secret, TranslationRequest, TranslationResult, Translator,
};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};

use crate::client::{
    build_http_client, decode_json_response, elapsed_ms, insert_sensitive_header, is_loopback_url,
    map_reqwest_error, validate_http_url, validate_sensitive_header_value, RedirectMode,
    DEFAULT_HTTP_TIMEOUT_MS, DEFAULT_MAX_REDIRECTS,
};

/// Configuration for [`GasWebhookTranslator`].
#[derive(Debug, Clone)]
pub struct GasWebhookConfig {
    /// Deployed Google Apps Script Web App URL or custom webhook endpoint.
    ///
    /// Held as a [`Secret`]: a GAS deployment URL works as a bearer capability, so it is
    /// redacted in `Debug` and never included in error messages.
    pub endpoint_url: Secret,
    /// Optional Bearer authorization token.
    pub bearer_token: Option<Secret>,
    /// HTTP timeout in milliseconds (`100..=120_000`).
    pub timeout_ms: u64,
}

impl GasWebhookConfig {
    /// Creates a [`GasWebhookConfig`] with [`DEFAULT_HTTP_TIMEOUT_MS`].
    #[must_use]
    pub fn new(endpoint_url: impl Into<Secret>, bearer_token: Option<Secret>) -> Self {
        Self {
            endpoint_url: endpoint_url.into(),
            bearer_token,
            timeout_ms: DEFAULT_HTTP_TIMEOUT_MS,
        }
    }
}

/// [`Translator`] adapter posting JSON to a GAS Web App (`302 Found` redirect-capable).
#[derive(Clone)]
pub struct GasWebhookTranslator {
    client: Client,
    endpoint_url: Secret,
    auth_header: Option<HeaderValue>,
}

impl std::fmt::Debug for GasWebhookTranslator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GasWebhookTranslator")
            .field("endpoint_url", &self.endpoint_url)
            .field("has_bearer_token", &self.auth_header.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Serialize)]
struct GasWebhookRequest<'a> {
    text: &'a str,
    source: &'a str,
    target: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GasWebhookResponse {
    #[serde(default)]
    translated_text: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

impl GasWebhookTranslator {
    /// Provider identifier returned by [`Translator::provider_id`].
    pub const PROVIDER_ID: &'static str = "gas-webhook";

    /// Constructs a validated [`GasWebhookTranslator`] configured with SSRF-guarded
    /// redirect handling (`max_hops = 5`, rejecting HTTPS->HTTP downgrades and link-local /
    /// private network redirects unless the initial endpoint is explicitly loopback).
    pub fn new(config: &GasWebhookConfig) -> Result<Self, CoreError> {
        let auth_header = config
            .bearer_token
            .as_ref()
            .map(|t| t.expose().trim())
            .filter(|t| !t.is_empty())
            .map(|token| {
                validate_sensitive_header_value(
                    "GasWebhook.bearer_token",
                    &format!("Bearer {token}"),
                )
            })
            .transpose()?;

        let endpoint_url = validate_http_url(
            "GasWebhook.endpoint_url",
            config.endpoint_url.expose(),
            auth_header.is_some(),
        )?;

        let allow_loopback_targets =
            Url::parse(&endpoint_url).is_ok_and(|url| is_loopback_url(&url));

        let client = build_http_client(
            config.timeout_ms,
            RedirectMode::SafeGas {
                max_hops: DEFAULT_MAX_REDIRECTS,
                allow_loopback_targets,
            },
        )?;

        Ok(Self {
            client,
            endpoint_url: Secret::new(endpoint_url),
            auth_header,
        })
    }
}

#[async_trait]
impl Translator for GasWebhookTranslator {
    fn provider_id(&self) -> &'static str {
        Self::PROVIDER_ID
    }

    async fn translate(&self, req: TranslationRequest) -> Result<TranslationResult, CoreError> {
        let started = Instant::now();
        if req.tgt_lang.as_str().eq_ignore_ascii_case("auto") {
            return Err(CoreError::InvalidLanguageCode(
                "target language cannot be 'auto' for GAS Webhook".to_owned(),
            ));
        }

        let source = if req.src_lang.as_str().eq_ignore_ascii_case("auto") {
            ""
        } else {
            req.src_lang.as_str()
        };

        let (protected_text, guard) =
            GlossaryPlaceholderGuard::protect(&req.text, &req.context.glossary);

        let payload = GasWebhookRequest {
            text: &protected_text,
            source,
            target: req.tgt_lang.as_str(),
        };

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(ref auth_header) = self.auth_header {
            insert_sensitive_header(&mut headers, AUTHORIZATION, auth_header.clone());
        }

        let response = self
            .client
            .post(self.endpoint_url.expose())
            .headers(headers)
            .json(&payload)
            .send()
            .await
            .map_err(|err| map_reqwest_error(Self::PROVIDER_ID, &err))?;

        let decoded: GasWebhookResponse = decode_json_response(Self::PROVIDER_ID, response).await?;

        if let Some(err_msg) = decoded.error.filter(|m| !m.trim().is_empty()) {
            return Err(CoreError::Translation(format!(
                "{} returned error: {err_msg}",
                Self::PROVIDER_ID
            )));
        }

        let raw_translated = decoded.translated_text.ok_or_else(|| {
            CoreError::Translation(format!(
                "{} response missing 'translatedText' field",
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
