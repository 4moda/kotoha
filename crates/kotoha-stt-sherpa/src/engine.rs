//! Streaming `SherpaSttEngine` and per-speaker `SherpaSttStream` implementing `kotoha_core` ports.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use kotoha_core::domain::{
    CoreError, LanguageCode, SpeakerKey, SttDecodingMethod, SttEngine, SttHotwordsConfig,
    SttStream, TranscriptEvent, UtteranceId,
};

use crate::backend::{RecognizerBackend, RecognizerStreamBackend};
use crate::config::SherpaSttConfig;
use crate::hotwords::{HotwordsRegistry, HotwordsSnapshot};

type SharedStreamBackend = Arc<Mutex<Option<Box<dyn RecognizerStreamBackend>>>>;

struct StreamActivityEntry {
    speaker_key: SpeakerKey,
    last_activity: Mutex<Instant>,
    reaped: AtomicBool,
    backend_stream: SharedStreamBackend,
}

impl std::fmt::Debug for StreamActivityEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamActivityEntry")
            .field("speaker_key", &self.speaker_key)
            .field("reaped", &self.reaped.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl StreamActivityEntry {
    fn touch(&self, now: Instant) {
        if let Ok(mut guard) = self.last_activity.lock() {
            *guard = now;
        }
    }

    fn is_idle_at(&self, now: Instant, timeout: std::time::Duration) -> bool {
        self.last_activity
            .lock()
            .is_ok_and(|last| now.saturating_duration_since(*last) >= timeout)
    }

    fn mark_reaped(&self) {
        self.reaped.store(true, Ordering::Release);
    }

    fn is_reaped(&self) -> bool {
        self.reaped.load(Ordering::Acquire)
    }

    fn drop_backend_stream(&self) {
        if let Ok(mut guard) = self.backend_stream.lock() {
            drop(guard.take());
        }
    }
}

/// `sherpa-onnx` streaming [`SttEngine`] adapter supporting contextual hotwords and idle stream
/// lifecycle management.
pub struct SherpaSttEngine {
    base_config: SherpaSttConfig,
    supports_hotwords: bool,
    config: RwLock<SherpaSttConfig>,
    backend: Arc<dyn RecognizerBackend>,
    hotwords: Arc<HotwordsRegistry>,
    next_utterance_id: Arc<AtomicU64>,
    next_stream_id: AtomicU64,
    active_streams: Arc<Mutex<HashMap<u64, Arc<StreamActivityEntry>>>>,
}

impl std::fmt::Debug for SherpaSttEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SherpaSttEngine")
            .field("supports_hotwords", &self.supports_hotwords)
            .field("hotwords_generation", &self.hotwords_generation())
            .field("active_streams", &self.active_stream_count())
            .finish_non_exhaustive()
    }
}

impl SherpaSttEngine {
    /// Creates a [`SherpaSttEngine`] backed by the optional native `sherpa-onnx` library.
    #[cfg(feature = "native-sherpa")]
    pub fn new_native(config: SherpaSttConfig) -> Result<Self, CoreError> {
        config.validate_files_exist()?;
        let hotwords = Arc::new(HotwordsRegistry::in_memory(config.hotwords_score));
        let snapshot = hotwords.snapshot()?;
        let backend = Arc::new(crate::backend::native::NativeSherpaBackend::new(
            &config, &snapshot,
        )?);
        Self::with_backend_and_registry(config, backend, hotwords)
    }

    /// Creates a native [`SherpaSttEngine`] that mirrors updated hotwords to a managed temp file in
    /// `hotwords_dir`.
    #[cfg(feature = "native-sherpa")]
    pub fn new_native_with_hotwords_dir(
        config: SherpaSttConfig,
        hotwords_dir: &Path,
    ) -> Result<Self, CoreError> {
        config.validate_files_exist()?;
        let hotwords = Arc::new(HotwordsRegistry::with_managed_dir(
            hotwords_dir,
            config.hotwords_score,
        )?);
        let snapshot = hotwords.snapshot()?;
        let backend = Arc::new(crate::backend::native::NativeSherpaBackend::new(
            &config, &snapshot,
        )?);
        Self::with_backend_and_registry(config, backend, hotwords)
    }

