//! Auto-setup utilities for Kotoha: Mumble plugin installer, HTTPS/SHA-256 model downloader,
//! and persistent settings store.

pub mod error;
pub mod model_downloader;
pub mod plugin_installer;
pub mod settings_store;

pub use error::SetupError;
pub use model_downloader::{
    part_path_for, DownloadPhase, DownloadProgress, DownloadedArtifact, ModelDownloader,
    ModelDownloaderConfig, DEFAULT_DOWNLOAD_TIMEOUT_SECS, DEFAULT_MAX_DOWNLOAD_REDIRECTS,
    PENDING_RELEASE_MANIFEST_SHA256,
};
pub use plugin_installer::{
    discover_mumble_plugin_dirs, discover_mumble_plugin_dirs_for,
    discover_mumble_plugin_dirs_with_custom, install_plugin, install_plugin_for_os,
    is_locked_file_io_error, locked_plugin_guidance, plugin_filename, plugin_filename_for,
    MumblePluginDirCandidate, MumblePluginDirKind, PluginInstallOutcome, TargetPlatform,
};
pub use settings_store::{
    AppSettings, FileSettingsStore, OverlayStyleSettings, DEFAULT_OVERLAY_FADE_OUT_SECONDS,
    DEFAULT_OVERLAY_FONT_SIZE_PX, DEFAULT_OVERLAY_HEIGHT, DEFAULT_OVERLAY_MAX_LINES,
    DEFAULT_OVERLAY_OPACITY, DEFAULT_OVERLAY_POSITION_X, DEFAULT_OVERLAY_POSITION_Y,
    DEFAULT_OVERLAY_WIDTH, SETTINGS_FILENAME,
};
