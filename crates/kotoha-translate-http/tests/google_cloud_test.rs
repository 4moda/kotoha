//! Integration tests for `kotoha-translate-http` Google Cloud translator (`src/google_cloud.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{sample_request, MockHttpServer, ScriptedHttpResponse};
use kotoha_core::domain::Translator;
use kotoha_translate_http::{GoogleCloudConfig, GoogleCloudTranslator, GOOGLE_CLOUD_V2_ENDPOINT};

#[tokio::test]
async fn google_cloud_uses_x_goog_api_key_header_never_query_string() {
    let default_gc =
        GoogleCloudTranslator::new(&GoogleCloudConfig::new("gc-key", Some("proj-1".to_owned())))
            .unwrap();
    assert_eq!(default_gc.endpoint_url(), GOOGLE_CLOUD_V2_ENDPOINT);

    let server = MockHttpServer::start(|req| {
        let parsed: serde_json::Value = serde_json::from_str(&req.body).expect("gc json");
        let q = parsed["q"].as_str().unwrap();
        ScriptedHttpResponse::json_ok(
            serde_json::json!({
                "data": {
                    "translations": [{
                        "translatedText": format!("GC[{q}]")
                    }]
                }
            })
            .to_string(),
        )
    })
    .await;

    let mut cfg = GoogleCloudConfig::new("gc-secret-xyz", Some("my-gcp-project".to_owned()));
    cfg.base_url = Some(server.base_url.clone());
    let translator = GoogleCloudTranslator::new(&cfg).unwrap();
    assert_eq!(translator.provider_id(), "google-cloud");

    let result = translator
        .translate(sample_request("Hold Logi on Ferox"))
        .await
        .expect("google translate");
    assert_eq!(result.translated_text, "GC[Hold ロジ on Ferox]");

    let captured = server.requests();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].target, "/language/translate/v2");
    assert!(!captured[0].target.contains("key="));
    assert!(!captured[0].target.contains("translate_a/single"));
    assert_eq!(
        captured[0]
            .headers
            .get("x-goog-api-key")
            .map(String::as_str),
        Some("gc-secret-xyz")
    );
    assert_eq!(
        captured[0]
            .headers
            .get("x-goog-user-project")
            .map(String::as_str),
        Some("my-gcp-project")
    );

    let body: serde_json::Value = serde_json::from_str(&captured[0].body).unwrap();
    assert_eq!(body["q"], "Hold __TERM_0__ on __TERM_1__");
    assert_eq!(body["source"], "en");
    assert_eq!(body["target"], "ja");
    assert_eq!(body["format"], "text");
}
