//! Runtime hot-swappable translation provider & model profile catalog manager.

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::domain::{
    default_model_catalog, CoreError, ModelProfile, TranslationProviderConfig, TranslationRequest,
    TranslationResult, Translator, TranslatorFactory,
};

struct ActiveTranslatorState {
    translator: Arc<dyn Translator>,
    config: TranslationProviderConfig,
}

/// Manages the active [`Translator`] behind an async lock for zero-downtime hot-swapping,
/// alongside the selectable [`ModelProfile`] catalog.
pub struct TranslatorManager {
    state: RwLock<ActiveTranslatorState>,
    model_catalog: Vec<ModelProfile>,
}

impl TranslatorManager {
    /// Creates a new [`TranslatorManager`] initialized with `initial_translator` and `initial_config`.
    #[must_use]
    pub fn new(
        initial_translator: Arc<dyn Translator>,
        initial_config: TranslationProviderConfig,
    ) -> Self {
        Self {
            state: RwLock::new(ActiveTranslatorState {
                translator: initial_translator,
                config: initial_config,
            }),
            model_catalog: default_model_catalog(),
        }
    }

    /// Creates a [`TranslatorManager`] from `initial_config` using `factory`.
    pub fn from_config(
        initial_config: TranslationProviderConfig,
        factory: &dyn TranslatorFactory,
    ) -> Result<Self, CoreError> {
        let translator = factory.create(&initial_config)?;
        Ok(Self::new(translator, initial_config))
    }

    /// Hot-swaps the active translation provider or local model profile without interrupting IPC/STT.
    pub async fn switch_provider(
        &self,
        config: TranslationProviderConfig,
        factory: &dyn TranslatorFactory,
    ) -> Result<(), CoreError> {
        if let TranslationProviderConfig::LocalModel { model_id, .. } = &config {
            if self.find_model_profile(model_id).is_none() {
                return Err(CoreError::UnknownModelProfile(model_id.clone()));
            }
        }

        let next_translator = factory.create(&config)?;
        let mut guard = self.state.write().await;
        guard.translator = next_translator;
        guard.config = config;
        drop(guard);
        Ok(())
    }

    /// Returns a cloned handle to the currently active [`Translator`].
    pub async fn current_translator(&self) -> Arc<dyn Translator> {
        Arc::clone(&self.state.read().await.translator)
    }

    /// Returns a clone of the currently active [`TranslationProviderConfig`].
    pub async fn current_config(&self) -> TranslationProviderConfig {
        self.state.read().await.config.clone()
    }

    /// Returns the provider ID of the currently active [`Translator`].
    pub async fn current_provider_id(&self) -> String {
        self.state.read().await.translator.provider_id().to_owned()
    }

    /// Translates `req` using whichever [`Translator`] is currently active.
    pub async fn translate(&self, req: TranslationRequest) -> Result<TranslationResult, CoreError> {
        let translator = self.current_translator().await;
        translator.translate(req).await
    }

    /// Lists all available [`ModelProfile`] entries in the catalog.
    #[must_use]
    pub fn available_models(&self) -> &[ModelProfile] {
        &self.model_catalog
    }

    /// Looks up a [`ModelProfile`] by `model_id`.
    #[must_use]
    pub fn find_model_profile(&self, model_id: &str) -> Option<&ModelProfile> {
        self.model_catalog.iter().find(|m| m.model_id == model_id)
    }
}
