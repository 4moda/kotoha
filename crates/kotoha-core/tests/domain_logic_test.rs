//! Unit tests for `kotoha-core` pure domain logic (`src/domain/logic.rs`).

use std::collections::BTreeMap;

use kotoha_core::domain::{
    protect_terms, restore_terms, ContextWindow, GlossaryEntry, GlossaryPlaceholderGuard,
    HistoryEntry, PreFilter,
};

#[test]
fn domain_prefilter_and_context_window() {
    let filter = PreFilter::default();
    assert!(!filter.should_translate(""));
    assert!(!filter.should_translate("   \t\n  "));
    assert!(!filter.should_translate("...!?"));
    assert!(!filter.should_translate("uh"));
    assert!(!filter.should_translate("Um... hmm!"));
    assert!(filter.should_translate("Hello world"));
    assert!(filter.should_translate("uh wait a second"));

    let mut ctx = ContextWindow::new(3);
    ctx.set_glossary(vec![GlossaryEntry {
        term: "GG".to_owned(),
        translation: "ナイスゲーム".to_owned(),
    }]);
    for i in 1..=5 {
        ctx.push_turn(HistoryEntry {
            speaker_name: "Alice".to_owned(),
            original_text: format!("src-{i}"),
            translated_text: format!("tgt-{i}"),
        });
    }

    let snap = ctx.snapshot();
    assert_eq!(snap.glossary.len(), 1);
    assert_eq!(
        snap.glossary.get("GG").map(String::as_str),
        Some("ナイスゲーム")
    );
    assert_eq!(snap.recent_history.len(), 3);
    assert_eq!(snap.recent_history[0].original_text, "src-3");
    assert_eq!(snap.recent_history[2].original_text, "src-5");
}

#[test]
fn domain_glossary_placeholder_guard_longest_match_and_restore() {
    let terms = BTreeMap::from([
        ("Ice".to_owned(), "氷".to_owned()),
        ("Ice Beam".to_owned(), "冷凍ビーム".to_owned()),
        ("Beam".to_owned(), "光線".to_owned()),
        ("MWD".to_owned(), "マイクロワープドライブ".to_owned()),
    ]);

    let input = "Use Ice Beam then MWD and Ice Beam again, not Ice!";
    let (protected, guard) = protect_terms(input, &terms);
    assert_eq!(
        protected,
        "Use __TERM_0__ then __TERM_1__ and __TERM_0__ again, not __TERM_2__!"
    );
    assert_eq!(guard.len(), 3);

    // Simulate NMT translating surrounding words while keeping placeholders intact
    let translated_with_placeholders =
        "__TERM_0__ を使ってから __TERM_1__ と __TERM_0__ を再び、__TERM_2__ ではなく！";
    let restored = restore_terms(translated_with_placeholders, &guard);
    assert_eq!(
        restored,
        "冷凍ビーム を使ってから マイクロワープドライブ と 冷凍ビーム を再び、氷 ではなく！"
    );

    // Also test direct guard API with empty matches
    let (untouched, empty_guard) =
        GlossaryPlaceholderGuard::protect("nothing matches here", &terms);
    assert_eq!(untouched, "nothing matches here");
    assert!(empty_guard.is_empty());
    assert_eq!(empty_guard.restore("そのまま"), "そのまま");
}

#[test]
fn domain_glossary_placeholder_guard_respects_ascii_word_boundaries() {
    let terms = BTreeMap::from([
        ("Logi".to_owned(), "ロジ".to_owned()),
        ("M W D".to_owned(), "MWD".to_owned()),
        ("シールド".to_owned(), "Shield".to_owned()),
    ]);

    // "logical", "my_Logi", "Logi2" must NOT be corrupted; standalone "Logi", "M W D", and CJK
    // substring "シールド" MUST match.
    let input = "This logical my_Logi Logi2 needs Logi and M W D with シールド強化";
    let (protected, guard) = protect_terms(input, &terms);
    assert_eq!(
        protected,
        "This logical my_Logi Logi2 needs __TERM_0__ and __TERM_1__ with __TERM_2__強化"
    );
    let restored = restore_terms(&protected, &guard);
    assert_eq!(
        restored,
        "This logical my_Logi Logi2 needs ロジ and MWD with Shield強化"
    );
}
