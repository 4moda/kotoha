//! Application state, DTO serialization parity with `app/ui/src/services/tauriBridge.ts`,
//! and helpers for settings persistence, glossary profile loading, and embedded plugin extraction.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kotoha_core::application::{GlossaryManager, TranslatorManager};
use kotoha_core::domain::{
    default_model_catalog, GlossaryProfile, GlossaryRepository, GlossarySettings, LanguageCode,
    ModelProfile, ProviderSecretKind, Secret, SecretStore, SecretWriter, SubtitleEntry,
    TranslationProviderConfig, Translator, TranslatorFactory,
};
use kotoha_core::infrastructure::{InMemorySecretStore, MockTranslator, TomlGlossaryRepository};
use kotoha_protocol::resolve_endpoint;
use kotoha_setup::{
    cleanup_stale_part_files, discover_mumble_plugin_dirs, plugin_filename, AppSettings,
    DownloadPhase, DownloadProgress, FileSettingsStore, MumblePluginDirKind, OverlayStyleSettings,
    PluginInstallOutcome, TargetPlatform,
};
use kotoha_translate_http::HttpTranslatorFactory;
use kotoha_translate_local::{CompositeTranslatorFactory, LocalNmtTranslatorFactory};
use serde::{Deserialize, Serialize};
use tokio::sync::{watch, Mutex, RwLock};
use tokio::task::spawn_blocking;

/// Maximum number of recent subtitle entries kept in memory for initial UI hydration.
pub const MAX_RECENT_SUBTITLES: usize = 64;

/// Tauri event name for 3-stage subtitle updates (`kotoha://subtitle`).
pub const EVENT_SUBTITLE: &str = "kotoha://subtitle";

/// Tauri event name for IPC connection state updates (`kotoha://connection-status`).
pub const EVENT_CONNECTION_STATUS: &str = "kotoha://connection-status";

/// Tauri event name for model download progress (`kotoha://download-progress`).
pub const EVENT_DOWNLOAD_PROGRESS: &str = "kotoha://download-progress";

/// Tauri event name for overlay lock changes (`kotoha://overlay-lock-changed`).
pub const EVENT_OVERLAY_LOCK_CHANGED: &str = "kotoha://overlay-lock-changed";

/// Tauri event name for overlay style updates (`kotoha://overlay-style-changed`).
pub const EVENT_OVERLAY_STYLE_CHANGED: &str = "kotoha://overlay-style-changed";

/// Embedded Mumble shared library bytes injected at compile time via `build.rs`.
const EMBEDDED_MUMBLE_PLUGIN: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/embedded_mumble_plugin.bin"));

/// Returns the compile-time embedded Mumble plugin bytes if non-empty.
#[must_use]
pub const fn embedded_mumble_plugin_bytes() -> Option<&'static [u8]> {
    if EMBEDDED_MUMBLE_PLUGIN.is_empty() {
        None
    } else {
        Some(EMBEDDED_MUMBLE_PLUGIN)
    }
}

/// Host operating system identifier matching `HostOsPlatform` in `app/ui/src/types/kotoha.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostOsPlatform {
    /// Microsoft Windows.
    Windows,
    /// Apple macOS.
    Macos,
    /// Linux.
    Linux,
}

impl HostOsPlatform {
    /// Returns the current host platform.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }

    /// Maps [`TargetPlatform`] to [`HostOsPlatform`].
    #[must_use]
    pub const fn from_target_platform(platform: TargetPlatform) -> Self {
        match platform {
            TargetPlatform::Windows => Self::Windows,
            TargetPlatform::MacOs => Self::Macos,
            TargetPlatform::Linux => Self::Linux,
        }
    }
}

/// IPC connection lifecycle state matching `ConnectionState` in `app/ui/src/types/kotoha.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    /// Plugin is actively connected and sending frames or heartbeats.
    Connected,
    /// Connection dropped and awaiting reconnect.
    Reconnecting,
    /// No active plugin connected.
    Disconnected,
}

/// IPC connection snapshot matching `ConnectionStatus` in `app/ui/src/types/kotoha.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionStatusDto {
    /// Lifecycle state (`connected` | `reconnecting` | `disconnected`).
    pub state: ConnectionState,
    /// Source application name (e.g., `"mumble"`).
    pub source: String,
    /// Reported plugin version from `Hello`, or `null`.
    pub plugin_version: Option<String>,
    /// Active IPC endpoint string.
    pub endpoint: String,
    /// Number of currently tracked speakers across active sources.
    pub active_speakers: usize,
    /// Cumulative dropped frames reported by `SourceSessionManager`.
    pub dropped_frames: u64,
    /// Milliseconds elapsed since the last observed heartbeat/activity, or `null`.
    pub last_heartbeat_ms: Option<u64>,
}

impl ConnectionStatusDto {
    /// Creates an initial disconnected snapshot for `endpoint`.
    #[must_use]
    pub fn disconnected(endpoint: impl Into<String>) -> Self {
        Self {
            state: ConnectionState::Disconnected,
            source: "mumble".to_owned(),
            plugin_version: None,
            endpoint: endpoint.into(),
            active_speakers: 0,
            dropped_frames: 0,
            last_heartbeat_ms: None,
        }
    }
}

/// Overlay display options matching `OverlayDisplayOptions` in `app/ui/src/types/kotoha.ts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayDisplayOptionsDto {
    /// Maximum visible subtitle cards/lines.
    pub max_lines: u8,
    /// Whether to show original source text under translated text.
    pub show_source_text: bool,
    /// Whether the overlay window ignores cursor events (click-through).
    pub locked: bool,
    /// Subtitle font size in pixels.
    pub font_size_px: u16,
    /// Overlay background opacity in `0.1..=1.0`.
    pub background_opacity: f32,
}

impl From<&OverlayStyleSettings> for OverlayDisplayOptionsDto {
    fn from(value: &OverlayStyleSettings) -> Self {
        Self {
            max_lines: value.max_lines,
            show_source_text: value.show_source_text,
            locked: value.locked,
            font_size_px: value.font_size_px,
            background_opacity: value.opacity,
        }
    }
}

