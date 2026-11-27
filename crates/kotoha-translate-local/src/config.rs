//! Configuration, `ModelProfile` lookup, hardware device resolution, and model directory checks.

use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use kotoha_core::domain::{
    default_model_catalog, ComputeDevice, CoreError, ModelBackend, ModelProfile,
};
use sha2::{Digest, Sha256};

/// Model family controlling language prefix token behavior in `CTranslate2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalNmtFamily {
    /// `MarianMT` / `FuguMT` / OPUS-MT bilingual models (direct translation without language prefixes).
    MarianOrOpus,
    /// Meta NLLB-200 multilingual models requiring Flores-200 source & target prefix tokens.
    Nllb200,
}

/// Concrete hardware backend chosen after evaluating [`ComputeDevice`] against model profile
/// and host capability flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedDevice {
    /// CPU execution (`ct2rs::Device::CPU`).
    Cpu,
    /// NVIDIA CUDA execution (`ct2rs::Device::CUDA`).
    Cuda,
    /// Apple Silicon Metal / `CoreML` fallback execution.
    Metal,
}

/// Host hardware capabilities detected (or mocked in tests) when resolving [`ComputeDevice::Auto`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HardwareCapabilities {
    /// Whether CUDA runtime/GPU is available.
    pub cuda_available: bool,
    /// Whether Apple Silicon Metal is available.
    pub metal_available: bool,
}

impl HardwareCapabilities {
    /// Probes compile-time / runtime host accelerator hints conservatively.
    #[allow(clippy::missing_const_for_fn)]
    #[must_use]
    pub fn detect() -> Self {
        #[cfg(target_os = "linux")]
        let cuda_available = Path::new("/dev/nvidiactl").exists();
        #[cfg(target_os = "windows")]
        let cuda_available = {
            let sys_root = std::env::var_os("SystemRoot")
                .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
            sys_root.join("System32").join("nvcuda.dll").is_file()
        };
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let cuda_available = false;

        let metal_available = cfg!(all(target_os = "macos", target_arch = "aarch64"));
        Self {
            cuda_available,
            metal_available,
        }
    }
}

/// Configuration for [`crate::LocalNmtTranslator`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalNmtConfig {
    /// Root directory containing `<model_id>/` subdirectories.
    pub models_root_dir: PathBuf,
    /// Model profile ID from [`default_model_catalog`] or custom catalog.
    pub model_id: String,
    /// Requested compute device (`Auto`, `Cpu`, `Cuda`, `Metal`).
    pub device: ComputeDevice,
    /// Worker/intra-op thread count (`0` = automatic).
    pub threads: usize,
    /// Whether to verify `model.bin` SHA-256 against [`ModelProfile::sha256`].
    pub verify_sha256: bool,
}

impl LocalNmtConfig {
    /// Creates a new [`LocalNmtConfig`] with `verify_sha256 = true` and `threads = 0` (auto).
    #[must_use]
    pub fn new(
        models_root_dir: impl Into<PathBuf>,
        model_id: impl Into<String>,
        device: ComputeDevice,
    ) -> Self {
        Self {
            models_root_dir: models_root_dir.into(),
            model_id: model_id.into(),
            device,
            threads: 0,
            verify_sha256: true,
        }
    }

    /// Validates `self.model_id` against path traversal and returns `<models_root_dir>/<model_id>`.
    pub fn validated_model_dir(&self) -> Result<PathBuf, CoreError> {
        let valid_id = validate_model_id(&self.model_id)?;
        Ok(self.models_root_dir.join(valid_id))
    }
}

/// Validates that `raw` is a single safe directory component (no `/`, `\`, `..`, `:`, NUL, or
/// leading/trailing whitespace issues) and returns the trimmed `model_id` slice.
pub fn validate_model_id(raw: &str) -> Result<&str, CoreError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(CoreError::NotConfigured(
            "local NMT model_id must not be empty".to_owned(),
        ));
    }
    if trimmed.bytes().any(|b| b == 0)
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains(':')
        || trimmed.contains("..")
    {
        return Err(CoreError::NotConfigured(format!(
            "local NMT model_id '{trimmed}' contains forbidden path characters"
        )));
    }

    let mut components = Path::new(trimmed).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(trimmed),
        _ => Err(CoreError::NotConfigured(format!(
            "local NMT model_id '{trimmed}' must be a single relative directory name"
        ))),
    }
}

/// Looks up a [`ModelProfile`] by validated `model_id` across `custom_profiles` first, then
/// [`default_model_catalog`].
pub fn resolve_model_profile(
    model_id: &str,
    custom_profiles: &[ModelProfile],
) -> Result<ModelProfile, CoreError> {
    let trimmed = validate_model_id(model_id)?;

    let mut profile = custom_profiles
        .iter()
        .find(|p| p.model_id == trimmed)
        .cloned()
        .or_else(|| {
            default_model_catalog()
                .into_iter()
                .find(|p| p.model_id == trimmed)
        })
        .ok_or_else(|| CoreError::UnknownModelProfile(trimmed.to_owned()))?;

    let validated_profile_id = validate_model_id(&profile.model_id)?.to_owned();
    profile.model_id = validated_profile_id;

    if profile.backend != ModelBackend::CTranslate2 {
        return Err(CoreError::NotConfigured(format!(
            "model profile '{}' uses backend {:?}, expected CTranslate2",
            profile.model_id, profile.backend
        )));
    }

    Ok(profile)
}

