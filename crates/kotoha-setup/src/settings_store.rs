//! Application settings store with atomic JSON persistence.
//!
//! Settings contain no secrets at all (see `kotoha_core::domain::SecretStore`), so they are
//! serialized as-is. Settings files written by older builds that embedded plaintext keys are
//! migrated on load: the values move into the secret store and the file is rewritten.

use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use kotoha_core::domain::{
    ComputeDevice, GlossarySettings, LanguageCode, ProviderSecretKind, Secret, SecretWriter,
    TranslationProviderConfig, DEFAULT_HOTWORDS_SCORE,
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

/// Placeholder older builds wrote instead of a real secret; never migrated as a value.
const LEGACY_SECRET_SENTINEL: &str = "__KOTOHA_SECRET_STORE__";

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
}

/// Persistent JSON settings store writing settings atomically to `kotoha-settings.json`
/// (`0600` on Unix).
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

    /// Saves `settings` safely: validates, serializes (settings hold no secrets) and writes
    /// atomically via unique `.tmp` sibling + `sync_all` + `0600` permissions (Unix) + `rename`.
    pub fn save(&self, settings: &AppSettings) -> Result<(), SetupError> {
        settings.validate()?;

        let json = serde_json::to_string_pretty(settings).map_err(|err| {
            SetupError::InvalidConfig(format!("failed to serialize settings: {err}"))
        })?;

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
    /// exist).
    ///
    /// Files written by older builds may embed plaintext `api_key` / `bearer_token` /
    /// `endpoint_url` values inside `translation_provider`. Those values are moved into
    /// `secrets` (placeholders and empty values are dropped) and the file is rewritten in the
    /// current secret-free shape, so loading never fails because of the legacy layout.
    pub fn load(&self, secrets: &dyn SecretWriter) -> Result<AppSettings, SetupError> {
        let raw = match fs::read_to_string(&self.settings_path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(AppSettings::default()),
            Err(err) => return Err(SetupError::Io(err)),
        };

        let corrupted = |err: serde_json::Error| {
            SetupError::InvalidConfig(format!(
                "corrupted settings JSON at '{}': {err}",
                self.settings_path.display()
            ))
        };
        let mut value: serde_json::Value = serde_json::from_str(&raw).map_err(corrupted)?;
        let migrated = migrate_legacy_provider_secrets(&mut value, secrets)?;
        let loaded: AppSettings = serde_json::from_value(value).map_err(corrupted)?;
        loaded.validate()?;

        if migrated {
            self.save(&loaded)?;
        }
        Ok(loaded)
    }
}

/// Moves legacy plaintext secret fields out of `translation_provider` into `secrets`.
/// Returns `true` if any legacy field was present (the caller then rewrites the file).
fn migrate_legacy_provider_secrets(
    value: &mut serde_json::Value,
    secrets: &dyn SecretWriter,
) -> Result<bool, SetupError> {
    let Some(provider) = value
        .get_mut("translation_provider")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(false);
    };
    let kind = provider
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();

    let mappings: &[(&str, Option<ProviderSecretKind>)] = match kind.as_str() {
        "open_ai_compatible" | "openai_compatible" => &[
            ("api_key", Some(ProviderSecretKind::OpenAiApiKey)),
            ("bearer_token", None),
            ("endpoint_url", None),
        ],
        "deep_l" | "deepl" => &[
            ("api_key", Some(ProviderSecretKind::DeepLApiKey)),
            ("bearer_token", None),
            ("endpoint_url", None),
        ],
        "google_cloud" => &[
            ("api_key", Some(ProviderSecretKind::GoogleCloudApiKey)),
            ("bearer_token", None),
            ("endpoint_url", None),
        ],
        "gas_webhook" => &[
            ("api_key", None),
            ("bearer_token", Some(ProviderSecretKind::GasBearerToken)),
            ("endpoint_url", Some(ProviderSecretKind::GasEndpointUrl)),
        ],
        _ => &[
            ("api_key", None),
            ("bearer_token", None),
            ("endpoint_url", None),
        ],
    };

    let mut migrated = false;
    // Endpoint first, so a legacy bearer token written afterwards is not cleared by the
    // "new endpoint invalidates the token" rule applied by the application layer.
    for (field, target) in mappings.iter().rev() {
        let Some(old) = provider.remove(*field) else {
            continue;
        };
        migrated = true;
        let text = old.as_str().map(str::trim).unwrap_or_default();
        if let Some(kind) = target {
            if !text.is_empty() && text != LEGACY_SECRET_SENTINEL {
                secrets.set(&kind.secret_ref(), Secret::new(text))?;
            }
        }
    }
    Ok(migrated)
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
