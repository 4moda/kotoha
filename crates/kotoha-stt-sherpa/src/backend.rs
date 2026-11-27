//! Recognizer backend traits and optional native `sherpa-onnx` implementation.

use kotoha_core::domain::CoreError;

use crate::config::SherpaSttConfig;
use crate::hotwords::HotwordsSnapshot;

/// Minimal recognizer hypothesis returned by a [`RecognizerStreamBackend`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecognizerHypothesis {
    /// Recognized text accumulated in the current stream segment.
    pub text: String,
    /// Native finality flag reported by the underlying recognizer (if any).
    pub is_final: bool,
}

/// Stateful per-speaker stream handle abstracted over `sherpa-onnx::OnlineStream` (or test mocks).
pub trait RecognizerStreamBackend: Send {
    /// Feeds `samples` at `sample_rate` Hz into the stream.
    fn accept_waveform(&mut self, sample_rate: i32, samples: &[f32]) -> Result<(), CoreError>;

    /// Marks end of input so trailing buffered frames are flushed for decoding.
    fn input_finished(&mut self) -> Result<(), CoreError>;

    /// Returns `true` when enough acoustic frames are buffered for another decode step.
    fn is_ready(&mut self) -> Result<bool, CoreError>;

    /// Advances the recognizer by one decoding step.
    fn decode(&mut self) -> Result<(), CoreError>;

    /// Retrieves the current hypothesis text, if available.
    fn get_result(&mut self) -> Result<Option<RecognizerHypothesis>, CoreError>;

    /// Returns `true` if endpoint detection triggered at the end of the current utterance.
    fn is_endpoint(&mut self) -> Result<bool, CoreError>;

    /// Resets stream acoustic/decoder state for the next utterance.
    fn reset(&mut self) -> Result<(), CoreError>;
}

/// Backend recognizer factory abstracted over `sherpa-onnx::OnlineRecognizer` (or test mocks).
pub trait RecognizerBackend: Send + Sync {
    /// Creates a new per-speaker stream configured with the supplied [`HotwordsSnapshot`].
    fn create_stream(
        &self,
        hotwords: &HotwordsSnapshot,
    ) -> Result<Box<dyn RecognizerStreamBackend>, CoreError>;

    /// Rebuilds or reconfigures the recognizer when hotwords/decoding parameters change.
    fn on_hotwords_updated(
        &self,
        _config: &SherpaSttConfig,
        _hotwords: &HotwordsSnapshot,
    ) -> Result<(), CoreError> {
        Ok(())
    }
}

/// Native `sherpa-onnx` backend adapter enabled via `--features native-sherpa`.
#[cfg(feature = "native-sherpa")]
pub mod native {
    use std::sync::{Arc, RwLock};