    /// Creates a [`SherpaSttEngine`] from an explicit [`RecognizerBackend`] (useful for unit tests
    /// or custom recognizers).
    pub fn with_backend(
        config: SherpaSttConfig,
        backend: Arc<dyn RecognizerBackend>,
    ) -> Result<Self, CoreError> {
        config.validate()?;
        let hotwords = Arc::new(HotwordsRegistry::in_memory(config.hotwords_score));
        Self::with_backend_and_registry(config, backend, hotwords)
    }

    /// Creates a [`SherpaSttEngine`] from an explicit [`RecognizerBackend`] and a managed hotwords
    /// directory.
    pub fn with_backend_and_hotwords_dir(
        config: SherpaSttConfig,
        backend: Arc<dyn RecognizerBackend>,
        hotwords_dir: &Path,
    ) -> Result<Self, CoreError> {
        config.validate()?;
        let hotwords = Arc::new(HotwordsRegistry::with_managed_dir(
            hotwords_dir,
            config.hotwords_score,
        )?);
        Self::with_backend_and_registry(config, backend, hotwords)
    }

    fn with_backend_and_registry(
        config: SherpaSttConfig,
        backend: Arc<dyn RecognizerBackend>,
        hotwords: Arc<HotwordsRegistry>,
    ) -> Result<Self, CoreError> {
        config.validate()?;
        let supports_hotwords = config.model.supports_hotwords();
        Ok(Self {
            base_config: config.clone(),
            supports_hotwords,
            config: RwLock::new(config),
            backend,
            hotwords,
            next_utterance_id: Arc::new(AtomicU64::new(1)),
            next_stream_id: AtomicU64::new(1),
            active_streams: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Returns a cloned snapshot of the current [`SherpaSttConfig`].
    pub fn config(&self) -> Result<SherpaSttConfig, CoreError> {
        self.config
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| CoreError::Stt("sherpa engine config lock poisoned".to_owned()))
    }

    /// Returns the effective decoding method currently in effect for new/reloaded streams.
    pub fn effective_decoding_method(&self) -> Result<SttDecodingMethod, CoreError> {
        let cfg = self.config()?;
        let hw = self.hotwords.snapshot()?;
        Ok(cfg.effective_decoding_method(!hw.is_empty()))
    }

    /// Returns the current monotonic hotwords generation counter.
    #[must_use]
    pub fn hotwords_generation(&self) -> u64 {
        self.hotwords.generation()
    }

    /// Returns a snapshot of the current active hotwords configuration.
    pub fn hotwords_snapshot(&self) -> Result<HotwordsSnapshot, CoreError> {
        self.hotwords.snapshot()
    }

    /// Returns the number of currently tracked live streams.
    #[must_use]
    pub fn active_stream_count(&self) -> usize {
        self.active_streams
            .lock()
            .map_or(0, |streams| streams.len())
    }

    /// Returns the speaker keys of all currently tracked active streams.
    #[must_use]
    pub fn active_speakers(&self) -> Vec<SpeakerKey> {
        self.active_streams.lock().map_or_else(
            |_| Vec::new(),
            |streams| streams.values().map(|e| e.speaker_key.clone()).collect(),
        )
    }

    /// Reaps any tracked streams whose last activity exceeds [`SherpaSttConfig::idle_timeout`].
    ///
    /// Expired streams are removed from tracking and their underlying `RecognizerStreamBackend`
    /// is immediately dropped outside the map lock. Returns the number of streams reaped.
    pub fn reap_idle_streams(&self) -> Result<usize, CoreError> {
        self.reap_idle_streams_at(Instant::now())
    }

    /// Reaps streams idle relative to `now` (deterministic helper for tests).
    pub fn reap_idle_streams_at(&self, now: Instant) -> Result<usize, CoreError> {
        let timeout = self.config()?.idle_timeout;
        let expired_entries = {
            let mut streams = self
                .active_streams
                .lock()
                .map_err(|_| CoreError::Stt("active stream registry lock poisoned".to_owned()))?;
            let idle_ids: Vec<u64> = streams
                .iter()
                .filter_map(|(&id, entry)| entry.is_idle_at(now, timeout).then_some(id))
                .collect();
            let mut expired = Vec::with_capacity(idle_ids.len());
            for id in idle_ids {
                if let Some(entry) = streams.remove(&id) {
                    entry.mark_reaped();
                    expired.push(entry);
                }
            }
            drop(streams);
            expired
        };

        let reaped_count = expired_entries.len();
        for entry in expired_entries {
            entry.drop_backend_stream();
        }
        Ok(reaped_count)
    }

    /// Backdates a tracked stream's last-activity timestamp for deterministic idle-timeout tests.
    #[doc(hidden)]
    pub fn set_stream_last_activity_for_test(&self, speaker_key: &SpeakerKey, instant: Instant) {
        if let Ok(streams) = self.active_streams.lock() {
            for entry in streams.values() {
                if &entry.speaker_key == speaker_key {
                    entry.touch(instant);
                }
            }
        }
    }

    /// Creates a concrete [`SherpaSttStream`] directly (convenience wrapper over
    /// [`SttEngine::create_stream`]).
    pub fn create_typed_stream(
        &self,
        speaker_key: SpeakerKey,
    ) -> Result<SherpaSttStream, CoreError> {
        let cfg = self.config()?;
        let snapshot = self.hotwords.snapshot()?;
        let inner = self.backend.create_stream(&snapshot)?;
        let backend_stream: SharedStreamBackend = Arc::new(Mutex::new(Some(inner)));

        let stream_id = self.next_stream_id.fetch_add(1, Ordering::Relaxed);
        let activity = Arc::new(StreamActivityEntry {
            speaker_key: speaker_key.clone(),
            last_activity: Mutex::new(Instant::now()),
            reaped: AtomicBool::new(false),
            backend_stream: Arc::clone(&backend_stream),
        });
        {
            let mut streams = self
                .active_streams
                .lock()
                .map_err(|_| CoreError::Stt("active stream registry lock poisoned".to_owned()))?;
            streams.insert(stream_id, Arc::clone(&activity));
        }

        Ok(SherpaSttStream {
            stream_id,
            speaker_key,
            sample_rate: cfg.sample_rate,
            backend: Arc::clone(&self.backend),
            hotwords: Arc::clone(&self.hotwords),
            hotwords_generation: snapshot.generation,
            backend_stream,
            next_utterance_id: Arc::clone(&self.next_utterance_id),
            active_utterance: None,
            last_interim_text: String::new(),
            activity,
            active_streams: Arc::clone(&self.active_streams),
        })
    }
}

impl SttEngine for SherpaSttEngine {
    fn name(&self) -> &'static str {
        "sherpa-onnx"
    }

    fn hotwords_supported(&self) -> bool {
        self.supports_hotwords
    }

    fn update_hotwords(&self, config: &SttHotwordsConfig) -> Result<(), CoreError> {
        let current_cfg = self.config()?;
        let mut candidate_cfg = current_cfg;
        candidate_cfg.hotwords_score =
            crate::config::validate_hotwords_score(config.hotwords_score)?;

        let has_hotwords = !crate::hotwords::sanitize_hotwords(&config.hotwords).is_empty();
        let effective_decoding = if !self.supports_hotwords {
            SttDecodingMethod::GreedySearch
        } else if has_hotwords || config.decoding_method == SttDecodingMethod::ModifiedBeamSearch {
            SttDecodingMethod::ModifiedBeamSearch
        } else {
            self.base_config.decoding_method
        };
        candidate_cfg.decoding_method = effective_decoding;

        let backend = Arc::clone(&self.backend);
        let cfg_for_backend = candidate_cfg.clone();
        self.hotwords
            .commit_with(config, effective_decoding, |candidate_snapshot| {
                backend.on_hotwords_updated(&cfg_for_backend, candidate_snapshot)
            })?;

        let mut cfg_guard = self
            .config
            .write()
            .map_err(|_| CoreError::Stt("sherpa engine config lock poisoned".to_owned()))?;
        *cfg_guard = candidate_cfg;
        drop(cfg_guard);

        Ok(())
    }

    fn create_stream(
        &self,
        speaker_key: SpeakerKey,
        _src_lang: &LanguageCode,
    ) -> Result<Box<dyn SttStream>, CoreError> {
        Ok(Box::new(self.create_typed_stream(speaker_key)?))
    }
}

/// Per-speaker streaming speech recognition handle backed by `sherpa-onnx`.
pub struct SherpaSttStream {
    stream_id: u64,
    speaker_key: SpeakerKey,
    sample_rate: i32,
    backend: Arc<dyn RecognizerBackend>,
    hotwords: Arc<HotwordsRegistry>,
    hotwords_generation: u64,
    backend_stream: SharedStreamBackend,
    next_utterance_id: Arc<AtomicU64>,
    active_utterance: Option<UtteranceId>,
    last_interim_text: String,
    activity: Arc<StreamActivityEntry>,
    active_streams: Arc<Mutex<HashMap<u64, Arc<StreamActivityEntry>>>>,
}

impl SherpaSttStream {
    /// Returns the speaker key bound to this stream.
    #[must_use]
    pub const fn speaker_key(&self) -> &SpeakerKey {
        &self.speaker_key
    }

