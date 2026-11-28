//! Tests for `src/factory.rs`: fixed-ref secret resolution via `SecretStore` and leak-free
//! diagnostics (including the "confused deputy" guard).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::sync::Arc;

use common::{sample_request, MockHttpServer, ScriptedHttpResponse};
use kotoha_core::domain::{
    CoreError, ProviderSecretKind, SecretRef, TranslationProviderConfig, Translator,
    TranslatorFactory,
};
use kotoha_core::infrastructure::InMemorySecretStore;
use kotoha_translate_http::{GasWebhookConfig, GasWebhookTranslator, HttpTranslatorFactory};

const GAS_URL: &str = "https://script.google.com/macros/s/AKfycbx-CAPABILITY-ID/exec";
const DEEPL_KEY: &str = "deepl-super-secret";
const GC_KEY: &str = "gc-super-secret";
const OPENAI_KEY: &str = "sk-openai-super-secret";
const TOKEN: &str = "gas-bearer-super-secret";

fn store_with(skip: Option<ProviderSecretKind>) -> Arc<InMemorySecretStore> {
    let mut store = InMemorySecretStore::new();
    for (kind, value) in [
        (ProviderSecretKind::OpenAiApiKey, OPENAI_KEY),
        (ProviderSecretKind::DeepLApiKey, DEEPL_KEY),
        (ProviderSecretKind::GoogleCloudApiKey, GC_KEY),
        (ProviderSecretKind::GasEndpointUrl, GAS_URL),
        (ProviderSecretKind::GasBearerToken, TOKEN),
    ] {
        if Some(kind) != skip {
            store = store.with(&kind.secret_ref(), value);
        }
    }
    Arc::new(store)
}

fn configs() -> Vec<TranslationProviderConfig> {
    vec![
        TranslationProviderConfig::OpenAiCompatible {
            base_url: "https://api.openai.com/v1".to_owned(),
            model: "gpt-4o-mini".to_owned(),
            timeout_ms: 2_000,
        },
        TranslationProviderConfig::DeepL {
            use_free_endpoint: true,
            timeout_ms: 2_000,
        },
        TranslationProviderConfig::GoogleCloud {
            project_id: None,
            timeout_ms: 2_000,
        },
        TranslationProviderConfig::GasWebhook { timeout_ms: 2_000 },
    ]
}

fn assert_no_secrets(text: &str) {
    for needle in [
        DEEPL_KEY,
        GC_KEY,
        OPENAI_KEY,
        TOKEN,
        "CAPABILITY-ID",
        "/macros/s/",
    ] {
        assert!(
            !text.contains(needle),
            "a secret value leaked into the error text"
        );
    }
}

#[test]
fn factory_resolves_fixed_secrets_for_every_provider() {
    let factory = HttpTranslatorFactory::with_secret_store(store_with(None));
    for config in configs() {
        let translator = factory.create(&config).expect("create translator");
        assert_no_secrets(&format!("{config:?}"));
        assert_no_secrets(translator.provider_id());
    }
    assert_no_secrets(&format!("{factory:?}"));
}

#[test]
fn missing_required_secret_errors_and_optional_secret_is_skipped() {
    for (skipped, provider_index) in [
        (ProviderSecretKind::DeepLApiKey, 1),
        (ProviderSecretKind::GoogleCloudApiKey, 2),
        (ProviderSecretKind::GasEndpointUrl, 3),
    ] {
        let factory = HttpTranslatorFactory::with_secret_store(store_with(Some(skipped)));
        let all = configs();
        let failures: Vec<(usize, CoreError)> = all
            .iter()
            .enumerate()
            .filter_map(|(i, c)| factory.create(c).err().map(|e| (i, e)))
            .collect();
        assert_eq!(failures.len(), 1, "only the owning provider fails");
        let (idx, err) = &failures[0];
        assert_eq!(*idx, provider_index);
        assert!(matches!(err, CoreError::SecretMissing(_)));
        let msg = err.to_string();
        assert!(
            msg.contains(skipped.secret_ref().as_str()),
            "missing-secret error must name the skipped secret ref"
        );
        assert_no_secrets(&msg);
    }

    // OpenAI-compatible key and GAS bearer token are optional.
    for skipped in [
        ProviderSecretKind::OpenAiApiKey,
        ProviderSecretKind::GasBearerToken,
    ] {
        let factory = HttpTranslatorFactory::with_secret_store(store_with(Some(skipped)));
        for config in configs() {
            factory
                .create(&config)
                .expect("optional secret may be absent");
        }
    }
}

