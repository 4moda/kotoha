//! Unit tests for `kotoha-core` domain entities (`src/domain/entities.rs`).

#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, HashSet};

use kotoha_core::domain::{
    default_model_catalog, GlossaryProfile, GlossarySettings, LanguageCode, MergedGlossary,
    SpeakerKey, SttDecodingMethod, DEFAULT_HOTWORDS_SCORE,
};

#[test]
fn domain_speaker_key_color_is_deterministic_and_hex() {
    let a1 = SpeakerKey::new("mumble", 42);
    let a2 = SpeakerKey::new("mumble", 42);
    let b = SpeakerKey::new("ts3", 42);

    assert_eq!(a1.color_hex(), a2.color_hex());
    assert!(a1.color_hex().starts_with('#'));
    assert_eq!(a1.color_hex().len(), 7);
    assert!(b.color_hex().starts_with('#'));
}

#[test]
fn domain_language_code_validation() {
    assert!(LanguageCode::new("en").is_ok());
    assert!(LanguageCode::new("ja").is_ok());
    assert!(LanguageCode::new("en-US").is_ok());
    assert!(LanguageCode::new("auto").is_ok());

    assert!(LanguageCode::new("").is_err());
    assert!(LanguageCode::new("a").is_err());
    assert!(LanguageCode::new(" en").is_err());
    assert!(LanguageCode::new("en_US").is_err());
    assert!(LanguageCode::new("en--US").is_err());
}

#[test]
fn domain_glossary_profile_merge_language_filter_and_hotword_keys() {
    let en = LanguageCode::new("en").unwrap();
    let ja = LanguageCode::new("ja").unwrap();
    let ko = LanguageCode::new("ko").unwrap();

    let base_profile = GlossaryProfile::new(
        "general",
        en.clone(),
        ja.clone(),
        BTreeMap::from([
            ("GG".to_owned(), "ナイスゲーム".to_owned()),
            ("M W D".to_owned(), "マイクロワープドライブ".to_owned()),
            ("Ice Beam".to_owned(), "冷凍ビーム".to_owned()),
        ]),
    );
    let game_override = GlossaryProfile::new(
        "eve",
        en.clone(),
        ja.clone(),
        BTreeMap::from([
            ("GG".to_owned(), "お疲れ様".to_owned()),
            ("Tackle".to_owned(), "タックル".to_owned()),
        ]),
    );
    let wrong_lang = GlossaryProfile::new(
        "eve-ko",
        en.clone(),
        ko,
        BTreeMap::from([("Tackle".to_owned(), "다른".to_owned())]),
    );

    let profiles = vec![base_profile, game_override, wrong_lang];

    // Direct merge by pair: later overrides earlier, mismatched language ignored
    let raw_merged = GlossaryProfile::merge(&profiles, &en, &ja);
    assert_eq!(raw_merged.get("GG").map(String::as_str), Some("お疲れ様"));
    assert_eq!(
        raw_merged.get("Tackle").map(String::as_str),
        Some("タックル")
    );
    assert_eq!(raw_merged.len(), 4);

    // Priority merge using GlossarySettings.enabled_profiles
    let settings = GlossarySettings {
        enabled_profiles: vec!["eve".to_owned(), "general".to_owned(), "eve-ko".to_owned()],
        glossary_dir: None,
    };
    let merged =
        MergedGlossary::from_enabled_profiles(&profiles, &settings.enabled_profiles, &en, &ja);
    // "general" came after "eve" in enabled_profiles so "GG" reverts to general's translation
    assert_eq!(
        merged.terms().get("GG").map(String::as_str),
        Some("ナイスゲーム")
    );
    assert_eq!(
        merged.terms().get("Tackle").map(String::as_str),
        Some("タックル")
    );

    // Raw keys including space-separated subword hints ("M W D") are preserved as-is for STT
    let hotwords = merged.hotword_keys();
    assert_eq!(
        hotwords,
        vec![
            "GG".to_owned(),
            "Ice Beam".to_owned(),
            "M W D".to_owned(),
            "Tackle".to_owned()
        ]
    );

    let hw_cfg = merged.to_stt_hotwords_config(DEFAULT_HOTWORDS_SCORE);
    assert_eq!(
        hw_cfg.decoding_method,
        SttDecodingMethod::ModifiedBeamSearch
    );
    assert!((hw_cfg.hotwords_score - 1.5).abs() < f32::EPSILON);
    assert_eq!(hw_cfg.hotwords, hotwords);

    let empty_cfg = MergedGlossary::default().to_stt_hotwords_config(DEFAULT_HOTWORDS_SCORE);
    assert_eq!(empty_cfg.decoding_method, SttDecodingMethod::GreedySearch);
    assert_eq!(empty_cfg.hotwords, Vec::<String>::new());
}

#[test]
fn domain_model_catalog_contains_design_profiles() {
    let catalog = default_model_catalog();
    let ids: HashSet<&str> = catalog.iter().map(|m| m.model_id.as_str()).collect();
    assert!(ids.contains("fugumt-en-ja-int8"));
    assert!(ids.contains("opus-mt-en-ja-int8"));
    assert!(ids.contains("nllb-200-distilled-600m-int8"));
    assert!(ids.contains("nllb-200-1.3b-fp16"));
}
