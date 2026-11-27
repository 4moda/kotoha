//! Desktop binary entrypoint for Kotoha Tauri v2 application.

// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    kotoha_app_lib::run();
}
