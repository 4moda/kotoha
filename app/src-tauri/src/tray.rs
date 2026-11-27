//! System tray icon and menu actions for Kotoha.

use std::sync::Arc;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};

use crate::commands::{apply_overlay_lock, apply_overlay_visibility};
use crate::state::AppState;

const MENU_ID_TOGGLE_OVERLAY: &str = "toggle_overlay";
const MENU_ID_TOGGLE_LOCK: &str = "toggle_lock";
const MENU_ID_OPEN_SETTINGS: &str = "open_settings";
const MENU_ID_QUIT: &str = "quit";

/// Configures the Kotoha system tray icon and context menu items.
pub fn setup_tray(app: &AppHandle) -> Result<(), tauri::Error> {
    let toggle_overlay_item = MenuItem::with_id(
        app,
        MENU_ID_TOGGLE_OVERLAY,
        "字幕の表示 / 非表示",
        true,
        None::<&str>,
    )?;
    let toggle_lock_item = MenuItem::with_id(
        app,
        MENU_ID_TOGGLE_LOCK,
        "オーバーレイ位置ロック切替",
        true,
        None::<&str>,
    )?;
    let open_settings_item = MenuItem::with_id(
        app,
        MENU_ID_OPEN_SETTINGS,
        "設定 / セットアップを開く",
        true,
        None::<&str>,
    )?;
    let quit_item = MenuItem::with_id(app, MENU_ID_QUIT, "終了", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &toggle_overlay_item,
            &toggle_lock_item,
            &open_settings_item,
            &quit_item,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id("kotoha-tray")
        .menu(&menu)
        .tooltip("Kotoha — Voice Chat Translator")
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            handle_tray_menu_event(app, event.id().as_ref());
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    builder.build(app)?;
    Ok(())
}

fn handle_tray_menu_event(app: &AppHandle, menu_id: &str) {
    match menu_id {
        MENU_ID_TOGGLE_OVERLAY => {
            let app_handle = app.clone();
            tauri::async_runtime::spawn(async move {
                let state = app_handle.state::<Arc<AppState>>().inner().clone();
                let _ = apply_overlay_visibility(&app_handle, &state, None).await;
            });
        }
        MENU_ID_TOGGLE_LOCK => {
            let app_handle = app.clone();
            tauri::async_runtime::spawn(async move {
                let state = app_handle.state::<Arc<AppState>>().inner().clone();
                let next_locked = !state.snapshot().await.overlay_options.locked;
                let _ = apply_overlay_lock(&app_handle, &state, next_locked).await;
            });
        }
        MENU_ID_OPEN_SETTINGS => {
            if let Some(settings_window) = app.get_webview_window("settings") {
                let _ = settings_window.show();
                let _ = settings_window.unminimize();
                let _ = settings_window.set_focus();
            }
        }
        MENU_ID_QUIT => {
            app.exit(0);
        }
        _ => {}
    }
}