    /// Returns the hotwords generation currently active on this stream.
    #[must_use]
    pub const fn hotwords_generation(&self) -> u64 {
        self.hotwords_generation
    }

    /// Returns `true` if this stream has been reaped by [`SherpaSttEngine::reap_idle_streams`].
    #[must_use]
    pub fn is_reaped(&self) -> bool {
        self.activity.is_reaped()
    }

    /// Convenience wrapper over [`SttStream::accept_audio`] with a caller-supplied timestamp (or `0`
    /// when not tracked externally).
    pub fn push_audio(
        &mut self,
        samples_16k_mono: &[f32],
        timestamp_ms: u64,
    ) -> Result<Vec<TranscriptEvent>, CoreError> {
        self.accept_audio(samples_16k_mono, timestamp_ms)
    }

    /// Convenience wrapper over [`SttStream::flush`] returning at most one finalized utterance.
    pub fn flush_utterance(
        &mut self,
        timestamp_ms: u64,
    ) -> Result<Option<TranscriptEvent>, CoreError> {
        let mut events = self.flush(timestamp_ms)?;
        Ok(events.pop())
    }

    fn ensure_active_stream(&mut self) -> Result<(), CoreError> {
        if self.activity.is_reaped() {
            if let Ok(mut guard) = self.backend_stream.lock() {
                drop(guard.take());
            }
            self.clear_utterance_state();
            self.activity.reaped.store(false, Ordering::Release);
            if let Ok(mut streams) = self.active_streams.lock() {
                streams.insert(self.stream_id, Arc::clone(&self.activity));
            }
        }

        let needs_rebuild = {
            let guard = self
                .backend_stream
                .lock()
                .map_err(|_| CoreError::Stt("sherpa stream lock poisoned".to_owned()))?;
            let current_gen = self.hotwords.generation();
            guard.is_none()
                || (self.active_utterance.is_none() && current_gen != self.hotwords_generation)
        };

        // Only swap to a newer hotword generation between utterances so in-progress acoustic state
        // is never discarded mid-sentence.
        if needs_rebuild {
            let snapshot = self.hotwords.snapshot()?;
            let rebuilt = self.backend.create_stream(&snapshot)?;
            let mut guard = self
                .backend_stream
                .lock()
                .map_err(|_| CoreError::Stt("sherpa stream lock poisoned".to_owned()))?;
            *guard = Some(rebuilt);
            drop(guard);
            self.hotwords_generation = snapshot.generation;
        }

        Ok(())
    }

