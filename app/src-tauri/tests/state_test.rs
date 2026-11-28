//! Parity and state tests verifying `AppState`, command DTO shapes, and IPC bridge events
//! against `app/ui/src/services/tauriBridge.ts` without requiring a display server.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use kotoha_app_lib::bridge::ConnectionStatusMonitor;
use kotoha_app_lib::state::{
    embedded_mumble_plugin_bytes, map_plugin_install_outcome, AppState, ConnectionState,
    DownloadProgressEventDto, HostOsPlatform, ModelDownloadStatus, OverlayStyleUpdateInput,
    PluginInstallStatus, EVENT_CONNECTION_STATUS, EVENT_DOWNLOAD_PROGRESS,
    EVENT_OVERLAY_LOCK_CHANGED, EVENT_OVERLAY_STYLE_CHANGED, EVENT_SUBTITLE,
};
use kotoha_core::domain::{
    AudioInputEvent, ComputeDevice, ProviderSecretKind, SpeakerKey, SubtitleEntry, SubtitleStage,
    TranslationProviderConfig, UtteranceId,
};
use kotoha_setup::{DownloadPhase, DownloadProgress, PluginInstallOutcome};

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("{prefix}_{}_{}", std::process::id(), nanos))
}

#[tokio::test]
async fn app_state_serializes_runtime_snapshot_matching_tauri_bridge_ts() {
    let data_dir = unique_temp_dir("kotoha_tauri_state");
    let glossary_dir = data_dir.join("glossaries");
    fs::create_dir_all(&glossary_dir).unwrap();
    fs::write(
        glossary_dir.join("sample.toml"),
        "name = \"My Profile\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\"\n\n[terms]\n\"Ferox\" = \"Ferox\"\n\"Logi\" = \"ロジ\"\n",
    )
    .unwrap();

    let state = AppState::new(&data_dir).expect("AppState::new should succeed");
    let snapshot = state.snapshot().await;
    let json = serde_json::to_value(&snapshot).expect("snapshot should serialize");

    assert_eq!(json["src_lang"], "en");
    assert_eq!(json["tgt_lang"], "ja");
    assert_eq!(json["provider"]["type"], "local_model");
    assert_eq!(json["provider"]["model_id"], "fugumt-en-ja-int8");
    assert_eq!(json["provider"]["device"], "auto");
    assert!(json["provider"]["threads"].is_null());

    assert_eq!(json["overlay_options"]["locked"], true);
    assert_eq!(json["overlay_options"]["show_source_text"], true);
    assert!(json["overlay_options"]["max_lines"].is_u64());
    assert!(json["overlay_options"]["font_size_px"].is_u64());
    assert!(json["overlay_options"]["background_opacity"].is_number());
    assert_eq!(json["overlay_visible"], true);
    assert_eq!(json["wizard_completed"], false);

    assert_eq!(json["glossary"]["profiles"][0]["id"], "my-profile");
    assert_eq!(json["glossary"]["profiles"][0]["name"], "My Profile");
    assert_eq!(json["glossary"]["profiles"][0]["src_lang"], "en");
    assert_eq!(json["glossary"]["profiles"][0]["tgt_lang"], "ja");
    assert_eq!(json["glossary"]["profiles"][0]["term_count"], 2);
    assert_eq!(json["glossary"]["profiles"][0]["enabled"], true);
    assert_eq!(
        json["glossary"]["enabled_profiles"],
        serde_json::json!(["My Profile"])
    );
    assert!((json["glossary"]["hotwords_score"].as_f64().unwrap() - 1.5).abs() < 1e-6);

    assert_eq!(json["connection"]["state"], "disconnected");
    assert_eq!(json["connection"]["source"], "mumble");
    assert_eq!(json["connection"]["active_speakers"], 0);
    assert_eq!(json["connection"]["dropped_frames"], 0);
    assert!(json["connection"]["endpoint"].is_string());
    assert_eq!(json["subtitles"], serde_json::json!([]));
    assert!(matches!(
        json["platform"].as_str().unwrap(),
        "windows" | "macos" | "linux"
    ));

    let _ = fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn app_state_persists_sanitized_provider_secrets_glossary_overlay_and_wizard_completion() {
    let data_dir = unique_temp_dir("kotoha_tauri_persist");
    let glossary_dir = data_dir.join("glossaries");
    fs::create_dir_all(&glossary_dir).unwrap();
    fs::write(
        glossary_dir.join("fps.toml"),
        "name = \"FPS Callouts\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\"\n\n[terms]\n\"Mid\" = \"ミッド\"\n",
    )
    .unwrap();

    let state = AppState::new(&data_dir).expect("AppState::new should succeed");

    // 1. Switch provider to DeepL and store the key through the write-only command path;
    //    the snapshot only exposes booleans and the settings file never contains the key.
    let deepl_config = TranslationProviderConfig::DeepL {
        use_free_endpoint: true,
        timeout_ms: 10_000,
    };
    let updated = state
        .switch_translation_provider(
            deepl_config.clone(),
            Some("en".to_owned()),
            Some("ja".to_owned()),
        )
        .await
        .expect("switch_translation_provider should succeed");
    assert_eq!(updated.provider, deepl_config);
    assert!(!updated.secrets.deepl_api_key);
    let status = state
        .set_provider_secret(
            ProviderSecretKind::DeepLApiKey,
            "super-secret-deepl-key".to_owned(),
        )
        .await
        .expect("set_provider_secret");
    assert!(status.deepl_api_key);
    assert!(state.snapshot().await.secrets.deepl_api_key);
    assert_eq!(
        state.translator_manager().current_provider_id().await,
        "deepl"
    );
    let snapshot_json = serde_json::to_string(&state.snapshot().await).unwrap();
    assert!(!snapshot_json.contains("super-secret-deepl-key"));
    let disk_json = fs::read_to_string(state.settings_path()).expect("settings JSON must exist");
    assert!(!disk_json.contains("super-secret-deepl-key"));
    assert!(!format!("{:?}", state.snapshot().await).contains("super-secret-deepl-key"));

    // 2. Switch to Mock provider and test active hot-swap + overlay style + glossary update.
    let mock_state = state
        .switch_translation_provider(
            TranslationProviderConfig::Mock {
                prefix: "[ja] ".to_owned(),
            },
            None,
            None,
        )
        .await
        .expect("mock provider switch should succeed");
    assert_eq!(
        mock_state.provider,
        TranslationProviderConfig::Mock {
            prefix: "[ja] ".to_owned(),
        }
    );

    let overlay = state
        .update_overlay_style(OverlayStyleUpdateInput {
            max_lines: Some(5),
            show_source_text: Some(false),
            locked: Some(false),
            font_size_px: Some(22),
            background_opacity: Some(0.9),
        })
        .await
        .expect("update_overlay_style should succeed");
    assert_eq!(overlay.max_lines, 5);
    assert!(!overlay.show_source_text);
    assert!(!overlay.locked);
    assert_eq!(overlay.font_size_px, 22);
    assert!((overlay.background_opacity - 0.9).abs() < 1e-6);

    let toggled_hidden = state.set_overlay_visible(None).await;
    assert!(!toggled_hidden);
    assert!(state.set_overlay_visible(Some(true)).await);

    let glossary = state
        .update_glossary_settings(vec!["FPS Callouts".to_owned()], 2.4, None)
        .await
        .expect("update_glossary_settings should succeed");
    assert_eq!(glossary.enabled_profiles, vec!["FPS Callouts".to_owned()]);
    assert!((glossary.hotwords_score - 2.4).abs() < 1e-6);
    assert!(glossary.profiles.iter().all(|p| p.enabled));

    let completed = state
        .complete_first_run_wizard()
        .await
        .expect("complete_first_run_wizard should succeed");
    assert!(completed.wizard_completed);

    // 3. Reload AppState from disk and verify persisted values (while LocalModel remains valid).
    state
        .switch_translation_provider(
            TranslationProviderConfig::LocalModel {
                model_id: "opus-mt-en-ja-int8".to_owned(),
                device: ComputeDevice::Cpu,
                threads: Some(4),
            },
            None,
            None,
        )
        .await
        .expect("LocalModel config should persist even before model download");

    let reloaded = AppState::new(&data_dir).expect("AppState reload should succeed");
    let reloaded_snap = reloaded.snapshot().await;
    assert!(reloaded_snap.wizard_completed);
    assert!(!reloaded_snap.overlay_options.locked);
    assert_eq!(reloaded_snap.overlay_options.max_lines, 5);
    assert_eq!(
        reloaded_snap.provider,
        TranslationProviderConfig::LocalModel {
            model_id: "opus-mt-en-ja-int8".to_owned(),
            device: ComputeDevice::Cpu,
            threads: Some(4),
        }
    );

    let _ = fs::remove_dir_all(&data_dir);
}

#[test]
fn event_names_and_dto_mappings_match_tauri_bridge_contract() {
    assert_eq!(EVENT_SUBTITLE, "kotoha://subtitle");
    assert_eq!(EVENT_CONNECTION_STATUS, "kotoha://connection-status");
    assert_eq!(EVENT_DOWNLOAD_PROGRESS, "kotoha://download-progress");
    assert_eq!(EVENT_OVERLAY_LOCK_CHANGED, "kotoha://overlay-lock-changed");
    assert_eq!(
        EVENT_OVERLAY_STYLE_CHANGED,
        "kotoha://overlay-style-changed"
    );

    // In default dev builds without KOTOHA_MUMBLE_PLUGIN_PATH, embedded bytes are None.
    if option_env!("KOTOHA_MUMBLE_PLUGIN_PATH").is_none() {
        assert!(embedded_mumble_plugin_bytes().is_none());
    }

    let subtitle = SubtitleEntry {
        utterance_id: UtteranceId(101),
        speaker_key: SpeakerKey::new("mumble", 12),
        speaker_name: "Alex".to_owned(),
        speaker_color: "#38BDF8".to_owned(),
        src_text: "Hello".to_owned(),
        translated_text: Some("こんにちは".to_owned()),
        stage: SubtitleStage::Translated,
        failure_reason: None,
        timestamp_ms: 1_700_000_001_000,
    };
    let sub_json = serde_json::to_value(&subtitle).unwrap();
    assert_eq!(sub_json["stage"], "translated");
    assert_eq!(sub_json["utterance_id"], 101);
    assert_eq!(sub_json["speaker_key"]["source"], "mumble");
    assert_eq!(sub_json["speaker_key"]["user_id"], 12);

    let dl_dto = DownloadProgressEventDto::from_setup_progress(
        &DownloadProgress {
            model_id: "fugumt-en-ja-int8".to_owned(),
            bytes_downloaded: 512,
            total_bytes: None,
            phase: DownloadPhase::Verifying,
        },
        1024,
    );
    let dl_json = serde_json::to_value(&dl_dto).unwrap();
    assert_eq!(dl_json["status"], "verifying_sha256");
    assert_eq!(dl_json["sha256_verified"], false);
    assert_eq!(dl_json["total_bytes"], 1024);

    let dl_done = DownloadProgressEventDto::from_setup_progress(
        &DownloadProgress {
            model_id: "fugumt-en-ja-int8".to_owned(),
            bytes_downloaded: 1024,
            total_bytes: Some(1024),
            phase: DownloadPhase::Completed,
        },
        1024,
    );
    assert_eq!(dl_done.status, ModelDownloadStatus::Verified);
    assert!(dl_done.sha256_verified);

    let locked_outcome = PluginInstallOutcome::LockedByRunningApp {
        path: PathBuf::from(r"C:\Mumble\Plugins\kotoha_mumble.dll"),
        guidance: "Close Mumble and retry.".to_owned(),
    };
    let plugin_dto = map_plugin_install_outcome(
        &locked_outcome,
        HostOsPlatform::Windows,
        vec![r"C:\Mumble\Plugins".to_owned()],
        false,
    );
    assert_eq!(plugin_dto.status, PluginInstallStatus::LockedByRunningApp);
    assert_eq!(plugin_dto.platform, HostOsPlatform::Windows);
    assert_eq!(plugin_dto.target_path, r"C:\Mumble\Plugins");

    let flatpak_outcome = PluginInstallOutcome::Installed {
        path: PathBuf::from(
            "/home/user/.var/app/info.mumble.Mumble/data/Mumble/Mumble/Plugins/libkotoha_mumble.so",
        ),
        sha256: "abc".to_owned(),
        replaced_existing: false,
    };
    let flatpak_dto = map_plugin_install_outcome(
        &flatpak_outcome,
        HostOsPlatform::Linux,
        vec!["/home/user/.var/app/info.mumble.Mumble/data/Mumble/Mumble/Plugins".to_owned()],
        true,
    );
    assert_eq!(flatpak_dto.status, PluginInstallStatus::Installed);
    assert!(flatpak_dto.is_flatpak);
    assert!(flatpak_dto
        .flatpak_hint
        .as_deref()
        .unwrap()
        .contains("xdg-run/kotoha_v1.sock"));

    let mut monitor = ConnectionStatusMonitor::new("/tmp/kotoha_test.sock");
    let connected = monitor
        .observe(&AudioInputEvent::SourceConnected {
            conn_id: 1,
            source: "mumble".to_owned(),
            plugin_version: "0.1.0".to_owned(),
        })
        .expect("SourceConnected should emit connection status");
    assert_eq!(connected.state, ConnectionState::Connected);
    assert_eq!(connected.plugin_version.as_deref(), Some("0.1.0"));

    let hb = monitor
        .observe(&AudioInputEvent::Heartbeat {
            conn_id: 1,
            source: "mumble".to_owned(),
        })
        .expect("Heartbeat should emit connection status");
    assert_eq!(hb.state, ConnectionState::Connected);
    assert!(hb.last_heartbeat_ms.is_some());

    // Superseded connection disconnect must NOT mark status disconnected.
    let _ = monitor
        .observe(&AudioInputEvent::SourceConnected {
            conn_id: 2,
            source: "mumble".to_owned(),
            plugin_version: "0.1.1".to_owned(),
        })
        .unwrap();
    assert!(monitor
        .observe(&AudioInputEvent::SourceDisconnected {
            conn_id: 1,
            source: "mumble".to_owned(),
            reason: kotoha_core::domain::DisconnectReason::Superseded,
        })
        .is_none());

    let disconnected = monitor
        .observe(&AudioInputEvent::SourceDisconnected {
            conn_id: 2,
            source: "mumble".to_owned(),
            reason: kotoha_core::domain::DisconnectReason::Eof,
        })
        .expect("owning disconnect should emit disconnected status");
    assert_eq!(disconnected.state, ConnectionState::Disconnected);
    assert!(disconnected.last_heartbeat_ms.is_none());
}

#[tokio::test]
async fn non_ascii_glossary_profile_names_receive_distinct_deterministic_ids_and_empty_dir_normalizes(
) {
    let data_dir = unique_temp_dir("kotoha_tauri_non_ascii_glossary");
    let glossary_dir = data_dir.join("glossaries");
    fs::create_dir_all(&glossary_dir).unwrap();
    fs::write(
        glossary_dir.join("a_general.toml"),
        "name = \"一般\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\"\n\n[terms]\n\"Hello\" = \"こんにちは\"\n",
    )
    .unwrap();
    fs::write(
        glossary_dir.join("b_game.toml"),
        "name = \"ゲーム\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\"\n\n[terms]\n\"GG\" = \"おつ\"\n",
    )
    .unwrap();
    fs::write(
        glossary_dir.join("c_mixed.toml"),
        "name = \"FPS ゲーム\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\"\n\n[terms]\n\"Mid\" = \"ミッド\"\n",
    )
    .unwrap();

    // Pre-seed settings with empty/whitespace glossary_dir: AppState::new must normalize it to None
    // so GlossaryManager::from_settings loads from default_glossary_dir cleanly.
    fs::write(
        data_dir.join("kotoha-settings.json"),
        r#"{
  "first_run_completed": true,
  "src_lang": "en",
  "tgt_lang": "ja",
  "translation_provider": { "type": "mock", "prefix": "[ja] " },
  "glossary": { "enabled_profiles": ["一般"], "glossary_dir": "   " },
  "hotwords_score": 1.5,
  "overlay": {
    "opacity": 0.82,
    "font_size_px": 20,
    "max_lines": 4,
    "show_source_text": true,
    "fade_out_seconds": 12,
    "locked": true,
    "position_x": 80,
    "position_y": 680,
    "width": 760,
    "height": 260
  },
  "custom_mumble_plugin_dir": null
}"#,
    )
    .unwrap();

    let state = AppState::new(&data_dir).expect("AppState::new with whitespace glossary_dir");
    let snap = state.snapshot().await;
    assert_eq!(snap.glossary.profiles.len(), 3);

    let general = snap
        .glossary
        .profiles
        .iter()
        .find(|p| p.name == "一般")
        .unwrap();
    let game = snap
        .glossary
        .profiles
        .iter()
        .find(|p| p.name == "ゲーム")
        .unwrap();
    let mixed = snap
        .glossary
        .profiles
        .iter()
        .find(|p| p.name == "FPS ゲーム")
        .unwrap();

    assert_ne!(general.id, "profile");
    assert_ne!(game.id, "profile");
    assert_ne!(general.id, game.id);
    assert!(general.id.starts_with("profile-"));
    assert!(game.id.starts_with("profile-"));
    assert!(mixed.id.starts_with("fps-"));
    assert!(general.enabled);
    assert!(!game.enabled);

    // Passing Some("") to update_glossary_settings normalizes to None and reloads default dir.
    let updated = state
        .update_glossary_settings(vec!["ゲーム".to_owned()], 1.8, Some("   ".to_owned()))
        .await
        .expect("update_glossary_settings with empty string should normalize");
    assert_eq!(updated.enabled_profiles, vec!["ゲーム".to_owned()]);
    assert_eq!(
        updated.glossary_dir,
        Some(glossary_dir.display().to_string())
    );
    assert!(state
        .sanitized_settings()
        .await
        .glossary
        .glossary_dir
        .is_none());

    // Deterministic across reloads
    let reloaded = AppState::new(&data_dir).expect("reload should succeed");
    let reloaded_snap = reloaded.snapshot().await;
    let general_reloaded = reloaded_snap
        .glossary
        .profiles
        .iter()
        .find(|p| p.name == "一般")
        .unwrap();
    assert_eq!(general_reloaded.id, general.id);

    let _ = fs::remove_dir_all(&data_dir);
}

