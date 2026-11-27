//! Tauri v2 IPC command handlers matching `app/ui/src/services/tauriBridge.ts` 1:1.

use std::path::PathBuf;
use std::sync::Arc;

use kotoha_core::domain::TranslationProviderConfig;
use kotoha_setup::{install_plugin, ModelDownloader, SetupError};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::bridge::emit_overlay_style_changed;
use crate::state::{
    map_plugin_install_outcome, resolve_mumble_plugin_bytes, AppRuntimeStateDto, AppState,
    DownloadProgressEventDto, GlossarySettingsStateDto, HostOsPlatform, ModelDownloadStatus,
    OverlayDisplayOptionsDto, OverlayStyleUpdateInput, PluginInstallResultDto, PluginInstallStatus,
    EVENT_DOWNLOAD_PROGRESS, EVENT_OVERLAY_LOCK_CHANGED,
};

/// Returns the complete runtime snapshot for the React UI.
#[tauri::command]
pub async fn get_app_state(state: State<'_, Arc<AppState>>) -> Result<AppRuntimeStateDto, String> {
    Ok(state.snapshot().await)
}

/// Sets click-through lock state on the `overlay` window, persists it, and emits
/// `kotoha://overlay-lock-changed`.
#[tauri::command]
pub async fn set_overlay_locked(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    locked: bool,
) -> Result<OverlayDisplayOptionsDto, String> {
    apply_overlay_lock(&app, state.inner(), locked).await
}

/// Shows or hides the `overlay` window and returns the new visibility boolean.
#[tauri::command]
pub async fn toggle_overlay_visible(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    visible: Option<bool>,
) -> Result<bool, String> {
    apply_overlay_visibility(&app, state.inner(), visible).await
}

/// Installs the Mumble plugin binary into the auto-detected or `customDir` directory.
#[tauri::command]
pub async fn install_mumble_plugin(
    state: State<'_, Arc<AppState>>,
    custom_dir: Option<String>,
) -> Result<PluginInstallResultDto, String> {
    state
        .record_custom_mumble_dir(custom_dir.as_deref())
        .await?;
    let (detected_paths, is_flatpak) = state.detected_mumble_info().await;
    let platform = HostOsPlatform::current();

    let trimmed_custom = custom_dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let Some(target_dir_str) = trimmed_custom
        .map(str::to_owned)
        .or_else(|| detected_paths.first().cloned())
    else {
        return Ok(PluginInstallResultDto {
            status: PluginInstallStatus::NotFound,
            platform,
            target_path: String::new(),
            detected_paths,
            is_flatpak,
            flatpak_hint: None,
            message: "No Mumble Plugins directory could be detected on this system.".to_owned(),
        });
    };

    let plugin_bytes = match resolve_mumble_plugin_bytes() {
        Ok(bytes) => bytes,
        Err(err) => {
            return Ok(PluginInstallResultDto {
                status: PluginInstallStatus::NotFound,
                platform,
                target_path: target_dir_str,
                detected_paths,
                is_flatpak,
                flatpak_hint: None,
                message: err,
            });
        }
    };

    let target_dir = PathBuf::from(&target_dir_str);
    let outcome = tokio::task::spawn_blocking(move || install_plugin(&target_dir, &plugin_bytes))
        .await
        .map_err(|err| format!("plugin installation task failed: {err}"))?;

    match outcome {
        Ok(result) => Ok(map_plugin_install_outcome(
            &result,
            platform,
            detected_paths,
            is_flatpak,
        )),
        Err(SetupError::InvalidConfig(msg)) => Ok(PluginInstallResultDto {
            status: PluginInstallStatus::Error,
            platform,
            target_path: target_dir_str,
            detected_paths,
            is_flatpak,
            flatpak_hint: None,
            message: msg,
        }),
        Err(err) => Ok(PluginInstallResultDto {
            status: PluginInstallStatus::Error,
            platform,
            target_path: target_dir_str,
            detected_paths,
            is_flatpak,
            flatpak_hint: None,
            message: err.to_string(),
        }),
    }
}

