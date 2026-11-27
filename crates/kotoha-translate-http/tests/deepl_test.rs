//! Integration tests for `kotoha-translate-http` `DeepL` translator (`src/deepl.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use common::{sample_request, MockHttpServer, ScriptedHttpResponse};
use kotoha_core::domain::{CoreError, LanguageCode, Translator};
use kotoha_translate_http::{
    DeepLConfig, DeepLTranslator, DEEPL_FREE_ENDPOINT, DEEPL_PRO_ENDPOINT,
};

#[tokio::test]
async fn deepl_endpoint_selection_language_mapping_and_placeholder_guard() {
    let free = DeepLTranslator::new(&DeepLConfig::new("dl-key", true)).unwrap();
    assert_eq!(free.endpoint_url(), DEEPL_FREE_ENDPOINT);
    let pro = DeepLTranslator::new(&DeepLConfig::new("dl-key", false)).unwrap();
    assert_eq!(pro.endpoint_url(), DEEPL_PRO_ENDPOINT);

    // Verify M4 source_lang and target_lang mappings comprehensively
    assert_eq!(
        DeepLTranslator::map_source_lang(&LanguageCode::new("auto").unwrap()),
        None
    );
    assert_eq!(
        DeepLTranslator::map_source_lang(&LanguageCode::new("en").unwrap()).as_deref(),
        Some("EN")
    );
    assert_eq!(
        DeepLTranslator::map_source_lang(&LanguageCode::new("en-US").unwrap()).as_deref(),
        Some("EN")
    );
    assert_eq!(
        DeepLTranslator::map_source_lang(&LanguageCode::new("pt-BR").unwrap()).as_deref(),
        Some("PT")
    );
    assert_eq!(
        DeepLTranslator::map_source_lang(&LanguageCode::new("zh-Hant").unwrap()).as_deref(),
        Some("ZH")
    );
    assert_eq!(
        DeepLTranslator::map_source_lang(&LanguageCode::new("no").unwrap()).as_deref(),
        Some("NB")
    );
    assert_eq!(
        DeepLTranslator::map_source_lang(&LanguageCode::new("nb").unwrap()).as_deref(),
        Some("NB")
    );

    assert_eq!(
        DeepLTranslator::map_target_lang(&LanguageCode::new("en").unwrap()).unwrap(),
        "EN-US"
    );
    assert_eq!(
        DeepLTranslator::map_target_lang(&LanguageCode::new("en-GB").unwrap()).unwrap(),
        "EN-GB"
    );
    assert_eq!(
        DeepLTranslator::map_target_lang(&LanguageCode::new("en-US").unwrap()).unwrap(),
        "EN-US"
    );
    assert_eq!(
        DeepLTranslator::map_target_lang(&LanguageCode::new("pt").unwrap()).unwrap(),
        "PT-PT"
    );
    assert_eq!(
        DeepLTranslator::map_target_lang(&LanguageCode::new("pt-BR").unwrap()).unwrap(),
        "PT-BR"
    );
    assert_eq!(
        DeepLTranslator::map_target_lang(&LanguageCode::new("pt-PT").unwrap()).unwrap(),
        "PT-PT"
    );
    assert_eq!(
        DeepLTranslator::map_target_lang(&LanguageCode::new("zh").unwrap()).unwrap(),
        "ZH-HANS"
    );
    assert_eq!(
        DeepLTranslator::map_target_lang(&LanguageCode::new("zh-CN").unwrap()).unwrap(),
        "ZH-HANS"
    );
    assert_eq!(
        DeepLTranslator::map_target_lang(&LanguageCode::new("zh-Hans").unwrap()).unwrap(),
        "ZH-HANS"
    );
    assert_eq!(
        DeepLTranslator::map_target_lang(&LanguageCode::new("zh-TW").unwrap()).unwrap(),
        "ZH-HANT"
    );
    assert_eq!(
        DeepLTranslator::map_target_lang(&LanguageCode::new("zh-Hant").unwrap()).unwrap(),
        "ZH-HANT"
    );
    assert_eq!(
        DeepLTranslator::map_target_lang(&LanguageCode::new("ja").unwrap()).unwrap(),
        "JA"
    );
    assert!(matches!(
        DeepLTranslator::map_target_lang(&LanguageCode::new("auto").unwrap()),
        Err(CoreError::InvalidLanguageCode(_))
    ));

    let server = MockHttpServer::start(|req| {
        let parsed: serde_json::Value = serde_json::from_str(&req.body).expect("deepl json");
        let protected = parsed["text"][0].as_str().unwrap();
        ScriptedHttpResponse::json_ok(
            serde_json::json!({
                "translations": [{
                    "detected_source_language": "EN",
                    "text": format!("翻訳: {protected}")
                }]
            })
            .to_string(),
        )
    })
    .await;

    let mut cfg = DeepLConfig::new("deepl-secret-123", true);
    cfg.base_url = Some(server.base_url.clone());
    let translator = DeepLTranslator::new(&cfg).unwrap();
    assert_eq!(translator.provider_id(), "deepl");

    let translation = translator
        .translate(sample_request("Burn M W D and warp Ferox with Logi"))
        .await
        .expect("deepl translate");
    assert_eq!(
        translation.translated_text,
        "翻訳: Burn MWD and warp Ferox with ロジ"
    );

    let captured = server.requests();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].target, "/v2/translate");
    assert_eq!(
        captured[0].headers.get("authorization").map(String::as_str),
        Some("DeepL-Auth-Key deepl-secret-123")
    );
    let body: serde_json::Value = serde_json::from_str(&captured[0].body).unwrap();
    assert_eq!(body["source_lang"], "EN");
    assert_eq!(body["target_lang"], "JA");
    assert_eq!(
        body["text"][0].as_str().unwrap(),
        "Burn __TERM_0__ and warp __TERM_1__ with __TERM_2__"
    );
}