#[tokio::test]
async fn provider_never_sends_another_providers_secret_to_its_host() {
    // Confused-deputy guard: an OpenAI-compatible config pointing at an attacker-controlled host
    // can only ever carry the OpenAI key slot. Even when the DeepL / Google / GAS secrets are
    // configured, none of them reaches that host.
    let server = MockHttpServer::start(|_req| {
        ScriptedHttpResponse::json_ok(r#"{"choices":[{"message":{"content":"x"}}]}"#)
    })
    .await;

    let store = Arc::new(
        InMemorySecretStore::new()
            .with(&SecretRef::deepl_api_key(), DEEPL_KEY)
            .with(&SecretRef::google_cloud_api_key(), GC_KEY)
            .with(&SecretRef::gas_bearer_token(), TOKEN),
    );
    let factory = HttpTranslatorFactory::with_secret_store(store);
    let translator = factory
        .create(&TranslationProviderConfig::OpenAiCompatible {
            base_url: format!("{}/v1", server.base_url),
            model: "m".to_owned(),
            timeout_ms: 2_000,
        })
        .unwrap();
    let _ = translator.translate(sample_request("hello")).await;

    let recorded = server.requests();
    assert!(!recorded.is_empty());
    for request in recorded {
        let dump = format!("{request:?}");
        for secret in [DEEPL_KEY, GC_KEY, TOKEN] {
            assert!(!dump.contains(secret), "{dump}");
        }
        assert!(!request.headers.contains_key("authorization"), "{dump}");
    }
}

#[test]
fn gas_construction_errors_never_contain_the_endpoint_url_path() {
    let secret_path = "/macros/s/AKfycbx-CAPABILITY-ID/exec";
    let bad_urls = [
        format!("https://user:pass@script.google.com{secret_path}"),
        format!("https://script.google.com{secret_path}?token=abc"),
        format!("https://script.google.com{secret_path}#frag"),
        format!("http://169.254.169.254{secret_path}"),
        format!("ftp://script.google.com{secret_path}"),
        format!("http://example.com{secret_path}"),
        format!("not a url {secret_path}"),
    ];
    for url in bad_urls {
        let cfg = GasWebhookConfig::new(url, Some(TOKEN.into()));
        let err = GasWebhookTranslator::new(&cfg).expect_err("must be rejected");
        assert_no_secrets(&err.to_string());
        assert_no_secrets(&format!("{cfg:?}"));
    }

    let ok_cfg = GasWebhookConfig::new(GAS_URL, Some(TOKEN.into()));
    let translator = GasWebhookTranslator::new(&ok_cfg).unwrap();
    assert_no_secrets(&format!("{translator:?}"));
    assert_no_secrets(&format!("{ok_cfg:?}"));
}

#[tokio::test]
async fn gas_transport_errors_never_contain_the_endpoint_url_path() {
    let url = "http://127.0.0.1:1/macros/s/AKfycbx-CAPABILITY-ID/exec";
    let translator = GasWebhookTranslator::new(&GasWebhookConfig::new(url, None)).unwrap();
    let err = translator
        .translate(sample_request("hello"))
        .await
        .expect_err("connection must fail");
    assert_no_secrets(&err.to_string());
}

#[tokio::test]
async fn ambiguous_urls_never_reach_a_host_with_the_stored_key() {
    // `\` ends the authority for the WHATWG parser the HTTP client uses, so these URLs would
    // connect to the local mock server while naming another host after the `@`. They are
    // rejected at construction, so the stored key can never be sent.
    let server = MockHttpServer::start(|_req| {
        ScriptedHttpResponse::json_ok(r#"{"choices":[{"message":{"content":"x"}}]}"#)
    })
    .await;
    let store = Arc::new(
        InMemorySecretStore::new().with(&SecretRef::openai_api_key(), "sk-openai-never-leak"),
    );
    let factory = HttpTranslatorFactory::with_secret_store(store);
    for base_url in [
        format!("{}\\@api.openai.com/v1", server.base_url),
        format!(
            "{}@api.openai.com/v1",
            server.base_url.replace("http://", "http://user:pw")
        ),
        format!("{}/v1/@x", server.base_url),
    ] {
        let result = factory.create(&TranslationProviderConfig::OpenAiCompatible {
            base_url: base_url.clone(),
            model: "m".to_owned(),
            timeout_ms: 2_000,
        });
        assert!(result.is_err(), "{base_url}");
    }
    assert!(server.requests().is_empty());
}
