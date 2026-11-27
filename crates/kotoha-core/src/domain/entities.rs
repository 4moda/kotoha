//! Domain entities, value objects, and model catalog definitions (zero external I/O).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::error::CoreError;

const SPEAKER_COLOR_PALETTE: [&str; 12] = [
    "#38BDF8", // sky
    "#34D399", // emerald
    "#F472B6", // pink
    "#FBBF24", // amber
    "#A78BFA", // violet
    "#FB7185", // rose
    "#2DD4BF", // teal
    "#60A5FA", // blue
    "#F97316", // orange
    "#4ADE80", // green
    "#E879F9", // fuchsia
    "#FACC15", // yellow
];

/// Globally unique speaker identifier across multiple input sources (`source`, `user_id`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpeakerKey {
    /// Source application name from IPC `Hello.app_name` (e.g., `"mumble"`, `"ts3"`, `"mock"`).
    pub source: String,
    /// Speaker ID unique within that source.
    pub user_id: u32,
}

impl SpeakerKey {
    /// Creates a new [`SpeakerKey`].
    #[must_use]
    pub fn new(source: impl Into<String>, user_id: u32) -> Self {
        Self {
            source: source.into(),
            user_id,
        }
    }

    /// Returns a deterministic high-contrast hex color string (`#RRGGBB`) derived from this key.
    #[must_use]
    pub fn color_hex(&self) -> &'static str {
        // FNV-1a 64-bit hash over source + user_id for cross-platform determinism.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for &byte in self.source.as_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
        for byte in self.user_id.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        let idx = usize::try_from(hash % (SPEAKER_COLOR_PALETTE.len() as u64)).unwrap_or(0);
        SPEAKER_COLOR_PALETTE[idx]
    }
}

/// Validated ISO 639-1 / BCP-47 language code wrapper (e.g., `"en"`, `"ja"`, `"en-US"`, `"auto"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LanguageCode(String);

impl LanguageCode {
    /// Parses and validates a language code string.
    ///
    /// Accepts `"auto"` or ASCII alphanumeric subtags separated by `'-'` (e.g., `"en"`, `"ja"`,
    /// `"en-US"`, `"zh-Hant"`), with length in `2..=16`.
    pub fn new(code: impl Into<String>) -> Result<Self, CoreError> {
        let raw = code.into();
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed != raw || !(2..=16).contains(&trimmed.len()) {
            return Err(CoreError::InvalidLanguageCode(raw));
        }
        let is_valid = trimmed
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric()));
        if !is_valid {
            return Err(CoreError::InvalidLanguageCode(raw));
        }
        Ok(Self(raw))
    }

    /// Returns the underlying language code slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for LanguageCode {
    type Error = CoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<LanguageCode> for String {
    fn from(code: LanguageCode) -> Self {
        code.0
    }
}

/// Domain mirror of speaker talking states from IPC protocol v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TalkingState {
    /// Not speaking.
    Passive,
    /// Normal speaking.
    Talking,
    /// Whispering.
    Whispering,
    /// Shouting.
    Shouting,
    /// Speaking while muted.
    TalkingMuted,
}

impl TalkingState {
    /// Returns `true` when the speaker state represents active speech.
    #[must_use]
    pub const fn is_speaking(self) -> bool {
        matches!(
            self,
            Self::Talking | Self::Whispering | Self::Shouting | Self::TalkingMuted
        )
    }
}

/// Raw interleaved PCM audio chunk received from an IPC input source.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioChunk {
    /// Speaker identity (`source` + `user_id`).
    pub speaker_key: SpeakerKey,
    /// Monotonic sequence number from the source plugin.
    pub seq: u32,
    /// Sample rate in Hz (`8000..=192000`).
    pub sample_rate: u32,
    /// Channel count (`1..=8`).
    pub channels: u16,
    /// Whether this audio belongs to the local voice chat user.
    pub is_local_user: bool,
    /// Interleaved `f32` PCM samples in `[-1.0, 1.0]`.
    pub samples: Vec<f32>,
}

/// Normalized 16 kHz mono audio chunk ready for STT streaming.
#[derive(Debug, Clone, PartialEq)]
pub struct PreprocessedAudio {
    /// Speaker identity.
    pub speaker_key: SpeakerKey,
    /// Resampled 16 kHz mono `f32` PCM samples.
    pub samples_16k_mono: Vec<f32>,
    /// Timestamp in milliseconds.
    pub timestamp_ms: u64,
    /// Whether this audio belongs to the local user.
    pub is_local_user: bool,
}