/// Infers [`LocalNmtFamily`] from `model_id` / [`ModelProfile::supported_langs`].
#[must_use]
pub fn detect_model_family(profile: &ModelProfile) -> LocalNmtFamily {
    let id_lower = profile.model_id.to_ascii_lowercase();
    if id_lower.contains("nllb")
        || profile
            .supported_langs
            .iter()
            .any(|l| l.eq_ignore_ascii_case("multilingual"))
    {
        LocalNmtFamily::Nllb200
    } else {
        LocalNmtFamily::MarianOrOpus
    }
}

/// Resolves [`ComputeDevice`] into a concrete [`ResolvedDevice`] given host capabilities.
#[allow(clippy::missing_const_for_fn)]
#[must_use]
pub fn resolve_compute_device(requested: ComputeDevice, profile: &ModelProfile) -> ResolvedDevice {
    resolve_compute_device_with_caps(requested, profile, HardwareCapabilities::detect())
}

/// Deterministic [`resolve_compute_device`] helper accepting explicit [`HardwareCapabilities`].
///
/// Note: Because [`ModelBackend::CTranslate2`] executes on CPU (Accelerate/Ruy) rather than Metal
/// GPU on macOS, `ComputeDevice::Auto` resolves to [`ResolvedDevice::Cpu`] on macOS for
/// `CTranslate2` models even when `caps.metal_available` is `true` (explicit `ComputeDevice::Metal`
/// still returns [`ResolvedDevice::Metal`] if requested by caller).
#[must_use]
pub const fn resolve_compute_device_with_caps(
    requested: ComputeDevice,
    profile: &ModelProfile,
    caps: HardwareCapabilities,
) -> ResolvedDevice {
    match requested {
        ComputeDevice::Cpu => ResolvedDevice::Cpu,
        ComputeDevice::Cuda => ResolvedDevice::Cuda,
        ComputeDevice::Metal => ResolvedDevice::Metal,
        ComputeDevice::Auto => {
            let allows_metal_auto = !matches!(profile.backend, ModelBackend::CTranslate2);
            match profile.recommended_device {
                ComputeDevice::Cuda if caps.cuda_available => ResolvedDevice::Cuda,
                ComputeDevice::Metal if caps.metal_available && allows_metal_auto => {
                    ResolvedDevice::Metal
                }
                ComputeDevice::Auto => {
                    if caps.cuda_available {
                        ResolvedDevice::Cuda
                    } else if caps.metal_available && allows_metal_auto {
                        ResolvedDevice::Metal
                    } else {
                        ResolvedDevice::Cpu
                    }
                }
                _ => ResolvedDevice::Cpu,
            }
        }
    }
}

/// Validates that `model_dir` contains `model.bin` and supported tokenizer files (`tokenizer.json`,
/// `source.spm` + `target.spm`, `sentencepiece.bpe.model`, or `vocab.json` + `merges.txt`).
pub fn validate_model_dir_layout(model_dir: &Path) -> Result<PathBuf, CoreError> {
    if !model_dir.is_dir() {
        return Err(CoreError::NotConfigured(format!(
            "local NMT model directory does not exist: '{}'",
            model_dir.display()
        )));
    }

    let model_bin = model_dir.join("model.bin");
    if !model_bin.is_file() {
        return Err(CoreError::NotConfigured(format!(
            "local NMT model weights missing ('{}')",
            model_bin.display()
        )));
    }

    let has_hf_tokenizer = model_dir.join("tokenizer.json").is_file();
    let has_spm_pair =
        model_dir.join("source.spm").is_file() && model_dir.join("target.spm").is_file();
    let has_spm_bpe = model_dir.join("sentencepiece.bpe.model").is_file();
    let has_bpe_pair =
        model_dir.join("vocab.json").is_file() && model_dir.join("merges.txt").is_file();

    if !has_hf_tokenizer && !has_spm_pair && !has_spm_bpe && !has_bpe_pair {
        return Err(CoreError::NotConfigured(format!(
            "local NMT tokenizer files missing in '{}' (expected tokenizer.json, source.spm+target.spm, sentencepiece.bpe.model, or vocab.json+merges.txt)",
            model_dir.display()
        )));
    }

    Ok(model_bin)
}

/// Computes the lowercase hex SHA-256 digest of a file on disk using streaming reads.
pub fn compute_file_sha256_hex(path: &Path) -> Result<String, CoreError> {
    let mut file = File::open(path).map_err(|err| {
        CoreError::Translation(format!(
            "failed to open '{}' for SHA-256 verification: {err}",
            path.display()
        ))
    })?;

    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|err| {
            CoreError::Translation(format!(
                "failed to read '{}' during SHA-256 verification: {err}",
                path.display()
            ))
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    let digest = hasher.finalize();
    Ok(bytes_to_lower_hex(&digest))
}

/// Verifies `model_bin` against `expected_sha256` (case-insensitive 64-hex-char SHA-256 digest).
pub fn verify_file_sha256(model_bin: &Path, expected_sha256: &str) -> Result<String, CoreError> {
    let expected = expected_sha256.trim().to_ascii_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(CoreError::NotConfigured(format!(
            "invalid expected SHA-256 digest '{}' for '{}'",
            expected_sha256,
            model_bin.display()
        )));
    }

    let actual = compute_file_sha256_hex(model_bin)?;
    if actual != expected {
        return Err(CoreError::Translation(format!(
            "SHA-256 verification failed for '{}': expected {expected}, got {actual}",
            model_bin.display()
        )));
    }

    Ok(actual)
}

fn bytes_to_lower_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}