/// Partial overlay style patch matching `OverlayStyleUpdateInput` in `tauriBridge.ts`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct OverlayStyleUpdateInput {
    /// Updated maximum visible lines.
    pub max_lines: Option<u8>,
    /// Updated source-text visibility flag.
    pub show_source_text: Option<bool>,
    /// Updated click-through lock state.
    pub locked: Option<bool>,
    /// Updated font size in pixels.
    pub font_size_px: Option<u16>,
    /// Updated background opacity.
    pub background_opacity: Option<f32>,
}

/// Glossary profile summary matching `GlossaryProfileSummary` in `app/ui/src/types/kotoha.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlossaryProfileSummaryDto {
    /// Stable profile slug ID.
    pub id: String,
    /// Profile display name.
    pub name: String,
    /// Source language code.
    pub src_lang: String,
    /// Target language code.
    pub tgt_lang: String,
    /// Number of terms defined in `[terms]`.
    pub term_count: usize,
    /// Whether this profile is currently enabled.
    pub enabled: bool,
    /// Optional relative or full file path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
}

/// Glossary settings state matching `GlossarySettingsState` in `app/ui/src/types/kotoha.ts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GlossarySettingsStateDto {
    /// All loaded glossary profiles.
    pub profiles: Vec<GlossaryProfileSummaryDto>,
    /// Enabled profile names in priority order.
    pub enabled_profiles: Vec<String>,
    /// STT hotword boost score.
    pub hotwords_score: f32,
    /// Configured glossary directory path, or `null`.
    pub glossary_dir: Option<String>,
}

/// Plugin install status matching `PluginInstallStatus` in `app/ui/src/types/kotoha.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginInstallStatus {
    /// Newly installed or updated on disk.
    Installed,
    /// Existing target already matched SHA-256.
    AlreadyUpToDate,
    /// Target file was locked by a running Mumble process.
    LockedByRunningApp,
    /// Plugin binary or target directory was not found.
    NotFound,
    /// Unexpected I/O or configuration error.
    Error,
}

/// Plugin installation result matching `PluginInstallResult` in `app/ui/src/types/kotoha.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInstallResultDto {
    /// Outcome status code.
    pub status: PluginInstallStatus,
    /// Target host OS platform.
    pub platform: HostOsPlatform,
    /// Resolved target plugin directory path.
    pub target_path: String,
    /// Discovered candidate plugin directory paths.
    pub detected_paths: Vec<String>,
    /// Whether a Flatpak Mumble installation was detected.
    pub is_flatpak: bool,
    /// Flatpak sandbox socket guidance when `is_flatpak == true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flatpak_hint: Option<String>,
    /// Human-readable status message.
    pub message: String,
}

/// Model download lifecycle status matching `ModelDownloadStatus` in `app/ui/src/types/kotoha.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelDownloadStatus {
    /// Download idle.
    Idle,
    /// Connecting or streaming bytes to `.part`.
    Downloading,
    /// Computing SHA-256 and renaming atomically.
    VerifyingSha256,
    /// SHA-256 verification succeeded.
    Verified,
    /// Download or verification failed.
    Failed,
}

/// Download progress event matching `DownloadProgressEvent` in `app/ui/src/types/kotoha.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadProgressEventDto {
    /// Catalog model ID.
    pub model_id: String,
    /// Bytes downloaded so far.
    pub bytes_downloaded: u64,
    /// Expected total bytes.
    pub total_bytes: u64,
    /// Current download status.
    pub status: ModelDownloadStatus,
    /// `true` once SHA-256 verification passes.
    pub sha256_verified: bool,
    /// Optional error message when `status == failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

impl DownloadProgressEventDto {
    /// Maps a [`DownloadProgress`] update emitted by `kotoha-setup` into the UI event schema.
    #[must_use]
    pub fn from_setup_progress(progress: &DownloadProgress, fallback_total_bytes: u64) -> Self {
        let total_bytes = progress.total_bytes.unwrap_or(fallback_total_bytes);
        let (status, sha256_verified) = match progress.phase {
            DownloadPhase::Connecting | DownloadPhase::Downloading => {
                (ModelDownloadStatus::Downloading, false)
            }
            DownloadPhase::Verifying => (ModelDownloadStatus::VerifyingSha256, false),
            DownloadPhase::Completed => (ModelDownloadStatus::Verified, true),
        };
        Self {
            model_id: progress.model_id.clone(),
            bytes_downloaded: progress.bytes_downloaded,
            total_bytes,
            status,
            sha256_verified,
            error_message: None,
        }
    }
}

/// "Is configured" flags for every provider secret. The UI never receives secret values; it can
/// only write them (`set_provider_secret`) or clear them (`clear_provider_secret`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct SecretStatusDto {
    /// OpenAI-compatible API key is stored.
    pub openai_api_key: bool,
    /// `DeepL` API key is stored.
    pub deepl_api_key: bool,
    /// Google Cloud API key is stored.
    pub google_cloud_api_key: bool,
    /// GAS / webhook endpoint URL is stored.
    pub gas_endpoint_url: bool,
    /// GAS / webhook bearer token is stored.
    pub gas_bearer_token: bool,
}

impl SecretStatusDto {
    fn from_store(store: &dyn SecretStore) -> Self {
        let flag = |kind: ProviderSecretKind| store.is_configured(&kind.secret_ref());
        Self {
            openai_api_key: flag(ProviderSecretKind::OpenAiApiKey),
            deepl_api_key: flag(ProviderSecretKind::DeepLApiKey),
            google_cloud_api_key: flag(ProviderSecretKind::GoogleCloudApiKey),
            gas_endpoint_url: flag(ProviderSecretKind::GasEndpointUrl),
            gas_bearer_token: flag(ProviderSecretKind::GasBearerToken),
        }
    }
}

/// Whether the configured translation provider could actually be built, matching
/// `TranslatorStatus` in `app/ui/src/types/kotoha.ts`. Carries user-safe text only (no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TranslatorStatusDto {
    /// The configured provider is active.
    Ok,
    /// The configured provider is not active; placeholder (mock) translations are produced.
    Error {
        /// User-safe explanation.
        reason: String,
    },
}

impl TranslatorStatusDto {
    fn error(reason: impl Into<String>) -> Self {
        Self::Error {
            reason: reason.into(),
        }
    }
}

