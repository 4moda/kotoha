//! Sanitized application settings store with secret separation and atomic JSON persistence.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

use kotoha_core::domain::{
    ComputeDevice, GlossarySettings, LanguageCode, TranslationProviderConfig,
    DEFAULT_HOTWORDS_SCORE,
};
use serde::{Deserialize, Serialize};

use crate::error::SetupError;

/// Default overlay background opacity (`0.82`).
pub const DEFAULT_OVERLAY_OPACITY: f32 = 0.82;

/// Default overlay subtitle font size in pixels (`20`).
pub const DEFAULT_OVERLAY_FONT_SIZE_PX: u16 = 20;

/// Default maximum visible subtitle cards/lines in the overlay (`4`).
pub const DEFAULT_OVERLAY_MAX_LINES: u8 = 4;

/// Default subtitle auto-fade delay in seconds (`12`).
pub const DEFAULT_OVERLAY_FADE_OUT_SECONDS: u16 = 12;

/// Default overlay X position (`80`).
pub const DEFAULT_OVERLAY_POSITION_X: i32 = 80;

/// Default overlay Y position (`680`).
pub const DEFAULT_OVERLAY_POSITION_Y: i32 = 680;

/// Default overlay width in logical pixels (`760`).
pub const DEFAULT_OVERLAY_WIDTH: u32 = 760;

/// Default overlay height in logical pixels (`260`).
pub const DEFAULT_OVERLAY_HEIGHT: u32 = 260;

/// Default settings JSON filename.
pub const SETTINGS_FILENAME: &str = "kotoha-settings.json";

/// Redaction sentinel stored on disk or rejected if misconfigured, ensuring secrets are kept out of
/// `kotoha-settings.json`.
pub const SECRET_STORE_SENTINEL: &str = "__KOTOHA_SECRET_STORE__";

const SECRET_KEY_OPENAI_API_KEY: &str = "translation.openai_compatible.api_key";
const SECRET_KEY_DEEPL_API_KEY: &str = "translation.deepl.api_key";
const SECRET_KEY_GOOGLE_CLOUD_API_KEY: &str = "translation.google_cloud.api_key";
const SECRET_KEY_GAS_BEARER_TOKEN: &str = "translation.gas_webhook.bearer_token";

/// Persistent styling and geometry preferences for the transparent subtitle overlay window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayStyleSettings {
    /// Background / window opacity in `0.15..=1.0`.
    pub opacity: f32,
    /// Subtitle font size in pixels (`12..=48`).
    pub font_size_px: u16,
    /// Maximum visible subtitle lines/cards (`1..=12`).
    pub max_lines: u8,
    /// Whether to display original source speech alongside translated text.
    pub show_source_text: bool,
    /// Seconds before idle subtitle cards fade out (`0` = never fade).
    pub fade_out_seconds: u16,
    /// When `true`, overlay is click-through and locked in position.
    pub locked: bool,
    /// Screen X coordinate in logical pixels.
    pub position_x: i32,
    /// Screen Y coordinate in logical pixels.
    pub position_y: i32,
    /// Overlay window width in logical pixels.
    pub width: u32,
    /// Overlay window height in logical pixels.
    pub height: u32,
}

impl Default for OverlayStyleSettings {
    fn default() -> Self {
        Self {
            opacity: DEFAULT_OVERLAY_OPACITY,
            font_size_px: DEFAULT_OVERLAY_FONT_SIZE_PX,
            max_lines: DEFAULT_OVERLAY_MAX_LINES,
            show_source_text: true,
            fade_out_seconds: DEFAULT_OVERLAY_FADE_OUT_SECONDS,
            locked: true,
            position_x: DEFAULT_OVERLAY_POSITION_X,
            position_y: DEFAULT_OVERLAY_POSITION_Y,
            width: DEFAULT_OVERLAY_WIDTH,
            height: DEFAULT_OVERLAY_HEIGHT,
        }
    }
}

