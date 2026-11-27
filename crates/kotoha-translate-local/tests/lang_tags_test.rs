//! Unit tests for `kotoha-translate-local` Flores-200 language tag mapping (`src/lang_tags.rs`).

#![allow(clippy::unwrap_used)]

use kotoha_core::domain::LanguageCode;
use kotoha_translate_local::to_flores200;

#[test]
fn flores200_mapping_covers_iso_and_bcp47_codes() {
    assert_eq!(
        to_flores200(&LanguageCode::new("en").unwrap()).unwrap(),
        "eng_Latn"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("en-US").unwrap()).unwrap(),
        "eng_Latn"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("ja").unwrap()).unwrap(),
        "jpn_Jpan"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("zh").unwrap()).unwrap(),
        "zho_Hans"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("zh-TW").unwrap()).unwrap(),
        "zho_Hant"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("zh-Hant-TW").unwrap()).unwrap(),
        "zho_Hant"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("zh-Hans-CN").unwrap()).unwrap(),
        "zho_Hans"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("ko").unwrap()).unwrap(),
        "kor_Hang"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("fr").unwrap()).unwrap(),
        "fra_Latn"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("de").unwrap()).unwrap(),
        "deu_Latn"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("es").unwrap()).unwrap(),
        "spa_Latn"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("ru").unwrap()).unwrap(),
        "rus_Cyrl"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("pt-BR").unwrap()).unwrap(),
        "por_Latn"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("it").unwrap()).unwrap(),
        "ita_Latn"
    );

    assert!(to_flores200(&LanguageCode::new("auto").unwrap()).is_err());
    assert!(to_flores200(&LanguageCode::new("zz").unwrap()).is_err());
}