/// Full runtime snapshot matching `AppRuntimeState` in `app/ui/src/types/kotoha.ts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppRuntimeStateDto {
    /// Active source language code.
    pub src_lang: String,
    /// Active target language code.
    pub tgt_lang: String,
    /// Active translation provider configuration (contains no secrets).
    pub provider: TranslationProviderConfig,
    /// Which provider secrets are stored (booleans only; values are write-only).
    pub secrets: SecretStatusDto,
    /// Whether the configured provider is actually in use or a placeholder translator is active.
    pub translator_status: TranslatorStatusDto,
    /// Notice shown when the settings file was unusable and defaults were loaded instead.
    pub settings_notice: Option<String>,
    /// Current overlay display configuration.
    pub overlay_options: OverlayDisplayOptionsDto,
    /// Whether the overlay window is currently visible.
    pub overlay_visible: bool,
    /// Current glossary profiles, selection, and STT hotwords boost score.
    pub glossary: GlossarySettingsStateDto,
    /// Current IPC connection status.
    pub connection: ConnectionStatusDto,
    /// Recent subtitle cards for initial window hydration.
    pub subtitles: Vec<SubtitleEntry>,
    /// Whether the first-run setup wizard has been completed.
    pub wizard_completed: bool,
    /// Current host OS platform.
    pub platform: HostOsPlatform,
    /// Auto-detected Mumble plugin directories.
    pub detected_mumble_paths: Vec<String>,
    /// Whether Flatpak Mumble was detected on Linux.
    pub is_flatpak_mumble: bool,
}

/// Discovered Mumble plugin environment paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedMumblePaths {
    /// Formatted/displayable candidate directory paths.
    pub paths: Vec<String>,
    /// Whether a Flatpak Mumble installation was detected.
    pub is_flatpak: bool,
}

/// Shared Tauri application state managing configuration, translation, glossary, and IPC state.
pub struct AppState {
    settings: RwLock<AppSettings>,
    settings_store: FileSettingsStore,
    secret_store: Arc<InMemorySecretStore>,
    translator_manager: Arc<TranslatorManager>,
    translator_status: RwLock<TranslatorStatusDto>,
    settings_notice: Option<String>,
    language_pair_tx: watch::Sender<(LanguageCode, LanguageCode)>,
    glossary_manager: Arc<RwLock<GlossaryManager>>,
    glossary_file_paths: RwLock<HashMap<String, String>>,
    connection_status: RwLock<ConnectionStatusDto>,
    overlay_visible: RwLock<bool>,
    subtitles: Mutex<VecDeque<SubtitleEntry>>,
    models_root_dir: PathBuf,
    default_glossary_dir: PathBuf,
    detected_mumble_paths: RwLock<Vec<String>>,
    is_flatpak_mumble: RwLock<bool>,
    settings_write_mutex: Mutex<()>,
}