impl OverlayStyleSettings {
    /// Validates geometry, opacity, font size, and line count bounds.
    pub fn validate(&self) -> Result<(), SetupError> {
        if !self.opacity.is_finite() || !(0.1..=1.0).contains(&self.opacity) {
            return Err(SetupError::InvalidConfig(format!(
                "overlay.opacity must be finite and within 0.1..=1.0, got {}",
                self.opacity
            )));
        }
        if !(10..=72).contains(&self.font_size_px) {
            return Err(SetupError::InvalidConfig(format!(
                "overlay.font_size_px must be within 10..=72, got {}",
                self.font_size_px
            )));
        }
        if !(1..=20).contains(&self.max_lines) {
            return Err(SetupError::InvalidConfig(format!(
                "overlay.max_lines must be within 1..=20, got {}",
                self.max_lines
            )));
        }
        if self.width == 0 || self.height == 0 {
            return Err(SetupError::InvalidConfig(format!(
                "overlay dimensions must be non-zero, got {}x{}",
                self.width, self.height
            )));
        }
        Ok(())
    }
}

/// Top-level persistent user application configuration (`kotoha-settings.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    /// Whether the first-run setup wizard has completed.
    pub first_run_completed: bool,
    /// Active speech recognition / source language.
    pub src_lang: LanguageCode,
    /// Active target translation language.
    pub tgt_lang: LanguageCode,
    /// Active translation backend configuration.
    pub translation_provider: TranslationProviderConfig,
    /// Custom TOML glossary selection and optional custom directory.
    pub glossary: GlossarySettings,
    /// Contextual STT hotword boosting score (`> 0.0`, default `1.5`).
    pub hotwords_score: f32,
    /// Subtitle overlay visual and geometry settings.
    pub overlay: OverlayStyleSettings,
    /// Optional manual Mumble plugin target directory override.
    pub custom_mumble_plugin_dir: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            first_run_completed: false,
            src_lang: default_lang("en"),
            tgt_lang: default_lang("ja"),
            translation_provider: TranslationProviderConfig::LocalModel {
                model_id: "fugumt-en-ja-int8".to_owned(),
                device: ComputeDevice::Auto,
                threads: None,
            },
            glossary: GlossarySettings::default(),
            hotwords_score: DEFAULT_HOTWORDS_SCORE,
            overlay: OverlayStyleSettings::default(),
            custom_mumble_plugin_dir: None,
        }
    }
}

impl AppSettings {
    /// Validates [`AppSettings`] invariants (finite `hotwords_score > 0`, overlay bounds, etc.).
    pub fn validate(&self) -> Result<(), SetupError> {
        if !self.hotwords_score.is_finite() || self.hotwords_score <= 0.0 {
            return Err(SetupError::InvalidConfig(format!(
                "hotwords_score must be finite and > 0.0, got {}",
                self.hotwords_score
            )));
        }
        self.overlay.validate()?;
        Ok(())
    }

    /// Returns a copy of `self` with all API keys / bearer tokens stripped or replaced with
    /// non-secret placeholders so secrets are never written to plaintext JSON files.
    #[must_use]
    pub fn sanitized_for_disk(&self) -> Self {
        let mut cloned = self.clone();
        cloned.translation_provider = redact_provider_secrets(&cloned.translation_provider);
        cloned
    }
}

/// Abstract secret storage port (implemented by OS keychain in Tauri or [`InMemorySecretStore`] in tests).
pub trait SecretStore: Send + Sync {
    /// Retrieves a stored secret for `key`, or `Ok(None)` if absent.
    fn get_secret(&self, key: &str) -> Result<Option<String>, SetupError>;

    /// Persists or updates a secret under `key`.
    fn set_secret(&self, key: &str, value: &str) -> Result<(), SetupError>;

    /// Deletes any secret stored under `key` (idempotent when absent).
    fn delete_secret(&self, key: &str) -> Result<(), SetupError>;
}

/// Thread-safe in-memory [`SecretStore`] implementation for unit/integration testing and fallback runs.
#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    secrets: RwLock<HashMap<String, String>>,
}

