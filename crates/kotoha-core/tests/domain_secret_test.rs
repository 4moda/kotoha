//! Tests for `src/domain/secret.rs` and the secret-free `TranslationProviderConfig`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use kotoha_core::domain::{
    ComputeDevice, CoreError, ProviderSecretKind, Secret, SecretRef, TranslationProviderConfig,
    MAX_SECRET_REF_LEN, REDACTED,
};

const SECRET_VALUE: &str = "sk-live-0123456789-SECRET";

fn all_provider_configs() -> Vec<TranslationProviderConfig> {
    vec![
        TranslationProviderConfig::LocalModel {
            model_id: "fugumt-en-ja-int8".to_owned(),
            device: ComputeDevice::Cpu,
            threads: Some(2),
        },
        TranslationProviderConfig::OpenAiCompatible {
            base_url: "https://api.openai.com/v1".to_owned(),
            model: "gpt-4o-mini".to_owned(),
            timeout_ms: 5_000,
        },
        TranslationProviderConfig::DeepL {
            use_free_endpoint: true,
            timeout_ms: 5_000,
        },
        TranslationProviderConfig::GoogleCloud {
            project_id: Some("proj".to_owned()),
            timeout_ms: 5_000,
        },
        TranslationProviderConfig::GasWebhook { timeout_ms: 5_000 },
        TranslationProviderConfig::Mock {
            prefix: "[ja] ".to_owned(),
        },
    ]
}

fn openai(base_url: &str) -> TranslationProviderConfig {
    TranslationProviderConfig::OpenAiCompatible {
        base_url: base_url.to_owned(),
        model: "m".to_owned(),
        timeout_ms: 5_000,
    }
}

#[test]
fn secret_debug_and_display_are_redacted() {
    let secret = Secret::new(SECRET_VALUE);
    assert_eq!(secret.to_string(), REDACTED);
    assert_eq!(format!("{secret}"), "***");
    assert_eq!(format!("{secret:?}"), "Secret(***)");
    assert_eq!(format!("{secret:#?}"), "Secret(***)");
    assert_eq!(format!("{:?}", Some(&secret)), "Some(Secret(***))");
    assert_eq!(secret.expose(), SECRET_VALUE);
    assert!(!secret.is_blank());
    assert!(Secret::new("  ").is_blank());
    assert_eq!(Secret::from(SECRET_VALUE).expose(), SECRET_VALUE);
    assert_eq!(Secret::from(SECRET_VALUE.to_owned()).expose(), SECRET_VALUE);
}

#[test]
fn secret_ref_grammar_accepts_known_names_and_rejects_the_rest() {
    for kind in ProviderSecretKind::ALL {
        let r = kind.secret_ref();
        assert_eq!(SecretRef::new(r.as_str()).unwrap(), r);
    }
    assert_eq!(
        SecretRef::deepl_api_key().as_str(),
        "translation.deepl.api_key"
    );
    assert_eq!(
        SecretRef::gas_endpoint_url().as_str(),
        "translation.gas_webhook.endpoint_url"
    );

    let too_long = "a".repeat(MAX_SECRET_REF_LEN + 1);
    for bad in [
        "",
        ".",
        "a..b",
        ".a",
        "a.",
        "_a",
        "a_",
        "a__b",
        "a_.b",
        "A.b",
        "a-b",
        "a b",
        "a/b",
        "a.b\n",
        "ａ.b",
        too_long.as_str(),
    ] {
        let err = SecretRef::new(bad).expect_err(bad);
        assert!(matches!(err, CoreError::InvalidSecretRef(_)), "{bad}");
    }
    assert!(SecretRef::new("a".repeat(MAX_SECRET_REF_LEN)).is_ok());
}

#[test]
fn secret_ref_serde_validates_on_deserialize() {
    let r = SecretRef::deepl_api_key();
    assert_eq!(
        serde_json::to_string(&r).unwrap(),
        "\"translation.deepl.api_key\""
    );
    let back: SecretRef = serde_json::from_str("\"translation.deepl.api_key\"").unwrap();
    assert_eq!(back, r);
    assert!(serde_json::from_str::<SecretRef>("\"A-B\"").is_err());
}