impl AppState {
    /// Initializes [`AppState`] rooted under `data_dir` (loading persisted sanitized settings and
    /// TOML glossary profiles if present).
    ///
    /// A corrupt or out-of-range settings file never prevents startup: it is backed up, defaults
    /// are used and [`AppRuntimeStateDto::settings_notice`] tells the UI. A provider that cannot
    /// be built is reported through [`AppRuntimeStateDto::translator_status`]; a placeholder
    /// translator keeps the pipeline alive, but the UI is told it is not the configured one.
    pub fn new(data_dir: impl Into<PathBuf>) -> Result<Self, String> {
        let data_root = data_dir.into();
        let settings_store = FileSettingsStore::in_dir(&data_root);
        let secret_store = Arc::new(InMemorySecretStore::new());
        let loaded = settings_store.load_or_recover(secret_store.as_ref());
        let settings = loaded.settings;

        let models_root_dir = data_root.join("models");
        let default_glossary_dir = resolve_default_glossary_dir(&data_root);
        let removed = cleanup_stale_part_files(&models_root_dir);
        if removed > 0 {
            eprintln!("removed {removed} stale model download file(s)");
        }

        let factory = build_composite_factory(&models_root_dir, secret_store.clone());
        let (translator, status): (Arc<dyn Translator>, TranslatorStatusDto) =
            match factory.create(&settings.translation_provider) {
                Ok(translator) => (translator, TranslatorStatusDto::Ok),
                Err(_) => (
                    Arc::new(MockTranslator::new(
                        "mock",
                        format!("[{}] ", settings.tgt_lang.as_str()),
                    )),
                    TranslatorStatusDto::error(describe_provider_unavailable(
                        &settings.translation_provider,
                        secret_store.as_ref(),
                    )),
                ),
            };
        let translator_manager = Arc::new(TranslatorManager::new(
            translator,
            settings.translation_provider.clone(),
        ));

        Self::finish_init(
            settings,
            loaded.notice,
            status,
            settings_store,
            secret_store,
            translator_manager,
            models_root_dir,
            default_glossary_dir,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_init(
        settings: AppSettings,
        settings_notice: Option<String>,
        translator_status: TranslatorStatusDto,
        settings_store: FileSettingsStore,
        secret_store: Arc<InMemorySecretStore>,
        translator_manager: Arc<TranslatorManager>,
        models_root_dir: PathBuf,
        default_glossary_dir: PathBuf,
    ) -> Result<Self, String> {
        let (glossary_manager, file_paths) =
            load_glossary_manager(&settings, &default_glossary_dir)?;

        let detected = detect_mumble_environment(settings.custom_mumble_plugin_dir.as_deref());
        let endpoint = resolve_endpoint();
        let (language_pair_tx, _) =
            watch::channel((settings.src_lang.clone(), settings.tgt_lang.clone()));

        Ok(Self {
            settings: RwLock::new(settings),
            settings_store,
            secret_store,
            translator_manager,
            translator_status: RwLock::new(translator_status),
            settings_notice,
            language_pair_tx,
            glossary_manager: Arc::new(RwLock::new(glossary_manager)),
            glossary_file_paths: RwLock::new(file_paths),
            connection_status: RwLock::new(ConnectionStatusDto::disconnected(endpoint)),
            overlay_visible: RwLock::new(true),
            subtitles: Mutex::new(VecDeque::with_capacity(MAX_RECENT_SUBTITLES)),
            models_root_dir,
            default_glossary_dir,
            detected_mumble_paths: RwLock::new(detected.paths),
            is_flatpak_mumble: RwLock::new(detected.is_flatpak),
            settings_write_mutex: Mutex::new(()),
        })
    }

    /// Subscribes to `(src_lang, tgt_lang)` changes so the running pipeline can be reconfigured.
    #[must_use]
    pub fn subscribe_language_pair(&self) -> watch::Receiver<(LanguageCode, LanguageCode)> {
        self.language_pair_tx.subscribe()
    }

    /// Returns the shared [`TranslatorManager`].
    #[must_use]
    pub const fn translator_manager(&self) -> &Arc<TranslatorManager> {
        &self.translator_manager
    }

    /// Returns the shared [`GlossaryManager`] handle.
    #[must_use]
    pub const fn glossary_manager(&self) -> &Arc<RwLock<GlossaryManager>> {
        &self.glossary_manager
    }

    /// Returns the directory storing downloaded local translation models.
    #[must_use]
    pub fn models_root_dir(&self) -> &Path {
        &self.models_root_dir
    }

    /// Returns the default TOML glossary directory.
    #[must_use]
    pub fn default_glossary_dir(&self) -> &Path {
        &self.default_glossary_dir
    }

    /// Builds a [`CompositeTranslatorFactory`] targeting `self.models_root_dir` that resolves
    /// provider secrets from this state's secret store.
    #[must_use]
    pub fn translator_factory(&self) -> Arc<CompositeTranslatorFactory> {
        Arc::new(build_composite_factory(
            &self.models_root_dir,
            self.secret_store.clone(),
        ))
    }

    /// Returns the full runtime snapshot serialized for the React UI (`get_app_state`).
    pub async fn snapshot(&self) -> AppRuntimeStateDto {
        let settings = self.settings.read().await.clone();
        let overlay_options = OverlayDisplayOptionsDto::from(&settings.overlay);
        let overlay_visible = *self.overlay_visible.read().await;
        let glossary = self.glossary_snapshot(&settings).await;
        let connection = self.connection_status.read().await.clone();
        let subtitles = self.subtitles.lock().await.iter().cloned().collect();
        let detected_mumble_paths = self.detected_mumble_paths.read().await.clone();
        let is_flatpak_mumble = *self.is_flatpak_mumble.read().await;

        AppRuntimeStateDto {
            src_lang: settings.src_lang.as_str().to_owned(),
            tgt_lang: settings.tgt_lang.as_str().to_owned(),
            provider: settings.translation_provider.clone(),
            secrets: SecretStatusDto::from_store(self.secret_store.as_ref()),
            translator_status: self.translator_status.read().await.clone(),
            settings_notice: self.settings_notice.clone(),
            overlay_options,
            overlay_visible,
            glossary,
            connection,
            subtitles,
            wizard_completed: settings.first_run_completed,
            platform: HostOsPlatform::current(),
            detected_mumble_paths,
            is_flatpak_mumble,
        }
    }

    /// Updates and persists the overlay click-through lock flag.
    pub async fn set_overlay_locked(
        &self,
        locked: bool,
    ) -> Result<OverlayDisplayOptionsDto, String> {
        self.update_overlay_style(OverlayStyleUpdateInput {
            locked: Some(locked),
            ..Default::default()
        })
        .await
    }

    /// Updates and returns overlay visibility state.
    pub async fn set_overlay_visible(&self, visible: Option<bool>) -> bool {
        let mut guard = self.overlay_visible.write().await;
        let next = visible.unwrap_or(!*guard);
        *guard = next;
        next
    }

    /// Updates overlay styling preferences, validates bounds, and persists sanitized settings.
    pub async fn update_overlay_style(
        &self,
        style: OverlayStyleUpdateInput,
    ) -> Result<OverlayDisplayOptionsDto, String> {
        let _write_tx = self.settings_write_mutex.lock().await;
        let mut next = {
            let guard = self.settings.read().await;
            guard.clone()
        };

        if let Some(max_lines) = style.max_lines {
            next.overlay.max_lines = max_lines;
        }
        if let Some(show_source_text) = style.show_source_text {
            next.overlay.show_source_text = show_source_text;
        }
        if let Some(locked) = style.locked {
            next.overlay.locked = locked;
        }
        if let Some(font_size_px) = style.font_size_px {
            next.overlay.font_size_px = font_size_px;
        }
        if let Some(background_opacity) = style.background_opacity {
            next.overlay.opacity = background_opacity;
        }

        self.persist_settings_async(next.clone()).await?;
        let dto = OverlayDisplayOptionsDto::from(&next.overlay);
        {
            let mut guard = self.settings.write().await;
            *guard = next;
        }
        Ok(dto)
    }

    /// Switches the active translation provider and/or language pair and persists sanitized settings.
    pub async fn switch_translation_provider(
        &self,
        config: TranslationProviderConfig,
        src_lang: Option<String>,
        tgt_lang: Option<String>,
    ) -> Result<AppRuntimeStateDto, String> {
        let _write_tx = self.settings_write_mutex.lock().await;
        let current = {
            let guard = self.settings.read().await;
            guard.clone()
        };
        let mut next = current.clone();

        if let Some(raw_src) = src_lang {
            let trimmed = raw_src.trim();
            if !trimmed.is_empty() {
                next.src_lang = LanguageCode::new(trimmed.to_owned())
                    .map_err(|err| format!("invalid source language '{trimmed}': {err}"))?;
            }
        }
        if let Some(raw_tgt) = tgt_lang {
            let trimmed = raw_tgt.trim();
            if !trimmed.is_empty() {
                next.tgt_lang = LanguageCode::new(trimmed.to_owned())
                    .map_err(|err| format!("invalid target language '{trimmed}': {err}"))?;
            }
        }

        // A credential is bound to the host it was entered for: switching providers or pointing
        // the OpenAI-compatible provider at another origin drops the stored key so it can never
        // be sent to the new host.
        let effective_config = config;
        for secret_ref in
            effective_config.secrets_invalidated_by_change_from(&current.translation_provider)
        {
            self.secret_store
                .delete(&secret_ref)
                .map_err(|err| format!("failed to clear provider secret: {err}"))?;
        }
        next.translation_provider = effective_config.clone();

        let factory = self.translator_factory();
        let model_bin_path = match &effective_config {
            TranslationProviderConfig::LocalModel { model_id, .. } => {
                Some(self.models_root_dir.join(model_id).join("model.bin"))
            }
            _ => None,
        };
        let should_hot_swap = match &effective_config {
            TranslationProviderConfig::LocalModel { .. } => {
                if let Some(path) = model_bin_path {
                    spawn_blocking(move || path.is_file())
                        .await
                        .map_err(|err| format!("failed to check local model artifact: {err}"))?
                } else {
                    false
                }
            }
            TranslationProviderConfig::OpenAiCompatible {
                base_url, model, ..
            } => !base_url.trim().is_empty() && !model.trim().is_empty(),
            TranslationProviderConfig::DeepL { .. }
            | TranslationProviderConfig::GoogleCloud { .. }
            | TranslationProviderConfig::GasWebhook { .. } => effective_config
                .required_secret_refs()
                .iter()
                .all(|r| self.secret_store.is_configured(r)),
            TranslationProviderConfig::Mock { .. } => true,
        };

        let new_status = if should_hot_swap {
            self.translator_manager
                .switch_provider(effective_config.clone(), factory)
                .await
                .map_err(|err| format!("failed to switch translation provider: {err}"))?;
            TranslatorStatusDto::Ok
        } else {
            TranslatorStatusDto::error(describe_provider_unavailable(
                &effective_config,
                self.secret_store.as_ref(),
            ))
        };

        self.persist_settings_async(next.clone()).await?;

        {
            let mut glossary_mgr = self.glossary_manager.write().await;
            glossary_mgr.set_language_pair(next.src_lang.clone(), next.tgt_lang.clone());
        }
        *self.translator_status.write().await = new_status;
        {
            let mut guard = self.settings.write().await;
            *guard = next.clone();
        }
        // Wake the pipeline bridge so a changed language pair applies without a restart.
        self.language_pair_tx.send_if_modified(|pair| {
            let next_pair = (next.src_lang.clone(), next.tgt_lang.clone());
            if *pair == next_pair {
                false
            } else {
                *pair = next_pair;
                true
            }
        });

        Ok(self.snapshot().await)
    }

    /// Returns which provider secrets are stored (booleans only).
    #[must_use]
    pub fn secret_status(&self) -> SecretStatusDto {
        SecretStatusDto::from_store(self.secret_store.as_ref())
    }

    /// Stores a provider secret (write-only). `kind` must belong to the active provider; the
    /// value is never echoed back. Replacing the GAS endpoint also clears the GAS bearer token,
    /// and the active translator is rebuilt when the provider becomes usable.
    pub async fn set_provider_secret(
        &self,
        kind: ProviderSecretKind,
        value: String,
    ) -> Result<SecretStatusDto, String> {
        let secret = Secret::new(value.trim());
        if secret.is_blank() {
            return Err("secret value must not be empty (use clear to remove it)".to_owned());
        }
        let _write_tx = self.settings_write_mutex.lock().await;
        let config = self.settings.read().await.translation_provider.clone();
        if !kind.applies_to(&config) {
            return Err(
                "this secret does not belong to the active translation provider".to_owned(),
            );
        }

        let secret_ref = kind.secret_ref();
        let dependents = kind.dependent_refs_to_clear();
        let mut backup: Vec<(kotoha_core::domain::SecretRef, Option<Secret>)> = Vec::new();
        for r in std::iter::once(&secret_ref).chain(dependents.iter()) {
            backup.push((
                r.clone(),
                self.secret_store
                    .resolve_if_set(r)
                    .map_err(|err| format!("failed to read secret store: {err}"))?,
            ));
        }
        for dep in &dependents {
            self.secret_store
                .delete(dep)
                .map_err(|err| format!("failed to clear dependent secret: {err}"))?;
        }
        self.secret_store
            .set(&secret_ref, secret)
            .map_err(|err| format!("failed to store secret: {err}"))?;

        if let Err(err) = self.rebuild_active_provider(&config).await {
            // Roll back so an invalid value never replaces a working one.
            for (r, old) in backup {
                let _ = old.map_or_else(
                    || self.secret_store.delete(&r),
                    |v| self.secret_store.set(&r, v),
                );
            }
            return Err(err);
        }
        Ok(self.secret_status())
    }

    /// Deletes a provider secret (idempotent).
    pub async fn clear_provider_secret(
        &self,
        kind: ProviderSecretKind,
    ) -> Result<SecretStatusDto, String> {
        let _write_tx = self.settings_write_mutex.lock().await;
        self.secret_store
            .delete(&kind.secret_ref())
            .map_err(|err| format!("failed to clear secret: {err}"))?;
        for dep in kind.dependent_refs_to_clear() {
            self.secret_store
                .delete(&dep)
                .map_err(|err| format!("failed to clear dependent secret: {err}"))?;
        }
        Ok(self.secret_status())
    }

    async fn rebuild_active_provider(
        &self,
        config: &TranslationProviderConfig,
    ) -> Result<(), String> {
        let ready = config
            .required_secret_refs()
            .iter()
            .all(|r| self.secret_store.is_configured(r));
        if !ready || matches!(config, TranslationProviderConfig::LocalModel { .. }) {
            return Ok(());
        }
        self.translator_manager
            .switch_provider(config.clone(), self.translator_factory())
            .await
            .map_err(|err| format!("failed to switch translation provider: {err}"))?;
        *self.translator_status.write().await = TranslatorStatusDto::Ok;
        Ok(())
    }

    /// Updates glossary selection, `hotwords_score`, and optional custom glossary directory.
    pub async fn update_glossary_settings(
        &self,
        enabled_profiles: Vec<String>,
        hotwords_score: f32,
        glossary_dir: Option<String>,
    ) -> Result<GlossarySettingsStateDto, String> {
        let clamped_score = if hotwords_score.is_finite() {
            (hotwords_score.clamp(0.5, 3.0) * 10.0).round() / 10.0
        } else {
            1.5
        };

        let _write_tx = self.settings_write_mutex.lock().await;
        let mut next = {
            let guard = self.settings.read().await;
            guard.clone()
        };

        next.glossary.enabled_profiles.clone_from(&enabled_profiles);
        next.hotwords_score = clamped_score;
        if let Some(dir) = glossary_dir {
            next.glossary.glossary_dir = normalize_optional_dir(Some(dir.as_str()));
        } else {
            next.glossary.glossary_dir =
                normalize_optional_dir(next.glossary.glossary_dir.as_deref());
        }

        let target_dir = next
            .glossary
            .glossary_dir
            .as_deref()
            .map_or_else(|| self.default_glossary_dir.clone(), PathBuf::from);
        let default_dir_for_blocking = self.default_glossary_dir.clone();
        let next_glossary_for_blocking = next.glossary.clone();
        let src_lang = next.src_lang.clone();
        let tgt_lang = next.tgt_lang.clone();

        let disk_load = spawn_blocking(move || {
            reload_glossary_on_blocking(
                &target_dir,
                &default_dir_for_blocking,
                &next_glossary_for_blocking,
                src_lang,
                tgt_lang,
                clamped_score,
            )
        })
        .await
        .map_err(|err| format!("glossary reload task failed: {err}"))??;

        if disk_load.is_none() {
            let mut glossary_mgr = self.glossary_manager.write().await;
            glossary_mgr
                .set_hotwords_score(clamped_score)
                .map_err(|err| format!("invalid hotwords_score: {err}"))?;
        }

        self.persist_settings_async(next.clone()).await?;

        if let Some(ReloadedGlossary {
            manager: reloaded_mgr,
            file_paths: updated_paths,
        }) = disk_load
        {
            {
                let mut glossary_mgr = self.glossary_manager.write().await;
                *glossary_mgr = reloaded_mgr;
            }
            {
                let mut file_paths = self.glossary_file_paths.write().await;
                *file_paths = updated_paths;
            }
        } else {
            let mut glossary_mgr = self.glossary_manager.write().await;
            glossary_mgr
                .set_hotwords_score(clamped_score)
                .map_err(|err| format!("invalid hotwords_score: {err}"))?;
            glossary_mgr.set_enabled_profiles(enabled_profiles);
        }

        {
            let mut guard = self.settings.write().await;
            *guard = next.clone();
        }

        Ok(self.glossary_snapshot(&next).await)
    }

    /// Marks the first-run setup wizard as completed and persists sanitized settings.
    pub async fn complete_first_run_wizard(&self) -> Result<AppRuntimeStateDto, String> {
        let _write_tx = self.settings_write_mutex.lock().await;
        let mut next = {
            let guard = self.settings.read().await;
            guard.clone()
        };
        next.first_run_completed = true;
        self.persist_settings_async(next.clone()).await?;
        {
            let mut guard = self.settings.write().await;
            *guard = next;
        }

        Ok(self.snapshot().await)
    }

    /// Saves a custom Mumble plugin directory override when provided and updates detected paths.
    pub async fn record_custom_mumble_dir(&self, custom_dir: Option<&str>) -> Result<(), String> {
        let trimmed = custom_dir
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let dir_for_detect = trimmed.clone();
        let detected = spawn_blocking(move || detect_mumble_environment(dir_for_detect.as_deref()))
            .await
            .map_err(|err| format!("failed to detect Mumble plugin directories: {err}"))?;

        {
            let mut paths = self.detected_mumble_paths.write().await;
            *paths = detected.paths;
        }
        {
            let mut flatpak = self.is_flatpak_mumble.write().await;
            *flatpak = detected.is_flatpak;
        }

        if let Some(dir) = trimmed {
            let _write_tx = self.settings_write_mutex.lock().await;
            let mut next = {
                let guard = self.settings.read().await;
                guard.clone()
            };
            next.custom_mumble_plugin_dir = Some(dir);
            self.persist_settings_async(next.clone()).await?;
            let mut guard = self.settings.write().await;
            *guard = next;
        }
        Ok(())
    }

    /// Returns the detected Mumble paths and Flatpak detection state.
    pub async fn detected_mumble_info(&self) -> (Vec<String>, bool) {
        let paths = self.detected_mumble_paths.read().await.clone();
        let is_flatpak = *self.is_flatpak_mumble.read().await;
        (paths, is_flatpak)
    }

    /// Inserts or updates a subtitle entry in the bounded in-memory ring buffer.
    pub async fn record_subtitle(&self, entry: SubtitleEntry) {
        let mut guard = self.subtitles.lock().await;
        if let Some(existing) = guard
            .iter_mut()
            .find(|item| item.utterance_id == entry.utterance_id)
        {
            *existing = entry;
            return;
        }
        if guard.len() >= MAX_RECENT_SUBTITLES {
            guard.pop_front();
        }
        guard.push_back(entry);
    }

    /// Clears stored subtitles matching `source`.
    pub async fn clear_subtitles_for_source(&self, source: &str) {
        let mut guard = self.subtitles.lock().await;
        guard.retain(|item| item.speaker_key.source != source);
    }

    /// Replaces the current [`ConnectionStatusDto`] snapshot.
    pub async fn set_connection_status(&self, status: ConnectionStatusDto) {
        *self.connection_status.write().await = status;
    }

    /// Returns a clone of the current [`ConnectionStatusDto`] snapshot.
    pub async fn connection_status(&self) -> ConnectionStatusDto {
        self.connection_status.read().await.clone()
    }

    /// Looks up a [`ModelProfile`] by ID from the built-in catalog.
    #[must_use]
    pub fn find_model_profile(&self, model_id: &str) -> Option<ModelProfile> {
        default_model_catalog()
            .into_iter()
            .find(|profile| profile.model_id == model_id)
    }

    /// Returns a clone of the current persisted settings (they contain no secrets).
    pub async fn sanitized_settings(&self) -> AppSettings {
        self.settings.read().await.clone()
    }

    /// Returns the path where `kotoha-settings.json` is stored.
    #[must_use]
    pub fn settings_path(&self) -> &Path {
        self.settings_store.path()
    }

    /// Returns the source language currently configured in settings.
    pub async fn current_language_pair(&self) -> (LanguageCode, LanguageCode) {
        let guard = self.settings.read().await;
        (guard.src_lang.clone(), guard.tgt_lang.clone())
    }

    async fn glossary_snapshot(&self, settings: &AppSettings) -> GlossarySettingsStateDto {
        let mgr = self.glossary_manager.read().await;
        let file_paths = self.glossary_file_paths.read().await;
        let enabled_vec = mgr.enabled_profiles().to_vec();
        let enabled_set: HashSet<&str> = enabled_vec.iter().map(String::as_str).collect();

        let profiles = mgr
            .profiles()
            .iter()
            .map(|profile| {
                let id = slugify_profile_id(&profile.name);
                let enabled = enabled_set.contains(profile.name.as_str())
                    || enabled_set.contains(id.as_str());
                let file_path = file_paths.get(&profile.name).cloned();
                GlossaryProfileSummaryDto {
                    id,
                    name: profile.name.clone(),
                    src_lang: profile.src_lang.as_str().to_owned(),
                    tgt_lang: profile.tgt_lang.as_str().to_owned(),
                    term_count: profile.terms.len(),
                    enabled,
                    file_path,
                }
            })
            .collect();
        drop(file_paths);
        drop(mgr);

        let glossary_dir = normalize_optional_dir(settings.glossary.glossary_dir.as_deref())
            .or_else(|| Some(self.default_glossary_dir.display().to_string()));

        GlossarySettingsStateDto {
            profiles,
            enabled_profiles: enabled_vec,
            hotwords_score: settings.hotwords_score,
            glossary_dir,
        }
    }

    async fn persist_settings_async(&self, settings: AppSettings) -> Result<(), String> {
        let store = self.settings_store.clone();
        spawn_blocking(move || {
            store
                .save(&settings)
                .map_err(|err| format!("failed to persist settings: {err}"))
        })
        .await
        .map_err(|err| format!("settings persistence task failed: {err}"))?
    }
}

/// Returns a user-safe reason (never secret values) why `config` could not be activated.
fn describe_provider_unavailable(
    config: &TranslationProviderConfig,
    secrets: &dyn SecretStore,
) -> String {
    let missing_secret = config
        .required_secret_refs()
        .iter()
        .any(|r| !secrets.is_configured(r));
    let detail = if missing_secret {
        "the required API key / endpoint is not set (saved keys are lost when the app restarts)"
    } else if matches!(config, TranslationProviderConfig::LocalModel { .. }) {
        "the local translation model is not downloaded or could not be loaded"
    } else {
        "the provider could not be initialized; check its settings"
    };
    format!("The configured translation provider is not active because {detail}. Placeholder translations are shown instead.")
}

/// Converts [`PluginInstallOutcome`] + detected environment into [`PluginInstallResultDto`].
#[must_use]
pub fn map_plugin_install_outcome(
    outcome: &PluginInstallOutcome,
    platform: HostOsPlatform,
    detected_paths: Vec<String>,
    is_flatpak_hint: bool,
) -> PluginInstallResultDto {
    let target_dir = outcome
        .path()
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(
            || {
                let raw = outcome.path().to_string_lossy();
                match raw.rsplit_once(['/', '\\']) {
                    Some((dir, _)) => dir.to_owned(),
                    None => raw.into_owned(),
                }
            },
            |p| p.display().to_string(),
        );
    let is_flatpak = is_flatpak_hint || target_dir.contains("info.mumble.Mumble");
    let flatpak_hint = is_flatpak.then(|| {
        "Flatpak Mumble detected: grant runtime IPC socket access via \
         flatpak override --user --filesystem=xdg-run/kotoha_v1.sock info.mumble.Mumble"
            .to_owned()
    });

    match outcome {
        PluginInstallOutcome::Installed {
            path,
            replaced_existing,
            ..
        } => {
            let verb = if *replaced_existing {
                "Updated"
            } else {
                "Installed"
            };
            PluginInstallResultDto {
                status: PluginInstallStatus::Installed,
                platform,
                target_path: target_dir,
                detected_paths,
                is_flatpak,
                flatpak_hint,
                message: format!("{verb} Kotoha Mumble plugin at {}.", path.display()),
            }
        }
        PluginInstallOutcome::AlreadyUpToDate { path, .. } => PluginInstallResultDto {
            status: PluginInstallStatus::AlreadyUpToDate,
            platform,
            target_path: target_dir,
            detected_paths,
            is_flatpak,
            flatpak_hint,
            message: format!("Mumble plugin is already up to date at {}.", path.display()),
        },
        PluginInstallOutcome::LockedByRunningApp { guidance, .. } => PluginInstallResultDto {
            status: PluginInstallStatus::LockedByRunningApp,
            platform,
            target_path: target_dir,
            detected_paths,
            is_flatpak,
            flatpak_hint,
            message: guidance.clone(),
        },
    }
}

/// Resolves Mumble plugin bytes from embedded compile-time payload or local build artifact fallback.
pub fn resolve_mumble_plugin_bytes() -> Result<Vec<u8>, String> {
    if let Some(embedded) = embedded_mumble_plugin_bytes() {
        return Ok(embedded.to_vec());
    }

    let filename = plugin_filename().map_err(|err| err.to_string())?;
    let mut candidates = Vec::new();
    if let Ok(explicit_path) = std::env::var("KOTOHA_MUMBLE_PLUGIN_PATH") {
        let trimmed = explicit_path.trim();
        if !trimmed.is_empty() {
            candidates.push(PathBuf::from(trimmed));
        }
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(repo_root) = manifest_dir.parent().and_then(Path::parent) {
        candidates.push(repo_root.join("plugins/build/mumble").join(filename));
        candidates.push(
            repo_root
                .join("plugins/build/mumble/Release")
                .join(filename),
        );
        candidates.push(repo_root.join("plugins/build/mumble/Debug").join(filename));
    }

    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("plugins/build/mumble").join(filename));
        candidates.push(cwd.join("plugins/build/mumble/Release").join(filename));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            candidates.push(exe_dir.join(filename));
            candidates.push(exe_dir.join("resources").join(filename));
        }
    }

