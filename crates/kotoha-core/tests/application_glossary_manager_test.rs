//! Integration tests for `kotoha-core` glossary manager (`src/application/glossary_manager.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex as StdMutex};

use common::{CapturingTranslator, FailingHotwordSttEngine, HotwordTrackingSttEngine};
use kotoha_core::application::{
    GlossaryManager, PipelineConfig, PipelineService, TranslatorManager,
};
use kotoha_core::domain::{
    CoreError, GlossaryProfile, GlossarySettings, LanguageCode, TranslationProviderConfig,
};
use kotoha_core::infrastructure::{
    AudioPreprocessor, CollectingSubtitleSink, MockTranslator, TomlGlossaryRepository,
};

#[tokio::test]
async fn glossary_manager_priority_merge_and_hot_reload_syncs_pipeline_and_stt() {
    let temp_dir = std::env::temp_dir().join(format!("kotoha_glossary_mgr_{}", std::process::id()));
    let custom_dir =
        std::env::temp_dir().join(format!("kotoha_glossary_custom_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp_dir);
    let _ = std::fs::remove_dir_all(&custom_dir);
    std::fs::create_dir_all(&temp_dir).unwrap();
    std::fs::create_dir_all(&custom_dir).unwrap();

    std::fs::write(
        temp_dir.join("01_base.toml"),
        "name = \"Base\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\"\n[terms]\n\"GG\" = \"ナイス\"\n\"M W D\" = \"MWD\"\n",
    )
    .unwrap();
    std::fs::write(
        temp_dir.join("02_squad.toml"),
        "name = \"Squad\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\"\n[terms]\n\"GG\" = \"お疲れ様\"\n\"Logi\" = \"ロジ\"\n",
    )
    .unwrap();
    std::fs::write(
        temp_dir.join("03_ko.toml"),
        "name = \"Korean\"\nsrc_lang = \"en\"\ntgt_lang = \"ko\"\n[terms]\n\"Logi\" = \"로지\"\n",
    )
    .unwrap();
    std::fs::write(
        custom_dir.join("01_custom.toml"),
        "name = \"Custom\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\"\n[terms]\n\"Anchor\" = \"アンカー\"\n",
    )
    .unwrap();

    let repo = TomlGlossaryRepository::new();
    let en = LanguageCode::new("en").unwrap();
    let ja = LanguageCode::new("ja").unwrap();
    let mut mgr = GlossaryManager::from_dir(&repo, &temp_dir, en.clone(), ja.clone())
        .expect("GlossaryManager::from_dir");

    // Default priority order follows sorted files: Base -> Squad ("GG" overridden by Squad)
    assert_eq!(
        mgr.active_merged().terms().get("GG").map(String::as_str),
        Some("お疲れ様")
    );
    assert_eq!(
        mgr.active_merged().terms().get("M W D").map(String::as_str),
        Some("MWD")
    );
    assert_eq!(
        mgr.active_merged().terms().get("Logi").map(String::as_str),
        Some("ロジ")
    );
    assert_eq!(mgr.active_merged().len(), 3);

    let stt = Arc::new(HotwordTrackingSttEngine::new());
    let captured_requests = Arc::new(StdMutex::new(Vec::new()));
    let translator = Arc::new(CapturingTranslator {
        captured_requests: Arc::clone(&captured_requests),
    });
    let translator_mgr = Arc::new(TranslatorManager::new(
        translator,
        TranslationProviderConfig::Mock {
            prefix: "[ja] ".to_owned(),
        },
    ));
    let sink = Arc::new(CollectingSubtitleSink::new());
    let mut pipeline = PipelineService::new(
        PipelineConfig::new(en.clone(), ja.clone()),
        stt.clone(),
        translator_mgr,
        sink,
        Arc::new(AudioPreprocessor::process_chunk),
    );

    mgr.set_hotwords_score(2.25).expect("valid hotwords score");
    mgr.sync_pipeline(&mut pipeline)
        .await
        .expect("sync_pipeline");

    // Reorder priority so Base overrides Squad, and hot-reload into pipeline + STT
    mgr.update_enabled_and_sync_pipeline(
        vec!["Squad".to_owned(), "Base".to_owned()],
        &mut pipeline,
    )
    .await
    .expect("update_enabled_and_sync_pipeline");

    assert_eq!(
        pipeline
            .active_glossary()
            .terms()
            .get("GG")
            .map(String::as_str),
        Some("ナイス")
    );

    // Add a new file on disk and hot-reload directory -> STT & pipeline update immediately
    std::fs::write(
        temp_dir.join("04_fleet.toml"),
        "name = \"Fleet\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\"\n[terms]\n\"Ferox\" = \"フェロックス\"\n",
    )
    .unwrap();
    mgr.set_enabled_profiles(vec![
        "Base".to_owned(),
        "Squad".to_owned(),
        "Fleet".to_owned(),
    ]);
    mgr.reload_dir_and_sync_pipeline(&repo, &temp_dir, &mut pipeline)
        .await
        .expect("reload_dir_and_sync_pipeline");

    assert_eq!(
        pipeline
            .active_glossary()
            .terms()
            .get("Ferox")
            .map(String::as_str),
        Some("フェロックス")
    );

    // Intentional empty selection (disabling all profiles) stays empty across load_from_dir
    mgr.set_enabled_profiles(Vec::new());
    assert!(mgr.has_explicit_selection());
    assert!(mgr.active_merged().is_empty());
    mgr.load_from_dir(&repo, &temp_dir)
        .expect("reload with explicit empty selection");
    assert!(
        mgr.active_merged().is_empty(),
        "explicit empty selection must stay disabled after reload"
    );

    // apply_settings switching to custom_dir and back to None reloads default_dir cleanly
    mgr.apply_settings(
        &GlossarySettings {
            enabled_profiles: vec!["Custom".to_owned()],
            glossary_dir: Some(custom_dir.to_string_lossy().into_owned()),
        },
        Some(&repo),
        Some(&temp_dir),
    )
    .expect("apply custom glossary_dir");
    assert_eq!(
        mgr.active_merged()
            .terms()
            .get("Anchor")
            .map(String::as_str),
        Some("アンカー")
    );
    mgr.apply_settings(
        &GlossarySettings {
            enabled_profiles: vec!["Base".to_owned()],
            glossary_dir: None,
        },
        Some(&repo),
        Some(&temp_dir),
    )
    .expect("revert glossary_dir to default_dir");
    assert!(mgr.active_merged().terms().get("Anchor").is_none());
    assert_eq!(
        mgr.active_merged().terms().get("GG").map(String::as_str),
        Some("ナイス")
    );

    // Transactional rollback on pipeline sync error: state remains unchanged
    let mut failing_pipeline = PipelineService::new(
        PipelineConfig::new(en.clone(), ja.clone()),
        Arc::new(FailingHotwordSttEngine),
        Arc::new(TranslatorManager::new(
            Arc::new(MockTranslator::default()),
            TranslationProviderConfig::Mock {
                prefix: "[ja] ".to_owned(),
            },
        )),
        Arc::new(CollectingSubtitleSink::new()),
        Arc::new(AudioPreprocessor::process_chunk),
    );
    let err = mgr
        .update_enabled_and_sync_pipeline(vec!["Fleet".to_owned()], &mut failing_pipeline)
        .await
        .expect_err("failing pipeline must return error");
    assert!(matches!(err, CoreError::Stt(_)));
    assert_eq!(mgr.enabled_profiles(), &["Base".to_owned()]);
    assert!(mgr.active_merged().terms().get("Ferox").is_none());

    // Duplicate profile names rejected in set_profiles and load_dir
    let dup_err = mgr
        .set_profiles(vec![
            GlossaryProfile::new("Dup", en.clone(), ja.clone(), BTreeMap::new()),
            GlossaryProfile::new("Dup", en, ja, BTreeMap::new()),
        ])
        .expect_err("duplicate profile names must fail");
    assert!(matches!(dup_err, CoreError::Glossary(_)));

    std::fs::write(
        temp_dir.join("99_dup.toml"),
        "name = \"Base\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\"\n[terms]\n\"X\" = \"Y\"\n",
    )
    .unwrap();
    assert!(matches!(
        mgr.load_from_dir(&repo, &temp_dir),
        Err(CoreError::Glossary(_))
    ));

    {
        let updates = stt.hotword_updates.lock().expect("lock");
        assert_eq!(updates.len(), 3);
        assert!((updates[0].hotwords_score - 2.25).abs() < f32::EPSILON);
        assert_eq!(
            updates[2].hotwords,
            vec![
                "Ferox".to_owned(),
                "GG".to_owned(),
                "Logi".to_owned(),
                "M W D".to_owned()
            ]
        );
    }

    let _ = std::fs::remove_dir_all(&temp_dir);
    let _ = std::fs::remove_dir_all(&custom_dir);
}
