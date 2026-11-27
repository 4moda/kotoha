//! Secret value objects: [`Secret`] (redacted in-memory value), [`SecretRef`] (validated store
//! key) and [`ProviderSecretKind`] (the fixed set of secrets each translation provider may use).
//!
//! Provider settings contain **no** secrets and **no** secret references: which secret a provider
//! reads is derived from the provider variant itself ([`TranslationProviderConfig::secret_refs`]).
//! This prevents a "confused deputy" where a user-chosen reference (e.g. the `DeepL` key) is sent
//! to a user-chosen host (e.g. an OpenAI-compatible `base_url`).
//!
//! [`Secret`] deliberately implements neither `Serialize` nor `Deserialize` nor `PartialEq`, and
//! its `Debug` / `Display` output is always redacted. The raw value is only reachable via
//! [`Secret::expose`].

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::domain::entities::TranslationProviderConfig;
use crate::domain::error::CoreError;

/// Placeholder printed instead of any secret value.
pub const REDACTED: &str = "***";

/// Maximum length of a [`SecretRef`] name.
pub const MAX_SECRET_REF_LEN: usize = 64;

/// A secret string (API key, bearer token, capability URL) that never prints or serializes.
///
/// Zeroize-on-drop is intentionally not implemented: `unsafe_code` is forbidden workspace-wide
/// and a safe best-effort wipe can be optimized away. Adding the `zeroize` crate is a follow-up
/// together with OS keychain integration.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    /// Wraps `value` as a [`Secret`].
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the raw secret value. Call only at the point of use (building an HTTP header, URL).
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Returns `true` if the value is empty or whitespace only.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret({REDACTED})")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// Validated, non-secret name of a secret entry (e.g. `"translation.deepl.api_key"`).
///
/// Grammar: dot-separated segments, each `[a-z0-9]+` optionally joined by single underscores
/// (`translation.google_cloud.api_key`); at most [`MAX_SECRET_REF_LEN`] characters. The grammar
/// makes the mapping to environment variable names injective (see `EnvSecretStore`) and keeps
/// names safe for keychain entries.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SecretRef(String);

impl SecretRef {
    /// Validates and creates a [`SecretRef`].
    pub fn new(name: impl Into<String>) -> Result<Self, CoreError> {
        let name = name.into();
        validate_secret_ref(&name)?;
        Ok(Self(name))
    }

    /// Returns the key name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `translation.openai_compatible.api_key`.
    #[must_use]
    pub fn openai_api_key() -> Self {
        Self("translation.openai_compatible.api_key".to_owned())
    }

    /// `translation.deepl.api_key`.
    #[must_use]
    pub fn deepl_api_key() -> Self {
        Self("translation.deepl.api_key".to_owned())
    }

    /// `translation.google_cloud.api_key`.
    #[must_use]
    pub fn google_cloud_api_key() -> Self {
        Self("translation.google_cloud.api_key".to_owned())
    }

    /// `translation.gas_webhook.endpoint_url`.
    #[must_use]
    pub fn gas_endpoint_url() -> Self {
        Self("translation.gas_webhook.endpoint_url".to_owned())
    }

    /// `translation.gas_webhook.bearer_token`.
    #[must_use]
    pub fn gas_bearer_token() -> Self {
        Self("translation.gas_webhook.bearer_token".to_owned())
    }
}

fn validate_secret_ref(name: &str) -> Result<(), CoreError> {
    let invalid = |why: &str| CoreError::InvalidSecretRef(format!("{why} (len {})", name.len()));
    if name.is_empty() {
        return Err(invalid("secret reference must not be empty"));
    }
    if name.len() > MAX_SECRET_REF_LEN {
        return Err(invalid("secret reference is too long"));
    }
    for segment in name.split('.') {
        if segment.is_empty() {
            return Err(invalid("secret reference has an empty segment"));
        }
        for part in segment.split('_') {
            if part.is_empty()
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            {
                return Err(invalid(
                    "secret reference must match [a-z0-9]+(_[a-z0-9]+)* segments joined by '.'",
                ));
            }
        }
    }
    Ok(())
}

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The closed set of secrets a translation provider can use. Exposed to the UI as a write-only
/// command argument (`snake_case`); the UI never reads values back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSecretKind {
    /// API key for OpenAI-compatible endpoints (optional for local servers).
    OpenAiApiKey,
    /// `DeepL` authentication key.
    DeepLApiKey,
    /// Google Cloud Translation API key.
    GoogleCloudApiKey,
    /// GAS / webhook endpoint URL (a bearer capability, therefore a secret).
    GasEndpointUrl,
    /// Optional bearer token for the GAS / webhook endpoint.
    GasBearerToken,
}

impl ProviderSecretKind {
    /// All kinds, in a stable order.
    pub const ALL: [Self; 5] = [
        Self::OpenAiApiKey,
        Self::DeepLApiKey,
        Self::GoogleCloudApiKey,
        Self::GasEndpointUrl,
        Self::GasBearerToken,
    ];

    /// Fixed store key for this kind.
    #[must_use]
    pub fn secret_ref(self) -> SecretRef {
        match self {
            Self::OpenAiApiKey => SecretRef::openai_api_key(),
            Self::DeepLApiKey => SecretRef::deepl_api_key(),
            Self::GoogleCloudApiKey => SecretRef::google_cloud_api_key(),
            Self::GasEndpointUrl => SecretRef::gas_endpoint_url(),
            Self::GasBearerToken => SecretRef::gas_bearer_token(),
        }
    }

    /// Returns `true` if `config` is the provider this secret belongs to.
    #[must_use]
    pub const fn applies_to(self, config: &TranslationProviderConfig) -> bool {
        matches!(
            (self, config),
            (
                Self::OpenAiApiKey,
                TranslationProviderConfig::OpenAiCompatible { .. }
            ) | (Self::DeepLApiKey, TranslationProviderConfig::DeepL { .. })
                | (
                    Self::GoogleCloudApiKey,
                    TranslationProviderConfig::GoogleCloud { .. }
                )
                | (
                    Self::GasEndpointUrl | Self::GasBearerToken,
                    TranslationProviderConfig::GasWebhook { .. }
                )
        )
    }

    /// Secrets that must be cleared whenever this one is replaced: a bearer token must not
    /// follow a changed GAS endpoint to a new host.
    #[must_use]
    pub fn dependent_refs_to_clear(self) -> Vec<SecretRef> {
        match self {
            Self::GasEndpointUrl => vec![SecretRef::gas_bearer_token()],
            _ => Vec::new(),
        }
    }
}