    for candidate in &candidates {
        if candidate.is_file() {
            let bytes = fs::read(candidate).map_err(|err| {
                format!(
                    "failed to read Mumble plugin binary at '{}': {err}",
                    candidate.display()
                )
            })?;
            if !bytes.is_empty() {
                return Ok(bytes);
            }
        }
    }

    Err(format!(
        "Mumble plugin binary ({filename}) is not embedded and was not found in local build paths"
    ))
}

/// Detects Mumble plugin directories across Windows, macOS, and Linux (including Flatpak).
#[must_use]
pub fn detect_mumble_environment(custom_dir: Option<&str>) -> DetectedMumblePaths {
    let custom_path = custom_dir
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let candidates = discover_mumble_plugin_dirs(custom_path.as_deref()).unwrap_or_default();
    let is_flatpak = candidates
        .iter()
        .any(|c| c.kind == MumblePluginDirKind::LinuxFlatpak && c.detected_on_disk);
    let paths = candidates
        .into_iter()
        .map(|c| c.path.display().to_string())
        .collect();
    DetectedMumblePaths { paths, is_flatpak }
}

/// Resolves the default application data directory (`kotoha`) for persistent settings, models, and
/// glossaries.
#[must_use]
pub fn resolve_app_data_dir() -> PathBuf {
    if let Some(override_dir) = std::env::var_os("KOTOHA_DATA_DIR") {
        let p = PathBuf::from(override_dir);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }

    #[cfg(target_os = "windows")]
    if let Some(app_data) = std::env::var_os("APPDATA") {
        if !app_data.is_empty() {
            return PathBuf::from(app_data).join("Kotoha");
        }
    }

    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join("Library/Application Support/Kotoha");
        }
    }

    if let Some(xdg_config) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg_config.is_empty() {
            return PathBuf::from(xdg_config).join("kotoha");
        }
    }

    if let Some(home) = std::env::var_os("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".config/kotoha");
        }
    }

    PathBuf::from(".kotoha")
}