/// Domain input event emitted by an [`crate::domain::ports::AudioInputSource`] or IPC server.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioInputEvent {
    /// Client connection completed the `Hello` handshake.
    SourceConnected {
        /// Unique monotonically assigned connection ID.
        conn_id: u64,
        /// Source application name (`Hello.app_name`).
        source: String,
        /// Plugin version (`Hello.plugin_version`).
        plugin_version: String,
    },
    /// Client connection closed or errored.
    SourceDisconnected {
        /// Connection ID of the disconnected client.
        conn_id: u64,
        /// Source application name (if `Hello` had completed).
        source: String,
    },
    /// Client requested resetting all active speakers/streams for this source.
    SourceReset {
        /// Connection ID of the client sending `SourceReset`.
        conn_id: u64,
        /// Source application name.
        source: String,
    },
    /// Speaker display name registration or update.
    UserInfoUpdated {
        /// Owning connection ID of the client sending `UserInfo`.
        conn_id: u64,
        /// Speaker identity.
        speaker_key: SpeakerKey,
        /// Display name string (may be empty if user has no display name set).
        name: String,
    },
    /// Speaker left the channel/server.
    UserRemoved {
        /// Owning connection ID of the client sending `UserRemoved`.
        conn_id: u64,
        /// Speaker identity.
        speaker_key: SpeakerKey,
    },
    /// Speaker talking state changed.
    TalkingStateChanged {
        /// Owning connection ID of the client sending `TalkingState`.
        conn_id: u64,
        /// Speaker identity.
        speaker_key: SpeakerKey,
        /// New talking state.
        state: TalkingState,
    },
    /// Audio PCM frame received for a speaker.
    AudioFrame {
        /// Owning connection ID of the client sending `AudioFrame`.
        conn_id: u64,
        /// Audio PCM chunk payload.
        chunk: AudioChunk,
    },
    /// Periodic keepalive heartbeat from an active source connection.
    Heartbeat {
        /// Connection ID of the client sending `Heartbeat`.
        conn_id: u64,
        /// Source application name.
        source: String,
    },
}

/// Unique identifier correlating interim STT, final STT, and translated subtitle updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct UtteranceId(pub u64);

/// Event emitted by an [`crate::domain::ports::SttStream`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptEvent {
    /// Utterance identifier.
    pub utterance_id: UtteranceId,
    /// Speaker identity.
    pub speaker_key: SpeakerKey,
    /// Recognized text.
    pub text: String,
    /// `false` for partial/interim hypothesis, `true` for finalized utterance.
    pub is_final: bool,
    /// Timestamp in milliseconds.
    pub timestamp_ms: u64,
}

/// A recent translated turn kept in the context window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Speaker display name.
    pub speaker_name: String,
    /// Source-language utterance text.
    pub original_text: String,
    /// Target-language translated text.
    pub translated_text: String,
}

/// Custom glossary term pair injected into translation requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlossaryEntry {
    /// Source-language term or acronym.
    pub term: String,
    /// Preferred target-language translation.
    pub translation: String,
}

/// Default hotword boosting score used by [`SttHotwordsConfig`].
pub const DEFAULT_HOTWORDS_SCORE: f32 = 1.5;

/// Language-pair-scoped custom glossary profile (e.g., loaded from a TOML profile).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlossaryProfile {
    /// Profile identifier or display name (e.g., `"eve-online"`, `"valorant"`).
    pub name: String,
    /// Source language code (e.g., `"en"`).
    pub src_lang: LanguageCode,
    /// Target language code (e.g., `"ja"`).
    pub tgt_lang: LanguageCode,
    /// Ordered mapping of `source term / STT hotword -> fixed target translation`.
    #[serde(default)]
    pub terms: BTreeMap<String, String>,
}

