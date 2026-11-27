//! Integration tests for `kotoha-translate-http` shared HTTP client & factory (`src/client.rs` & `src/factory.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::time::Duration;

use common::{sample_request, MockHttpServer, ScriptedHttpResponse};
use kotoha_core::domain::{CoreError, TranslationProviderConfig, Translator, TranslatorFactory};
use kotoha_translate_http::{
    DeepLConfig, DeepLTranslator, GasWebhookConfig, GasWebhookTranslator, GoogleCloudConfig,
    GoogleCloudTranslator, HttpTranslatorFactory, OpenAiCompatibleConfig,
    OpenAiCompatibleTranslator, MAX_HTTP_RESPONSE_BYTES,
};

#[tokio::test]
async fn http_translators_block_redirects_on_api_key_endpoints_bound_response_size_and_redact_secrets(
) {
    let oversized_body = "x".repeat(MAX_HTTP_RESPONSE_BYTES + 128);
    let server = MockHttpServer::start(move |req| match req.target.as_str() {
        "/rate-limit/v1/chat/completions" => ScriptedHttpResponse::json_status(
            "429 Too Many Requests",
            r#"{"error":{"message":"slow down"}}"#,
        ),
        "/server-err/v1/chat/completions" => {
            ScriptedHttpResponse::json_status("500 Internal Server Error", r#"{"error":"boom"}"#)
        }
        "/bad-json/v1/chat/completions" => ScriptedHttpResponse::json_ok("{not valid json"),
        "/slow/v1/chat/completions" => ScriptedHttpResponse {
            status_line: "200 OK",
            headers: vec![("Content-Type", "application/json".to_owned())],
            body: r#"{"choices":[{"message":{"content":"late"}}]}"#.to_owned(),
            delay: Duration::from_millis(250),
        },
        "/oversized/v1/chat/completions" => ScriptedHttpResponse::json_ok(oversized_body.clone()),
        "/redirect-api/v1/chat/completions" | "/redirect-api/v2/translate" => {
            ScriptedHttpResponse::redirect_302("http://127.0.0.1:9/stolen-key".to_owned())
        }
        _ => ScriptedHttpResponse::json_status("404 Not Found", "{}"),
    })
    .await;

    let make = |prefix: &str, timeout_ms: u64| {
        OpenAiCompatibleTranslator::new(&OpenAiCompatibleConfig {
            base_url: format!("{}/{prefix}/v1", server.base_url),
            model: "test-model".to_owned(),
            api_key: None,
            timeout_ms,
        })
        .unwrap()
    };

    let err_429 = make("rate-limit", 5_000)
        .translate(sample_request("hi"))
        .await
        .expect_err("429");
    assert!(
        matches!(err_429, CoreError::Translation(ref m) if m.contains("429") && m.contains("slow down"))
    );

    let err_500 = make("server-err", 5_000)
        .translate(sample_request("hi"))
        .await
        .expect_err("500");
    assert!(
        matches!(err_500, CoreError::Translation(ref m) if m.contains("500") && m.contains("boom"))
    );

    let err_json = make("bad-json", 5_000)
        .translate(sample_request("hi"))
        .await
        .expect_err("bad json");
    assert!(matches!(err_json, CoreError::Translation(ref m) if m.contains("invalid JSON")));

    let err_timeout = make("slow", 120)
        .translate(sample_request("hi"))
        .await
        .expect_err("timeout");
    assert!(matches!(err_timeout, CoreError::Translation(ref m) if m.contains("timed out")));

    // [C1] Oversized > 1 MiB response rejected
    let err_oversized = make("oversized", 5_000)
        .translate(sample_request("hi"))
        .await
        .expect_err("oversized body > 1 MiB must fail");
    assert!(
        matches!(err_oversized, CoreError::Translation(ref m) if m.contains("exceeds maximum limit"))
    );

    // [C2] API-key endpoints MUST NOT follow 302 redirects (non-2xx 302 fails without second hop)
    let deepl_redir = DeepLTranslator::new(&DeepLConfig {
        api_key: "super-secret-dl".to_owned(),
        use_free_endpoint: true,
        base_url: Some(format!("{}/redirect-api", server.base_url)),
        timeout_ms: 5_000,
    })
    .unwrap();
    let redir_err = deepl_redir
        .translate(sample_request("hi"))
        .await
        .expect_err("DeepL must not follow 302 redirect");
    assert!(matches!(redir_err, CoreError::Translation(ref m) if m.contains("302")));

    // [M6] Timeout bounds, URL sanitization, and Debug secret redaction
    assert!(matches!(
        OpenAiCompatibleTranslator::new(&OpenAiCompatibleConfig {
            base_url: "https://api.openai.com/v1".to_owned(),
            model: "gpt-4o-mini".to_owned(),
            api_key: None,
            timeout_ms: 99,
        }),
        Err(CoreError::NotConfigured(_))
    ));
    assert!(matches!(
        OpenAiCompatibleTranslator::new(&OpenAiCompatibleConfig {
            base_url: "https://api.openai.com/v1".to_owned(),
            model: "gpt-4o-mini".to_owned(),
            api_key: None,
            timeout_ms: 120_001,
        }),
        Err(CoreError::NotConfigured(_))
    ));
    assert!(matches!(
        OpenAiCompatibleTranslator::new(&OpenAiCompatibleConfig {
            base_url: "http://api.openai.com/v1".to_owned(),
            model: "gpt-4o-mini".to_owned(),
            api_key: Some("sk-secret".to_owned()),
            timeout_ms: 5_000,
        }),
        Err(CoreError::NotConfigured(_))
    ));
    assert!(matches!(
        GasWebhookTranslator::new(&GasWebhookConfig::new(
            "https://user:pass@script.google.com/macros/s/1/exec",
            None
        )),
        Err(CoreError::NotConfigured(_))
    ));
    assert!(matches!(
        GasWebhookTranslator::new(&GasWebhookConfig::new(
            "http://169.254.169.254/latest/meta-data",
            None
        )),
        Err(CoreError::NotConfigured(_))
    ));

    let dl_cfg = DeepLConfig::new("ultra-secret-deepl", true);
    let dl_t = DeepLTranslator::new(&dl_cfg).unwrap();
    assert!(!format!("{dl_cfg:?}").contains("ultra-secret-deepl"));
    assert!(!format!("{dl_t:?}").contains("ultra-secret-deepl"));

    let gc_cfg = GoogleCloudConfig::new("ultra-secret-gc", None);
    let gc_t = GoogleCloudTranslator::new(&gc_cfg).unwrap();
    assert!(!format!("{gc_cfg:?}").contains("ultra-secret-gc"));
    assert!(!format!("{gc_t:?}").contains("ultra-secret-gc"));

    let gas_cfg = GasWebhookConfig::new(
        "https://script.google.com/macros/s/1/exec",
        Some("ultra-secret-gas".to_owned()),
    );
    let gas_t = GasWebhookTranslator::new(&gas_cfg).unwrap();
    assert!(!format!("{gas_cfg:?}").contains("ultra-secret-gas"));
    assert!(!format!("{gas_t:?}").contains("ultra-secret-gas"));

    // Factory constructs all HTTP providers and rejects invalid config cleanly
    let factory = HttpTranslatorFactory::new()
        .with_deepl_base_url(&server.base_url)
        .with_google_cloud_base_url(&server.base_url);
    assert!(factory
        .create(&TranslationProviderConfig::Mock {
            prefix: "[ja] ".to_owned(),
        })
        .is_ok());
    assert!(factory
        .create(&TranslationProviderConfig::OpenAiCompatible {
            base_url: format!("{}/rate-limit/v1", server.base_url),
            model: "gpt-4o-mini".to_owned(),
            api_key: None,
            timeout_ms: 2_000,
        })
        .is_ok());
    assert!(factory
        .create(&TranslationProviderConfig::DeepL {
            api_key: "key".to_owned(),
            use_free_endpoint: true,
            timeout_ms: 2_000,
        })
        .is_ok());
    assert!(factory
        .create(&TranslationProviderConfig::GoogleCloud {
            api_key: "key".to_owned(),
            project_id: None,
            timeout_ms: 2_000,
        })
        .is_ok());
    assert!(factory
        .create(&TranslationProviderConfig::GasWebhook {
            endpoint_url: format!("{}/macros/s/abc/exec", server.base_url),
            bearer_token: None,
            timeout_ms: 2_000,
        })
        .is_ok());
    assert!(matches!(
        factory.create(&TranslationProviderConfig::DeepL {
            api_key: String::new(),
            use_free_endpoint: true,
            timeout_ms: 2_000,
        }),
        Err(CoreError::NotConfigured(_))
    ));
}
