//! Local NMT inference backend trait and optional `ct2rs` native `CTranslate2` implementation.

use std::path::Path;

use kotoha_core::domain::CoreError;

use crate::config::ResolvedDevice;

/// Parameters for a single local NMT inference pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTranslateParams<'a> {
    /// Protected source text (with glossary placeholders already applied).
    pub text: &'a str,
    /// Optional source prefix token (e.g., Flores-200 tag `"eng_Latn"` for NLLB-200).
    pub source_prefix: Option<&'a str>,
    /// Optional target prefix token (e.g., Flores-200 tag `"jpn_Jpan"` for NLLB-200).
    pub target_prefix: Option<&'a str>,
}

/// Synchronous inference backend abstracted over `ct2rs::Translator` (or test doubles).
pub trait LocalNmtBackend: Send + Sync {
    /// Translates `params.text` using optional model-family source/target prefix tokens.
    fn translate(&self, params: LocalTranslateParams<'_>) -> Result<String, CoreError>;
}

/// Native CTranslate2 (`ct2rs`) backend enabled via `--features native-ct2`.
#[cfg(feature = "native-ct2")]
pub mod native {
    use std::path::Path;

    use ct2rs::{Config, Device, TranslationOptions, Translator};
    use kotoha_core::domain::CoreError;

    use super::{LocalNmtBackend, LocalTranslateParams};
    use crate::config::ResolvedDevice;

    /// Native `ct2rs::Translator` wrapper configured for the selected device and thread count.
    pub struct Ct2NativeBackend {
        translator: Translator<ct2rs::tokenizers::auto::Tokenizer>,
        device: ResolvedDevice,
    }

    impl std::fmt::Debug for Ct2NativeBackend {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Ct2NativeBackend")
                .field("device", &self.device)
                .finish_non_exhaustive()
        }
    }

    impl Ct2NativeBackend {
        /// Loads a CTranslate2 model directory using `ct2rs::Translator::new`.
        pub fn new(
            model_dir: &Path,
            device: ResolvedDevice,
            threads: usize,
        ) -> Result<Self, CoreError> {
            let ct2_device = match device {
                ResolvedDevice::Cuda => Device::CUDA,
                ResolvedDevice::Cpu | ResolvedDevice::Metal => Device::CPU,
            };

            let config = Config {
                device: ct2_device,
                num_threads_per_replica: threads,
                ..Config::default()
            };

            let translator = Translator::new(model_dir, &config).map_err(|err| {
                CoreError::Translation(format!(
                    "failed to initialize ct2rs Translator at '{}': {err}",
                    model_dir.display()
                ))
            })?;

            Ok(Self { translator, device })
        }
    }

    impl LocalNmtBackend for Ct2NativeBackend {
        fn translate(&self, params: LocalTranslateParams<'_>) -> Result<String, CoreError> {
            let sources = [params.text];
            let options = TranslationOptions::default();

            let results = match (params.source_prefix, params.target_prefix) {
                (Some(src_tag), Some(tgt_tag)) => {
                    let source_prefixes = vec![vec![src_tag]];
                    let target_prefixes = vec![vec![tgt_tag]];
                    self.translator
                        .translate_batch_with_source_and_target_prefix(
                            &sources,
                            &source_prefixes,
                            &target_prefixes,
                            &options,
                            None,
                        )
                }
                (Some(src_tag), None) => {
                    let source_prefixes = vec![vec![src_tag]];
                    self.translator.translate_batch_with_source_prefix(
                        &sources,
                        &source_prefixes,
                        &options,
                        None,
                    )
                }
                (None, Some(tgt_tag)) => {
                    let target_prefixes = vec![vec![tgt_tag]];
                    self.translator.translate_batch_with_target_prefix(
                        &sources,
                        &target_prefixes,
                        &options,
                        None,
                    )
                }
                (None, None) => self.translator.translate_batch(&sources, &options, None),
            }
            .map_err(|err| CoreError::Translation(format!("ct2rs translation failed: {err}")))?;

            results
                .into_iter()
                .next()
                .map(|(text, _score)| text)
                .ok_or_else(|| {
                    CoreError::Translation("ct2rs returned empty result batch".to_owned())
                })
        }
    }
}

/// Trait allowing factory-driven instantiation of [`LocalNmtBackend`] from a validated model dir.
pub trait LocalNmtBackendLoader: Send + Sync {
    /// Instantiates a [`LocalNmtBackend`] for `model_dir` on `device` with `threads`.
    fn load(
        &self,
        model_id: &str,
        model_dir: &Path,
        device: ResolvedDevice,
        threads: usize,
    ) -> Result<std::sync::Arc<dyn LocalNmtBackend>, CoreError>;
}

/// Default native loader (returns [`CoreError::NotConfigured`] if compiled without `native-ct2`).
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultLocalNmtBackendLoader;

impl LocalNmtBackendLoader for DefaultLocalNmtBackendLoader {
    #[cfg(feature = "native-ct2")]
    fn load(
        &self,
        _model_id: &str,
        model_dir: &Path,
        device: ResolvedDevice,
        threads: usize,
    ) -> Result<std::sync::Arc<dyn LocalNmtBackend>, CoreError> {
        let backend = native::Ct2NativeBackend::new(model_dir, device, threads)?;
        Ok(std::sync::Arc::new(backend))
    }

    #[cfg(not(feature = "native-ct2"))]
    fn load(
        &self,
        model_id: &str,
        _model_dir: &Path,
        _device: ResolvedDevice,
        _threads: usize,
    ) -> Result<std::sync::Arc<dyn LocalNmtBackend>, CoreError> {
        Err(CoreError::NotConfigured(format!(
            "native CTranslate2 backend for model '{model_id}' requires compiling kotoha-translate-local with feature 'native-ct2'"
        )))
    }
}