impl GlossaryProfile {
    /// Creates a new [`GlossaryProfile`].
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        src_lang: LanguageCode,
        tgt_lang: LanguageCode,
        terms: BTreeMap<String, String>,
    ) -> Self {
        Self {
            name: name.into(),
            src_lang,
            tgt_lang,
            terms,
        }
    }

    /// Returns `true` when this profile targets `(src_lang, tgt_lang)`.
    #[must_use]
    pub fn matches_pair(&self, src_lang: &LanguageCode, tgt_lang: &LanguageCode) -> bool {
        &self.src_lang == src_lang && &self.tgt_lang == tgt_lang
    }

    /// Merges all profiles matching `(src_lang, tgt_lang)` in slice order (later overrides earlier).
    #[must_use]
    pub fn merge(
        profiles: &[Self],
        src_lang: &LanguageCode,
        tgt_lang: &LanguageCode,
    ) -> BTreeMap<String, String> {
        let mut merged = BTreeMap::new();
        for profile in profiles {
            if profile.matches_pair(src_lang, tgt_lang) {
                for (key, value) in &profile.terms {
                    if !key.trim().is_empty() {
                        merged.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        merged
    }
}

/// Merged active glossary terms ordered deterministically for STT hotwords and translation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedGlossary {
    terms: BTreeMap<String, String>,
}

impl MergedGlossary {
    /// Creates a [`MergedGlossary`] from an explicit `BTreeMap` (dropping blank keys).
    #[must_use]
    pub fn new(terms: BTreeMap<String, String>) -> Self {
        Self {
            terms: terms
                .into_iter()
                .filter(|(k, _)| !k.trim().is_empty())
                .collect(),
        }
    }

    /// Merges all `profiles` matching `(src_lang, tgt_lang)` in slice order (later overrides earlier).
    #[must_use]
    pub fn from_profiles(
        profiles: &[GlossaryProfile],
        src_lang: &LanguageCode,
        tgt_lang: &LanguageCode,
    ) -> Self {
        Self {
            terms: GlossaryProfile::merge(profiles, src_lang, tgt_lang),
        }
    }

    /// Merges profiles matching `enabled_profiles` in priority order (later overrides earlier)
    /// and filtered by `(src_lang, tgt_lang)`.
    #[must_use]
    pub fn from_enabled_profiles(
        profiles: &[GlossaryProfile],
        enabled_profiles: &[String],
        src_lang: &LanguageCode,
        tgt_lang: &LanguageCode,
    ) -> Self {
        let mut merged = BTreeMap::new();
        for name in enabled_profiles {
            for profile in profiles
                .iter()
                .filter(|p| p.name == *name && p.matches_pair(src_lang, tgt_lang))
            {
                for (key, value) in &profile.terms {
                    if !key.trim().is_empty() {
                        merged.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        Self { terms: merged }
    }

    /// Returns the merged `source -> target` translation map.
    #[must_use]
    pub const fn terms(&self) -> &BTreeMap<String, String> {
        &self.terms
    }

    /// Consumes this [`MergedGlossary`] and returns its inner `BTreeMap`.
    #[must_use]
    pub fn into_terms(self) -> BTreeMap<String, String> {
        self.terms
    }

    /// Returns `true` if no glossary terms are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Returns the number of merged glossary entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.terms.len()
    }

    /// Extracts STT hotword keys verbatim from the merged glossary (preserving internal spaces
    /// such as `"M W D"` for BPE/subword hotword biasing while skipping empty/whitespace-only keys).
    #[must_use]
    pub fn hotword_keys(&self) -> Vec<String> {
        self.terms
            .keys()
            .filter(|key| !key.trim().is_empty())
            .cloned()
            .collect()
    }

    /// Builds an [`SttHotwordsConfig`] from [`Self::hotword_keys`].
    #[must_use]
    pub fn to_stt_hotwords_config(&self, hotwords_score: f32) -> SttHotwordsConfig {
        let hotwords = self.hotword_keys();
        let decoding_method = if hotwords.is_empty() {
            SttDecodingMethod::GreedySearch
        } else {
            SttDecodingMethod::ModifiedBeamSearch
        };
        SttHotwordsConfig {
            decoding_method,
            hotwords_score,
            hotwords,
        }
    }

    /// Converts the terms into a list of [`GlossaryEntry`] values.
    #[must_use]
    pub fn entries(&self) -> Vec<GlossaryEntry> {
        self.terms
            .iter()
            .map(|(term, translation)| GlossaryEntry {
                term: term.clone(),
                translation: translation.clone(),
            })
            .collect()
    }
}

impl From<BTreeMap<String, String>> for MergedGlossary {
    fn from(terms: BTreeMap<String, String>) -> Self {
        Self::new(terms)
    }
}

impl From<Vec<GlossaryEntry>> for MergedGlossary {
    fn from(entries: Vec<GlossaryEntry>) -> Self {
        let mut terms = BTreeMap::new();
        for entry in entries {
            if !entry.term.trim().is_empty() {
                terms.insert(entry.term, entry.translation);
            }
        }
        Self { terms }
    }
}

impl From<MergedGlossary> for BTreeMap<String, String> {
    fn from(glossary: MergedGlossary) -> Self {
        glossary.terms
    }
}

/// Persistent user configuration for custom glossary profile selection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlossarySettings {
    /// Enabled profile names in priority order (later overrides earlier).
    #[serde(default)]
    pub enabled_profiles: Vec<String>,
    /// Optional custom directory containing `.toml` glossary profiles.
    #[serde(default)]
    pub glossary_dir: Option<String>,
}

/// Decoding strategy for streaming STT engines supporting hotwords.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SttDecodingMethod {
    /// Greedy decoding (default when no hotwords are active).
    #[default]
    GreedySearch,
    /// Modified beam search with contextual hotword biasing.
    ModifiedBeamSearch,
}

/// Runtime hotword configuration forwarded to [`crate::domain::ports::SttEngine::update_hotwords`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SttHotwordsConfig {
    /// Active decoding strategy (`ModifiedBeamSearch` when hotwords active, else `GreedySearch`).
    pub decoding_method: SttDecodingMethod,
    /// Contextual boosting score for hotwords (default `1.5`).
    pub hotwords_score: f32,
    /// Normalized hotword phrases passed to the STT recognizer.
    #[serde(default)]
    pub hotwords: Vec<String>,
}

impl Default for SttHotwordsConfig {
    fn default() -> Self {
        Self {
            decoding_method: SttDecodingMethod::GreedySearch,
            hotwords_score: DEFAULT_HOTWORDS_SCORE,
            hotwords: Vec::new(),
        }
    }
}

impl SttHotwordsConfig {
    /// Creates an [`SttHotwordsConfig`] choosing `ModifiedBeamSearch` if `hotwords` is non-empty,
    /// or `GreedySearch` otherwise, with [`DEFAULT_HOTWORDS_SCORE`].
    #[must_use]
    pub fn from_hotwords(hotwords: Vec<String>) -> Self {
        let decoding_method = if hotwords.is_empty() {
            SttDecodingMethod::GreedySearch
        } else {
            SttDecodingMethod::ModifiedBeamSearch
        };
        Self {
            decoding_method,
            hotwords_score: DEFAULT_HOTWORDS_SCORE,
            hotwords,
        }
    }
}

/// Context passed to a translator alongside the utterance to translate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslateContext {
    /// Recent conversation turns (`5..=10` typical, default 8).
    #[serde(default)]
    pub recent_history: Vec<HistoryEntry>,
    /// Merged user-defined glossary mappings (`source term -> target translation`).
    #[serde(default)]
    pub glossary: BTreeMap<String, String>,
}

/// Request sent to a [`crate::domain::ports::Translator`] implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationRequest {
    /// Utterance identifier.
    pub utterance_id: UtteranceId,
    /// Speaker identity.
    pub speaker_key: SpeakerKey,
    /// Speaker display name for LLM/context-aware prompts.
    pub speaker_name: String,
    /// Source text to translate.
    pub text: String,
    /// Source language code.
    pub src_lang: LanguageCode,
    /// Target language code.
    pub tgt_lang: LanguageCode,
    /// Conversation context and glossary.
    pub context: TranslateContext,
}

