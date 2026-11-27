//! Pure domain logic (pre-filtering, context window management, and glossary placeholder guarding)
//! with zero external I/O.

use std::collections::{BTreeMap, HashMap, VecDeque};

use crate::domain::entities::{HistoryEntry, MergedGlossary, TranslateContext};

/// Default number of recent translated turns preserved in [`ContextWindow`].
pub const DEFAULT_CONTEXT_WINDOW_CAPACITY: usize = 8;

/// Pure filter deciding whether finalized STT text merits translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreFilter {
    filler_words: Vec<String>,
    min_alphanumeric_chars: usize,
}

impl Default for PreFilter {
    fn default() -> Self {
        Self::new(
            ["uh", "um", "ah", "er", "hmm", "mm", "mhm", "eh"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            1,
        )
    }
}

impl PreFilter {
    /// Creates a [`PreFilter`] with custom filler words (matched case-insensitively after trimming
    /// outer punctuation) and minimum alphanumeric character count.
    #[must_use]
    pub fn new(filler_words: Vec<String>, min_alphanumeric_chars: usize) -> Self {
        let normalized_fillers = filler_words
            .into_iter()
            .map(|w| normalize_token(&w))
            .filter(|w| !w.is_empty())
            .collect();
        Self {
            filler_words: normalized_fillers,
            min_alphanumeric_chars,
        }
    }

    /// Returns `true` if `text` is meaningful speech that should be sent to translation.
    ///
    /// Rejects:
    /// - Empty or whitespace-only strings
    /// - Strings containing only punctuation / symbols (fewer than `min_alphanumeric_chars`)
    /// - Single-word filler utterances (e.g., `"uh"`, `"Um..."`, `"hmm!"`)
    #[must_use]
    pub fn should_translate(&self, text: &str) -> bool {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return false;
        }

        let alnum_count = trimmed.chars().filter(|c| c.is_alphanumeric()).count();
        if alnum_count < self.min_alphanumeric_chars {
            return false;
        }

        // Reject utterances composed solely of one or more configured filler tokens.
        let mut tokens = trimmed.split_whitespace().peekable();
        !(tokens.peek().is_some()
            && tokens.all(|tok| {
                let norm = normalize_token(tok);
                norm.is_empty() || self.filler_words.iter().any(|f| f == &norm)
            }))
    }
}

fn normalize_token(token: &str) -> String {
    token
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// Bounded ring buffer storing recent conversation turns and active glossary terms for translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextWindow {
    capacity: usize,
    history: VecDeque<HistoryEntry>,
    glossary: BTreeMap<String, String>,
}

impl Default for ContextWindow {
    fn default() -> Self {
        Self::new(DEFAULT_CONTEXT_WINDOW_CAPACITY)
    }
}

impl ContextWindow {
    /// Creates a [`ContextWindow`] keeping at most `capacity` recent turns.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            history: VecDeque::with_capacity(capacity),
            glossary: BTreeMap::new(),
        }
    }

    /// Returns the configured maximum history turn count.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Replaces the active custom glossary entries.
    pub fn set_glossary(&mut self, glossary: impl Into<MergedGlossary>) {
        self.glossary = glossary.into().into_terms();
    }

    /// Returns a reference to the active glossary map (`source term -> target translation`).
    #[must_use]
    pub const fn glossary(&self) -> &BTreeMap<String, String> {
        &self.glossary
    }

    /// Appends a completed turn, evicting the oldest turn if capacity is reached.
    pub fn push_turn(&mut self, entry: HistoryEntry) {
        if self.capacity == 0 {
            return;
        }
        if self.history.len() == self.capacity {
            self.history.pop_front();
        }
        self.history.push_back(entry);
    }

    /// Clears all buffered conversation history while keeping glossary entries intact.
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// Builds a snapshot [`TranslateContext`] for the next translation request.
    #[must_use]
    pub fn snapshot(&self) -> TranslateContext {
        TranslateContext {
            recent_history: self.history.iter().cloned().collect(),
            glossary: self.glossary.clone(),
        }
    }
}

/// Protects glossary terms during translation using deterministic placeholders.
///
/// Replaces terms with `__TERM_0__`, `__TERM_1__`, ... via longest-match-first scanning, then
/// restores them to their fixed target translations after translation completes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlossaryPlaceholderGuard {
    replacements: Vec<(String, String)>,
}