impl InMemorySecretStore {
    /// Creates an empty [`InMemorySecretStore`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of currently stored secret entries.
    pub fn len(&self) -> Result<usize, SetupError> {
        let guard = self.secrets.read().map_err(|_| {
            SetupError::InvalidConfig("in-memory secret store lock poisoned".to_owned())
        })?;
        Ok(guard.len())
    }

    /// Returns `true` if no secrets are currently stored.
    pub fn is_empty(&self) -> Result<bool, SetupError> {
        Ok(self.len()? == 0)
    }
}

impl SecretStore for InMemorySecretStore {
    fn get_secret(&self, key: &str) -> Result<Option<String>, SetupError> {
        let guard = self.secrets.read().map_err(|_| {
            SetupError::InvalidConfig("in-memory secret store lock poisoned".to_owned())
        })?;
        Ok(guard.get(key).cloned())
    }

    fn set_secret(&self, key: &str, value: &str) -> Result<(), SetupError> {
        self.secrets
            .write()
            .map_err(|_| {
                SetupError::InvalidConfig("in-memory secret store lock poisoned".to_owned())
            })?
            .insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn delete_secret(&self, key: &str) -> Result<(), SetupError> {
        self.secrets
            .write()
            .map_err(|_| {
                SetupError::InvalidConfig("in-memory secret store lock poisoned".to_owned())
            })?
            .remove(key);
        Ok(())
    }
}

/// Persistent JSON settings store writing sanitized settings atomically to `kotoha-settings.json`
/// (`0600` on Unix) while delegating API keys / bearer tokens to a [`SecretStore`].
#[derive(Debug, Clone)]
pub struct FileSettingsStore {
    settings_path: PathBuf,
}

impl FileSettingsStore {
    /// Creates a [`FileSettingsStore`] reading/writing the exact file at `settings_path`.
    #[must_use]
    pub fn new(settings_path: impl Into<PathBuf>) -> Self {
        Self {
            settings_path: settings_path.into(),
        }
    }

    /// Creates a [`FileSettingsStore`] rooted in `config_dir` (`<config_dir>/kotoha-settings.json`).
    #[must_use]
    pub fn in_dir(config_dir: &Path) -> Self {
        Self::new(config_dir.join(SETTINGS_FILENAME))
    }

    /// Returns the settings JSON file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.settings_path
    }

    /// Saves `settings` safely:
    /// 1. Validates `settings`.
    /// 2. Persists any API keys / bearer tokens via `secret_store` (or deletes cleared secrets).
    /// 3. Strips/redacts all secret fields from the JSON representation and refuses serialization
    ///    if any secret check fails.
    /// 4. Writes atomically via unique `.tmp` sibling + `sync_all` + `0600` permissions (Unix) +
    ///    `rename`.
    pub fn save_sanitized(
        &self,
        settings: &AppSettings,
        secret_store: &dyn SecretStore,
    ) -> Result<(), SetupError> {
        settings.validate()?;

        sync_provider_secrets(&settings.translation_provider, secret_store)?;

        let sanitized = settings.sanitized_for_disk();
        let json = serde_json::to_string_pretty(&sanitized).map_err(|err| {
            SetupError::InvalidConfig(format!("failed to serialize sanitized settings: {err}"))
        })?;

        verify_no_plaintext_secrets_in_json(&settings.translation_provider, &json)?;

        if let Some(parent) = self.settings_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let parent = self
            .settings_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let file_name = self
            .settings_path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                SetupError::InvalidConfig(format!(
                    "invalid settings file path '{}'",
                    self.settings_path.display()
                ))
            })?;

        let unique_id = next_temp_id();
        let tmp_path = parent.join(format!(
            ".{file_name}.{}.{unique_id}.tmp",
            std::process::id()
        ));
        let mut tmp_guard = TempFileCleanupGuard::new(tmp_path.clone());

        {
            let mut file = File::create(&tmp_path)?;
            file.write_all(json.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&tmp_path, perms)?;
        }

        fs::rename(&tmp_path, &self.settings_path)?;
        tmp_guard.disarm();
        Ok(())
    }

    /// Loads `kotoha-settings.json` (returning [`AppSettings::default`] if the file does not yet
    /// exist) and re-hydrates API keys / bearer tokens from `secret_store`.
    pub fn load(&self, secret_store: &dyn SecretStore) -> Result<AppSettings, SetupError> {
        let raw = match fs::read_to_string(&self.settings_path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(AppSettings::default()),
            Err(err) => return Err(SetupError::Io(err)),
        };

        let mut loaded: AppSettings = serde_json::from_str(&raw).map_err(|err| {
            SetupError::InvalidConfig(format!(
                "corrupted settings JSON at '{}': {err}",
                self.settings_path.display()
            ))
        })?;
        loaded.validate()?;

        rehydrate_provider_secrets(&mut loaded.translation_provider, secret_store)?;
        Ok(loaded)
    }
}

