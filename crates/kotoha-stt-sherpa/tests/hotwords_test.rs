//! Unit tests for `kotoha-stt-sherpa` hotwords formatting and sanitization (`src/hotwords.rs`).

#![allow(clippy::expect_used)]

use kotoha_core::domain::{SttDecodingMethod, SttHotwordsConfig};
use kotoha_stt_sherpa::{
    format_hotwords_text, format_hotwords_text_with_scores, sanitize_hotwords,
};

#[test]
fn hotwords_formatting_sanitizes_deduplicates_and_guards_sherpa_syntax() {
    let raw = vec![
        "  M W D  ".to_owned(),
        "M W D\r\n".to_owned(),
        "Ferox\0Injected".to_owned(),
        "#commented_out".to_owned(),
        "##   double_comment".to_owned(),
        "foo :9.9".to_owned(),
        "###".to_owned(),
        "   ".to_owned(),
        "\t\n".to_owned(),
        "Logi".to_owned(),
        "Logi".to_owned(),
    ];

    let sanitized = sanitize_hotwords(&raw);
    assert_eq!(
        sanitized,
        vec![
            "FeroxInjected".to_owned(),
            "Logi".to_owned(),
            "M W D".to_owned(),
            "commented_out".to_owned(),
            "double_comment".to_owned(),
            "foo  9.9".to_owned(),
        ]
    );

    let hw_cfg = SttHotwordsConfig::from_hotwords(vec![
        "M W D".to_owned(),
        "Anchor".to_owned(),
        "M W D\n".to_owned(),
        "#Fleet:9.5".to_owned(),
    ]);
    let formatted = format_hotwords_text(&hw_cfg).expect("format hotwords");
    assert_eq!(formatted, "Anchor\nFleet 9.5\nM W D\n");

    let scored = format_hotwords_text_with_scores(&hw_cfg, Some(2.25)).expect("scored format");
    assert_eq!(scored, "Anchor :2.25\nFleet 9.5 :2.25\nM W D :2.25\n");

    for bad_score in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let invalid = SttHotwordsConfig {
            decoding_method: SttDecodingMethod::ModifiedBeamSearch,
            hotwords_score: bad_score,
            hotwords: vec!["Logi".to_owned()],
        };
        assert!(format_hotwords_text(&invalid).is_err());
    }
}
