//! Configuration types and validation for the `sherpa-onnx` streaming STT adapter.

use std::path::{Path, PathBuf};
use std::time::Duration;

use kotoha_core::domain::{ComputeDevice, CoreError, SttDecodingMethod, DEFAULT_HOTWORDS_SCORE};
use serde::{Deserialize, Serialize};

/// Default target audio sample rate expected by `sherpa-onnx` streaming models (`16000` Hz).
pub const DEFAULT_SAMPLE_RATE: i32 = 16_000;

/// Default feature dimension expected by `sherpa-onnx` fbank extractors (`80`).
pub const DEFAULT_FEATURE_DIM: i32 = 80;

/// Default number of CPU threads allocated to ONNX Runtime inference (`2`).
pub const DEFAULT_NUM_THREADS: i32 = 2;

/// Default beam width when `modified_beam_search` decoding is active (`4`).
pub const DEFAULT_MAX_ACTIVE_PATHS: i32 = 4;

/// Default idle duration after which inactive speaker STT streams are reaped (`60` seconds).
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Default endpointing rule 1 trailing silence threshold in seconds.
pub const DEFAULT_RULE1_MIN_TRAILING_SILENCE: f32 = 2.4;

/// Default endpointing rule 2 trailing silence threshold in seconds.
pub const DEFAULT_RULE2_MIN_TRAILING_SILENCE: f32 = 1.2;

/// Default endpointing rule 3 maximum utterance length in seconds before forced endpoint.
pub const DEFAULT_RULE3_MIN_UTTERANCE_LENGTH: f32 = 20.0;

/// Modeling unit used by `sherpa-onnx` tokenizers when hotword biasing is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SherpaModelingUnit {
    /// Chinese / Japanese character tokens (`"cjkchar"`).
    CjkChar,
    /// Byte-Pair Encoding subword tokens (`"bpe"`).
    Bpe,
    /// Mixed CJK character + BPE subword tokens (`"cjkchar+bpe"`).
    CjkCharPlusBpe,
}

impl SherpaModelingUnit {
    /// Returns the `sherpa-onnx` configuration string for this modeling unit.
    #[must_use]
    pub const fn as_sherpa_str(self) -> &'static str {
        match self {
            Self::CjkChar => "cjkchar",
            Self::Bpe => "bpe",
            Self::CjkCharPlusBpe => "cjkchar+bpe",
        }
    }
}

/// Supported `sherpa-onnx` online streaming model families.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "family", rename_all = "snake_case")]
pub enum SherpaModelFamily {
    /// Streaming Zipformer / RNN-T Transducer model (`encoder` + `decoder` + `joiner` + `tokens`).
    ZipformerTransducer {
        /// Path to the encoder ONNX model file.
        encoder: PathBuf,
        /// Path to the decoder ONNX model file.
        decoder: PathBuf,
        /// Path to the joiner ONNX model file.
        joiner: PathBuf,
        /// Path to `tokens.txt`.
        tokens: PathBuf,
        /// Optional SentencePiece/BPE vocabulary path (`bpe.vocab`) for subword hotword biasing.
        #[serde(default)]
        bpe_vocab: Option<PathBuf>,
        /// Optional modeling unit override (`"cjkchar"`, `"bpe"`, or `"cjkchar+bpe"`).
        #[serde(default)]
        modeling_unit: Option<SherpaModelingUnit>,
    },
    /// Streaming Paraformer model (`encoder` + `decoder` + `tokens`).
    Paraformer {
        /// Path to the Paraformer encoder ONNX model file.
        encoder: PathBuf,
        /// Path to the Paraformer decoder ONNX model file.
        decoder: PathBuf,
        /// Path to `tokens.txt`.
        tokens: PathBuf,
    },
}

impl SherpaModelFamily {
    /// Returns `true` if this model family supports transducer `modified_beam_search` + hotwords.
    #[must_use]
    pub const fn supports_hotwords(&self) -> bool {
        matches!(self, Self::ZipformerTransducer { .. })
    }

    /// Validates that configured paths are syntactically non-empty (and free of interior NULs).
    pub fn validate(&self) -> Result<(), CoreError> {
        match self {
            Self::ZipformerTransducer {
                encoder,
                decoder,
                joiner,
                tokens,
                bpe_vocab,
                modeling_unit,
            } => {
                validate_non_empty_path("transducer.encoder", encoder)?;
                validate_non_empty_path("transducer.decoder", decoder)?;
                validate_non_empty_path("transducer.joiner", joiner)?;
                validate_non_empty_path("tokens", tokens)?;
                if let Some(vocab) = bpe_vocab {
                    validate_non_empty_path("bpe_vocab", vocab)?;
                } else if matches!(
                    modeling_unit,
                    Some(SherpaModelingUnit::Bpe | SherpaModelingUnit::CjkCharPlusBpe)
                ) {
                    return Err(CoreError::Stt(
                        "sherpa modeling_unit 'bpe' or 'cjkchar+bpe' requires bpe_vocab to be configured"
                            .to_owned(),
                    ));
                }
            }
            Self::Paraformer {
                encoder,
                decoder,
                tokens,
            } => {
                validate_non_empty_path("paraformer.encoder", encoder)?;
                validate_non_empty_path("paraformer.decoder", decoder)?;
                validate_non_empty_path("tokens", tokens)?;
            }
        }
        Ok(())
    }