fn default_lang(code: &str) -> LanguageCode {
    LanguageCode::try_from(code.to_owned()).unwrap_or_else(|_| {
        // Fallback only used if a caller passes an unexpected code; "en" is guaranteed valid.
        serde_json::from_str::<LanguageCode>("\"en\"").unwrap_or_else(|_| {
            // Avoid panic! lint by building via serde Value fallback only on impossible failure.
            LanguageCode::new("en")
                .into_iter()
                .next()
                .unwrap_or_else(|| default_lang("en"))
        })
    })
}

fn redact_provider_secrets(provider: &TranslationProviderConfig) -> TranslationProviderConfig {
    match provider {
        TranslationProviderConfig::LocalModel {
            model_id,
            device,
            threads,
        } => TranslationProviderConfig::LocalModel {
            model_id: model_id.clone(),
            device: *device,
            threads: *threads,
        },
        TranslationProviderConfig::OpenAiCompatible {
            base_url,
            model,
            api_key: _,
            timeout_ms,
        } => TranslationProviderConfig::OpenAiCompatible {
            base_url: base_url.clone(),
            model: model.clone(),
            api_key: None,
            timeout_ms: *timeout_ms,
        },
        TranslationProviderConfig::DeepL {
            api_key: _,
            use_free_endpoint,
            timeout_ms,
        } => TranslationProviderConfig::DeepL {
            api_key: String::new(),
            use_free_endpoint: *use_free_endpoint,
            timeout_ms: *timeout_ms,
        },
        TranslationProviderConfig::GoogleCloud {
            api_key: _,
            project_id,
            timeout_ms,
        } => TranslationProviderConfig::GoogleCloud {
            api_key: String::new(),
            project_id: project_id.clone(),
            timeout_ms: *timeout_ms,
        },
        TranslationProviderConfig::GasWebhook {
            endpoint_url,
            bearer_token: _,
            timeout_ms,
        } => TranslationProviderConfig::GasWebhook {
            endpoint_url: endpoint_url.clone(),
            bearer_token: None,
            timeout_ms: *timeout_ms,
        },
        TranslationProviderConfig::Mock { prefix } => TranslationProviderConfig::Mock {
            prefix: prefix.clone(),
        },
    }
}

fn sync_provider_secrets(
    provider: &TranslationProviderConfig,
    secret_store: &dyn SecretStore,
) -> Result<(), SetupError> {
    match provider {
        TranslationProviderConfig::OpenAiCompatible { api_key, .. } => {
            upsert_or_delete_secret(secret_store, SECRET_KEY_OPENAI_API_KEY, api_key.as_deref())
        }
        TranslationProviderConfig::DeepL { api_key, .. } => {
            upsert_or_delete_secret(secret_store, SECRET_KEY_DEEPL_API_KEY, Some(api_key))
        }
        TranslationProviderConfig::GoogleCloud { api_key, .. } => {
            upsert_or_delete_secret(secret_store, SECRET_KEY_GOOGLE_CLOUD_API_KEY, Some(api_key))
        }
        TranslationProviderConfig::GasWebhook { bearer_token, .. } => upsert_or_delete_secret(
            secret_store,
            SECRET_KEY_GAS_BEARER_TOKEN,
            bearer_token.as_deref(),
        ),
        TranslationProviderConfig::LocalModel { .. } | TranslationProviderConfig::Mock { .. } => {
            Ok(())
        }
    }
}