    use kotoha_core::domain::CoreError;
    use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig, OnlineStream};

    use super::{RecognizerBackend, RecognizerHypothesis, RecognizerStreamBackend};
    use crate::config::{SherpaModelFamily, SherpaSttConfig};
    use crate::hotwords::HotwordsSnapshot;

    /// Native `sherpa-onnx` streaming recognizer backend wrapping [`OnlineRecognizer`].
    ///
    /// Recognizer-level hotwords are configured via `hotwords_file` / `hotwords_buf` inside
    /// [`build_native_config`] and reloaded whenever [`super::RecognizerBackend::on_hotwords_updated`]
    /// succeeds, so streams are created directly with `OnlineRecognizer::create_stream()` without
    /// redundantly compiling the same hotwords list per stream.
    pub struct NativeSherpaBackend {
        recognizer: RwLock<Arc<OnlineRecognizer>>,
    }

    impl std::fmt::Debug for NativeSherpaBackend {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("NativeSherpaBackend")
                .finish_non_exhaustive()
        }
    }

    impl NativeSherpaBackend {
        /// Validates model files and instantiates a [`NativeSherpaBackend`].
        pub fn new(
            config: &SherpaSttConfig,
            hotwords: &HotwordsSnapshot,
        ) -> Result<Self, CoreError> {
            config.validate_files_exist()?;
            let recognizer = build_native_recognizer(config, hotwords)?;
            Ok(Self {
                recognizer: RwLock::new(Arc::new(recognizer)),
            })
        }
    }

    impl RecognizerBackend for NativeSherpaBackend {
        fn create_stream(
            &self,
            _hotwords: &HotwordsSnapshot,
        ) -> Result<Box<dyn RecognizerStreamBackend>, CoreError> {
            let recognizer = {
                let guard = self.recognizer.read().map_err(|_| {
                    CoreError::Stt("native sherpa recognizer lock poisoned".to_owned())
                })?;
                Arc::clone(&guard)
            };

            let stream = recognizer.create_stream();
            Ok(Box::new(NativeSherpaStream { recognizer, stream }))
        }

        fn on_hotwords_updated(
            &self,
            config: &SherpaSttConfig,
            hotwords: &HotwordsSnapshot,
        ) -> Result<(), CoreError> {
            let rebuilt = build_native_recognizer(config, hotwords)?;
            let mut guard = self
                .recognizer
                .write()
                .map_err(|_| CoreError::Stt("native sherpa recognizer lock poisoned".to_owned()))?;
            *guard = Arc::new(rebuilt);
            drop(guard);
            Ok(())
        }
    }

    struct NativeSherpaStream {
        recognizer: Arc<OnlineRecognizer>,
        stream: OnlineStream,
    }

    impl RecognizerStreamBackend for NativeSherpaStream {
        fn accept_waveform(&mut self, sample_rate: i32, samples: &[f32]) -> Result<(), CoreError> {
            self.stream.accept_waveform(sample_rate, samples);
            Ok(())
        }

        fn input_finished(&mut self) -> Result<(), CoreError> {
            self.stream.input_finished();
            Ok(())
        }

        fn is_ready(&mut self) -> Result<bool, CoreError> {
            Ok(self.recognizer.is_ready(&self.stream))
        }

        fn decode(&mut self) -> Result<(), CoreError> {
            self.recognizer.decode(&self.stream);
            Ok(())
        }

        fn get_result(&mut self) -> Result<Option<RecognizerHypothesis>, CoreError> {
            Ok(self
                .recognizer
                .get_result(&self.stream)
                .map(|res| RecognizerHypothesis {
                    text: res.text,
                    is_final: res.is_final,
                }))
        }

        fn is_endpoint(&mut self) -> Result<bool, CoreError> {
            Ok(self.recognizer.is_endpoint(&self.stream))
        }

        fn reset(&mut self) -> Result<(), CoreError> {
            self.recognizer.reset(&self.stream);
            Ok(())
        }
    }

    /// Translates [`SherpaSttConfig`] and [`HotwordsSnapshot`] into `sherpa_onnx::OnlineRecognizerConfig`.
    #[must_use]
    pub fn build_native_config(
        config: &SherpaSttConfig,
        hotwords: &HotwordsSnapshot,
    ) -> OnlineRecognizerConfig {
        let mut native = OnlineRecognizerConfig::default();
        native.feat_config.sample_rate = config.sample_rate;
        native.feat_config.feature_dim = config.feature_dim;

        native.model_config.num_threads = config.num_threads;
        native.model_config.provider = Some(config.provider_name().to_owned());
        native.model_config.debug = config.debug;

        match &config.model {
            SherpaModelFamily::ZipformerTransducer {
                encoder,
                decoder,
                joiner,
                tokens,
                bpe_vocab,
                modeling_unit,
            } => {
                native.model_config.transducer.encoder =
                    Some(encoder.to_string_lossy().into_owned());
                native.model_config.transducer.decoder =
                    Some(decoder.to_string_lossy().into_owned());
                native.model_config.transducer.joiner = Some(joiner.to_string_lossy().into_owned());
                native.model_config.tokens = Some(tokens.to_string_lossy().into_owned());
                native.model_config.bpe_vocab =
                    bpe_vocab.as_ref().map(|p| p.to_string_lossy().into_owned());
                native.model_config.modeling_unit =
                    modeling_unit.map(|u| u.as_sherpa_str().to_owned());
            }
            SherpaModelFamily::Paraformer {
                encoder,
                decoder,
                tokens,
            } => {
                native.model_config.paraformer.encoder =
                    Some(encoder.to_string_lossy().into_owned());
                native.model_config.paraformer.decoder =
                    Some(decoder.to_string_lossy().into_owned());
                native.model_config.tokens = Some(tokens.to_string_lossy().into_owned());
            }
        }

        let has_hotwords = !hotwords.is_empty();
        let method = config.effective_decoding_method(has_hotwords);
        native.decoding_method = Some(SherpaSttConfig::decoding_method_str(method).to_owned());
        native.max_active_paths = config.max_active_paths;

        native.enable_endpoint = config.endpoint.enable;
        native.rule1_min_trailing_silence = config.endpoint.rule1_min_trailing_silence;
        native.rule2_min_trailing_silence = config.endpoint.rule2_min_trailing_silence;
        native.rule3_min_utterance_length = config.endpoint.rule3_min_utterance_length;

        if config.model.supports_hotwords() {
            native.hotwords_score = if has_hotwords {
                hotwords.hotwords_score
            } else {
                config.hotwords_score
            };
            if let Some(file_path) = &hotwords.file_path {
                if has_hotwords {
                    native.hotwords_file = Some(file_path.to_string_lossy().into_owned());
                }
            } else if has_hotwords {
                native.hotwords_buf = Some(hotwords.formatted_text.as_bytes().to_vec());
            }
        }

        native
    }

    fn build_native_recognizer(
        config: &SherpaSttConfig,
        hotwords: &HotwordsSnapshot,
    ) -> Result<OnlineRecognizer, CoreError> {
        let native_cfg = build_native_config(config, hotwords);
        OnlineRecognizer::create(&native_cfg).ok_or_else(|| {
            CoreError::Stt(format!(
                "sherpa-onnx OnlineRecognizer::create failed for provider '{}'",
                config.provider_name()
            ))
        })
    }
}