    /// Verifies that all configured model files exist on disk as regular files.
    pub fn validate_files_exist(&self) -> Result<(), CoreError> {
        self.validate()?;
        match self {
            Self::ZipformerTransducer {
                encoder,
                decoder,
                joiner,
                tokens,
                bpe_vocab,
                ..
            } => {
                ensure_file_exists("transducer.encoder", encoder)?;
                ensure_file_exists("transducer.decoder", decoder)?;
                ensure_file_exists("transducer.joiner", joiner)?;
                ensure_file_exists("tokens", tokens)?;
                if let Some(vocab) = bpe_vocab {
                    ensure_file_exists("bpe_vocab", vocab)?;
                }
            }
            Self::Paraformer {
                encoder,
                decoder,
                tokens,
            } => {
                ensure_file_exists("paraformer.encoder", encoder)?;
                ensure_file_exists("paraformer.decoder", decoder)?;
                ensure_file_exists("tokens", tokens)?;
            }
        }
        Ok(())
    }
}

/// Endpoint detection thresholds controlling when streaming hypotheses finalize an utterance.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SherpaEndpointConfig {
    /// Enables `sherpa-onnx` built-in endpoint detection.
    pub enable: bool,
    /// Trailing silence (seconds) before endpointing even without decoded tokens (Rule 1).
    pub rule1_min_trailing_silence: f32,
    /// Trailing silence (seconds) before endpointing after decoded tokens exist (Rule 2).
    pub rule2_min_trailing_silence: f32,
    /// Maximum utterance duration (seconds) before forced endpointing (Rule 3).
    pub rule3_min_utterance_length: f32,
}

impl Default for SherpaEndpointConfig {
    fn default() -> Self {
        Self {
            enable: true,
            rule1_min_trailing_silence: DEFAULT_RULE1_MIN_TRAILING_SILENCE,
            rule2_min_trailing_silence: DEFAULT_RULE2_MIN_TRAILING_SILENCE,
            rule3_min_utterance_length: DEFAULT_RULE3_MIN_UTTERANCE_LENGTH,
        }
    }
}

impl SherpaEndpointConfig {
    /// Validates that all endpointing thresholds are finite and non-negative.
    pub fn validate(&self) -> Result<(), CoreError> {
        validate_non_negative_finite(
            "endpoint.rule1_min_trailing_silence",
            self.rule1_min_trailing_silence,
        )?;
        validate_non_negative_finite(
            "endpoint.rule2_min_trailing_silence",
            self.rule2_min_trailing_silence,
        )?;
        validate_non_negative_finite(
            "endpoint.rule3_min_utterance_length",
            self.rule3_min_utterance_length,
        )?;
        Ok(())
    }
}

/// Top-level configuration for [`crate::SherpaSttEngine`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SherpaSttConfig {
    /// Model architecture and artifact paths.
    pub model: SherpaModelFamily,
    /// Target compute device mapped to ONNX Runtime execution provider.
    pub device: ComputeDevice,
    /// Number of ONNX Runtime intra-op threads (`>= 1`, default `2`).
    pub num_threads: i32,
    /// Beam search active paths (`>= 1`, default `4`) used for `modified_beam_search`.
    pub max_active_paths: i32,
    /// Base decoding method when no hotwords are active.
    pub decoding_method: SttDecodingMethod,
    /// Contextual hotword boosting score (finite and `> 0.0`, default `1.5`).
    pub hotwords_score: f32,
    /// Endpointing rules for utterance segmentation.
    pub endpoint: SherpaEndpointConfig,
    /// Inactivity timeout after which idle speaker streams can be reaped.
    pub idle_timeout: Duration,
    /// Audio sample rate in Hz (default `16000`).
    pub sample_rate: i32,
    /// Fbank feature dimension (default `80`).
    pub feature_dim: i32,
    /// Enables verbose `sherpa-onnx` C-library debug logging.
    pub debug: bool,
}

impl SherpaSttConfig {
    /// Creates a [`SherpaSttConfig`] for `model` with recommended streaming defaults.
    #[must_use]
    pub fn new(model: SherpaModelFamily) -> Self {
        Self {
            model,
            device: ComputeDevice::Auto,
            num_threads: DEFAULT_NUM_THREADS,
            max_active_paths: DEFAULT_MAX_ACTIVE_PATHS,
            decoding_method: SttDecodingMethod::GreedySearch,
            hotwords_score: DEFAULT_HOTWORDS_SCORE,
            endpoint: SherpaEndpointConfig::default(),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            sample_rate: DEFAULT_SAMPLE_RATE,
            feature_dim: DEFAULT_FEATURE_DIM,
            debug: false,
        }
    }