/// Downloads and verifies `modelId` from the catalog, streaming `kotoha://download-progress` events.
#[tauri::command]
pub async fn download_model_profile(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    model_id: String,
) -> Result<DownloadProgressEventDto, String> {
    let profile = state
        .find_model_profile(&model_id)
        .ok_or_else(|| format!("unknown model profile '{model_id}'"))?;

    let fallback_total_bytes = u64::from(profile.size_mb).saturating_mul(1024 * 1024);
    let dest_path = state
        .models_root_dir()
        .join(&profile.model_id)
        .join("model.bin");

    let downloader = ModelDownloader::new();
    let app_for_progress = app.clone();
    let model_id_for_progress = profile.model_id.clone();

    let download_res = downloader
        .download_file_for_model(
            &profile.model_id,
            &profile.download_url,
            &dest_path,
            &profile.sha256,
            move |progress| {
                let event =
                    DownloadProgressEventDto::from_setup_progress(&progress, fallback_total_bytes);
                let _ = app_for_progress.emit(EVENT_DOWNLOAD_PROGRESS, &event);
            },
        )
        .await;

    match download_res {
        Ok(artifact) => {
            let final_event = DownloadProgressEventDto {
                model_id: model_id_for_progress,
                bytes_downloaded: artifact.bytes_downloaded,
                total_bytes: artifact.bytes_downloaded.max(fallback_total_bytes),
                status: ModelDownloadStatus::Verified,
                sha256_verified: artifact.verified,
                error_message: None,
            };
            let _ = app.emit(EVENT_DOWNLOAD_PROGRESS, &final_event);
            Ok(final_event)
        }
        Err(err) => {
            let failed_event = DownloadProgressEventDto {
                model_id: model_id_for_progress,
                bytes_downloaded: 0,
                total_bytes: fallback_total_bytes,
                status: ModelDownloadStatus::Failed,
                sha256_verified: false,
                error_message: Some(err.to_string()),
            };
            let _ = app.emit(EVENT_DOWNLOAD_PROGRESS, &failed_event);
            Err(err.to_string())
        }
    }
}

/// Switches the active translation provider and optional source/target language pair.
#[tauri::command]
pub async fn switch_translation_provider(
    state: State<'_, Arc<AppState>>,
    config: TranslationProviderConfig,
    src_lang: Option<String>,
    tgt_lang: Option<String>,
) -> Result<AppRuntimeStateDto, String> {
    state
        .switch_translation_provider(config, src_lang, tgt_lang)
        .await
}

/// Updates enabled glossary profiles, `hotwords_score`, and optional custom glossary directory.
#[tauri::command]
pub async fn update_glossary_settings(
    state: State<'_, Arc<AppState>>,
    enabled_profiles: Vec<String>,
    hotwords_score: f32,
    glossary_dir: Option<String>,
) -> Result<GlossarySettingsStateDto, String> {
    state
        .update_glossary_settings(enabled_profiles, hotwords_score, glossary_dir)
        .await
}

/// Updates overlay styling preferences, applies click-through lock if included, and emits
/// `kotoha://overlay-style-changed` to all windows.
#[tauri::command]
pub async fn update_overlay_style(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    style: OverlayStyleUpdateInput,
) -> Result<OverlayDisplayOptionsDto, String> {
    let previous_locked = state.snapshot().await.overlay_options.locked;
    let updated = state.update_overlay_style(style).await?;
    if updated.locked != previous_locked {
        apply_window_ignore_cursor(&app, updated.locked)?;
        let _ = app.emit(EVENT_OVERLAY_LOCK_CHANGED, updated.locked);
    }
    let _ = emit_overlay_style_changed(&app, &updated);
    Ok(updated)
}

/// Marks the first-run setup wizard as completed and returns the refreshed runtime state.
#[tauri::command]
pub async fn complete_first_run_wizard(
    state: State<'_, Arc<AppState>>,
) -> Result<AppRuntimeStateDto, String> {
    state.complete_first_run_wizard().await
}

/// Helper shared between the Tauri command and system tray menu for locking/unlocking the overlay.
pub async fn apply_overlay_lock(
    app: &AppHandle,
    state: &Arc<AppState>,
    locked: bool,
) -> Result<OverlayDisplayOptionsDto, String> {
    let updated = state.set_overlay_locked(locked).await?;
    apply_window_ignore_cursor(app, locked)?;
    let _ = app.emit(EVENT_OVERLAY_LOCK_CHANGED, locked);
    let _ = emit_overlay_style_changed(app, &updated);
    Ok(updated)
}

/// Helper shared between the Tauri command and system tray menu for toggling overlay visibility.
pub async fn apply_overlay_visibility(
    app: &AppHandle,
    state: &Arc<AppState>,
    visible: Option<bool>,
) -> Result<bool, String> {
    let next_visible = state.set_overlay_visible(visible).await;
    if let Some(overlay_window) = app.get_webview_window("overlay") {
        if next_visible {
            overlay_window
                .show()
                .map_err(|err| format!("failed to show overlay window: {err}"))?;
        } else {
            overlay_window
                .hide()
                .map_err(|err| format!("failed to hide overlay window: {err}"))?;
        }
    }
    Ok(next_visible)
}

fn apply_window_ignore_cursor(app: &AppHandle, locked: bool) -> Result<(), String> {
    if let Some(overlay_window) = app.get_webview_window("overlay") {
        overlay_window
            .set_ignore_cursor_events(locked)
            .map_err(|err| format!("failed to set overlay click-through state: {err}"))?;
    }
    Ok(())
}
