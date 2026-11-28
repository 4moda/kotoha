//! Unit tests for `kotoha-setup` Mumble plugin installer (`src/plugin_installer.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use common::{sha256_hex, unique_temp_dir};
use kotoha_setup::{
    discover_mumble_plugin_dirs_for, discover_mumble_plugin_dirs_with_custom, install_plugin,
    install_plugin_for_os, is_locked_file_io_error, locked_plugin_guidance, plugin_filename_for,
    MumblePluginDirKind, PluginInstallOutcome, SetupError, TargetPlatform,
};

#[test]
fn discovers_os_specific_mumble_plugin_directories_across_windows_macos_linux_and_flatpak() {
    // Windows: %AppData%\Mumble\Mumble\Plugins and %LocalAppData%\Mumble\Mumble\Plugins
    let win_env = BTreeMap::from([
        (
            "APPDATA".to_owned(),
            r"C:\Users\Alice\AppData\Roaming".to_owned(),
        ),
        (
            "LOCALAPPDATA".to_owned(),
            r"C:\Users\Alice\AppData\Local".to_owned(),
        ),
    ]);
    let win_dirs = discover_mumble_plugin_dirs_for(TargetPlatform::Windows, &win_env);
    assert_eq!(win_dirs.len(), 2);
    assert_eq!(win_dirs[0].kind, MumblePluginDirKind::WindowsAppData);
    assert_eq!(
        win_dirs[0].path,
        PathBuf::from(r"C:\Users\Alice\AppData\Roaming\Mumble\Mumble\Plugins")
    );
    assert_eq!(win_dirs[1].kind, MumblePluginDirKind::WindowsLocalAppData);
    assert_eq!(
        win_dirs[1].path,
        PathBuf::from(r"C:\Users\Alice\AppData\Local\Mumble\Mumble\Plugins")
    );

    // macOS: ~/Library/Application Support/Mumble/Mumble/Plugins
    let mac_env = BTreeMap::from([("HOME".to_owned(), "/Users/alice".to_owned())]);
    let mac_dirs = discover_mumble_plugin_dirs_for(TargetPlatform::MacOs, &mac_env);
    assert_eq!(mac_dirs.len(), 1);
    assert_eq!(
        mac_dirs[0].kind,
        MumblePluginDirKind::MacOsApplicationSupport
    );
    assert_eq!(
        mac_dirs[0].path,
        PathBuf::from("/Users/alice/Library/Application Support/Mumble/Mumble/Plugins")
    );

    // Linux with explicit XDG_DATA_HOME + Flatpak
    let linux_xdg_env = BTreeMap::from([
        ("HOME".to_owned(), "/home/alice".to_owned()),
        (
            "XDG_DATA_HOME".to_owned(),
            "/var/home/alice/data/".to_owned(),
        ),
    ]);
    let linux_xdg_dirs = discover_mumble_plugin_dirs_for(TargetPlatform::Linux, &linux_xdg_env);
    assert_eq!(linux_xdg_dirs.len(), 2);
    assert_eq!(linux_xdg_dirs[0].kind, MumblePluginDirKind::LinuxXdgData);
    assert_eq!(
        linux_xdg_dirs[0].path,
        PathBuf::from("/var/home/alice/data/Mumble/Mumble/Plugins")
    );
    assert_eq!(linux_xdg_dirs[1].kind, MumblePluginDirKind::LinuxFlatpak);
    assert_eq!(
        linux_xdg_dirs[1].path,
        PathBuf::from("/home/alice/.var/app/info.mumble.Mumble/data/Mumble/Mumble/Plugins")
    );

    // Linux fallback (~/.local/share) when XDG_DATA_HOME is unset/empty + Flatpak + custom dir
    let linux_fallback_env = BTreeMap::from([
        ("HOME".to_owned(), "/home/bob".to_owned()),
        ("XDG_DATA_HOME".to_owned(), "   ".to_owned()),
    ]);
    let custom_path = Path::new("/opt/mumble-plugins");
    let linux_fallback_dirs = discover_mumble_plugin_dirs_with_custom(
        TargetPlatform::Linux,
        &linux_fallback_env,
        Some(custom_path),
    );
    assert_eq!(linux_fallback_dirs.len(), 3);
    assert_eq!(
        linux_fallback_dirs[0].path,
        PathBuf::from("/home/bob/.local/share/Mumble/Mumble/Plugins")
    );
    assert_eq!(
        linux_fallback_dirs[1].path,
        PathBuf::from("/home/bob/.var/app/info.mumble.Mumble/data/Mumble/Mumble/Plugins")
    );
    assert_eq!(linux_fallback_dirs[2].kind, MumblePluginDirKind::Custom);
    assert_eq!(linux_fallback_dirs[2].path, custom_path);

    // Deduplicates identical custom directory
    let dedup_dirs = discover_mumble_plugin_dirs_with_custom(
        TargetPlatform::MacOs,
        &mac_env,
        Some(Path::new(
            "/Users/alice/Library/Application Support/Mumble/Mumble/Plugins",
        )),
    );
    assert_eq!(dedup_dirs.len(), 1);
}