/// Response produced by a [`crate::domain::ports::Translator`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationResult {
    /// Utterance identifier matching the request.
    pub utterance_id: UtteranceId,
    /// Original source text.
    pub original_text: String,
    /// Translated target text.
    pub translated_text: String,
    /// Identifier of the provider that served the translation.
    pub provider_id: String,
    /// Translation latency in milliseconds.
    pub latency_ms: u64,
}

/// Lifecycle stage of a [`SubtitleEntry`] in the 3-stage UI update model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleStage {
    /// Phase A: streaming partial STT hypothesis.
    Interim,
    /// Phase B: finalized STT text awaiting translation (or kept untranslated if filtered).
    FinalPendingTranslation,
    /// Phase C: translation completed and attached.
    Translated,
}

/// Subtitle card payload emitted to [`crate::domain::ports::SubtitleSink`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtitleEntry {
    /// Utterance identifier linking Phase A/B/C updates for the same card.
    pub utterance_id: UtteranceId,
    /// Speaker identity.
    pub speaker_key: SpeakerKey,
    /// Resolved display name (or fallback such as `"User #42"`).
    pub speaker_name: String,
    /// Deterministic hex color (`#RRGGBB`) for the speaker.
    pub speaker_color: String,
    /// Original source-language text.
    pub src_text: String,
    /// Translated text once [`SubtitleStage::Translated`] is reached.
    pub translated_text: Option<String>,
    /// Current 3-stage lifecycle phase.
    pub stage: SubtitleStage,
    /// Timestamp in milliseconds.
    pub timestamp_ms: u64,
}

