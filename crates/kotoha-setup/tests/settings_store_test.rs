//! Unit tests for `kotoha-setup` settings persistence (`src/settings_store.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::fs;

use common::unique_temp_dir;
use kotoha_core::domain::{
    CoreError, GlossarySettings, LanguageCode, ProviderSecretKind, SecretRef, SecretStore,
    TranslationProviderConfig,
};
use kotoha_core::infrastructure::InMemorySecretStore;
use kotoha_setup::{
    AppSettings, FileSettingsStore, OverlayStyleSettings, SetupError, SETTINGS_FILENAME,
};

fn settings_with(provider: TranslationProviderConfig) -> AppSettings {
    AppSettings {
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
    }
}

#[test]
fn settings_round_trip_as_is_and_file_contains_no_secret_values() {
    let root = unique_temp_dir("kotoha_settings_sec");
    let _ = fs::remove_dir_all(&root);
    let store = FileSettingsStore::in_dir(&root);
    assert_eq!(store.path(), root.join(SETTINGS_FILENAME));

    // The secret store holds a value for every kind; none may ever reach the settings file.
    let secrets = InMemorySecretStore::new()
        .with(
            &SecretRef::openai_api_key(),
            "sk-openai-super-secret-token-999",
        )
        .with(&SecretRef::deepl_api_key(), "deepl-secret-auth-key-xyz:fx")
        .with(
            &SecretRef::google_cloud_api_key(),
            "AIzaSyGoogleCloudSecretKey123",
        )
        .with(
            &SecretRef::gas_endpoint_url(),
            "https://script.google.com/macros/s/CAPABILITY-ID/exec",
        )
        .with(
            &SecretRef::gas_bearer_token(),
            "gas-webhook-bearer-secret-456",
        );

    let providers = [
        TranslationProviderConfig::OpenAiCompatible {
            base_url: "https://api.openai.com/v1".to_owned(),
            model: "gpt-4o-mini".to_owned(),
            timeout_ms: 15_000,
        },
        TranslationProviderConfig::DeepL {
            use_free_endpoint: true,
            timeout_ms: 10_000,
        },
        TranslationProviderConfig::GoogleCloud {
            project_id: Some("kotoha-gcp-proj".to_owned()),
            timeout_ms: 10_000,
        },
        TranslationProviderConfig::GasWebhook { timeout_ms: 12_000 },
    ];

    for provider in providers {
        let settings = settings_with(provider);
        store.save(&settings).expect("save should succeed");

        let raw_on_disk = fs::read_to_string(store.path()).expect("read settings JSON");
        for needle in [
            "sk-openai-super-secret",
            "deepl-secret-auth-key",
            "AIzaSyGoogle",
            "CAPABILITY-ID",
            "script.google.com",
            "gas-webhook-bearer",
            "api_key",
            "bearer_token",
            "endpoint_url",
        ] {
            assert!(
                !raw_on_disk.contains(needle),
                "'{needle}' leaked into kotoha-settings.json: {raw_on_disk}"
            );
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "settings file must have 0600 permissions");
        }

        // Settings serialize as-is and loading does not touch the secret store.
        assert_eq!(store.load(&secrets).expect("load"), settings);
    }
    for kind in ProviderSecretKind::ALL {
        assert!(secrets.is_configured(&kind.secret_ref()));
    }

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn legacy_settings_with_plaintext_secrets_are_migrated_into_the_secret_store() {
    let root = unique_temp_dir("kotoha_settings_legacy");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let store = FileSettingsStore::in_dir(&root);

    let base = serde_json::to_value(settings_with(TranslationProviderConfig::Mock {
        prefix: "x".to_owned(),
    }))
    .unwrap();

    let legacy_providers = [
        (
            serde_json::json!({
                "type": "open_ai_compatible", "base_url": "https://api.openai.com/v1",
                "model": "gpt-4o-mini", "api_key": "sk-legacy-openai", "timeout_ms": 9000
            }),
            vec![(ProviderSecretKind::OpenAiApiKey, "sk-legacy-openai")],
            TranslationProviderConfig::OpenAiCompatible {
                base_url: "https://api.openai.com/v1".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                timeout_ms: 9000,
            },
        ),
        (
            serde_json::json!({
                "type": "deep_l", "api_key": "legacy-deepl:fx",
                "use_free_endpoint": true, "timeout_ms": 8000
            }),
            vec![(ProviderSecretKind::DeepLApiKey, "legacy-deepl:fx")],
            TranslationProviderConfig::DeepL {
                use_free_endpoint: true,
                timeout_ms: 8000,
            },
        ),
        (
            serde_json::json!({
                "type": "google_cloud", "api_key": "__KOTOHA_SECRET_STORE__",
                "project_id": "p", "timeout_ms": 7000
            }),
            vec![],
            TranslationProviderConfig::GoogleCloud {
                project_id: Some("p".to_owned()),
                timeout_ms: 7000,
            },
        ),
        (
            serde_json::json!({
                "type": "gas_webhook",
                "endpoint_url": "https://script.google.com/macros/s/LEGACY-CAP/exec",
                "bearer_token": null, "timeout_ms": 6000
            }),
            vec![(
                ProviderSecretKind::GasEndpointUrl,
                "https://script.google.com/macros/s/LEGACY-CAP/exec",
            )],
            TranslationProviderConfig::GasWebhook { timeout_ms: 6000 },
        ),
    ];

    for (legacy_provider, expected_secrets, expected_config) in legacy_providers {
        let mut doc = base.clone();
        doc["translation_provider"] = legacy_provider;
        fs::write(store.path(), serde_json::to_string_pretty(&doc).unwrap()).unwrap();

        let secrets = InMemorySecretStore::new();
        let loaded = store.load(&secrets).expect("legacy file must load");
        assert_eq!(loaded.translation_provider, expected_config);

        // Plaintext values moved into the store (placeholders / null dropped).
        for kind in ProviderSecretKind::ALL {
            let expected = expected_secrets.iter().find(|(k, _)| *k == kind);
            match expected {
                Some((_, value)) => {
                    assert_eq!(
                        secrets.resolve(&kind.secret_ref()).unwrap().expose(),
                        *value
                    );
                }
                None => assert!(!secrets.is_configured(&kind.secret_ref()), "{kind:?}"),
            }
        }

        // File rewritten in the new shape: no plaintext, no legacy field names.
        let rewritten = fs::read_to_string(store.path()).unwrap();
        for needle in [
            "sk-legacy",
            "legacy-deepl",
            "LEGACY-CAP",
            "__KOTOHA_SECRET_STORE__",
            "api_key",
            "endpoint_url",
            "bearer_token",
        ] {
            assert!(!rewritten.contains(needle), "{needle} in {rewritten}");
        }
        // Idempotent: a second load changes nothing and does not need the secrets again.
        assert_eq!(store.load(&InMemorySecretStore::new()).unwrap(), loaded);
    }

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn settings_store_handles_missing_file_defaults_corrupt_json_and_validation() {
    let root = unique_temp_dir("kotoha_settings_err");
    let _ = fs::remove_dir_all(&root);
    let store = FileSettingsStore::in_dir(&root);
    let secrets = InMemorySecretStore::new();

    // 1. Missing settings file yields clean AppSettings::default()
    let default_loaded = store
        .load(&secrets)
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
        .load(&secrets)
        .expect_err("corrupt JSON must fail cleanly");
    assert!(matches!(corrupt_err, SetupError::InvalidConfig(ref m) if m.contains("corrupted")));

    // 3. Invalid hotwords_score or overlay opacity fails validation before touching disk
    let mut bad_settings = AppSettings {
        hotwords_score: 0.0,
        ..AppSettings::default()
    };
    assert!(matches!(
        store.save(&bad_settings),
        Err(SetupError::InvalidConfig(_))
    ));

    bad_settings.hotwords_score = 1.5;
    bad_settings.overlay.opacity = f32::NAN;
    assert!(matches!(
        store.save(&bad_settings),
        Err(SetupError::InvalidConfig(_))
    ));

    bad_settings.overlay.opacity = 0.8;
    bad_settings.overlay.width = 0;
    assert!(matches!(
        store.save(&bad_settings),
        Err(SetupError::InvalidConfig(_))
    ));

    // 4. CoreError converts into SetupError without echoing secrets
    let converted: SetupError =
        CoreError::SecretMissing("translation.deepl.api_key".to_owned()).into();
    assert!(matches!(converted, SetupError::Core(_)));
    assert!(converted.to_string().contains("translation.deepl.api_key"));

    let _ = fs::remove_dir_all(&root);
}