fn build_composite_factory(
    models_root_dir: &Path,
    secrets: Arc<dyn SecretStore>,
) -> CompositeTranslatorFactory {
    let local_factory = LocalNmtTranslatorFactory::new(models_root_dir);
    let http_factory = HttpTranslatorFactory::with_secret_store(secrets);
    CompositeTranslatorFactory::new(local_factory, http_factory)
}

fn resolve_default_glossary_dir(data_root: &Path) -> PathBuf {
    let data_glossaries = data_root.join("glossaries");
    if data_glossaries.is_dir() {
        return data_glossaries;
    }

    if let Ok(cwd) = std::env::current_dir() {
        let cwd_glossaries = cwd.join("glossaries");
        if cwd_glossaries.is_dir() {
            return cwd_glossaries;
        }
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(repo_root) = manifest_dir.parent().and_then(Path::parent) {
        let repo_glossaries = repo_root.join("glossaries");
        if repo_glossaries.is_dir() {
            return repo_glossaries;
        }
    }

    data_glossaries
}

struct ReloadedGlossary {
    manager: GlossaryManager,
    file_paths: HashMap<String, String>,
}

fn reload_glossary_on_blocking(
    target_dir: &Path,
    default_dir: &Path,
    glossary_settings: &GlossarySettings,
    src_lang: LanguageCode,
    tgt_lang: LanguageCode,
    hotwords_score: f32,
) -> Result<Option<ReloadedGlossary>, String> {
    if !target_dir.is_dir() {
        return Ok(None);
    }
    let repo = TomlGlossaryRepository::new();
    let mut manager =
        GlossaryManager::from_settings(&repo, default_dir, glossary_settings, src_lang, tgt_lang)
            .map_err(|err| format!("failed to apply glossary settings: {err}"))?;
    manager
        .set_hotwords_score(hotwords_score)
        .map_err(|err| format!("invalid hotwords_score: {err}"))?;
    let file_paths = scan_glossary_file_paths(target_dir);
    Ok(Some(ReloadedGlossary {
        manager,
        file_paths,
    }))
}

fn normalize_optional_dir(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn load_glossary_manager(
    settings: &AppSettings,
    default_glossary_dir: &Path,
) -> Result<(GlossaryManager, HashMap<String, String>), String> {
    let repo = TomlGlossaryRepository::new();
    let mut normalized_glossary_settings = settings.glossary.clone();
    normalized_glossary_settings.glossary_dir =
        normalize_optional_dir(settings.glossary.glossary_dir.as_deref());

    let target_dir = normalized_glossary_settings
        .glossary_dir
        .as_deref()
        .map_or(default_glossary_dir, Path::new);

    let mut mgr = if target_dir.is_dir() {
        let has_explicit_selection = !normalized_glossary_settings.enabled_profiles.is_empty()
            || settings.first_run_completed
            || normalized_glossary_settings.glossary_dir.is_some();

        if has_explicit_selection {
            GlossaryManager::from_settings(
                &repo,
                default_glossary_dir,
                &normalized_glossary_settings,
                settings.src_lang.clone(),
                settings.tgt_lang.clone(),
            )
            .map_err(|err| {
                format!(
                    "failed to load glossaries from '{}': {err}",
                    target_dir.display()
                )
            })?
        } else {
            GlossaryManager::from_dir(
                &repo,
                target_dir,
                settings.src_lang.clone(),
                settings.tgt_lang.clone(),
            )
            .map_err(|err| {
                format!(
                    "failed to load glossaries from '{}': {err}",
                    target_dir.display()
                )
            })?
        }
    } else {
        GlossaryManager::from_profiles(
            settings.src_lang.clone(),
            settings.tgt_lang.clone(),
            Vec::<GlossaryProfile>::new(),
        )
    };

    mgr.set_hotwords_score(settings.hotwords_score)
        .map_err(|err| format!("invalid hotwords_score in settings: {err}"))?;

    let file_paths = if target_dir.is_dir() {
        scan_glossary_file_paths(target_dir)
    } else {
        HashMap::new()
    };

    Ok((mgr, file_paths))
}

fn scan_glossary_file_paths(dir: &Path) -> HashMap<String, String> {
    let repo = TomlGlossaryRepository::new();
    let mut map = HashMap::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return map;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let is_toml = path.is_file()
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
        if !is_toml {
            continue;
        }
        if let Ok(profile) = repo.load_file(&path) {
            map.insert(profile.name, path.display().to_string());
        }
    }
    map
}

fn fnv1a_64_hex(input: &str) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0100_0000_01b3;

    let mut hash = FNV_OFFSET_BASIS;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

fn slugify_profile_id(name: &str) -> String {
    let trimmed_input = name.trim();
    let mut out = String::with_capacity(trimmed_input.len());
    let mut last_dash = false;
    let mut has_non_ascii_alnum = false;

    for ch in trimmed_input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else {
            if !ch.is_ascii_whitespace() && ch != '-' && ch != '_' {
                has_non_ascii_alnum = true;
            }
            if !last_dash && !out.is_empty() {
                out.push('-');
                last_dash = true;
            }
        }
    }

    let trimmed = out.trim_end_matches('-');
    if trimmed.is_empty() {
        format!("profile-{}", fnv1a_64_hex(trimmed_input))
    } else if has_non_ascii_alnum {
        format!("{trimmed}-{}", fnv1a_64_hex(trimmed_input))
    } else {
        trimmed.to_owned()
    }
}
