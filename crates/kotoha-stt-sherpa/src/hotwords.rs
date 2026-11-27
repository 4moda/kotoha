//! Contextual hotwords formatting, validation, and dynamic state management.
//!
//! Per `DESIGN.md` Section 4.2 (Generic TOML Glossary & Non-Goals):
//! - Hotwords preserve internal spaces verbatim (e.g. `"M W D"`) for BPE/subword biasing.
//! - No phonetic/reading tables, no post-STT correction LLM, and no `[stt_corrections]` map.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};

use kotoha_core::domain::{
    CoreError, SttDecodingMethod, SttHotwordsConfig, DEFAULT_HOTWORDS_SCORE,
};

use crate::config::validate_hotwords_score;

/// Normalized hotwords snapshot paired with a monotonically increasing generation counter.
#[derive(Debug, Clone, PartialEq)]
pub struct HotwordsSnapshot {
    /// Monotonic generation ID incremented on every committed [`HotwordsRegistry`] update.
    pub generation: u64,
    /// Effective decoding method (`ModifiedBeamSearch` when hotwords are non-empty).
    pub decoding_method: SttDecodingMethod,
    /// Validated hotwords boosting score (`> 0.0` and finite).
    pub hotwords_score: f32,
    /// Deduplicated, sanitized hotword phrases in deterministic order.
    pub phrases: Vec<String>,
    /// Line-formatted `sherpa-onnx` hotwords payload (each phrase on its own line).
    pub formatted_text: String,
    /// Optional path to the managed hotwords file kept in sync on disk when hotwords are non-empty.
    pub file_path: Option<PathBuf>,
}

impl HotwordsSnapshot {
    /// Returns `true` if no hotword phrases are active.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.phrases.is_empty()
    }
}

/// Sanitizes and deduplicates raw hotword phrases while preserving internal spaces verbatim.
///
/// Rules:
/// - Strips ASCII/Unicode control characters (including `\r`, `\n`, `\t`, `\0`) to prevent
///   line-injection or C-string NUL truncation.
/// - Strips leading `#` comment markers and replaces `:` score delimiters with spaces so injected
///   `sherpa-onnx` line-parser syntax (`#comment`, `foo :9.9`) cannot corrupt hotword parsing.
/// - Trims outer whitespace while preserving internal spaces (e.g. `"M W D"` stays `"M W D"`).
/// - Drops empty/whitespace-only entries.
/// - Deduplicates deterministically via `BTreeSet` ordering.
#[must_use]
pub fn sanitize_hotwords(raw_phrases: &[String]) -> Vec<String> {
    let mut unique = BTreeSet::new();
    for raw in raw_phrases {
        let cleaned = sanitize_phrase(raw);
        if !cleaned.is_empty() {
            unique.insert(cleaned);
        }
    }
    unique.into_iter().collect()
}

/// Formats `SttHotwordsConfig` into `sherpa-onnx` hotwords text (one phrase per line).
///
/// Validates `config.hotwords_score` (must be finite and `> 0.0`), preserves internal spaces such
/// as `"M W D"` verbatim, strips control characters/newlines and `sherpa-onnx` comment/score
/// delimiters, and deduplicates phrases deterministically.
pub fn format_hotwords_text(config: &SttHotwordsConfig) -> Result<String, CoreError> {
    validate_hotwords_score(config.hotwords_score)?;
    let phrases = sanitize_hotwords(&config.hotwords);
    Ok(join_hotwords_lines(&phrases))
}

/// Formats `SttHotwordsConfig` with an explicit per-phrase `:score` suffix on each line
/// (`"<phrase> :<score>"`), matching `sherpa-onnx` hotwords file syntax when per-phrase overrides
/// are desired.
pub fn format_hotwords_text_with_scores(
    config: &SttHotwordsConfig,
    per_phrase_score: Option<f32>,
) -> Result<String, CoreError> {
    validate_hotwords_score(config.hotwords_score)?;
    if let Some(score) = per_phrase_score {
        validate_hotwords_score(score)?;
    }

    let phrases = sanitize_hotwords(&config.hotwords);
    if phrases.is_empty() {
        return Ok(String::new());
    }

    let mut out = String::new();
    for phrase in &phrases {
        out.push_str(phrase);
        if let Some(score) = per_phrase_score {
            use std::fmt::Write as _;
            let _ = write!(out, " :{score:.2}");
        }
        out.push('\n');
    }
    Ok(out)
}

