//! Flores-200 language tag mapping for NLLB-200 multilingual NMT models.

use kotoha_core::domain::{CoreError, LanguageCode};

/// Maps a Kotoha [`LanguageCode`] (ISO 639-1 / BCP-47) to an NLLB-200 Flores-200 tag
/// (e.g., `"en"` -> `"eng_Latn"`, `"ja"` -> `"jpn_Jpan"`, `"zh-TW"` / `"zh-Hant-TW"` -> `"zho_Hant"`).
pub fn to_flores200(code: &LanguageCode) -> Result<&'static str, CoreError> {
    let raw = code.as_str().trim();
    if raw.eq_ignore_ascii_case("auto") {
        return Err(CoreError::InvalidLanguageCode(
            "NLLB-200 requires explicit source and target language codes (cannot use 'auto')"
                .to_owned(),
        ));
    }

    let normalized = raw.to_ascii_lowercase().replace('_', "-");
    if let Some(exact) = map_exact_tag(&normalized) {
        return Ok(exact);
    }

    // Check Traditional/Simplified Chinese multi-subtag prefixes before collapsing to primary "zh".
    if normalized.starts_with("zh-hant")
        || normalized.starts_with("zho-hant")
        || normalized.starts_with("zh-tw")
        || normalized.starts_with("zh-hk")
        || normalized.starts_with("zh-mo")
    {
        return Ok("zho_Hant");
    }
    if normalized.starts_with("zh-hans")
        || normalized.starts_with("zho-hans")
        || normalized.starts_with("zh-cn")
        || normalized.starts_with("zh-sg")
    {
        return Ok("zho_Hans");
    }

    // Fallback to primary language subtag (e.g. "en-GB" -> "en", "fr-CA" -> "fr").
    if let Some((primary, _)) = normalized.split_once('-') {
        if let Some(mapped) = map_exact_tag(primary) {
            return Ok(mapped);
        }
    }

    Err(CoreError::InvalidLanguageCode(format!(
        "unsupported NLLB-200 language code: '{raw}'"
    )))
}

fn map_exact_tag(tag: &str) -> Option<&'static str> {
    match tag {
        "en" | "eng" => Some("eng_Latn"),
        "ja" | "jpn" => Some("jpn_Jpan"),
        "zh" | "zh-cn" | "zh-sg" | "zh-hans" | "zho-hans" => Some("zho_Hans"),
        "zh-tw" | "zh-hk" | "zh-mo" | "zh-hant" | "zho-hant" => Some("zho_Hant"),
        "ko" | "kor" => Some("kor_Hang"),
        "fr" | "fra" => Some("fra_Latn"),
        "de" | "deu" => Some("deu_Latn"),
        "es" | "spa" => Some("spa_Latn"),
        "ru" | "rus" => Some("rus_Cyrl"),
        "pt" | "pt-br" | "pt-pt" | "por" => Some("por_Latn"),
        "it" | "ita" => Some("ita_Latn"),
        "nl" | "nld" => Some("nld_Latn"),
        "pl" | "pol" => Some("pol_Latn"),
        "tr" | "tur" => Some("tur_Latn"),
        "uk" | "ukr" => Some("ukr_Cyrl"),
        "vi" | "vie" => Some("vie_Latn"),
        "th" | "tha" => Some("tha_Thai"),
        "id" | "ind" => Some("ind_Latn"),
        "ms" | "zsm" => Some("zsm_Latn"),
        "ar" | "arb" => Some("arb_Arab"),
        "hi" | "hin" => Some("hin_Deva"),
        "sv" | "swe" => Some("swe_Latn"),
        "da" | "dan" => Some("dan_Latn"),
        "fi" | "fin" => Some("fin_Latn"),
        "no" | "nb" | "nob" => Some("nob_Latn"),
        "cs" | "ces" => Some("ces_Latn"),
        "el" | "ell" => Some("ell_Grek"),
        "he" | "heb" => Some("heb_Hebr"),
        "hu" | "hun" => Some("hun_Latn"),
        "ro" | "ron" => Some("ron_Latn"),
        "tl" | "fil" | "tgl" => Some("tgl_Latn"),
        _ => None,
    }
}