#[test]
fn config_debug_and_serialization_never_contain_secret_values_or_refs() {
    for config in all_provider_configs() {
        for rendered in [
            format!("{config:?}"),
            format!("{config:#?}"),
            serde_json::to_string(&config).unwrap(),
        ] {
            assert!(!rendered.contains(SECRET_VALUE), "{rendered}");
            assert!(!rendered.contains("script.google.com"), "{rendered}");
            assert!(!rendered.contains("api_key"), "{rendered}");
            assert!(!rendered.contains("bearer_token"), "{rendered}");
            assert!(!rendered.contains("endpoint_url"), "{rendered}");
            assert!(!rendered.contains("_ref"), "{rendered}");
        }
        let json = serde_json::to_string(&config).unwrap();
        let back: TranslationProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, config);
    }
}

#[test]
fn secret_refs_are_fixed_per_provider_variant() {
    let configs = all_provider_configs();
    let names = |c: &TranslationProviderConfig| -> Vec<String> {
        c.secret_refs().iter().map(ToString::to_string).collect()
    };
    assert!(names(&configs[0]).is_empty());
    assert_eq!(
        names(&configs[1]),
        ["translation.openai_compatible.api_key"]
    );
    assert_eq!(names(&configs[2]), ["translation.deepl.api_key"]);
    assert_eq!(names(&configs[3]), ["translation.google_cloud.api_key"]);
    assert_eq!(
        names(&configs[4]),
        [
            "translation.gas_webhook.endpoint_url",
            "translation.gas_webhook.bearer_token"
        ]
    );
    assert!(names(&configs[5]).is_empty());

    assert!(configs[1].required_secret_refs().is_empty());
    assert_eq!(configs[1].optional_secret_refs().len(), 1);
    assert_eq!(configs[4].required_secret_refs().len(), 1);
}

#[test]
fn provider_secret_kind_applies_only_to_its_own_provider() {
    let configs = all_provider_configs();
    let expected = [
        (ProviderSecretKind::OpenAiApiKey, 1),
        (ProviderSecretKind::DeepLApiKey, 2),
        (ProviderSecretKind::GoogleCloudApiKey, 3),
        (ProviderSecretKind::GasEndpointUrl, 4),
        (ProviderSecretKind::GasBearerToken, 4),
    ];
    for (kind, owner) in expected {
        for (i, config) in configs.iter().enumerate() {
            assert_eq!(
                kind.applies_to(config),
                i == owner,
                "{kind:?} vs {config:?}"
            );
        }
    }
    assert_eq!(
        ProviderSecretKind::GasEndpointUrl.dependent_refs_to_clear(),
        vec![SecretRef::gas_bearer_token()]
    );
    assert!(ProviderSecretKind::DeepLApiKey
        .dependent_refs_to_clear()
        .is_empty());
    assert_eq!(
        serde_json::to_string(&ProviderSecretKind::GasBearerToken).unwrap(),
        "\"gas_bearer_token\""
    );
}

#[test]
fn openai_key_is_invalidated_unless_the_origin_is_unchanged() {
    let key = vec![SecretRef::openai_api_key()];
    let base = openai("https://api.openai.com/v1");

    // Same origin (path / case / credentials differences ignored): key survives.
    assert!(openai("https://API.openai.com/v2/")
        .secrets_invalidated_by_change_from(&base)
        .is_empty());
    assert!(openai("https://api.openai.com")
        .secrets_invalidated_by_change_from(&base)
        .is_empty());

    // Different host, scheme or port: key must be re-entered.
    for next in [
        "https://evil.example/v1",
        "http://api.openai.com/v1",
        "https://api.openai.com:8443/v1",
        "https://api.openai.com@evil.example/v1",
        // WHATWG treats `\` as a path separator, so this connects to evil.example.
        "https://evil.example\\@api.openai.com/v1",
        "https://evil.example\\.api.openai.com/v1",
        "https://user:pw@api.openai.com/v1",
        "https://api.openai.com%2f@evil.example/v1",
        "https://api.openai.com /v1",
        "not a url",
    ] {
        assert_eq!(
            openai(next).secrets_invalidated_by_change_from(&base),
            key,
            "{next}"
        );
    }

    // Coming from another provider: the stale key is dropped.
    for previous in all_provider_configs() {
        if matches!(previous, TranslationProviderConfig::OpenAiCompatible { .. }) {
            continue;
        }
        assert_eq!(base.secrets_invalidated_by_change_from(&previous), key);
    }

    // Other providers never invalidate anything.
    for config in all_provider_configs() {
        if !matches!(config, TranslationProviderConfig::OpenAiCompatible { .. }) {
            assert!(config.secrets_invalidated_by_change_from(&base).is_empty());
        }
    }
}