impl GlossaryPlaceholderGuard {
    /// Replaces all matching glossary terms in `text` (longest-match-first, single-pass) with
    /// `__TERM_0__`, `__TERM_1__`, ... and returns `(protected_text, guard)`.
    #[must_use]
    pub fn protect(text: &str, terms: &BTreeMap<String, String>) -> (String, Self) {
        if text.is_empty() || terms.is_empty() {
            return (text.to_owned(), Self::default());
        }

        // Longest-match-first, breaking ties lexicographically for determinism.
        let mut sorted_terms: Vec<(&str, &str)> = terms
            .iter()
            .filter(|(k, _)| !k.trim().is_empty())
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        sorted_terms.sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

        if sorted_terms.is_empty() {
            return (text.to_owned(), Self::default());
        }

        let mut protected = String::with_capacity(text.len());
        let mut replacements: Vec<(String, String)> = Vec::new();
        let mut assigned: HashMap<&str, String> = HashMap::new();

        let mut pos = 0usize;
        while pos < text.len() {
            let tail = &text[pos..];
            if let Some(&(src_term, tgt_term)) = sorted_terms.iter().find(|&&(src_term, _)| {
                tail.starts_with(src_term) && is_word_boundary_match(text, pos, src_term)
            }) {
                let placeholder = assigned
                    .entry(src_term)
                    .or_insert_with(|| {
                        let ph = format!("__TERM_{}__", replacements.len());
                        replacements.push((ph.clone(), tgt_term.to_owned()));
                        ph
                    })
                    .clone();
                protected.push_str(&placeholder);
                pos += src_term.len();
            } else if let Some(ch) = tail.chars().next() {
                protected.push(ch);
                pos += ch.len_utf8();
            } else {
                break;
            }
        }

        (protected, Self { replacements })
    }

    /// Convenience wrapper around [`Self::protect`] accepting a [`MergedGlossary`].
    #[must_use]
    pub fn protect_glossary(text: &str, glossary: &MergedGlossary) -> (String, Self) {
        Self::protect(text, glossary.terms())
    }

    /// Restores recorded placeholders (`__TERM_0__`, `__TERM_1__`, ...) in `translated_text`
    /// back to their target glossary translations using a single-pass scan.
    #[must_use]
    pub fn restore(&self, translated_text: &str) -> String {
        if self.replacements.is_empty() || translated_text.is_empty() {
            return translated_text.to_owned();
        }

        let mut restored = String::with_capacity(translated_text.len());
        let mut pos = 0usize;
        while pos < translated_text.len() {
            let tail = &translated_text[pos..];
            if let Some((placeholder, target)) = self
                .replacements
                .iter()
                .find(|(placeholder, _)| tail.starts_with(placeholder.as_str()))
            {
                restored.push_str(target);
                pos += placeholder.len();
            } else if let Some(ch) = tail.chars().next() {
                restored.push(ch);
                pos += ch.len_utf8();
            } else {
                break;
            }
        }

        restored
    }

    /// Returns the recorded `(placeholder, target_translation)` pairs in assignment order.
    #[must_use]
    pub fn replacements(&self) -> &[(String, String)] {
        &self.replacements
    }

    /// Returns the number of distinct matched terms guarded by this instance.
    #[must_use]
    pub fn len(&self) -> usize {
        self.replacements.len()
    }

    /// Returns `true` if no glossary terms matched during [`Self::protect`].
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.replacements.is_empty()
    }
}

/// Functional helper delegating to [`GlossaryPlaceholderGuard::protect`].
#[must_use]
pub fn protect_terms(
    text: &str,
    terms: &BTreeMap<String, String>,
) -> (String, GlossaryPlaceholderGuard) {
    GlossaryPlaceholderGuard::protect(text, terms)
}

/// Functional helper delegating to [`GlossaryPlaceholderGuard::restore`].
#[must_use]
pub fn restore_terms(translated_text: &str, guard: &GlossaryPlaceholderGuard) -> String {
    guard.restore(translated_text)
}

const fn is_ascii_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

const fn is_word_boundary_match(text: &str, pos: usize, term: &str) -> bool {
    let term_bytes = term.as_bytes();
    let text_bytes = text.as_bytes();
    if let Some(&first) = term_bytes.first() {
        if is_ascii_word_byte(first) && pos > 0 && is_ascii_word_byte(text_bytes[pos - 1]) {
            return false;
        }
    }
    let end = pos + term_bytes.len();
    if let Some(&last) = term_bytes.last() {
        if is_ascii_word_byte(last) && end < text_bytes.len() && is_ascii_word_byte(text_bytes[end])
        {
            return false;
        }
    }
    true
}