    fn clear_utterance_state(&mut self) {
        self.active_utterance = None;
        self.last_interim_text.clear();
    }

    fn drop_backend_stream(&self) -> Result<(), CoreError> {
        let taken = self
            .backend_stream
            .lock()
            .map_err(|_| CoreError::Stt("sherpa stream lock poisoned".to_owned()))?
            .take();
        drop(taken);
        Ok(())
    }

    fn reload_stream_if_hotwords_stale(&mut self) -> Result<(), CoreError> {
        let current_gen = self.hotwords.generation();
        if current_gen != self.hotwords_generation {
            let snapshot = self.hotwords.snapshot()?;
            let rebuilt = self.backend.create_stream(&snapshot)?;
            let mut guard = self
                .backend_stream
                .lock()
                .map_err(|_| CoreError::Stt("sherpa stream lock poisoned".to_owned()))?;
            *guard = Some(rebuilt);
            drop(guard);
            self.hotwords_generation = snapshot.generation;
        }
        Ok(())
    }
}

impl SttStream for SherpaSttStream {
    fn accept_audio(
        &mut self,
        samples_16k_mono: &[f32],
        timestamp_ms: u64,
    ) -> Result<Vec<TranscriptEvent>, CoreError> {
        self.activity.touch(Instant::now());

        if samples_16k_mono.is_empty() {
            return Ok(Vec::new());
        }

        self.ensure_active_stream()?;

        let sample_rate = self.sample_rate;
        let (hypothesis, is_endpoint) = {
            let mut guard = self
                .backend_stream
                .lock()
                .map_err(|_| CoreError::Stt("sherpa stream lock poisoned".to_owned()))?;
            let stream = guard
                .as_deref_mut()
                .ok_or_else(|| CoreError::Stt("sherpa stream backend unavailable".to_owned()))?;
            stream.accept_waveform(sample_rate, samples_16k_mono)?;
            while stream.is_ready()? {
                stream.decode()?;
            }
            let out = (stream.get_result()?, stream.is_endpoint()?);
            drop(guard);
            out
        };

        let raw_text = hypothesis.as_ref().map_or("", |h| h.text.trim()).to_owned();
        let native_final = hypothesis.as_ref().is_some_and(|h| h.is_final);
        let finalized = is_endpoint || native_final;

        let mut events = Vec::new();
        if finalized {
            // If an endpoint triggered after Interim text was already emitted, fall back to
            // `last_interim_text` when the recognizer yields empty text on reset/boundary so the
            // subtitle card never stays stuck in Interim state.
            let final_text = if raw_text.is_empty() {
                self.last_interim_text.clone()
            } else {
                raw_text
            };

            if !final_text.is_empty() {
                let next_ids = &self.next_utterance_id;
                let utt_id = *self
                    .active_utterance
                    .get_or_insert_with(|| UtteranceId(next_ids.fetch_add(1, Ordering::Relaxed)));
                events.push(TranscriptEvent {
                    utterance_id: utt_id,
                    speaker_key: self.speaker_key.clone(),
                    text: final_text,
                    is_final: true,
                    timestamp_ms,
                });
            }

            {
                let mut guard = self
                    .backend_stream
                    .lock()
                    .map_err(|_| CoreError::Stt("sherpa stream lock poisoned".to_owned()))?;
                if let Some(stream) = guard.as_deref_mut() {
                    stream.reset()?;
                }
            }
            self.clear_utterance_state();
            self.reload_stream_if_hotwords_stale()?;
        } else if !raw_text.is_empty() {
            let next_ids = &self.next_utterance_id;
            let utt_id = *self
                .active_utterance
                .get_or_insert_with(|| UtteranceId(next_ids.fetch_add(1, Ordering::Relaxed)));
            if raw_text != self.last_interim_text {
                self.last_interim_text.clone_from(&raw_text);
                events.push(TranscriptEvent {
                    utterance_id: utt_id,
                    speaker_key: self.speaker_key.clone(),
                    text: raw_text,
                    is_final: false,
                    timestamp_ms,
                });
            }
        }

        Ok(events)
    }

