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

#[tokio::test]
async fn gas_webhook_redirect_limit_is_per_request_not_per_client() {
    // Google Apps Script answers every POST with a 302; the hop budget must reset per request.
    let server = MockHttpServer::start(|req| match (req.method.as_str(), req.target.as_str()) {
        ("POST", "/macros/s/abc/exec") => ScriptedHttpResponse::redirect_302("/final".to_owned()),
        ("GET", "/final") => ScriptedHttpResponse::json_ok(
            serde_json::json!({ "translatedText": "GAS: __TERM_0__ と __TERM_1__" }).to_string(),
        ),
        _ => ScriptedHttpResponse::json_status("404 Not Found", "{}"),
    })
    .await;

    let gas = GasWebhookTranslator::new(&GasWebhookConfig {
        endpoint_url: format!("{}/macros/s/abc/exec", server.base_url),
        bearer_token: None,
        timeout_ms: 5_000,
    })
    .unwrap();

    for call in 0..12 {
        let translation = gas
            .translate(sample_request("Ferox with M W D"))
            .await
            .unwrap_or_else(|err| panic!("call {call} must succeed: {err:?}"));
        assert_eq!(translation.translated_text, "GAS: Ferox と MWD");
    }
    assert_eq!(server.requests().len(), 24);
}

#[tokio::test]
async fn gas_webhook_rejects_more_than_max_redirects_within_a_single_request() {
    let server = MockHttpServer::start(|req| {
        let target = req.target.as_str();
        match (req.method.as_str(), target) {
            ("POST", "/macros/s/ok/exec") => {
                ScriptedHttpResponse::redirect_302("/chain5/1".to_owned())
            }
            ("POST", "/macros/s/loop/exec") => {
                ScriptedHttpResponse::redirect_302("/hop/1".to_owned())
            }
            ("GET", t) if t.starts_with("/chain5/") => {
                let n: usize = t["/chain5/".len()..].parse().unwrap();
                if n < 5 {
                    ScriptedHttpResponse::redirect_302(format!("/chain5/{}", n + 1))
                } else {
                    ScriptedHttpResponse::json_ok(
                        serde_json::json!({ "translatedText": "ok" }).to_string(),
                    )
                }
            }
            ("GET", t) if t.starts_with("/hop/") => {
                let n: usize = t["/hop/".len()..].parse().unwrap();
                ScriptedHttpResponse::redirect_302(format!("/hop/{}", n + 1))
            }
            _ => ScriptedHttpResponse::json_status("404 Not Found", "{}"),
        }
    })
    .await;

    let make = |path: &str| {
        GasWebhookTranslator::new(&GasWebhookConfig {
            endpoint_url: format!("{}{path}", server.base_url),
            bearer_token: None,
            timeout_ms: 5_000,
        })
        .unwrap()
    };

    // Exactly 5 redirects (the configured maximum) in a single request is still accepted.
    make("/macros/s/ok/exec")
        .translate(sample_request("Hello"))
        .await
        .expect("5 redirects in one request are allowed");

    // An endless redirect chain is cut off after the limit.
    let err = make("/macros/s/loop/exec")
        .translate(sample_request("Hello"))
        .await
        .expect_err("redirect chain beyond max_hops must be rejected");
    assert!(
        matches!(err, CoreError::Translation(ref msg) if msg.contains("redirect blocked")),
        "unexpected error: {err:?}"
    );
}
