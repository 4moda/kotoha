//! Shared test helpers and mock adapters for `kotoha-core` integration tests.

#![allow(dead_code, clippy::expect_used)]

use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use kotoha_core::domain::{
    CoreError, LanguageCode, SpeakerKey, SttEngine, SttHotwordsConfig, SttStream,
    TranslationRequest, TranslationResult, Translator,
};
use kotoha_core::infrastructure::MockSttEngine;

pub fn test_endpoint(tag: &str) -> String {
    let pid = std::process::id();
    if cfg!(windows) {
        format!(r"\\.\pipe\kt_core_{pid}_{tag}")
    } else {
        // Keep path well under macOS 104-byte sun_path limit.
        format!("/tmp/kt_{pid}_{tag}.sock")
    }
}

pub struct HotwordTrackingSttEngine {
    pub inner: MockSttEngine,
    pub hotword_updates: Arc<StdMutex<Vec<SttHotwordsConfig>>>,
}

impl HotwordTrackingSttEngine {
    pub fn new() -> Self {
        Self {
            inner: MockSttEngine::new(0.01, 1),
            hotword_updates: Arc::new(StdMutex::new(Vec::new())),
        }
    }
}

impl Default for HotwordTrackingSttEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SttEngine for HotwordTrackingSttEngine {
    fn name(&self) -> &'static str {
        "hotword-tracking-mock"
    }

    fn hotwords_supported(&self) -> bool {
        true
    }

    fn update_hotwords(&self, config: &SttHotwordsConfig) -> Result<(), CoreError> {
        self.hotword_updates
            .lock()
            .expect("lock")
            .push(config.clone());
        Ok(())
    }

    fn create_stream(
        &self,
        speaker_key: SpeakerKey,
        src_lang: &LanguageCode,
    ) -> Result<Box<dyn SttStream>, CoreError> {
        self.inner.create_stream(speaker_key, src_lang)
    }
}

pub struct CapturingTranslator {
    pub captured_requests: Arc<StdMutex<Vec<TranslationRequest>>>,
}

#[async_trait]
impl Translator for CapturingTranslator {
    fn provider_id(&self) -> &'static str {
        "capturing-mock"
    }

    async fn translate(&self, req: TranslationRequest) -> Result<TranslationResult, CoreError> {
        self.captured_requests
            .lock()
            .expect("lock")
            .push(req.clone());
        Ok(TranslationResult {
            utterance_id: req.utterance_id,
            original_text: req.text.clone(),
            translated_text: format!("[ja] {}", req.text),
            provider_id: "capturing-mock".to_owned(),
            latency_ms: 0,
        })
    }
}

pub struct DelayedFailingOrTaggedTranslator {
    pub delay: Duration,
    pub should_fail: bool,
}

#[async_trait]
impl Translator for DelayedFailingOrTaggedTranslator {
    fn provider_id(&self) -> &'static str {
        "delayed"
    }

    async fn translate(&self, req: TranslationRequest) -> Result<TranslationResult, CoreError> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        if self.should_fail {
            return Err(CoreError::Translation("simulated failure".to_owned()));
        }
        Ok(TranslationResult {
            utterance_id: req.utterance_id,
            original_text: req.text.clone(),
            translated_text: format!("[ja] {}", req.text),
            provider_id: "delayed".to_owned(),
            latency_ms: 0,
        })
    }
}

pub struct FailingHotwordSttEngine;

impl SttEngine for FailingHotwordSttEngine {
    fn name(&self) -> &'static str {
        "failing-hw"
    }

    fn hotwords_supported(&self) -> bool {
        true
    }

    fn update_hotwords(&self, _config: &SttHotwordsConfig) -> Result<(), CoreError> {
        Err(CoreError::Stt("simulated hotwords error".to_owned()))
    }

    fn create_stream(
        &self,
        speaker_key: SpeakerKey,
        src_lang: &LanguageCode,
    ) -> Result<Box<dyn SttStream>, CoreError> {
        MockSttEngine::default().create_stream(speaker_key, src_lang)
    }
}