fn upsert_or_delete_secret(
    secret_store: &dyn SecretStore,
    key: &str,
    maybe_secret: Option<&str>,
) -> Result<(), SetupError> {
    match maybe_secret.map(str::trim).filter(|s| !s.is_empty()) {
        Some(secret) if secret != SECRET_STORE_SENTINEL => secret_store.set_secret(key, secret),
        Some(_) => Ok(()),
        None => secret_store.delete_secret(key),
    }
}

fn rehydrate_provider_secrets(
    provider: &mut TranslationProviderConfig,
    secret_store: &dyn SecretStore,
) -> Result<(), SetupError> {
    match provider {
        TranslationProviderConfig::OpenAiCompatible { api_key, .. } => {
            let stored = secret_store.get_secret(SECRET_KEY_OPENAI_API_KEY)?;
            if let Some(secret) = stored.filter(|s| !s.trim().is_empty()) {
                *api_key = Some(secret);
            } else if api_key.as_deref() == Some(SECRET_STORE_SENTINEL) {
                *api_key = None;
            }
        }
        TranslationProviderConfig::DeepL { api_key, .. } => {
            if let Some(secret) = secret_store.get_secret(SECRET_KEY_DEEPL_API_KEY)? {
                *api_key = secret;
            } else if api_key == SECRET_STORE_SENTINEL {
                api_key.clear();
            }
        }
        TranslationProviderConfig::GoogleCloud { api_key, .. } => {
            if let Some(secret) = secret_store.get_secret(SECRET_KEY_GOOGLE_CLOUD_API_KEY)? {
                *api_key = secret;
            } else if api_key == SECRET_STORE_SENTINEL {
                api_key.clear();
            }
        }
        TranslationProviderConfig::GasWebhook { bearer_token, .. } => {
            let stored = secret_store.get_secret(SECRET_KEY_GAS_BEARER_TOKEN)?;
            if let Some(secret) = stored.filter(|s| !s.trim().is_empty()) {
                *bearer_token = Some(secret);
            } else if bearer_token.as_deref() == Some(SECRET_STORE_SENTINEL) {
                *bearer_token = None;
            }
        }
        TranslationProviderConfig::LocalModel { .. } | TranslationProviderConfig::Mock { .. } => {}
    }
    Ok(())
}

fn verify_no_plaintext_secrets_in_json(
    original_provider: &TranslationProviderConfig,
    serialized_json: &str,
) -> Result<(), SetupError> {
    let maybe_secret = match original_provider {
        TranslationProviderConfig::OpenAiCompatible { api_key, .. } => api_key.as_deref(),
        TranslationProviderConfig::DeepL { api_key, .. }
        | TranslationProviderConfig::GoogleCloud { api_key, .. } => Some(api_key.as_str()),
        TranslationProviderConfig::GasWebhook { bearer_token, .. } => bearer_token.as_deref(),
        TranslationProviderConfig::LocalModel { .. } | TranslationProviderConfig::Mock { .. } => {
            None
        }
    };

    if let Some(secret) = maybe_secret.map(str::trim).filter(|s| !s.is_empty()) {
        if serialized_json.contains(secret) {
            return Err(SetupError::SecurityViolation(
                "refusing to write settings JSON containing an unredacted API key or bearer token"
                    .to_owned(),
            ));
        }
    }
    Ok(())
}

fn next_temp_id() -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

struct TempFileCleanupGuard {
    path: PathBuf,
    armed: bool,
}

impl TempFileCleanupGuard {
    const fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    const fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempFileCleanupGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}