fn sanitize_phrase(raw: &str) -> String {
    let mut cleaned = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_control() {
            continue;
        }
        if ch == ':' {
            cleaned.push(' ');
        } else {
            cleaned.push(ch);
        }
    }
    cleaned
        .trim()
        .trim_start_matches(|ch: char| ch == '#' || ch.is_whitespace())
        .trim_end()
        .to_owned()
}

fn join_hotwords_lines(phrases: &[String]) -> String {
    if phrases.is_empty() {
        return String::new();
    }
    let mut out = phrases.join("\n");
    out.push('\n');
    out
}

/// Prepared hotwords update produced before committing disk state or live snapshot.
#[derive(Debug, Clone)]
pub(crate) struct PreparedHotwordsUpdate {
    pub snapshot: HotwordsSnapshot,
}

/// Thread-safe dynamic hotwords store tracking generation updates and an optional managed file on disk.
#[derive(Debug)]
pub struct HotwordsRegistry {
    generation: AtomicU64,
    snapshot: RwLock<HotwordsSnapshot>,
    write_lock: Mutex<()>,
    managed_file: Option<ManagedHotwordsFile>,
}

#[derive(Debug)]
struct ManagedHotwordsFile {
    path: PathBuf,
    remove_on_drop: bool,
}

impl Drop for ManagedHotwordsFile {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct TempFileGuard {
    path: PathBuf,
    armed: bool,
}

impl TempFileGuard {
    const fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl HotwordsRegistry {
    /// Creates a new [`HotwordsRegistry`] without creating any temp file on disk.
    #[must_use]
    pub fn in_memory(initial_score: f32) -> Self {
        Self::new_inner(initial_score, None)
    }

    /// Creates a new [`HotwordsRegistry`] backed by a unique managed file path inside `dir`
    /// (deleted automatically on drop).
    ///
    /// Does not eagerly create an empty file on disk until non-empty hotwords are committed.
    pub fn with_managed_dir(dir: &Path, initial_score: f32) -> Result<Self, CoreError> {
        fs::create_dir_all(dir).map_err(|err| {
            CoreError::Stt(format!(
                "failed to create managed hotwords directory '{}': {err}",
                dir.display()
            ))
        })?;
        let unique_id = next_temp_file_id();
        let path = dir.join(format!(
            "kotoha_hotwords_{}_{unique_id}.txt",
            std::process::id()
        ));
        let managed = ManagedHotwordsFile {
            path,
            remove_on_drop: true,
        };
        Ok(Self::new_inner(initial_score, Some(managed)))
    }

    /// Creates a new [`HotwordsRegistry`] backed by a caller-specified file path (not removed on drop).
    ///
    /// Does not eagerly create an empty file on disk until non-empty hotwords are committed.
    pub fn with_explicit_file(path: PathBuf, initial_score: f32) -> Result<Self, CoreError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|err| {
                    CoreError::Stt(format!(
                        "failed to create hotwords parent directory '{}': {err}",
                        parent.display()
                    ))
                })?;
            }
        }
        let managed = ManagedHotwordsFile {
            path,
            remove_on_drop: false,
        };
        Ok(Self::new_inner(initial_score, Some(managed)))
    }

    fn new_inner(initial_score: f32, managed_file: Option<ManagedHotwordsFile>) -> Self {
        let valid_score = if initial_score.is_finite() && initial_score > 0.0 {
            initial_score
        } else {
            DEFAULT_HOTWORDS_SCORE
        };
        let file_path = managed_file.as_ref().map(|m| m.path.clone());
        Self {
            generation: AtomicU64::new(0),
            snapshot: RwLock::new(HotwordsSnapshot {
                generation: 0,
                decoding_method: SttDecodingMethod::GreedySearch,
                hotwords_score: valid_score,
                phrases: Vec::new(),
                formatted_text: String::new(),
                file_path,
            }),
            write_lock: Mutex::new(()),
            managed_file,
        }
    }

    /// Returns the current monotonic hotwords generation counter.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Returns a cloned snapshot of the current hotwords state.
    pub fn snapshot(&self) -> Result<HotwordsSnapshot, CoreError> {
        self.snapshot
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| CoreError::Stt("hotwords registry lock poisoned".to_owned()))
    }

    /// Validates and prepares a candidate snapshot without mutating the registry or writing disk.
    pub(crate) fn prepare(
        &self,
        config: &SttHotwordsConfig,
        effective_decoding: SttDecodingMethod,
    ) -> Result<PreparedHotwordsUpdate, CoreError> {
        let score = validate_hotwords_score(config.hotwords_score)?;
        let phrases = sanitize_hotwords(&config.hotwords);
        let formatted_text = join_hotwords_lines(&phrases);
        let next_gen = self.generation.load(Ordering::Acquire) + 1;
        let file_path = self.managed_file.as_ref().map(|m| m.path.clone());

        Ok(PreparedHotwordsUpdate {
            snapshot: HotwordsSnapshot {
                generation: next_gen,
                decoding_method: effective_decoding,
                hotwords_score: score,
                phrases,
                formatted_text,
                file_path,
            },
        })
    }

    /// Validates, normalizes, and atomically commits `config`, writing any managed file under lock.
    pub fn update(&self, config: &SttHotwordsConfig) -> Result<HotwordsSnapshot, CoreError> {
        let phrases = sanitize_hotwords(&config.hotwords);
        let effective_decoding = if phrases.is_empty() {
            config.decoding_method
        } else {
            SttDecodingMethod::ModifiedBeamSearch
        };
        self.commit_with(config, effective_decoding, |_| Ok(()))
    }

    /// Transactionally prepares candidate state, runs `on_before_commit` (e.g. rebuilding native
    /// recognizer), writes the managed file if needed, and commits the live snapshot under write lock.
    ///
    /// If `on_before_commit` fails, neither disk state nor in-memory snapshot/generation is mutated.
    pub(crate) fn commit_with<F>(
        &self,
        config: &SttHotwordsConfig,
        effective_decoding: SttDecodingMethod,
        on_before_commit: F,
    ) -> Result<HotwordsSnapshot, CoreError>
    where
        F: FnOnce(&HotwordsSnapshot) -> Result<(), CoreError>,
    {
        let _write_guard = self
            .write_lock
            .lock()
            .map_err(|_| CoreError::Stt("hotwords registry write lock poisoned".to_owned()))?;

        let mut prepared = self.prepare(config, effective_decoding)?;
        let next_gen = self.generation.load(Ordering::Acquire) + 1;
        prepared.snapshot.generation = next_gen;

        let previous = self.snapshot()?;
        self.sync_managed_file(&prepared.snapshot, next_gen)?;
        if let Err(err) = on_before_commit(&prepared.snapshot) {
            let _ = self.sync_managed_file(&previous, next_gen);
            return Err(err);
        }

        let mut guard = self
            .snapshot
            .write()
            .map_err(|_| CoreError::Stt("hotwords registry lock poisoned".to_owned()))?;
        self.generation.store(next_gen, Ordering::Release);
        *guard = prepared.snapshot.clone();
        drop(guard);

        Ok(prepared.snapshot)
    }

    fn sync_managed_file(
        &self,
        snapshot: &HotwordsSnapshot,
        gen_tag: u64,
    ) -> Result<(), CoreError> {
        let Some(managed) = &self.managed_file else {
            return Ok(());
        };

        if snapshot.is_empty() {
            match fs::remove_file(&managed.path) {
                Ok(()) => return Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(err) => {
                    return Err(CoreError::Stt(format!(
                        "failed to remove cleared hotwords file '{}': {err}",
                        managed.path.display()
                    )));
                }
            }
        }

        let parent = managed.path.parent().unwrap_or_else(|| Path::new("."));
        let unique_id = next_temp_file_id();
        let tmp_path = parent.join(format!(
            "kotoha_hw_tmp_{}_{gen_tag}_{unique_id}.tmp",
            std::process::id()
        ));
        let mut tmp_guard = TempFileGuard::new(tmp_path.clone());

        fs::write(&tmp_path, snapshot.formatted_text.as_bytes()).map_err(|err| {
            CoreError::Stt(format!(
                "failed to write temporary hotwords file '{}': {err}",
                tmp_path.display()
            ))
        })?;
        fs::rename(&tmp_path, &managed.path).map_err(|err| {
            CoreError::Stt(format!(
                "failed to commit hotwords file '{}': {err}",
                managed.path.display()
            ))
        })?;
        tmp_guard.disarm();
        Ok(())
    }
}

fn next_temp_file_id() -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}