fn openai_config(base_url: &str) -> TranslationProviderConfig {
    TranslationProviderConfig::OpenAiCompatible {
        base_url: base_url.to_owned(),
        model: "gpt-4o-mini".to_owned(),
        timeout_ms: 5_000,
    }
}

#[tokio::test]
async fn provider_secrets_are_write_only_scoped_to_the_active_provider_and_host_bound() {
    let data_dir = unique_temp_dir("kotoha_tauri_secrets");
    let state = AppState::new(&data_dir).expect("AppState::new");

    // A secret that does not belong to the active provider is rejected (default: local model).
    let err = state
        .set_provider_secret(ProviderSecretKind::DeepLApiKey, "k".to_owned())
        .await
        .expect_err("wrong provider");
    assert!(!err.contains('k') || err.contains("active translation provider"));
    assert!(state
        .set_provider_secret(ProviderSecretKind::DeepLApiKey, "  ".to_owned())
        .await
        .is_err());

    // DeepL without a key is saved but not hot-swapped (is_configured gate).
    state
        .switch_translation_provider(
            TranslationProviderConfig::DeepL {
                use_free_endpoint: true,
                timeout_ms: 5_000,
            },
            None,
            None,
        )
        .await
        .unwrap();
    assert_ne!(
        state.translator_manager().current_provider_id().await,
        "deepl"
    );

    // OpenAI-compatible: key bound to the origin of base_url.
    state
        .switch_translation_provider(openai_config("https://api.openai.com/v1"), None, None)
        .await
        .unwrap();
    let status = state
        .set_provider_secret(
            ProviderSecretKind::OpenAiApiKey,
            "sk-openai-secret".to_owned(),
        )
        .await
        .unwrap();
    assert!(status.openai_api_key);

    // Same origin (different path): key kept.
    let snap = state
        .switch_translation_provider(openai_config("https://api.openai.com/v2"), None, None)
        .await
        .unwrap();
    assert!(snap.secrets.openai_api_key);

    // Different host: the key is cleared and must be re-entered (N1).
    let snap = state
        .switch_translation_provider(openai_config("https://evil.example/v1"), None, None)
        .await
        .unwrap();
    assert!(!snap.secrets.openai_api_key);

    // Re-enter, then switching away and back also requires re-entry.
    state
        .set_provider_secret(
            ProviderSecretKind::OpenAiApiKey,
            "sk-openai-secret".to_owned(),
        )
        .await
        .unwrap();
    state
        .switch_translation_provider(
            TranslationProviderConfig::Mock {
                prefix: "[ja] ".to_owned(),
            },
            None,
            None,
        )
        .await
        .unwrap();
    let snap = state
        .switch_translation_provider(openai_config("https://evil.example/v1"), None, None)
        .await
        .unwrap();
    assert!(!snap.secrets.openai_api_key);

    // GAS: replacing the endpoint clears the bearer token; clear is idempotent.
    state
        .switch_translation_provider(
            TranslationProviderConfig::GasWebhook { timeout_ms: 5_000 },
            None,
            None,
        )
        .await
        .unwrap();
    let url_a = "https://script.google.com/macros/s/CAP-A/exec";
    state
        .set_provider_secret(ProviderSecretKind::GasEndpointUrl, url_a.to_owned())
        .await
        .unwrap();
    let status = state
        .set_provider_secret(ProviderSecretKind::GasBearerToken, "tok".to_owned())
        .await
        .unwrap();
    assert!(status.gas_endpoint_url && status.gas_bearer_token);
    assert_eq!(
        state.translator_manager().current_provider_id().await,
        "gas-webhook"
    );
    let status = state
        .set_provider_secret(
            ProviderSecretKind::GasEndpointUrl,
            "https://script.google.com/macros/s/CAP-B/exec".to_owned(),
        )
        .await
        .unwrap();
    assert!(status.gas_endpoint_url && !status.gas_bearer_token);

    // An invalid endpoint is rejected without leaking it and the previous value is restored.
    let err = state
        .set_provider_secret(
            ProviderSecretKind::GasEndpointUrl,
            "ftp://script.google.com/macros/s/CAP-C/exec".to_owned(),
        )
        .await
        .expect_err("invalid scheme");
    assert!(
        !err.contains("CAP-C"),
        "error must not contain the GAS capability id"
    );
    assert!(state.secret_status().gas_endpoint_url);

    let status = state
        .clear_provider_secret(ProviderSecretKind::GasEndpointUrl)
        .await
        .unwrap();
    assert!(!status.gas_endpoint_url);
    state
        .clear_provider_secret(ProviderSecretKind::GasEndpointUrl)
        .await
        .unwrap();

    // Nothing secret was ever persisted.
    let disk_json = fs::read_to_string(state.settings_path()).unwrap();
    for needle in [
        "sk-openai-secret",
        "CAP-A",
        "CAP-B",
        "tok\"",
        "script.google.com",
    ] {
        assert!(!disk_json.contains(needle), "{needle} in {disk_json}");
    }

    let _ = fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn legacy_settings_file_with_plaintext_gas_url_loads_and_is_rewritten() {
    let data_dir = unique_temp_dir("kotoha_tauri_legacy");
    fs::create_dir_all(&data_dir).unwrap();
    let mut doc = serde_json::to_value(kotoha_setup::AppSettings::default()).unwrap();
    doc["translation_provider"] = serde_json::json!({
        "type": "gas_webhook",
        "endpoint_url": "https://script.google.com/macros/s/LEGACY-CAP/exec",
        "bearer_token": "__KOTOHA_SECRET_STORE__",
        "timeout_ms": 5000
    });
    fs::write(
        data_dir.join("kotoha-settings.json"),
        serde_json::to_string(&doc).unwrap(),
    )
    .unwrap();

    let state = AppState::new(&data_dir).expect("legacy settings must load");
    let snap = state.snapshot().await;
    assert_eq!(
        snap.provider,
        TranslationProviderConfig::GasWebhook { timeout_ms: 5_000 }
    );
    assert!(snap.secrets.gas_endpoint_url);
    assert!(!snap.secrets.gas_bearer_token);
    let rewritten = fs::read_to_string(state.settings_path()).unwrap();
    assert!(!rewritten.contains("LEGACY-CAP"));
    assert!(!rewritten.contains("endpoint_url"));

    let _ = fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn unbuildable_provider_is_reported_as_translator_error_without_secrets() {
    let data_dir = unique_temp_dir("kotoha_tauri_status");
    // Default provider is the local model, which is not downloaded in a fresh data dir.
    let state = AppState::new(&data_dir).unwrap();
    let json = serde_json::to_value(state.snapshot().await).unwrap();
    assert_eq!(json["translator_status"]["state"], "error");
    let reason = json["translator_status"]["reason"].as_str().unwrap();
    assert!(!reason.is_empty());
    assert!(json["settings_notice"].is_null());

    // Mock provider is always buildable.
    state
        .switch_translation_provider(
            TranslationProviderConfig::Mock {
                prefix: "[t] ".to_owned(),
            },
            None,
            None,
        )
        .await
        .unwrap();
    let json = serde_json::to_value(state.snapshot().await).unwrap();
    assert_eq!(json["translator_status"]["state"], "ok");

    let _ = fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn corrupt_settings_file_is_backed_up_and_startup_uses_defaults_with_notice() {
    let data_dir = unique_temp_dir("kotoha_tauri_corrupt");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("kotoha-settings.json"),
        "{ definitely not json",
    )
    .unwrap();

    let state = AppState::new(&data_dir).expect("corrupt settings must not abort startup");
    let json = serde_json::to_value(state.snapshot().await).unwrap();
    assert_eq!(json["src_lang"], "en");
    assert!(json["settings_notice"]
        .as_str()
        .is_some_and(|n| !n.is_empty()));
    let has_backup = fs::read_dir(&data_dir)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().contains(".bak-"));
    assert!(has_backup);

    let _ = fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn language_pair_changes_are_published_to_the_pipeline_bridge() {
    let data_dir = unique_temp_dir("kotoha_tauri_lang");
    let state = AppState::new(&data_dir).unwrap();
    let mut rx = state.subscribe_language_pair();
    assert_eq!(rx.borrow_and_update().1.as_str(), "ja");

    let config = state.snapshot().await.provider;
    state
        .switch_translation_provider(config.clone(), Some("ja".to_owned()), Some("en".to_owned()))
        .await
        .unwrap();
    assert!(rx.has_changed().unwrap());
    let pair = rx.borrow_and_update().clone();
    assert_eq!((pair.0.as_str(), pair.1.as_str()), ("ja", "en"));

    // Same pair again publishes nothing.
    state
        .switch_translation_provider(config, None, None)
        .await
        .unwrap();
    assert!(!rx.has_changed().unwrap());

    let _ = fs::remove_dir_all(&data_dir);
}
