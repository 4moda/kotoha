//! Integration tests for `kotoha-translate-http` GAS Webhook translator (`src/gas_webhook.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use common::{sample_request, sample_request_langs, MockHttpServer, ScriptedHttpResponse};
use kotoha_core::domain::{CoreError, Translator};
use kotoha_translate_http::{GasWebhookConfig, GasWebhookTranslator};

#[tokio::test]
async fn gas_webhook_follows_302_redirect_handles_auto_and_blocks_ssrf_redirects() {
    let server = MockHttpServer::start(|req| match (req.method.as_str(), req.target.as_str()) {
        ("POST", "/macros/s/abc/exec") => {
            let parsed: serde_json::Value = serde_json::from_str(&req.body).expect("gas post json");
            let text = parsed["text"].as_str().unwrap();
            let encoded_len = text.len();
            ScriptedHttpResponse::redirect_302(format!("/googleusercontent/echo?len={encoded_len}"))
        }
        ("GET", target) if target.starts_with("/googleusercontent/echo") => {
            ScriptedHttpResponse::json_ok(
                serde_json::json!({
                    "translatedText": "GAS: __TERM_0__ と __TERM_1__"
                })
                .to_string(),
            )
        }
        ("POST", "/macros/s/err/exec") => ScriptedHttpResponse::json_ok(
            serde_json::json!({
                "error": "LanguageApp quota exceeded"
            })
            .to_string(),
        ),
        ("POST", "/macros/s/ssrf-metadata/exec") => {
            ScriptedHttpResponse::redirect_302("http://169.254.169.254/latest/meta-data".to_owned())
        }
        _ => ScriptedHttpResponse::json_status("404 Not Found", "{}"),
    })
    .await;

    let gas = GasWebhookTranslator::new(&GasWebhookConfig {
        endpoint_url: format!("{}/macros/s/abc/exec", server.base_url),
        bearer_token: Some("gas-bearer-tok".to_owned()),
        timeout_ms: 5_000,
    })
    .unwrap();
    assert_eq!(gas.provider_id(), "gas-webhook");

    let translation = gas
        .translate(sample_request("Ferox with M W D"))
        .await
        .expect("gas 302 redirect flow succeeds");
    assert_eq!(translation.translated_text, "GAS: Ferox と MWD");

    // Verify src_lang="auto" sends "" and tgt_lang="auto" is rejected
    let auto_translation = gas
        .translate(sample_request_langs("Ferox with M W D", "auto", "ja"))
        .await
        .expect("gas src_lang=auto maps to empty string");
    assert_eq!(auto_translation.translated_text, "GAS: Ferox と MWD");

    let tgt_auto_err = gas
        .translate(sample_request_langs("Hello", "en", "auto"))
        .await
        .expect_err("gas tgt_lang=auto must fail");
    assert!(matches!(tgt_auto_err, CoreError::InvalidLanguageCode(_)));

    let captured = server.requests();
    assert_eq!(captured.len(), 4);
    assert_eq!(captured[0].method, "POST");
    assert_eq!(captured[0].target, "/macros/s/abc/exec");
    assert_eq!(
        captured[0].headers.get("authorization").map(String::as_str),
        Some("Bearer gas-bearer-tok")
    );
    assert_eq!(captured[1].method, "GET");
    assert!(captured[1]
        .target
        .starts_with("/googleusercontent/echo?len="));
    let auto_body: serde_json::Value = serde_json::from_str(&captured[2].body).unwrap();
    assert_eq!(auto_body["source"], "");
    assert_eq!(auto_body["target"], "ja");

    // Verify JSON {"error": "..."} payload on 200 OK maps cleanly to CoreError::Translation
    let gas_err = GasWebhookTranslator::new(&GasWebhookConfig {
        endpoint_url: format!("{}/macros/s/err/exec", server.base_url),
        bearer_token: None,
        timeout_ms: 5_000,
    })
    .unwrap();
    let err = gas_err
        .translate(sample_request("Hello"))
        .await
        .expect_err("gas error payload must fail");
    assert!(
        matches!(err, CoreError::Translation(ref msg) if msg.contains("LanguageApp quota exceeded")),
        "unexpected error: {err:?}"
    );

    // Verify redirect to cloud metadata link-local IP is blocked cleanly
    let ssrf_gas = GasWebhookTranslator::new(&GasWebhookConfig {
        endpoint_url: format!("{}/macros/s/ssrf-metadata/exec", server.base_url),
        bearer_token: None,
        timeout_ms: 5_000,
    })
    .unwrap();
    let ssrf_err = ssrf_gas
        .translate(sample_request("Hello"))
        .await
        .expect_err("link-local redirect must be blocked");
    assert!(matches!(ssrf_err, CoreError::Translation(_)));
}