    /// Maps `ComputeDevice` to the `sherpa-onnx` ONNX Runtime execution provider string.
    #[must_use]
    pub const fn provider_name(&self) -> &'static str {
        compute_device_to_provider(self.device)
    }

    /// Returns the effective decoding method given whether non-empty hotwords are active.
    ///
    /// For Transducer models, active hotwords enforce [`SttDecodingMethod::ModifiedBeamSearch`].
    /// Paraformer models always fall back to [`SttDecodingMethod::GreedySearch`] because
    /// `sherpa-onnx` hotword biasing is only supported on Transducer decoders.
    #[must_use]
    pub const fn effective_decoding_method(&self, has_hotwords: bool) -> SttDecodingMethod {
        if !self.model.supports_hotwords() {
            return SttDecodingMethod::GreedySearch;
        }
        if has_hotwords {
            SttDecodingMethod::ModifiedBeamSearch
        } else {
            self.decoding_method
        }
    }

    /// Formats a [`SttDecodingMethod`] into the exact `sherpa-onnx` C API string.
    #[must_use]
    pub const fn decoding_method_str(method: SttDecodingMethod) -> &'static str {
        match method {
            SttDecodingMethod::GreedySearch => "greedy_search",
            SttDecodingMethod::ModifiedBeamSearch => "modified_beam_search",
        }
    }

    /// Validates all configuration invariants without requiring files to exist on disk.
    pub fn validate(&self) -> Result<(), CoreError> {
        self.model.validate()?;
        self.endpoint.validate()?;

        if self.num_threads < 1 {
            return Err(CoreError::Stt(format!(
                "sherpa num_threads must be >= 1, got {}",
                self.num_threads
            )));
        }
        if self.max_active_paths < 1 {
            return Err(CoreError::Stt(format!(
                "sherpa max_active_paths must be >= 1, got {}",
                self.max_active_paths
            )));
        }
        validate_hotwords_score(self.hotwords_score)?;
        if self.sample_rate < 8_000 || self.sample_rate > 192_000 {
            return Err(CoreError::Stt(format!(
                "sherpa sample_rate must be in 8000..=192000, got {}",
                self.sample_rate
            )));
        }
        if self.feature_dim < 1 {
            return Err(CoreError::Stt(format!(
                "sherpa feature_dim must be >= 1, got {}",
                self.feature_dim
            )));
        }
        if self.idle_timeout.is_zero() {
            return Err(CoreError::Stt(
                "sherpa idle_timeout must be greater than zero".to_owned(),
            ));
        }
        Ok(())
    }

    /// Runs [`Self::validate`] and verifies all configured model files exist on disk.
    pub fn validate_files_exist(&self) -> Result<(), CoreError> {
        self.validate()?;
        self.model.validate_files_exist()
    }
}

/// Maps a domain [`ComputeDevice`] to the corresponding `sherpa-onnx` provider identifier.
#[must_use]
pub const fn compute_device_to_provider(device: ComputeDevice) -> &'static str {
    match device {
        ComputeDevice::Auto | ComputeDevice::Cpu => "cpu",
        ComputeDevice::Cuda => "cuda",
        ComputeDevice::Metal => "coreml",
    }
}

/// Validates that `score` is finite and strictly greater than `0.0`.
pub fn validate_hotwords_score(score: f32) -> Result<f32, CoreError> {
    if !score.is_finite() || score <= 0.0 {
        return Err(CoreError::Stt(format!(
            "hotwords_score must be finite and > 0.0, got {score}"
        )));
    }
    Ok(score)
}

fn validate_non_empty_path(field: &str, path: &Path) -> Result<(), CoreError> {
    let raw = path.to_str().ok_or_else(|| {
        CoreError::Stt(format!(
            "sherpa path field '{field}' is not valid UTF-8: {}",
            path.display()
        ))
    })?;
    if raw.trim().is_empty() {
        return Err(CoreError::Stt(format!(
            "sherpa path field '{field}' must not be empty"
        )));
    }
    if raw.bytes().any(|b| b == 0) {
        return Err(CoreError::Stt(format!(
            "sherpa path field '{field}' must not contain NUL bytes"
        )));
    }
    Ok(())
}

fn ensure_file_exists(field: &str, path: &Path) -> Result<(), CoreError> {
    if !path.is_file() {
        return Err(CoreError::Stt(format!(
            "sherpa model file for '{field}' does not exist or is not a file: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_non_negative_finite(field: &str, value: f32) -> Result<(), CoreError> {
    if !value.is_finite() || value < 0.0 {
        return Err(CoreError::Stt(format!(
            "sherpa field '{field}' must be finite and >= 0.0, got {value}"
        )));
    }
    Ok(())
}