/// Compute hardware target for local inference models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeDevice {
    /// Automatically prefer CUDA/Metal when available, falling back to CPU.
    Auto,
    /// CPU inference.
    Cpu,
    /// NVIDIA CUDA GPU inference.
    Cuda,
    /// Apple Silicon Metal GPU inference.
    Metal,
}

/// Model runtime backend identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelBackend {
    /// `CTranslate2` (`ct2rs`) neural machine translation backend.
    CTranslate2,
    /// ONNX Runtime (`sherpa-onnx` or similar) backend.
    Onnx,
    /// OpenAI-compatible HTTP endpoint (cloud or local Ollama/LM Studio).
    OpenAiCompatible,
}

/// Downloadable/selectable model profile metadata for runtime engine selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProfile {
    /// Unique catalog ID (e.g., `"fugumt-en-ja-int8"`).
    pub model_id: String,
    /// Human-friendly display label.
    pub display_name: String,
    /// Execution backend.
    pub backend: ModelBackend,
    /// Supported translation pairs or `"multilingual"`.
    pub supported_langs: Vec<String>,
    /// Recommended compute device.
    pub recommended_device: ComputeDevice,
    /// Approximate download/disk footprint in megabytes.
    pub size_mb: u32,
    /// SPDX or model license identifier (e.g., `"CC-BY-SA-4.0"`, `"CC-BY-NC-4.0"`).
    pub license: String,
    /// Expected SHA-256 hex digest for integrity verification.
    pub sha256: String,
    /// Model archive download URL.
    pub download_url: String,
}

/// Returns the built-in catalog of translation [`ModelProfile`]s specified in `DESIGN.md`.
#[must_use]
pub fn default_model_catalog() -> Vec<ModelProfile> {
    vec![
        ModelProfile {
            model_id: "fugumt-en-ja-int8".to_owned(),
            display_name: "FuguMT EN->JA (CPU Fast INT8)".to_owned(),
            backend: ModelBackend::CTranslate2,
            supported_langs: vec!["en->ja".to_owned()],
            recommended_device: ComputeDevice::Cpu,
            size_mb: 200,
            license: "CC-BY-SA-4.0".to_owned(),
            sha256: "pending-release-manifest".to_owned(),
            download_url: "https://huggingface.co/kotoha-models/fugumt-en-ja-ct2-int8".to_owned(),
        },
        ModelProfile {
            model_id: "opus-mt-en-ja-int8".to_owned(),
            display_name: "OPUS-MT EN->JA (CPU Permissive INT8)".to_owned(),
            backend: ModelBackend::CTranslate2,
            supported_langs: vec!["en->ja".to_owned()],
            recommended_device: ComputeDevice::Cpu,
            size_mb: 150,
            license: "CC-BY-4.0".to_owned(),
            sha256: "pending-release-manifest".to_owned(),
            download_url: "https://huggingface.co/kotoha-models/opus-mt-en-ja-ct2-int8".to_owned(),
        },
        ModelProfile {
            model_id: "nllb-200-distilled-600m-int8".to_owned(),
            display_name: "NLLB-200 Distilled 600M (Multilingual INT8, Non-Commercial)".to_owned(),
            backend: ModelBackend::CTranslate2,
            supported_langs: vec!["multilingual".to_owned()],
            recommended_device: ComputeDevice::Auto,
            size_mb: 650,
            license: "CC-BY-NC-4.0".to_owned(),
            sha256: "pending-release-manifest".to_owned(),
            download_url: "https://huggingface.co/kotoha-models/nllb-200-distilled-600m-ct2-int8"
                .to_owned(),
        },
        ModelProfile {
            model_id: "nllb-200-1.3b-fp16".to_owned(),
            display_name: "NLLB-200 1.3B (High-Accuracy GPU FP16, Non-Commercial)".to_owned(),
            backend: ModelBackend::CTranslate2,
            supported_langs: vec!["multilingual".to_owned()],
            recommended_device: ComputeDevice::Cuda,
            size_mb: 2600,
            license: "CC-BY-NC-4.0".to_owned(),
            sha256: "pending-release-manifest".to_owned(),
            download_url: "https://huggingface.co/kotoha-models/nllb-200-1.3b-ct2-fp16".to_owned(),
        },
    ]
}

