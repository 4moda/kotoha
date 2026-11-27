//! Build script for `kotoha-app`: optionally embeds Mumble plugin binary and runs `tauri_build`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=KOTOHA_MUMBLE_PLUGIN_PATH");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=capabilities/default.json");
    println!("cargo:rerun-if-changed=icons/32x32.png");
    println!("cargo:rerun-if-changed=icons/128x128.png");
    println!("cargo:rerun-if-changed=icons/icon.png");
    println!("cargo:rerun-if-changed=icons/icon.ico");

    embed_optional_mumble_plugin()?;
    tauri_build::build();
    Ok(())
}

fn embed_optional_mumble_plugin() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR must be set by Cargo")?);
    let dest = out_dir.join("embedded_mumble_plugin.bin");

    let raw_path = env::var("KOTOHA_MUMBLE_PLUGIN_PATH").unwrap_or_default();
    let trimmed = raw_path.trim();

    if trimmed.is_empty() {
        fs::write(&dest, [])?;
        return Ok(());
    }

    let plugin_path = Path::new(trimmed);
    println!("cargo:rerun-if-changed={}", plugin_path.display());
    if !plugin_path.is_file() {
        return Err(format!(
            "KOTOHA_MUMBLE_PLUGIN_PATH points to missing file: {}",
            plugin_path.display()
        )
        .into());
    }

    let bytes = fs::read(plugin_path).map_err(|err| {
        format!(
            "failed to read KOTOHA_MUMBLE_PLUGIN_PATH '{}': {err}",
            plugin_path.display()
        )
    })?;
    if bytes.is_empty() {
        return Err(format!(
            "KOTOHA_MUMBLE_PLUGIN_PATH '{}' must not be empty",
            plugin_path.display()
        )
        .into());
    }

    fs::write(&dest, bytes)?;
    Ok(())
}
