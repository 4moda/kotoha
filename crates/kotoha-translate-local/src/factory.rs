//! Factories for `LocalModel` and composite 6-provider [`TranslatorFactory`] wiring.

use std::path::PathBuf;
use std::sync::Arc;

use kotoha_core::domain::{
    CoreError, ModelProfile, TranslationProviderConfig, Translator, TranslatorFactory,
};
use kotoha_translate_http::HttpTranslatorFactory;

use crate::backend::{DefaultLocalNmtBackendLoader, LocalNmtBackendLoader};
use crate::config::{
    resolve_compute_device_with_caps, resolve_model_profile, validate_model_dir_layout,
    verify_file_sha256, HardwareCapabilities,
};
use crate::translator::LocalNmtTranslator;

/// [`TranslatorFactory`] handling [`TranslationProviderConfig::LocalModel`] with model catalog
/// validation, SHA-256 checks, and pluggable [`LocalNmtBackendLoader`].
pub struct LocalNmtTranslatorFactory {
    models_root_dir: PathBuf,
    custom_profiles: Vec<ModelProfile>,
    verify_sha256: bool,
    capabilities: HardwareCapabilities,
    backend_loader: Arc<dyn LocalNmtBackendLoader>,
}

impl std::fmt::Debug for LocalNmtTranslatorFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalNmtTranslatorFactory")
            .field("models_root_dir", &self.models_root_dir)
            .field("custom_profiles", &self.custom_profiles.len())
            .field("verify_sha256", &self.verify_sha256)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

impl LocalNmtTranslatorFactory {
    /// Creates a [`LocalNmtTranslatorFactory`] backed by [`DefaultLocalNmtBackendLoader`].
    #[must_use]
    pub fn new(models_root_dir: impl Into<PathBuf>) -> Self {
        Self {
            models_root_dir: models_root_dir.into(),
            custom_profiles: Vec::new(),
            verify_sha256: true,
            capabilities: HardwareCapabilities::detect(),
            backend_loader: Arc::new(DefaultLocalNmtBackendLoader),
        }
    }

    /// Creates a [`LocalNmtTranslatorFactory`] with a custom [`LocalNmtBackendLoader`].
    #[must_use]
    pub fn with_loader(
        models_root_dir: impl Into<PathBuf>,
        backend_loader: Arc<dyn LocalNmtBackendLoader>,
    ) -> Self {
        Self {
            models_root_dir: models_root_dir.into(),
            custom_profiles: Vec::new(),
            verify_sha256: true,
            capabilities: HardwareCapabilities::detect(),
            backend_loader,
        }
    }

    /// Adds custom [`ModelProfile`] definitions (searched before the built-in catalog).
    #[must_use]
    pub fn with_custom_profiles(mut self, profiles: Vec<ModelProfile>) -> Self {
        self.custom_profiles = profiles;
        self
    }

    /// Controls whether `model.bin` SHA-256 verification is enforced before loading.
    #[must_use]
    pub const fn with_verify_sha256(mut self, verify: bool) -> Self {
        self.verify_sha256 = verify;
        self
    }

    /// Sets explicit host [`HardwareCapabilities`] (useful for deterministic testing).
    #[must_use]
    pub const fn with_capabilities(mut self, capabilities: HardwareCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
}

impl TranslatorFactory for LocalNmtTranslatorFactory {
    fn create(&self, config: &TranslationProviderConfig) -> Result<Arc<dyn Translator>, CoreError> {
        let TranslationProviderConfig::LocalModel {
            model_id,
            device,
            threads,
        } = config
        else {
            return Err(CoreError::NotConfigured(
                "LocalNmtTranslatorFactory only supports TranslationProviderConfig::LocalModel"
                    .to_owned(),
            ));
        };

        let profile = resolve_model_profile(model_id, &self.custom_profiles)?;
        let model_dir = self.models_root_dir.join(&profile.model_id);
        let model_bin = validate_model_dir_layout(&model_dir)?;
        let threads_val = threads.unwrap_or(0);
        if self.verify_sha256 {
            verify_file_sha256(&model_bin, &profile.sha256)?;
        }

        let resolved_device =
            resolve_compute_device_with_caps(*device, &profile, self.capabilities);
        let backend = self.backend_loader.load(
            &profile.model_id,
            &model_dir,
            resolved_device,
            threads_val,
        )?;
        let translator =
            LocalNmtTranslator::from_resolved(profile, resolved_device, threads_val, backend);
        Ok(Arc::new(translator))
    }
}

/// Composite [`TranslatorFactory`] routing all 6 [`TranslationProviderConfig`] variants across
/// `kotoha-translate-local`, `kotoha-translate-http`, and `kotoha-core`'s `MockTranslator`.
#[derive(Debug)]
pub struct CompositeTranslatorFactory {
    local_factory: LocalNmtTranslatorFactory,
    http_factory: HttpTranslatorFactory,
}

impl CompositeTranslatorFactory {
    /// Creates a [`CompositeTranslatorFactory`] wiring `local_factory` and `http_factory`.
    #[must_use]
    pub const fn new(
        local_factory: LocalNmtTranslatorFactory,
        http_factory: HttpTranslatorFactory,
    ) -> Self {
        Self {
            local_factory,
            http_factory,
        }
    }
}

impl TranslatorFactory for CompositeTranslatorFactory {
    fn create(&self, config: &TranslationProviderConfig) -> Result<Arc<dyn Translator>, CoreError> {
        match config {
            TranslationProviderConfig::LocalModel { .. } => self.local_factory.create(config),
            TranslationProviderConfig::OpenAiCompatible { .. }
            | TranslationProviderConfig::DeepL { .. }
            | TranslationProviderConfig::GoogleCloud { .. }
            | TranslationProviderConfig::GasWebhook { .. }
            | TranslationProviderConfig::Mock { .. } => self.http_factory.create(config),
        }
    }
}
