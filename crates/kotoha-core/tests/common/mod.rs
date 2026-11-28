//! Shared test helpers and mock adapters for `kotoha-core` integration tests.

#![allow(dead_code, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use kotoha_core::domain::{
    CoreError, LanguageCode, SpeakerKey, SttEngine, SttHotwordsConfig, SttStream, SubtitleEntry,
    SubtitleSink, TranscriptEvent, TranslationRequest, TranslationResult, Translator,
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

/// How [`ScriptedTranslator`] answers a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslatorScript {
    /// Every request fails with a simulated HTTP 429.
    AlwaysFail,
    /// Requests with an odd `utterance_id` fail; even ones succeed.
    FailOddUtterances,
    /// Requests with an odd `utterance_id` panic; even ones succeed.
    PanicOnOddUtterances,
}

/// Translator mock that fails or panics according to a [`TranslatorScript`] and records every
/// request it receives (including its context snapshot).
pub struct ScriptedTranslator {
    pub script: TranslatorScript,
    pub captured_requests: Arc<StdMutex<Vec<TranslationRequest>>>,
}

impl ScriptedTranslator {
    pub fn new(script: TranslatorScript) -> Self {
        Self {
            script,
            captured_requests: Arc::new(StdMutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl Translator for ScriptedTranslator {
    fn provider_id(&self) -> &'static str {
        "scripted"
    }

    async fn translate(&self, req: TranslationRequest) -> Result<TranslationResult, CoreError> {
        self.captured_requests
            .lock()
            .expect("lock")
            .push(req.clone());
        let odd = req.utterance_id.0 % 2 == 1;
        match self.script {
            TranslatorScript::AlwaysFail => {
                return Err(CoreError::Translation(
                    "scripted rate limited (HTTP 429 Too Many Requests): secret-detail".to_owned(),
                ));
            }
            TranslatorScript::FailOddUtterances if odd => {
                return Err(CoreError::Translation("scripted timeout".to_owned()));
            }
            TranslatorScript::PanicOnOddUtterances if odd => {
                panic!("scripted translator panic");
            }
            TranslatorScript::FailOddUtterances | TranslatorScript::PanicOnOddUtterances => {}
        }
        Ok(TranslationResult {
            utterance_id: req.utterance_id,
            original_text: req.text.clone(),
            translated_text: format!("[ja] {}", req.text),
            provider_id: "scripted".to_owned(),
            latency_ms: 0,
        })
    }
}

/// STT engine wrapping [`MockSttEngine`] whose streams fail `accept_audio` a configured number of
/// times in total (on the `fail_on_call`-th call of a stream) and which counts created streams.
pub struct FlakySttEngine {
    pub inner: MockSttEngine,
    pub fail_on_call: usize,
    pub failures_remaining: Arc<AtomicUsize>,
    pub streams_created: Arc<AtomicUsize>,
}

impl FlakySttEngine {
    pub fn new(inner: MockSttEngine, fail_on_call: usize, failures: usize) -> Self {
        Self {
            inner,
            fail_on_call,
            failures_remaining: Arc::new(AtomicUsize::new(failures)),
            streams_created: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl SttEngine for FlakySttEngine {
    fn name(&self) -> &'static str {
        "flaky-mock"
    }

    fn create_stream(
        &self,
        speaker_key: SpeakerKey,
        src_lang: &LanguageCode,
    ) -> Result<Box<dyn SttStream>, CoreError> {
        self.streams_created.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FlakySttStream {
            inner: self.inner.create_stream(speaker_key, src_lang)?,
            calls: 0,
            fail_on_call: self.fail_on_call,
            failures_remaining: Arc::clone(&self.failures_remaining),
        }))
    }
}

struct FlakySttStream {
    inner: Box<dyn SttStream>,
    calls: usize,
    fail_on_call: usize,
    failures_remaining: Arc<AtomicUsize>,
}

impl SttStream for FlakySttStream {
    fn accept_audio(
        &mut self,
        samples_16k_mono: &[f32],
        timestamp_ms: u64,
    ) -> Result<Vec<TranscriptEvent>, CoreError> {
        self.calls += 1;
        if self.calls == self.fail_on_call
            && self
                .failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
        {
            return Err(CoreError::Stt("simulated decoder failure".to_owned()));
        }
        self.inner.accept_audio(samples_16k_mono, timestamp_ms)
    }

    fn flush(&mut self, timestamp_ms: u64) -> Result<Vec<TranscriptEvent>, CoreError> {
        self.inner.flush(timestamp_ms)
    }
}

/// Subtitle sink that rejects every `emit` / `clear_source` call but counts the attempts, like a
/// detached UI.
#[derive(Default)]
pub struct FailingSubtitleSink {
    pub emit_attempts: AtomicUsize,
    pub clear_attempts: AtomicUsize,
}

#[async_trait]
impl SubtitleSink for FailingSubtitleSink {
    async fn emit(&self, _entry: SubtitleEntry) -> Result<(), CoreError> {
        self.emit_attempts.fetch_add(1, Ordering::SeqCst);
        Err(CoreError::Sink("UI detached".to_owned()))
    }

    async fn clear_source(&self, _source: &str) -> Result<(), CoreError> {
        self.clear_attempts.fetch_add(1, Ordering::SeqCst);
        Err(CoreError::Sink("UI detached".to_owned()))
    }
}

/// How a [`ScriptedSttEngine`] stream misbehaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SttScript {
    /// Every `accept_audio` blocks the calling thread for the given time (CPU-bound decoder).
    Slow(Duration),
    /// The `n`-th `accept_audio` call across all streams panics (once).
    PanicOnCall(usize),
    /// `flush` forgets the open utterance and returns nothing (like a reaped backend stream).
    ForgetOnFlush,
}

/// STT engine wrapping [`MockSttEngine`] whose streams follow an [`SttScript`].
pub struct ScriptedSttEngine {
    pub inner: MockSttEngine,
    pub script: SttScript,
    pub calls: Arc<AtomicUsize>,
    pub streams_created: Arc<AtomicUsize>,
}

impl ScriptedSttEngine {
    pub fn new(inner: MockSttEngine, script: SttScript) -> Self {
        Self {
            inner,
            script,
            calls: Arc::new(AtomicUsize::new(0)),
            streams_created: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl SttEngine for ScriptedSttEngine {
    fn name(&self) -> &'static str {
        "scripted-stt"
    }

    fn create_stream(
        &self,
        speaker_key: SpeakerKey,
        src_lang: &LanguageCode,
    ) -> Result<Box<dyn SttStream>, CoreError> {
        self.streams_created.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(ScriptedSttStream {
            inner: self.inner.create_stream(speaker_key, src_lang)?,
            script: self.script,
            calls: Arc::clone(&self.calls),
        }))
    }
}

struct ScriptedSttStream {
    inner: Box<dyn SttStream>,
    script: SttScript,
    calls: Arc<AtomicUsize>,
}

impl SttStream for ScriptedSttStream {
    fn accept_audio(
        &mut self,
        samples_16k_mono: &[f32],
        timestamp_ms: u64,
    ) -> Result<Vec<TranscriptEvent>, CoreError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        match self.script {
            SttScript::Slow(delay) => std::thread::sleep(delay),
            SttScript::PanicOnCall(n) if n == call => panic!("scripted STT panic"),
            SttScript::PanicOnCall(_) | SttScript::ForgetOnFlush => {}
        }
        self.inner.accept_audio(samples_16k_mono, timestamp_ms)
    }

    fn flush(&mut self, timestamp_ms: u64) -> Result<Vec<TranscriptEvent>, CoreError> {
        if self.script == SttScript::ForgetOnFlush {
            return Ok(Vec::new());
        }
        self.inner.flush(timestamp_ms)
    }
}

/// STT engine whose `create_stream` always fails, counting the attempts.
#[derive(Default)]
pub struct UnavailableSttEngine {
    pub attempts: Arc<AtomicUsize>,
}

impl SttEngine for UnavailableSttEngine {
    fn name(&self) -> &'static str {
        "unavailable-stt"
    }

    fn create_stream(
        &self,
        _speaker_key: SpeakerKey,
        _src_lang: &LanguageCode,
    ) -> Result<Box<dyn SttStream>, CoreError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(CoreError::Stt("model failed to load".to_owned()))
    }
}

