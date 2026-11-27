//! Model-granular `LocalNmtTranslator` implementing `kotoha_core::domain::ports::Translator`.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use kotoha_core::domain::{
    CoreError, GlossaryPlaceholderGuard, LanguageCode, ModelProfile, TranslationRequest,
    TranslationResult, Translator,
};

use crate::backend::{LocalNmtBackend, LocalTranslateParams};
use crate::config::{
    detect_model_family, resolve_compute_device_with_caps, resolve_model_profile,
    validate_model_dir_layout, verify_file_sha256, HardwareCapabilities, LocalNmtConfig,
    LocalNmtFamily, ResolvedDevice,
};
use crate::lang_tags::to_flores200;

/// Local `CTranslate2` NMT [`Translator`] supporting model-granular switching (`FuguMT`, `OPUS-MT`,
/// `NLLB-200`), hardware device selection, SHA-256 verification, and glossary placeholder protection.
pub struct LocalNmtTranslator {
    provider_id: String,
    profile: ModelProfile,
    family: LocalNmtFamily,
    resolved_device: ResolvedDevice,
    threads: usize,
    backend: Arc<dyn LocalNmtBackend>,
}

impl std::fmt::Debug for LocalNmtTranslator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalNmtTranslator")
            .field("provider_id", &self.provider_id)
            .field("model_id", &self.profile.model_id)
            .field("family", &self.family)
            .field("resolved_device", &self.resolved_device)
            .field("threads", &self.threads)
            .finish_non_exhaustive()
    }
}

impl LocalNmtTranslator {
    /// Creates a [`LocalNmtTranslator`] after validating `config`, model directory layout,
    /// optional SHA-256 digest, and device resolution against `custom_profiles` + default catalog.
    pub fn with_backend(
        config: &LocalNmtConfig,
        custom_profiles: &[ModelProfile],
        caps: HardwareCapabilities,
        backend: Arc<dyn LocalNmtBackend>,
    ) -> Result<Self, CoreError> {
        let profile = resolve_model_profile(&config.model_id, custom_profiles)?;
        let model_dir = config.models_root_dir.join(&profile.model_id);
        let model_bin = validate_model_dir_layout(&model_dir)?;

        if config.verify_sha256 {
            verify_file_sha256(&model_bin, &profile.sha256)?;
        }

        let resolved_device = resolve_compute_device_with_caps(config.device, &profile, caps);
        Ok(Self::from_resolved(
            profile,
            resolved_device,
            config.threads,
            backend,
        ))
    }

    #[must_use]
    pub(crate) fn from_resolved(
        profile: ModelProfile,
        resolved_device: ResolvedDevice,
        threads: usize,
        backend: Arc<dyn LocalNmtBackend>,
    ) -> Self {
        let family = detect_model_family(&profile);
        let provider_id = format!("local:{}", profile.model_id);
        Self {
            provider_id,
            profile,
            family,
            resolved_device,
            threads,
            backend,
        }
    }

    /// Creates a [`LocalNmtTranslator`] backed by [`crate::backend::native::Ct2NativeBackend`].
    #[cfg(feature = "native-ct2")]
    pub fn new_native(
        config: &LocalNmtConfig,
        custom_profiles: &[ModelProfile],
    ) -> Result<Self, CoreError> {
        let caps = HardwareCapabilities::detect();
        let profile = resolve_model_profile(&config.model_id, custom_profiles)?;
        let model_dir = config.models_root_dir.join(&profile.model_id);
        let model_bin = validate_model_dir_layout(&model_dir)?;
        if config.verify_sha256 {
            verify_file_sha256(&model_bin, &profile.sha256)?;
        }
        let resolved_device = resolve_compute_device_with_caps(config.device, &profile, caps);
        let backend = Arc::new(crate::backend::native::Ct2NativeBackend::new(
            &model_dir,
            resolved_device,
            config.threads,
        )?);
        Ok(Self::from_resolved(
            profile,
            resolved_device,
            config.threads,
            backend,
        ))
    }

    /// Returns the resolved [`ModelProfile`].
    #[must_use]
    pub const fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// Returns the detected [`LocalNmtFamily`].
    #[must_use]
    pub const fn family(&self) -> LocalNmtFamily {
        self.family
    }

    /// Returns the concrete hardware [`ResolvedDevice`].
    #[must_use]
    pub const fn resolved_device(&self) -> ResolvedDevice {
        self.resolved_device
    }

    /// Returns the configured thread count (`0` = auto).
    #[must_use]
    pub const fn threads(&self) -> usize {
        self.threads
    }
}

fn validate_bilingual_pair(
    profile: &ModelProfile,
    src_lang: &LanguageCode,
    tgt_lang: &LanguageCode,
) -> Result<(), CoreError> {
    if src_lang.as_str().eq_ignore_ascii_case("auto")
        || tgt_lang.as_str().eq_ignore_ascii_case("auto")
    {
        return Err(CoreError::InvalidLanguageCode(format!(
            "local NMT model '{}' does not support 'auto' language codes",
            profile.model_id
        )));
    }

    if profile
        .supported_langs
        .iter()
        .any(|l| l.eq_ignore_ascii_case("multilingual"))
    {
        return Ok(());
    }

    let req_pair = format!("{}->{}", src_lang.as_str(), tgt_lang.as_str());
    let primary_pair = format!(
        "{}->{}",
        src_lang
            .as_str()
            .split('-')
            .next()
            .unwrap_or(src_lang.as_str()),
        tgt_lang
            .as_str()
            .split('-')
            .next()
            .unwrap_or(tgt_lang.as_str())
    );

    let matches = profile.supported_langs.iter().any(|pair| {
        pair.eq_ignore_ascii_case(&req_pair) || pair.eq_ignore_ascii_case(&primary_pair)
    });

    if !matches {
        return Err(CoreError::InvalidLanguageCode(format!(
            "language pair '{}' is not supported by local model '{}' (supported: {:?})",
            req_pair, profile.model_id, profile.supported_langs
        )));
    }

    Ok(())
}

#[async_trait]
impl Translator for LocalNmtTranslator {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    async fn translate(&self, req: TranslationRequest) -> Result<TranslationResult, CoreError> {
        let started = Instant::now();

        let (src_prefix, tgt_prefix) = match self.family {
            LocalNmtFamily::MarianOrOpus => {
                validate_bilingual_pair(&self.profile, &req.src_lang, &req.tgt_lang)?;
                (None, None)
            }
            LocalNmtFamily::Nllb200 => {
                let src_tag = to_flores200(&req.src_lang)?;
                let tgt_tag = to_flores200(&req.tgt_lang)?;
                (Some(src_tag), Some(tgt_tag))
            }
        };

        let (protected_text, guard) =
            GlossaryPlaceholderGuard::protect(&req.text, &req.context.glossary);

        let backend = Arc::clone(&self.backend);
        let protected_owned = protected_text;
        let raw_translated = tokio::task::spawn_blocking(move || {
            backend.translate(LocalTranslateParams {
                text: &protected_owned,
                source_prefix: src_prefix,
                target_prefix: tgt_prefix,
            })
        })
        .await
        .map_err(|err| {
            CoreError::Translation(format!("local NMT blocking worker task failed: {err}"))
        })??;

        let translated_text = guard.restore(&raw_translated);
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        Ok(TranslationResult {
            utterance_id: req.utterance_id,
            original_text: req.text,
            translated_text,
            provider_id: self.provider_id.clone(),
            latency_ms,
        })
    }
}