/// Runtime-selectable translation provider configuration.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranslationProviderConfig {
    /// Local `CTranslate2` NMT model selected by `model_id` and compute target.
    LocalModel {
        /// Catalog model ID (e.g., `"fugumt-en-ja-int8"`).
        model_id: String,
        /// Target compute device.
        device: ComputeDevice,
        /// Optional thread count (`None` or `Some(0)` for auto).
        threads: Option<usize>,
    },
    /// OpenAI-compatible chat completions endpoint (cloud LLM or local Ollama / LM Studio).
    OpenAiCompatible {
        /// Base URL such as `"https://api.openai.com/v1"` or `"http://localhost:11434/v1"`.
        base_url: String,
        /// Model name such as `"gpt-4o-mini"` or `"qwen2.5:7b"`.
        model: String,
        /// Optional API key (local endpoints like Ollama may omit it).
        api_key: Option<String>,
        /// Request timeout in milliseconds.
        timeout_ms: u64,
    },
    /// Official `DeepL` API (Free or Pro endpoint).
    DeepL {
        /// `DeepL` authentication key.
        api_key: String,
        /// Whether to target `api-free.deepl.com` (`true`) or `api.deepl.com` (`false`).
        use_free_endpoint: bool,
        /// Request timeout in milliseconds.
        timeout_ms: u64,
    },
    /// Official Google Cloud Translation API (v2/v3).
    GoogleCloud {
        /// Google Cloud API key or credential token.
        api_key: String,
        /// Optional GCP project ID (used by v3 endpoints).
        project_id: Option<String>,
        /// Request timeout in milliseconds.
        timeout_ms: u64,
    },
    /// User-deployed Google Apps Script (`LanguageApp.translate()`) or custom HTTP webhook.
    GasWebhook {
        /// Deployed webhook URL.
        endpoint_url: String,
        /// Optional bearer authorization token.
        bearer_token: Option<String>,
        /// Request timeout in milliseconds.
        timeout_ms: u64,
    },
    /// Deterministic mock translator for Phase 1 verification and E2E testing.
    Mock {
        /// Prefix prepended to translated text (e.g., `"[ja] "`).
        prefix: String,
    },
}

impl std::fmt::Debug for TranslationProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LocalModel {
                model_id,
                device,
                threads,
            } => f
                .debug_struct("LocalModel")
                .field("model_id", model_id)
                .field("device", device)
                .field("threads", threads)
                .finish(),
            Self::OpenAiCompatible {
                base_url,
                model,
                api_key,
                timeout_ms,
            } => f
                .debug_struct("OpenAiCompatible")
                .field("base_url", base_url)
                .field("model", model)
                .field("api_key", &api_key.as_ref().map(|_| "[REDACTED]"))
                .field("timeout_ms", timeout_ms)
                .finish(),
            Self::DeepL {
                api_key: _,
                use_free_endpoint,
                timeout_ms,
            } => f
                .debug_struct("DeepL")
                .field("api_key", &"[REDACTED]")
                .field("use_free_endpoint", use_free_endpoint)
                .field("timeout_ms", timeout_ms)
                .finish(),
            Self::GoogleCloud {
                api_key: _,
                project_id,
                timeout_ms,
            } => f
                .debug_struct("GoogleCloud")
                .field("api_key", &"[REDACTED]")
                .field("project_id", project_id)
                .field("timeout_ms", timeout_ms)
                .finish(),
            Self::GasWebhook {
                endpoint_url,
                bearer_token,
                timeout_ms,
            } => f
                .debug_struct("GasWebhook")
                .field("endpoint_url", endpoint_url)
                .field("bearer_token", &bearer_token.as_ref().map(|_| "[REDACTED]"))
                .field("timeout_ms", timeout_ms)
                .finish(),
            Self::Mock { prefix } => f.debug_struct("Mock").field("prefix", prefix).finish(),
        }
    }
}