/// Translator whose latency depends on the utterance id and which records every request.
pub struct DelayByUtteranceTranslator {
    pub delay_for: fn(u64) -> Duration,
    pub captured_requests: Arc<StdMutex<Vec<TranslationRequest>>>,
    pub in_flight: Arc<AtomicUsize>,
    pub peak_in_flight: Arc<AtomicUsize>,
}

impl DelayByUtteranceTranslator {
    pub fn new(delay_for: fn(u64) -> Duration) -> Self {
        Self {
            delay_for,
            captured_requests: Arc::new(StdMutex::new(Vec::new())),
            in_flight: Arc::new(AtomicUsize::new(0)),
            peak_in_flight: Arc::new(AtomicUsize::new(0)),
        }
    }
}

/// Decrements the in-flight counter even when the call is cancelled.
struct InFlightGuard(Arc<AtomicUsize>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl Translator for DelayByUtteranceTranslator {
    fn provider_id(&self) -> &'static str {
        "delay-by-utterance"
    }

    async fn translate(&self, req: TranslationRequest) -> Result<TranslationResult, CoreError> {
        self.captured_requests
            .lock()
            .expect("lock")
            .push(req.clone());
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_in_flight.fetch_max(now, Ordering::SeqCst);
        let _guard = InFlightGuard(Arc::clone(&self.in_flight));
        tokio::time::sleep((self.delay_for)(req.utterance_id.0)).await;
        Ok(TranslationResult {
            // Deliberately wrong: the pipeline must correlate by the request's utterance id.
            utterance_id: kotoha_core::domain::UtteranceId(u64::MAX),
            original_text: req.text.clone(),
            translated_text: format!("[ja] {}", req.text),
            provider_id: "delay-by-utterance".to_owned(),
            latency_ms: 0,
        })
    }
}

/// Sink recording every entry with the (real) instant it was emitted.
#[derive(Default)]
pub struct TimestampingSink {
    pub entries: StdMutex<Vec<(std::time::Instant, SubtitleEntry)>>,
}

#[async_trait]
impl SubtitleSink for TimestampingSink {
    async fn emit(&self, entry: SubtitleEntry) -> Result<(), CoreError> {
        self.entries
            .lock()
            .expect("lock")
            .push((std::time::Instant::now(), entry));
        Ok(())
    }
}
