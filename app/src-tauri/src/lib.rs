//! Kotoha Tauri v2 desktop application library entrypoint.

pub mod bridge;
pub mod commands;
pub mod state;
pub mod tray;

use std::sync::{Arc, Mutex};

use tauri::Manager;
use tokio::sync::watch;

use crate::bridge::spawn_background_pipeline;
use crate::state::{resolve_app_data_dir, AppState};
use crate::tray::setup_tray;

struct ShutdownSignal(Mutex<Option<watch::Sender<bool>>>);

impl ShutdownSignal {
    fn trigger(&self) {
        if let Ok(mut guard) = self.0.lock() {
            if let Some(tx) = guard.take() {
                let _ = tx.send(true);
            }
        }
    }
}

/// Boots the Tauri v2 desktop application, registers windows, system tray, IPC commands, and
/// background audio-to-subtitle pipeline.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let data_dir = resolve_app_data_dir();
    let app_state = match AppState::new(data_dir) {
        Ok(state) => Arc::new(state),
        Err(err) => {
            eprintln!("failed to initialize Kotoha application state: {err}");
            std::process::exit(1);
        }
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let builder = tauri::Builder::default()
        .manage(app_state)
        .manage(ShutdownSignal(Mutex::new(Some(shutdown_tx))))
        .invoke_handler(tauri::generate_handler![
            commands::get_app_state,
            commands::set_overlay_locked,
            commands::toggle_overlay_visible,
            commands::install_mumble_plugin,
            commands::download_model_profile,
            commands::switch_translation_provider,
            commands::set_provider_secret,
            commands::clear_provider_secret,
            commands::secret_status,
            commands::update_glossary_settings,
            commands::update_overlay_style,
            commands::complete_first_run_wizard,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let state = handle.state::<Arc<AppState>>().inner().clone();
            let initial_locked = tauri::async_runtime::block_on(async {
                state.snapshot().await.overlay_options.locked
            });

            if let Some(overlay_window) = app.get_webview_window("overlay") {
                let _ = overlay_window.set_ignore_cursor_events(initial_locked);
            }

            if let Err(err) = setup_tray(&handle) {
                eprintln!("warning: failed to initialize system tray icon: {err}");
            }
            spawn_background_pipeline(handle, shutdown_rx);
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "settings" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    if let Err(err) = window.hide() {
                        eprintln!("warning: failed to hide settings window on close: {err}");
                    }
                }
            }
        });

    let app = match builder.build(tauri::generate_context!()) {
        Ok(app) => app,
        Err(err) => {
            eprintln!("failed to build Kotoha Tauri application: {err}");
            std::process::exit(1);
        }
    };

    app.run(|app_handle, event| {
        if matches!(
            event,
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
        ) {
            app_handle.state::<ShutdownSignal>().trigger();
        }
    });
}
