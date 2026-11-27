//! Unit tests for `kotoha-core` TOML glossary repository (`src/infrastructure/glossary_toml.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

use kotoha_core::domain::{CoreError, GlossaryRepository};
use kotoha_core::infrastructure::TomlGlossaryRepository;

#[test]
fn toml_glossary_repository_loads_sample_validates_fields_and_sorts_dir() {
    let repo = TomlGlossaryRepository::new();

    // 1. Load repository sample glossary (glossaries/sample.toml)
    let sample_path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../glossaries/sample.toml");
    let sample = repo
        .load_file(&sample_path)
        .expect("sample.toml should load cleanly");
    assert_eq!(sample.name, "My Profile");
    assert_eq!(sample.src_lang.as_str(), "en");
    assert_eq!(sample.tgt_lang.as_str(), "ja");
    assert_eq!(sample.terms.get("Ferox").map(String::as_str), Some("Ferox"));
    assert_eq!(sample.terms.get("Logi").map(String::as_str), Some("ロジ"));
    assert_eq!(
        sample.terms.get("Anchor").map(String::as_str),
        Some("アンカー")
    );
    assert_eq!(sample.terms.get("M W D").map(String::as_str), Some("MWD"));

    // 2. Reject invalid inputs (empty name, invalid language code, blank term key/value, bad TOML)
    assert!(matches!(
        TomlGlossaryRepository::parse_str("name = \"   \"\nsrc_lang = \"en\"\ntgt_lang = \"ja\""),
        Err(CoreError::Glossary(_))
    ));
    assert!(matches!(
        TomlGlossaryRepository::parse_str(
            "name = \"ok\"\nsrc_lang = \"invalid_lang\"\ntgt_lang = \"ja\""
        ),
        Err(CoreError::InvalidLanguageCode(_))
    ));
    assert!(matches!(
        TomlGlossaryRepository::parse_str(
            "name = \"ok\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\"\n[terms]\n\"   \" = \"val\""
        ),
        Err(CoreError::Glossary(_))
    ));
    assert!(matches!(
        TomlGlossaryRepository::parse_str(
            "name = \"ok\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\"\n[terms]\n\"Key\" = \"  \""
        ),
        Err(CoreError::Glossary(_))
    ));
    assert!(matches!(
        TomlGlossaryRepository::parse_str("not = [valid toml"),
        Err(CoreError::Glossary(_))
    ));

    // 3. load_dir loads *.toml in deterministic sorted filename order and ignores non-.toml files
    let temp_dir = std::env::temp_dir().join(format!("kotoha_toml_dir_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).unwrap();

    std::fs::write(
        temp_dir.join("20_override.toml"),
        "name = \"Override\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\"\n[terms]\n\"GG\" = \"お疲れ様\"\n\"M W D\" = \"MWD\"\n",
    )
    .unwrap();
    std::fs::write(
        temp_dir.join("10_base.toml"),
        "name = \"Base\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\"\n[terms]\n\"GG\" = \"ナイスゲーム\"\n\"Logi\" = \"ロジ\"\n",
    )
    .unwrap();
    std::fs::write(temp_dir.join("notes.txt"), "not a glossary").unwrap();
    std::fs::write(temp_dir.join("README.md"), "# ignore me").unwrap();

    let loaded = repo.load_dir(&temp_dir).expect("load_dir");
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].name, "Base");
    assert_eq!(loaded[1].name, "Override");
    assert_eq!(
        loaded[1].terms.get("M W D").map(String::as_str),
        Some("MWD")
    );

    // 4. Reject control characters in name / term key / term value, oversized payloads (>1 MiB),
    // and verify case-insensitive .TOML extension support
    assert!(matches!(
        TomlGlossaryRepository::parse_str(
            "name = \"Bad\\nName\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\""
        ),
        Err(CoreError::Glossary(_))
    ));
    assert!(matches!(
        TomlGlossaryRepository::parse_str(
            "name = \"ok\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\"\n[terms]\n\"Key\\nInject\" = \"val\""
        ),
        Err(CoreError::Glossary(_))
    ));
    assert!(matches!(
        TomlGlossaryRepository::parse_str(
            "name = \"ok\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\"\n[terms]\n\"Key\" = \"val\\u0000null\""
        ),
        Err(CoreError::Glossary(_))
    ));
    let oversized_path = temp_dir.join("30_huge.TOML");
    std::fs::write(&oversized_path, vec![b' '; 1024 * 1024 + 1]).unwrap();
    assert!(matches!(
        repo.load_file(&oversized_path),
        Err(CoreError::Glossary(_))
    ));
    std::fs::write(
        &oversized_path,
        "name = \"UpperExt\"\nsrc_lang = \"en\"\ntgt_lang = \"ja\"\n[terms]\n\"Cap\" = \"キャップ\"\n",
    )
    .unwrap();
    let loaded_upper = repo.load_dir(&temp_dir).expect("load_dir with .TOML");
    assert_eq!(loaded_upper.len(), 3);
    assert_eq!(loaded_upper[2].name, "UpperExt");

    let _ = std::fs::remove_dir_all(&temp_dir);
}