#[test]
fn url_origin_fails_closed_on_ambiguous_authorities() {
    use kotoha_core::domain::url_origin;

    assert_eq!(
        url_origin("HTTPS://API.OpenAI.com:8443/v1?x#y").as_deref(),
        Some("https://api.openai.com:8443")
    );
    assert_eq!(
        url_origin("http://[::1]:11434/v1").as_deref(),
        Some("http://[::1]:11434")
    );
    for ambiguous in [
        "https://evil.example\\@api.openai.com/v1",
        "https://a@b/",
        "https://a%40b/",
        "https://a b/",
        "https://a\tb/",
        "https:///path",
        "localhost:11434/v1",
        "",
    ] {
        assert_eq!(url_origin(ambiguous), None, "{ambiguous:?}");
    }
}

#[test]
fn unchanged_but_unparseable_base_url_keeps_the_key() {
    // Nothing changed, so there is no new destination to protect against.
    let same = "http://localhost:11434\\v1";
    assert!(openai(same)
        .secrets_invalidated_by_change_from(&openai(same))
        .is_empty());
}

#[test]
fn provider_and_secret_kind_wire_tags_match_the_ui() {
    // Keep in sync with app/ui/src/types/kotoha.ts (and its contract test).
    let tags = [
        (
            r#"{"type":"local_model","model_id":"m","device":"auto","threads":null}"#,
            "local_model",
        ),
        (
            r#"{"type":"openai_compatible","base_url":"u","model":"m","timeout_ms":1}"#,
            "openai_compatible",
        ),
        (
            r#"{"type":"deepl","use_free_endpoint":true,"timeout_ms":1}"#,
            "deepl",
        ),
        (
            r#"{"type":"google_cloud","project_id":null,"timeout_ms":1}"#,
            "google_cloud",
        ),
        (r#"{"type":"gas_webhook","timeout_ms":1}"#, "gas_webhook"),
        (r#"{"type":"mock","prefix":"p"}"#, "mock"),
    ];
    for (json, tag) in tags {
        let config: TranslationProviderConfig = serde_json::from_str(json).expect(json);
        let value = serde_json::to_value(&config).unwrap();
        assert_eq!(value["type"], tag);
    }
    // Legacy tags written by earlier builds still load.
    for legacy in [
        r#"{"type":"open_ai_compatible","base_url":"u","model":"m","timeout_ms":1}"#,
        r#"{"type":"deep_l","use_free_endpoint":true,"timeout_ms":1}"#,
    ] {
        serde_json::from_str::<TranslationProviderConfig>(legacy).expect(legacy);
    }

    let kinds = [
        (ProviderSecretKind::OpenAiApiKey, "open_ai_api_key"),
        (ProviderSecretKind::DeepLApiKey, "deep_l_api_key"),
        (
            ProviderSecretKind::GoogleCloudApiKey,
            "google_cloud_api_key",
        ),
        (ProviderSecretKind::GasEndpointUrl, "gas_endpoint_url"),
        (ProviderSecretKind::GasBearerToken, "gas_bearer_token"),
    ];
    assert_eq!(kinds.len(), ProviderSecretKind::ALL.len());
    for (kind, tag) in kinds {
        assert_eq!(serde_json::to_value(kind).unwrap(), tag);
    }
}