    fn flush(&mut self, timestamp_ms: u64) -> Result<Vec<TranscriptEvent>, CoreError> {
        if self.activity.is_reaped() {
            self.drop_backend_stream()?;
            self.clear_utterance_state();
            return Ok(Vec::new());
        }

        self.activity.touch(Instant::now());

        let flushed_text = {
            let mut guard = self
                .backend_stream
                .lock()
                .map_err(|_| CoreError::Stt("sherpa stream lock poisoned".to_owned()))?;
            let Some(stream) = guard.as_deref_mut() else {
                drop(guard);
                return Ok(Vec::new());
            };
            stream.input_finished()?;
            while stream.is_ready()? {
                stream.decode()?;
            }
            let text = stream
                .get_result()?
                .map(|h| h.text.trim().to_owned())
                .unwrap_or_default();
            drop(guard);
            text
        };

        // Drop the underlying OnlineStream after `input_finished()` instead of relying on
        // `reset()`, because `sherpa-onnx` does not clear `input_finished_` in `reset()`.
        // Subsequent `accept_audio()` calls will lazily allocate a clean stream.
        self.drop_backend_stream()?;

        let final_text = if flushed_text.is_empty() {
            self.last_interim_text.clone()
        } else {
            flushed_text
        };

        let events = if final_text.is_empty() {
            Vec::new()
        } else {
            let next_ids = &self.next_utterance_id;
            let utt_id = self
                .active_utterance
                .take()
                .unwrap_or_else(|| UtteranceId(next_ids.fetch_add(1, Ordering::Relaxed)));
            vec![TranscriptEvent {
                utterance_id: utt_id,
                speaker_key: self.speaker_key.clone(),
                text: final_text,
                is_final: true,
                timestamp_ms,
            }]
        };

        self.clear_utterance_state();
        self.hotwords_generation = self.hotwords.generation();
        Ok(events)
    }
}

impl Drop for SherpaSttStream {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.backend_stream.lock() {
            drop(guard.take());
        }
        if let Ok(mut streams) = self.active_streams.lock() {
            streams.remove(&self.stream_id);
        }
    }
}
