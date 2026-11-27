//! Unit tests for `kotoha-setup` settings store and secret separation (`src/settings_store.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::fs;

use common::unique_temp_dir;
use kotoha_core::domain::{GlossarySettings, LanguageCode, TranslationProviderConfig};
use kotoha_setup::{
    AppSettings, FileSettingsStore, InMemorySecretStore, OverlayStyleSettings, SecretStore,
    SetupError, SETTINGS_FILENAME,
};

#[test]
fn settings_store_never_writes_plaintext_secrets_to_disk_and_rehydrates_on_load() {
    let root = unique_temp_dir("kotoha_settings_sec");
    let _ = fs::remove_dir_all(&root);
    let store = FileSettingsStore::in_dir(&root);
    assert_eq!(store.path(), root.join(SETTINGS_FILENAME));

    let secret_store = InMemorySecretStore::new();

    // Test every secret-bearing TranslationProviderConfig variant
    let cases = [
        (
            "sk-openai-super-secret-token-999",
            TranslationProviderConfig::OpenAiCompatible {
                base_url: "https://api.openai.com/v1".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                api_key: Some("sk-openai-super-secret-token-999".to_owned()),
                timeout_ms: 15_000,
            },
        ),
        (
            "deepl-secret-auth-key-xyz:fx",
            TranslationProviderConfig::DeepL {
                api_key: "deepl-secret-auth-key-xyz:fx".to_owned(),
                use_free_endpoint: true,
                timeout_ms: 10_000,
            },
        ),
        (
            "AIzaSyGoogleCloudSecretKey123",
            TranslationProviderConfig::GoogleCloud {
                api_key: "AIzaSyGoogleCloudSecretKey123".to_owned(),
                project_id: Some("kotoha-gcp-proj".to_owned()),
                timeout_ms: 10_000,
            },
        ),
        (
            "gas-webhook-bearer-secret-456",
            TranslationProviderConfig::GasWebhook {
                endpoint_url: "https://script.google.com/macros/s/demo/exec".to_owned(),
                bearer_token: Some("gas-webhook-bearer-secret-456".to_owned()),
                timeout_ms: 12_000,
            },
        ),
    ];

    for (secret, provider) in cases {
        let settings = AppSettings {
            first_run_completed: true,
            src_lang: LanguageCode::new("en").unwrap(),
            tgt_lang: LanguageCode::new("ja").unwrap(),
            translation_provider: provider,
            glossary: GlossarySettings {
                enabled_profiles: vec!["eve".to_owned()],
                glossary_dir: Some("/tmp/glossaries".to_owned()),
            },
            hotwords_score: 1.75,
            overlay: OverlayStyleSettings {
                opacity: 0.9,
                font_size_px: 22,
                max_lines: 6,
                show_source_text: true,
                fade_out_seconds: 15,
                locked: false,
                position_x: 120,
                position_y: 440,
                width: 820,
                height: 300,
            },
            custom_mumble_plugin_dir: Some("/opt/mumble/plugins".to_owned()),
        };

        store
            .save_sanitized(&settings, &secret_store)
            .expect("save_sanitized should succeed");

        // Verify the JSON file on disk NEVER contains the plaintext secret
        let raw_on_disk = fs::read_to_string(store.path()).expect("read settings JSON");
        assert!(
            !raw_on_disk.contains(secret),
            "plaintext secret leaked into kotoha-settings.json: {raw_on_disk}"
        );

        // Verify Unix 0600 permissions
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "settings file must have 0600 permissions");
        }

        // Verify round-trip re-hydration restores the full original AppSettings including secret
        let reloaded = store
            .load(&secret_store)
            .expect("load should re-hydrate secrets");
        assert_eq!(reloaded, settings);
    }

    // Verify clearing an optional secret removes it from SecretStore as well
    let cleared_openai = AppSettings {
        translation_provider: TranslationProviderConfig::OpenAiCompatible {
            base_url: "http://localhost:11434/v1".to_owned(),
            model: "qwen2.5:7b".to_owned(),
            api_key: None,
            timeout_ms: 15_000,
        },
        ..AppSettings::default()
    };
    store
        .save_sanitized(&cleared_openai, &secret_store)
        .expect("clearing optional secret should succeed");
    assert_eq!(
        secret_store
            .get_secret("translation.openai_compatible.api_key")
            .unwrap(),
        None
    );
    assert_eq!(store.load(&secret_store).unwrap(), cleared_openai);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn settings_store_handles_missing_file_defaults_corrupt_json_and_validation() {
    let root = unique_temp_dir("kotoha_settings_err");
    let _ = fs::remove_dir_all(&root);
    let store = FileSettingsStore::in_dir(&root);
    let secret_store = InMemorySecretStore::new();

    // 1. Missing settings file yields clean AppSettings::default()
    let default_loaded = store
        .load(&secret_store)
        .expect("missing file should return defaults");
    assert_eq!(default_loaded, AppSettings::default());
    assert!(!default_loaded.first_run_completed);
    assert_eq!(default_loaded.src_lang.as_str(), "en");
    assert_eq!(default_loaded.tgt_lang.as_str(), "ja");
    assert!(default_loaded.overlay.locked);

    // 2. Corrupt JSON fails cleanly with SetupError::InvalidConfig
    fs::create_dir_all(&root).unwrap();
    fs::write(store.path(), "{not valid json!!!").unwrap();
    let corrupt_err = store
        .load(&secret_store)
        .expect_err("corrupt JSON must fail cleanly");
    assert!(matches!(corrupt_err, SetupError::InvalidConfig(ref m) if m.contains("corrupted")));

    // 3. Invalid hotwords_score or overlay opacity fails validation before touching disk
    let mut bad_settings = AppSettings {
        hotwords_score: 0.0,
        ..AppSettings::default()
    };
    assert!(matches!(
        store.save_sanitized(&bad_settings, &secret_store),
        Err(SetupError::InvalidConfig(_))
    ));

    bad_settings.hotwords_score = 1.5;
    bad_settings.overlay.opacity = f32::NAN;
    assert!(matches!(
        store.save_sanitized(&bad_settings, &secret_store),
        Err(SetupError::InvalidConfig(_))
    ));

    bad_settings.overlay.opacity = 0.8;
    bad_settings.overlay.width = 0;
    assert!(matches!(
        store.save_sanitized(&bad_settings, &secret_store),
        Err(SetupError::InvalidConfig(_))
    ));

    // 4. SecretStore CRUD works cleanly
    assert!(secret_store.is_empty().unwrap());
    secret_store.set_secret("k1", "v1").unwrap();
    assert_eq!(
        secret_store.get_secret("k1").unwrap().as_deref(),
        Some("v1")
    );
    assert_eq!(secret_store.len().unwrap(), 1);
    secret_store.delete_secret("k1").unwrap();
    assert!(secret_store.is_empty().unwrap());

    let _ = fs::remove_dir_all(&root);
}
