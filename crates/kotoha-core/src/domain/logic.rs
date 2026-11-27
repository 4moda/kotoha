//! Pure domain logic (pre-filtering and context window management) with zero I/O.

use std::collections::VecDeque;

use crate::domain::entities::{GlossaryEntry, HistoryEntry, TranslateContext};

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

/// Bounded ring buffer storing recent conversation turns and glossary entries for translation context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextWindow {
    capacity: usize,
    history: VecDeque<HistoryEntry>,
    glossary: Vec<GlossaryEntry>,
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
            glossary: Vec::new(),
        }
    }

    /// Returns the configured maximum history turn count.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Replaces the active custom glossary entries.
    pub fn set_glossary(&mut self, glossary: Vec<GlossaryEntry>) {
        self.glossary = glossary;
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