#[test]
fn plugin_filenames_match_os_conventions() {
    assert_eq!(
        plugin_filename_for(TargetPlatform::Windows),
        "kotoha_mumble.dll"
    );
    assert_eq!(
        plugin_filename_for(TargetPlatform::MacOs),
        "libkotoha_mumble.dylib"
    );
    assert_eq!(
        plugin_filename_for(TargetPlatform::Linux),
        "libkotoha_mumble.so"
    );
}

#[test]
fn install_plugin_handles_fresh_install_idempotent_skip_upgrade_and_invalid_inputs() {
    let root = unique_temp_dir("kotoha_plugin_install");
    let _ = fs::remove_dir_all(&root);
    let target_dir = root.join("Mumble/Mumble/Plugins");

    let v1_bytes = b"KOTOHA_PLUGIN_BINARY_V1";
    let v1_sha = sha256_hex(v1_bytes);

    // 1. Fresh install creates directory and writes binary
    let outcome_fresh =
        install_plugin(&target_dir, v1_bytes).expect("fresh plugin install should succeed");
    assert!(
        matches!(
            outcome_fresh,
            PluginInstallOutcome::Installed {
                replaced_existing: false,
                ..
            }
        ),
        "expected fresh Installed outcome, got {outcome_fresh:?}"
    );
    let expected_path = outcome_fresh.path().to_path_buf();
    if let PluginInstallOutcome::Installed { sha256, .. } = &outcome_fresh {
        assert_eq!(sha256, &v1_sha);
    }
    assert_eq!(fs::read(&expected_path).unwrap(), v1_bytes);

    // Verify no leftover .tmp files remain
    let leftover_tmp: Vec<_> = fs::read_dir(&target_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(
        leftover_tmp.is_empty(),
        "leftover tmp files: {leftover_tmp:?}"
    );

    // 2. Idempotent reinstall with identical bytes returns AlreadyUpToDate and preserves file
    let outcome_same =
        install_plugin(&target_dir, v1_bytes).expect("idempotent check should succeed");
    assert_eq!(
        outcome_same,
        PluginInstallOutcome::AlreadyUpToDate {
            path: expected_path.clone(),
            sha256: v1_sha,
        }
    );

    // 3. Upgrade with new bytes atomically replaces the existing plugin
    let v2_bytes = b"KOTOHA_PLUGIN_BINARY_V2_UPDATED";
    let v2_sha = sha256_hex(v2_bytes);
    let outcome_upgrade =
        install_plugin(&target_dir, v2_bytes).expect("upgrade install should succeed");
    assert_eq!(
        outcome_upgrade,
        PluginInstallOutcome::Installed {
            path: expected_path.clone(),
            sha256: v2_sha,
            replaced_existing: true,
        }
    );
    assert_eq!(fs::read(&expected_path).unwrap(), v2_bytes);

    // 4. Rejects empty plugin payload or empty target_dir cleanly
    assert!(matches!(
        install_plugin(&target_dir, b""),
        Err(SetupError::InvalidConfig(_))
    ));
    assert!(matches!(
        install_plugin(Path::new(""), v2_bytes),
        Err(SetupError::InvalidConfig(_))
    ));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn locked_plugin_file_detection_and_guidance_work_cross_platform() {
    // Raw Windows ERROR_SHARING_VIOLATION (32) and ERROR_ACCESS_DENIED (5) only count on Windows.
    #[cfg(windows)]
    {
        assert!(is_locked_file_io_error(&io::Error::from_raw_os_error(32)));
        assert!(is_locked_file_io_error(&io::Error::from_raw_os_error(33)));
        assert!(is_locked_file_io_error(&io::Error::from_raw_os_error(5)));
    }
    // On Unix these are EIO / EPIPE / EDOM, not lock conditions; ETXTBSY (26) is.
    #[cfg(unix)]
    {
        assert!(!is_locked_file_io_error(&io::Error::from_raw_os_error(32)));
        assert!(!is_locked_file_io_error(&io::Error::from_raw_os_error(33)));
        assert!(!is_locked_file_io_error(&io::Error::from_raw_os_error(5)));
        assert!(is_locked_file_io_error(&io::Error::from_raw_os_error(26)));
    }
    // PermissionDenied ErrorKind
    assert!(is_locked_file_io_error(&io::Error::new(
        ErrorKind::PermissionDenied,
        "sharing violation"
    )));
    // Unrelated NotFound is NOT treated as locked
    assert!(!is_locked_file_io_error(&io::Error::new(
        ErrorKind::NotFound,
        "missing"
    )));

    let sample_path = Path::new("/tmp/Mumble/Plugins/kotoha_mumble.dll");
    let guidance = locked_plugin_guidance(sample_path);
    assert!(guidance.contains("Mumble"));
    assert!(guidance.contains("retry"));
    assert!(guidance.contains("再試行"));

    // Simulate an exclusive/locked existing target file on Windows or a non-writable existing
    // target directory scenario.
    let root = unique_temp_dir("kotoha_plugin_locked");
    let _ = fs::remove_dir_all(&root);
    let target_dir = root.join("Plugins");
    fs::create_dir_all(&target_dir).unwrap();

    let v1 = b"LOCKED_PLUGIN_V1";
    let installed = install_plugin_for_os(&target_dir, v1, TargetPlatform::Windows).unwrap();
    let plugin_path = installed.path().to_path_buf();

    #[cfg(windows)]
    {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;
        // Open without FILE_SHARE_DELETE | FILE_SHARE_WRITE so replacing/renaming hits sharing violation
        let _lock_handle = OpenOptions::new()
            .read(true)
            .share_mode(1) // FILE_SHARE_READ only
            .open(&plugin_path)
            .expect("open plugin with exclusive write/delete lock");

        let outcome =
            install_plugin_for_os(&target_dir, b"LOCKED_PLUGIN_V2", TargetPlatform::Windows)
                .expect("locked target should produce LockedByRunningApp outcome");
        assert!(
            matches!(
                outcome,
                PluginInstallOutcome::LockedByRunningApp { ref path, ref guidance }
                    if path == &plugin_path && guidance.contains("Mumble")
            ),
            "expected LockedByRunningApp, got {outcome:?}"
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // On Unix, making the existing plugin file unreadable (0000) prevents SHA-256 open with
        // EACCES (PermissionDenied) when running as a non-root user.
        let mut perms = fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&plugin_path, perms).unwrap();

        let is_root = fs::File::open(&plugin_path).is_ok();
        if !is_root {
            let outcome =
                install_plugin_for_os(&target_dir, b"LOCKED_PLUGIN_V2", TargetPlatform::Windows)
                    .expect("unreadable target should return LockedByRunningApp");
            assert!(
                matches!(
                    outcome,
                    PluginInstallOutcome::LockedByRunningApp { ref path, ref guidance }
                        if path == &plugin_path && guidance.contains("Mumble")
                ),
                "expected LockedByRunningApp, got {outcome:?}"
            );
        }

        let mut restore = fs::metadata(&plugin_path).unwrap().permissions();
        restore.set_mode(0o644);
        let _ = fs::set_permissions(&plugin_path, restore);
    }

    let _ = fs::remove_dir_all(&root);
}
