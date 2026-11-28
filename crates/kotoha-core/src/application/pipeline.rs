//! Application pipeline orchestrating input events, preprocessing, per-speaker STT,
//! pre-filtering, context injection, bounded deadline-aware translation, and 3-stage subtitle
//! output.
//!
//! # Execution model
//!
//! ```text
//!                       ┌──────────── per speaker (async task) ────────────┐
//! AudioInputEvent ─▶ router ─try_send─▶ [bounded queue] ─▶ SpeakerWorker ─spawn_blocking─▶ resample + STT
//!  (run() loop)    (session,           (speaker_queue_     │  emits Interim / Final under the
//!                   lifecycle)          capacity)          │  source gate, then enqueues finals
//!                                                          ▼
//!                                   [bounded job channel] ─▶ TranslationDispatcher
//!                                                          (max_pending_translations queue,
//!                                                           max_concurrent_translations in flight,
//!                                                           translation_deadline per final)
//! ```
//!
//! - **Router** ([`PipelineService::run`]): applies session ownership, routes audio to the
//!   speaker's worker with a non-blocking `try_send`, and manages worker lifecycle (spawn on first
//!   audio, stop on `UserRemoved`, discard on `SourceReset` / disconnect). It never runs STT and
//!   never awaits a worker, so control events are handled promptly while speakers decode.
//! - **Speaker worker**: one async task per [`SpeakerKey`] owning that speaker's [`SttStream`].
//!   CPU work (resampling and decoding) runs sequentially per speaker on the blocking pool via
//!   `spawn_blocking`, so speakers decode in parallel without blocking the async runtime. The
//!   worker also owns the idle timeout: after `speaker_idle_timeout` without input it flushes,
//!   finalizes and drops the stream, then exits.
//! - **Translation dispatcher**: one task that keeps finals in a bounded FIFO and starts at most
//!   `max_concurrent_translations` translator calls. Every final resolves within
//!   `translation_deadline` of being finalized: finals that cannot be translated in time (queue
//!   overflow, waiting too long, or a slow translator) become [`SubtitleStage::TranslationFailed`]
//!   with [`TRANSLATION_SKIPPED_REASON`].
//!
//! # Backpressure
//!
//! | Queue | Bound | When full |
//! |---|---|---|
//! | speaker audio queue | `speaker_queue_capacity` chunks | the chunk is dropped (`stt_queue_drops`) |
//! | end-of-speech notice | same queue | dropped; the idle timeout finalizes the utterance instead |
//! | translation queue | `max_pending_translations` | the oldest queued final is skipped as too late |
//! | translations in flight | `max_concurrent_translations` | further finals wait in the queue |
//!
//! # Ordering
//!
//! Each source has a *gate* (an async mutex) holding its epoch and [`ContextWindow`]. Subtitle
//! emits, the epoch check and context updates all happen while holding the gate, and
//! `SourceReset` advances the epoch and calls `clear_source` under the same gate, so no stale
//! update can slip in between the check and the emit. Finals take a per-source sequence number
//! when they are emitted; translated turns are appended to the context window strictly in that
//! order, even when translations complete out of order.
//!
//! # Error policy
//!
//! A single failing adapter call never stops the pipeline. Every error is classified with
//! [`CoreError::scope`], bounded by the call site with [`ErrorScope::at_most`], and recovered from
//! at that scope:
//!
//! | Call site | Scope ceiling | Recovery |
//! |---|---|---|
//! | `Resampler` (error or panic) | [`ErrorScope::Chunk`] | drop the chunk |
//! | `SttEngine::create_stream` (error or panic) | [`ErrorScope::Speaker`] | drop the chunk and back off before the next attempt |
//! | `SttStream::accept_audio` | [`ErrorScope::Speaker`] | `Chunk`: drop the chunk; otherwise (or on panic) drop the speaker's stream |
//! | `SttStream::flush` (error or panic) | [`ErrorScope::Speaker`] | drop the speaker's stream |
//! | `Translator::translate` (error or panic) | [`ErrorScope::Utterance`] | emit [`SubtitleStage::TranslationFailed`]; the context window does not record the turn |
//! | `SubtitleSink::emit` / `clear_source` (error or panic) | [`ErrorScope::Utterance`] | lose that update only |
//! | worker / translation task panic | [`ErrorScope::Speaker`] / [`ErrorScope::Utterance`] | count it and continue |
//!
//! A sink panic is caught around each call (`emit` is polled inside `catch_unwind`), so it never
//! unwinds a speaker worker and the utterance it belonged to still finalizes and translates.
//!
//! # Shutdown
//!
//! When the input channel closes, `run()` waits for workers and queued translations to drain, but
//! at most [`PipelineConfig::shutdown_timeout`]. An adapter that hangs past it (for example an STT
//! call stuck on the blocking pool) is abandoned: its task is aborted (a blocking call keeps its
//! thread until it returns) and `run()` returns `Ok(())`.
//!
//! A timed-out shutdown still resolves every final card: before anything is aborted, the
//! dispatcher reports each final that has not resolved yet (queued, in flight, or still in the
//! job channel) as [`SubtitleStage::TranslationFailed`] with [`TRANSLATION_SKIPPED_REASON`],
//! under the source gate, so a reset source is never revived and no final resolves twice. This
//! happens even when the final's own `translation_deadline` has not passed yet (the deadline is
//! counted from finalization, which can be late in the drain). The reports are given a short
//! grace period after the timeout; only a sink that hangs past it can lose them. Timeouts are
//! counted in [`PipelineStats::shutdown_timeouts`].
//!
//! Whenever a stream is dropped while an utterance is still open (idle timeout, STT error or
//! panic), the last interim text is emitted as the utterance's final card so that no card stays
//! stuck in [`SubtitleStage::Interim`].
//!
//! Non-fatal errors are counted in [`PipelineStats`] and logged with `tracing` using only fixed
//! messages, error kinds ([`CoreError::kind`]) and speaker identifiers, never adapter messages
//! (which may embed provider response bodies).
//!
//! # Future: audio → LLM path (#46)
//!
//! The speaker worker is the seam for a future `SpeechTranslator` mode: a worker can host a
//! recognizer that produces the transcript and translation in one call instead of the
//! `SttStream` + dispatcher pair, while keeping the same router, queues, gates and error policy.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::{oneshot, Mutex};
use tokio::task::{AbortHandle, JoinError, JoinSet};
use tokio::time::Instant;

use crate::application::session::{SessionAction, SourceSessionManager};
use crate::application::translator_manager::TranslatorManager;
use crate::domain::{
    AudioChunk, AudioInputEvent, ContextWindow, CoreError, ErrorScope, HistoryEntry, LanguageCode,
    MergedGlossary, PreFilter, Resampler, SpeakerKey, SttEngine, SttStream, SubtitleEntry,
    SubtitleSink, SubtitleStage, TranscriptEvent, TranslationRequest, DEFAULT_HOTWORDS_SCORE,
};

/// Factory creating one stateful [`Resampler`] per speaker worker.
///
/// Defined as a closure boundary so `application` does not depend on concrete DSP adapters.
/// Each worker calls it once when it starts (and again to replace a resampler that panicked);
/// the instance then runs on that worker's blocking thread with its filter state kept across
/// chunks.
pub type ResamplerFactory = Arc<dyn Fn() -> Box<dyn Resampler> + Send + Sync>;

/// User-safe reason attached to [`SubtitleStage::TranslationFailed`] when the translator panicked.
pub const TRANSLATOR_CRASHED_REASON: &str = "translation engine crashed";

/// User-safe reason attached to [`SubtitleStage::TranslationFailed`] when a final could not be
/// translated within [`PipelineConfig::translation_deadline`].
pub const TRANSLATION_SKIPPED_REASON: &str = "translation skipped (too late)";

/// Maximum number of queued inputs a speaker worker hands to one blocking STT call.
const MAX_STT_BATCH: usize = 32;
/// First delay before retrying a failed `SttEngine::create_stream`.
const CREATE_STREAM_BACKOFF_INITIAL: Duration = Duration::from_millis(100);
/// Upper bound of the `create_stream` retry delay.
const CREATE_STREAM_BACKOFF_MAX: Duration = Duration::from_secs(5);
/// How long [`PipelineService::run`] waits, after [`PipelineConfig::shutdown_timeout`], for the
/// dispatcher to report its unfinished finals as skipped before abandoning it.
const SHUTDOWN_RESOLVE_GRACE: Duration = Duration::from_secs(1);

/// Configuration for [`PipelineService`].
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Source language code passed to per-speaker STT streams and translation requests.
    pub src_lang: LanguageCode,
    /// Target language code passed to translation requests.
    pub tgt_lang: LanguageCode,
    /// Maximum number of translator calls in flight at once.
    pub max_concurrent_translations: usize,
    /// Number of recent translated turns preserved per source context window.
    pub context_window_capacity: usize,
    /// Maximum number of finals waiting for a translation slot. When the queue overflows, the
    /// oldest waiting final is skipped with [`TRANSLATION_SKIPPED_REASON`].
    pub max_pending_translations: usize,
    /// Time from finalization within which a final must be translated. Finals that are still
    /// waiting, or whose translator call is still running, at the deadline are skipped with
    /// [`TRANSLATION_SKIPPED_REASON`].
    pub translation_deadline: Duration,
    /// A speaker worker that receives no input for this long flushes, finalizes and drops its
    /// STT stream, then exits (it is recreated on the next chunk).
    pub speaker_idle_timeout: Duration,
    /// Capacity, in audio chunks, of each speaker worker's input queue. Chunks arriving while it
    /// is full are dropped so that one slow speaker never blocks the others.
    pub speaker_queue_capacity: usize,
    /// Upper bound on how long [`PipelineService::run`] waits, after its input channel closes,
    /// for workers and queued translations to drain. Work still running afterwards (e.g. a hung
    /// adapter) is abandoned, and every final not translated yet is reported with
    /// [`TRANSLATION_SKIPPED_REASON`] (`run()` gives those reports a further grace period of
    /// about a second). Keep it above `translation_deadline` so that a normal drain is rarely cut
    /// short.
    pub shutdown_timeout: Duration,
}

impl PipelineConfig {
    /// Creates a [`PipelineConfig`] with sensible defaults: `max_concurrent_translations = 4`,
    /// `context_window_capacity = 8`, `max_pending_translations = 16`,
    /// `translation_deadline = 10 s`, `speaker_idle_timeout = 5 s`,
    /// `speaker_queue_capacity = 256` (about 5 s of 20 ms frames), `shutdown_timeout = 15 s`.
    #[must_use]
    pub const fn new(src_lang: LanguageCode, tgt_lang: LanguageCode) -> Self {
        Self {
            src_lang,
            tgt_lang,
            max_concurrent_translations: 4,
            context_window_capacity: 8,
            max_pending_translations: 16,
            translation_deadline: Duration::from_secs(10),
            speaker_idle_timeout: Duration::from_secs(5),
            speaker_queue_capacity: 256,
            shutdown_timeout: Duration::from_secs(15),
        }
    }
}

/// Point-in-time snapshot of the pipeline's counters and gauges.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PipelineStats {
    /// Utterances whose translation failed (error or translator panic) and were reported with
    /// [`SubtitleStage::TranslationFailed`].
    pub translation_failures: u64,
    /// Finals skipped because they could not be translated within the deadline (reported with
    /// [`TRANSLATION_SKIPPED_REASON`]). Not included in `translation_failures`.
    pub translations_skipped: u64,
    /// STT errors from `create_stream`, `accept_audio`, or `flush`.
    pub stt_errors: u64,
    /// Speaker STT streams dropped because of an error or panic (recreated on the next chunk).
    pub stt_stream_resets: u64,
    /// Audio chunks lost (resample failure, STT error, `create_stream` back-off, or a full
    /// speaker queue).
    pub dropped_chunks: u64,
    /// Subset of `dropped_chunks` dropped because the speaker's queue was full.
    pub stt_queue_drops: u64,
    /// Speaker streams flushed and dropped by the idle timeout.
    pub idle_flushes: u64,
    /// Final cards emitted from the last interim text because a stream was dropped mid-utterance.
    pub synthesized_finals: u64,
    /// Failed `SubtitleSink::emit` / `clear_source` calls.
    pub sink_errors: u64,
    /// Contained panics (translator, STT, resample, sink, or task panics).
    pub task_panics: u64,
    /// Times [`PipelineService::run`] hit [`PipelineConfig::shutdown_timeout`] while draining.
    /// The finals still waiting for translation at that point are reported with
    /// [`TRANSLATION_SKIPPED_REASON`] (and counted in `translations_skipped`).
    pub shutdown_timeouts: u64,
    /// Gauge: finals currently queued or being translated.
    pub pending_translations: u64,
    /// High-water mark of `pending_translations`.
    pub peak_pending_translations: u64,
    /// Gauge: live speaker workers.
    pub active_speaker_workers: u64,
}

#[derive(Debug, Default)]
struct StatsCounters {
    translation_failures: AtomicU64,
    translations_skipped: AtomicU64,
    stt_errors: AtomicU64,
    stt_stream_resets: AtomicU64,
    dropped_chunks: AtomicU64,
    stt_queue_drops: AtomicU64,
    idle_flushes: AtomicU64,
    synthesized_finals: AtomicU64,
    sink_errors: AtomicU64,
    task_panics: AtomicU64,
    shutdown_timeouts: AtomicU64,
    pending_translations: AtomicU64,
    peak_pending_translations: AtomicU64,
    active_speaker_workers: AtomicU64,
}

/// Cheaply clonable, lock-free handle to the pipeline's counters.
///
/// Obtain it with [`PipelineService::stats_handle`] before moving the service into a task, then
/// read [`PipelineStatsHandle::snapshot`] while [`PipelineService::run`] is executing.
#[derive(Debug, Clone, Default)]
pub struct PipelineStatsHandle(Arc<StatsCounters>);

impl PipelineStatsHandle {
    /// Returns the current counter values.
    #[must_use]
    pub fn snapshot(&self) -> PipelineStats {
        let c = &self.0;
        let load = |a: &AtomicU64| a.load(Ordering::Relaxed);
        PipelineStats {
            translation_failures: load(&c.translation_failures),
            translations_skipped: load(&c.translations_skipped),
            stt_errors: load(&c.stt_errors),
            stt_stream_resets: load(&c.stt_stream_resets),
            dropped_chunks: load(&c.dropped_chunks),
            stt_queue_drops: load(&c.stt_queue_drops),
            idle_flushes: load(&c.idle_flushes),
            synthesized_finals: load(&c.synthesized_finals),
            sink_errors: load(&c.sink_errors),
            task_panics: load(&c.task_panics),
            shutdown_timeouts: load(&c.shutdown_timeouts),
            pending_translations: load(&c.pending_translations),
            peak_pending_translations: load(&c.peak_pending_translations),
            active_speaker_workers: load(&c.active_speaker_workers),
        }
    }

    fn incr(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn translation_failure(&self) {
        Self::incr(&self.0.translation_failures);
    }

    fn translation_skipped(&self) {
        Self::incr(&self.0.translations_skipped);
    }

    fn stt_error(&self) {
        Self::incr(&self.0.stt_errors);
    }

    fn stt_stream_reset(&self) {
        Self::incr(&self.0.stt_stream_resets);
    }

    fn dropped_chunk(&self) {
        Self::incr(&self.0.dropped_chunks);
    }

    fn stt_queue_drop(&self) {
        Self::incr(&self.0.stt_queue_drops);
        self.dropped_chunk();
    }

    fn idle_flush(&self) {
        Self::incr(&self.0.idle_flushes);
    }

    fn synthesized_final(&self) {
        Self::incr(&self.0.synthesized_finals);
    }

    fn sink_error(&self) {
        Self::incr(&self.0.sink_errors);
    }

    fn task_panic(&self) {
        Self::incr(&self.0.task_panics);
    }

    fn shutdown_timeout(&self) {
        Self::incr(&self.0.shutdown_timeouts);
    }

    fn set_pending_translations(&self, pending: usize) {
        let pending = u64::try_from(pending).unwrap_or(u64::MAX);
        self.0
            .pending_translations
            .store(pending, Ordering::Relaxed);
        self.0
            .peak_pending_translations
            .fetch_max(pending, Ordering::Relaxed);
    }

    fn set_active_speaker_workers(&self, workers: usize) {
        self.0.active_speaker_workers.store(
            u64::try_from(workers).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    /// Records the outcome of a `SubtitleSink` call; sink errors never exceed
    /// [`ErrorScope::Utterance`], so they are counted and swallowed.
    fn record_sink(&self, result: &Result<(), CoreError>) {
        if let Err(err) = result {
            self.sink_error();
            tracing::debug!(error_kind = err.kind(), "subtitle sink call failed");
        }
    }
}

/// Monotonic millisecond wall clock shared by the router and the workers.
#[derive(Debug, Clone, Default)]
struct Clock(Arc<AtomicU64>);

impl Clock {
    fn now_ms(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        self.0.fetch_max(now, Ordering::Relaxed).max(now)
    }
}

/// Per-source state that must change atomically with subtitle emits (see "Ordering").
struct SourceGate {
    epoch: u64,
    context: ContextWindow,
    /// Sequence number handed to the next final of this epoch.
    next_seq: u64,
    /// Sequence number whose completion is awaited before later turns enter the context.
    next_commit: u64,
    /// Completed translations waiting for earlier ones (`None`: nothing to record).
    completed: BTreeMap<u64, Option<HistoryEntry>>,
}

impl SourceGate {
    const fn new(context: ContextWindow) -> Self {
        Self {
            epoch: 0,
            context,
            next_seq: 0,
            next_commit: 0,
            completed: BTreeMap::new(),
        }
    }

    const fn reserve_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    /// Returns `true` once the outcome of final `seq` has been recorded with [`Self::complete`].
    fn is_resolved(&self, seq: u64) -> bool {
        seq < self.next_commit || self.completed.contains_key(&seq)
    }

    /// Records the outcome of final `seq` and appends every turn that is now in order.
    fn complete(&mut self, seq: u64, turn: Option<HistoryEntry>) {
        if seq < self.next_commit {
            return;
        }
        self.completed.entry(seq).or_insert(turn);
        while let Some(turn) = self.completed.remove(&self.next_commit) {
            if let Some(turn) = turn {
                self.context.push_turn(turn);
            }
            self.next_commit += 1;
        }
    }

    fn reset(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.context.clear_history();
        self.next_seq = 0;
        self.next_commit = 0;
        self.completed.clear();
    }
}

struct SourceShared {
    /// Lock-free mirror of `gate.epoch`, written only while holding the gate.
    epoch: AtomicU64,
    gate: Mutex<SourceGate>,
}

impl SourceShared {
    fn is_stale(&self, epoch: u64) -> bool {
        self.epoch.load(Ordering::Acquire) != epoch
    }
}

/// Collaborators shared by every speaker worker of one [`PipelineService::run`] call.
struct WorkerDeps {
    src_lang: LanguageCode,
    tgt_lang: LanguageCode,
    stt_engine: Arc<dyn SttEngine>,
    sink: Arc<dyn SubtitleSink>,
    resampler_factory: ResamplerFactory,
    pre_filter: PreFilter,
    stats: PipelineStatsHandle,
    clock: Clock,
    idle_timeout: Duration,
    jobs: mpsc::Sender<TranslationJob>,
}

enum WorkerInput {
    Audio {
        chunk: AudioChunk,
        timestamp_ms: u64,
        speaker_name: String,
    },
    EndOfSpeech {
        timestamp_ms: u64,
        speaker_name: String,
    },
}

/// Router-side handle of a live speaker worker.
struct WorkerHandle {
    tx: mpsc::Sender<WorkerInput>,
    discarded: Arc<AtomicBool>,
    task_id: tokio::task::Id,
}

/// State owned by one [`PipelineService::run`] call.
struct RunState {
    deps: Arc<WorkerDeps>,
    workers: JoinSet<()>,
    worker_keys: HashMap<tokio::task::Id, SpeakerKey>,
}

/// Coordinates end-to-end audio-to-subtitle processing across multiple sources and speakers.
pub struct PipelineService {
    config: PipelineConfig,
    stt_engine: Arc<dyn SttEngine>,
    translator_manager: Arc<TranslatorManager>,
    subtitle_sink: Arc<dyn SubtitleSink>,
    resampler_factory: ResamplerFactory,
    pre_filter: PreFilter,
    active_glossary: MergedGlossary,
    session_manager: SourceSessionManager,
    speaker_workers: HashMap<SpeakerKey, WorkerHandle>,
    sources: HashMap<String, Arc<SourceShared>>,
    clock: Clock,
    stats: PipelineStatsHandle,
}

impl PipelineService {
    /// Constructs a new [`PipelineService`].
    #[must_use]
    pub fn new(
        config: PipelineConfig,
        stt_engine: Arc<dyn SttEngine>,
        translator_manager: Arc<TranslatorManager>,
        subtitle_sink: Arc<dyn SubtitleSink>,
        resampler_factory: ResamplerFactory,
    ) -> Self {
        Self {
            config,
            stt_engine,
            translator_manager,
            subtitle_sink,
            resampler_factory,
            pre_filter: PreFilter::default(),
            active_glossary: MergedGlossary::default(),
            session_manager: SourceSessionManager::new(),
            speaker_workers: HashMap::new(),
            sources: HashMap::new(),
            clock: Clock::default(),
            stats: PipelineStatsHandle::default(),
        }
    }

    /// Replaces the default [`PreFilter`] rules (takes effect on the next [`Self::run`]).
    pub fn set_pre_filter(&mut self, pre_filter: PreFilter) {
        self.pre_filter = pre_filter;
    }

    /// Updates the active merged glossary at runtime using [`DEFAULT_HOTWORDS_SCORE`].
    ///
    /// Propagates the updated glossary to all active source [`ContextWindow`]s (and future
    /// sources) and forwards normalized hotwords to [`SttEngine::update_hotwords`].
    pub async fn set_glossary(
        &mut self,
        glossary: impl Into<MergedGlossary>,
    ) -> Result<(), CoreError> {
        self.set_glossary_with_score(glossary, DEFAULT_HOTWORDS_SCORE)
            .await
    }

    /// Updates the active merged glossary at runtime with an explicit STT hotword boosting score.
    pub async fn set_glossary_with_score(
        &mut self,
        glossary: impl Into<MergedGlossary>,
        hotwords_score: f32,
    ) -> Result<(), CoreError> {
        let merged = glossary.into();
        let hotwords_config = merged.to_stt_hotwords_config(hotwords_score);
        self.stt_engine.update_hotwords(&hotwords_config)?;
        self.active_glossary = merged.clone();
        for source in self.sources.values() {
            source
                .gate
                .lock()
                .await
                .context
                .set_glossary(merged.clone());
        }
        Ok(())
    }

    /// Returns the currently active [`MergedGlossary`].
    #[must_use]
    pub const fn active_glossary(&self) -> &MergedGlossary {
        &self.active_glossary
    }

    /// Returns read-only access to the underlying [`SourceSessionManager`].
    #[must_use]
    pub const fn session_manager(&self) -> &SourceSessionManager {
        &self.session_manager
    }

    /// Returns the number of live per-speaker workers (each owning at most one STT stream).
    ///
    /// Always `0` once [`Self::run`] has returned; use [`PipelineStats::active_speaker_workers`]
    /// while it is running.
    #[must_use]
    pub fn active_stream_count(&self) -> usize {
        self.speaker_workers.len()
    }

    /// Returns a snapshot of the counters.
    #[must_use]
    pub fn stats(&self) -> PipelineStats {
        self.stats.snapshot()
    }

    /// Returns a shared handle to the counters that stays readable after the service has been
    /// moved into a task running [`Self::run`].
    #[must_use]
    pub fn stats_handle(&self) -> PipelineStatsHandle {
        self.stats.clone()
    }

    /// Runs the pipeline consuming events from `rx` until the channel closes.
    ///
    /// On close, every speaker worker processes its queued audio, flushes and finalizes its
    /// stream, and every queued final is translated (or skipped at its deadline) before this
    /// returns. That drain is bounded by [`PipelineConfig::shutdown_timeout`]: work still running
    /// when it expires (a hung adapter) is abandoned and this returns anyway, after reporting
    /// every final not resolved yet as skipped ([`PipelineStats::shutdown_timeouts`]).
    ///
    /// Returns `Ok(())` when the input channel closes. Every adapter error and panic is recovered
    /// from (see the module docs); `Err` is reserved for [`ErrorScope::Fatal`] pipeline
    /// invariants.
    pub async fn run(&mut self, mut rx: mpsc::Receiver<AudioInputEvent>) -> Result<(), CoreError> {
        let (job_tx, job_rx) = mpsc::channel(self.config.max_pending_translations.max(1));
        let dispatcher = TranslationDispatcher {
            translator_manager: Arc::clone(&self.translator_manager),
            sink: Arc::clone(&self.subtitle_sink),
            stats: self.stats.clone(),
            max_concurrent: self.config.max_concurrent_translations.max(1),
            max_pending: self.config.max_pending_translations.max(1),
            deadline: self.config.translation_deadline,
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let dispatcher_task = tokio::spawn(dispatcher.run(job_rx, shutdown_rx));

        let mut state = RunState {
            deps: Arc::new(WorkerDeps {
                src_lang: self.config.src_lang.clone(),
                tgt_lang: self.config.tgt_lang.clone(),
                stt_engine: Arc::clone(&self.stt_engine),
                sink: Arc::clone(&self.subtitle_sink),
                resampler_factory: Arc::clone(&self.resampler_factory),
                pre_filter: self.pre_filter.clone(),
                stats: self.stats.clone(),
                clock: self.clock.clone(),
                idle_timeout: self.config.speaker_idle_timeout,
                jobs: job_tx,
            }),
            workers: JoinSet::new(),
            worker_keys: HashMap::new(),
        };

        loop {
            tokio::select! {
                biased;
                Some(res) = state.workers.join_next_with_id(), if !state.workers.is_empty() => {
                    self.on_worker_exit(&mut state, res);
                }
                event = rx.recv() => match event {
                    Some(event) => self.route_event(event, &mut state).await,
                    None => break,
                },
            }
        }

        // Graceful shutdown: closing every queue lets each worker drain, flush and finalize.
        self.speaker_workers.clear();
        let shutdown_deadline = Instant::now() + self.config.shutdown_timeout;
        let workers_done = tokio::time::timeout_at(shutdown_deadline, async {
            while let Some(res) = state.workers.join_next_with_id().await {
                self.on_worker_exit(&mut state, res);
            }
        })
        .await
        .is_ok();
        // The last job sender lives in `deps`; dropping it lets the dispatcher drain and stop.
        // Dropping the `JoinSet` also aborts any worker that did not finish in time.
        drop(state);
        let mut dispatcher_task = dispatcher_task;
        let dispatcher_done = workers_done
            && match tokio::time::timeout_at(shutdown_deadline, &mut dispatcher_task).await {
                Ok(res) => {
                    self.on_dispatcher_exit(res);
                    true
                }
                Err(_elapsed) => false,
            };
        if !dispatcher_done {
            // Whatever is still unfinished (a hung adapter, or finals whose deadline lies past
            // the shutdown deadline) is abandoned, but no final card may stay pending: the
            // dispatcher reports every job that has not resolved yet as skipped, then stops.
            self.stats.shutdown_timeout();
            tracing::warn!("pipeline shutdown timed out; skipping unfinished translations");
            // An `Err` means the dispatcher has already stopped; the wait below then returns.
            let _ = shutdown_tx.send(());
            match tokio::time::timeout(SHUTDOWN_RESOLVE_GRACE, &mut dispatcher_task).await {
                Ok(res) => self.on_dispatcher_exit(res),
                Err(_elapsed) => {
                    dispatcher_task.abort();
                    tracing::warn!("translation dispatcher did not stop in time; abandoning it");
                }
            }
        }
        self.stats.set_active_speaker_workers(0);
        Ok(())
    }

    fn on_dispatcher_exit(&self, res: Result<(), JoinError>) {
        if let Err(err) = res {
            if err.is_panic() {
                self.stats.task_panic();
                tracing::error!("translation dispatcher panicked");
            }
        }
    }

    async fn route_event(&mut self, event: AudioInputEvent, state: &mut RunState) {
        let action = self.session_manager.handle_event(&event);
        let now_ms = self.clock.now_ms();

        match action {
            SessionAction::RemoveSpeaker(speaker_key) => {
                // Closing the queue makes the worker drain, flush, finalize and exit.
                self.speaker_workers.remove(&speaker_key);
            }
            SessionAction::ClearSpeakers { source, .. } => {
                self.speaker_workers.retain(|key, worker| {
                    if key.source == source {
                        worker.discarded.store(true, Ordering::Release);
                        false
                    } else {
                        true
                    }
                });
                self.reset_source(&source).await;
            }
            SessionAction::Connected { .. }
            | SessionAction::None
            | SessionAction::AudioSequenceChecked(_) => {}
        }
        self.stats
            .set_active_speaker_workers(self.speaker_workers.len());

        match event {
            AudioInputEvent::AudioFrame { conn_id, chunk }
                if self
                    .session_manager
                    .is_owner(conn_id, &chunk.speaker_key.source) =>
            {
                let speaker_name = self
                    .session_manager
                    .speaker_display_name(&chunk.speaker_key);
                self.route_audio(chunk, now_ms, speaker_name, state);
            }
            AudioInputEvent::TalkingStateChanged {
                conn_id,
                speaker_key,
                state: talking,
            } if !talking.is_speaking()
                && self.session_manager.is_owner(conn_id, &speaker_key.source) =>
            {
                if let Some(worker) = self.speaker_workers.get(&speaker_key) {
                    let speaker_name = self.session_manager.speaker_display_name(&speaker_key);
                    // If the queue is full the notice is dropped; the idle timeout then finalizes.
                    let _ = worker.tx.try_send(WorkerInput::EndOfSpeech {
                        timestamp_ms: now_ms,
                        speaker_name,
                    });
                }
            }
            _ => {}
        }
    }

    fn route_audio(
        &mut self,
        chunk: AudioChunk,
        timestamp_ms: u64,
        speaker_name: String,
        state: &mut RunState,
    ) {
        let speaker_key = chunk.speaker_key.clone();
        let mut input = WorkerInput::Audio {
            chunk,
            timestamp_ms,
            speaker_name,
        };
        // Two attempts: the second one follows a worker that exited (idle) since the last chunk.
        for _ in 0..2 {
            if !self.speaker_workers.contains_key(&speaker_key) {
                self.spawn_worker(&speaker_key, state);
            }
            let Some(worker) = self.speaker_workers.get(&speaker_key) else {
                return;
            };
            match worker.tx.try_send(input) {
                Ok(()) => return,
                Err(TrySendError::Full(_)) => {
                    self.stats.stt_queue_drop();
                    tracing::debug!(
                        source = %speaker_key.source,
                        user_id = speaker_key.user_id,
                        "speaker queue full; audio chunk dropped"
                    );
                    return;
                }
                Err(TrySendError::Closed(returned)) => {
                    self.speaker_workers.remove(&speaker_key);
                    input = returned;
                }
            }
        }
        self.stats.dropped_chunk();
    }

    fn spawn_worker(&mut self, speaker_key: &SpeakerKey, state: &mut RunState) {
        let source = self.source(&speaker_key.source);
        let epoch = source.epoch.load(Ordering::Acquire);
        let discarded = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel(self.config.speaker_queue_capacity.max(1));
        let deps = Arc::clone(&state.deps);
        let worker = SpeakerWorker {
            speaker_color: speaker_key.color_hex().to_owned(),
            speaker_name: self.session_manager.speaker_display_name(speaker_key),
            recognizer: Some(SpeakerRecognizer::new(
                speaker_key.clone(),
                &deps,
                Arc::clone(&discarded),
            )),
            speaker_key: speaker_key.clone(),
            epoch,
            source,
            discarded: Arc::clone(&discarded),
            deps,
        };
        let handle = state.workers.spawn(worker.run(rx));
        let task_id = handle.id();
        state.worker_keys.insert(task_id, speaker_key.clone());
        self.speaker_workers.insert(
            speaker_key.clone(),
            WorkerHandle {
                tx,
                discarded,
                task_id,
            },
        );
        self.stats
            .set_active_speaker_workers(self.speaker_workers.len());
    }

    fn on_worker_exit(
        &mut self,
        state: &mut RunState,
        res: Result<(tokio::task::Id, ()), JoinError>,
    ) {
        let task_id = match res {
            Ok((id, ())) => id,
            Err(err) => {
                if err.is_panic() {
                    // A worker panic loses at most that speaker's stream; the next chunk respawns.
                    self.stats.task_panic();
                    self.stats.stt_stream_reset();
                    tracing::error!("speaker worker panicked");
                }
                err.id()
            }
        };
        if let Some(key) = state.worker_keys.remove(&task_id) {
            if self
                .speaker_workers
                .get(&key)
                .is_some_and(|w| w.task_id == task_id)
            {
                self.speaker_workers.remove(&key);
            }
        }
        self.stats
            .set_active_speaker_workers(self.speaker_workers.len());
    }

    fn source(&mut self, source: &str) -> Arc<SourceShared> {
        let capacity = self.config.context_window_capacity;
        let glossary = &self.active_glossary;
        Arc::clone(self.sources.entry(source.to_owned()).or_insert_with(|| {
            let mut window = ContextWindow::new(capacity);
            window.set_glossary(glossary.clone());
            Arc::new(SourceShared {
                epoch: AtomicU64::new(0),
                gate: Mutex::new(SourceGate::new(window)),
            })
        }))
    }

    /// Advances the source epoch, clears its context and its subtitles atomically with respect to
    /// every subtitle emit of that source.
    async fn reset_source(&self, source: &str) {
        let sink = Arc::clone(&self.subtitle_sink);
        let stats = self.stats.clone();
        let owned = source.to_owned();
        let clear = async move {
            let outcome = tokio::spawn(async move { sink.clear_source(&owned).await }).await;
            match outcome {
                Ok(result) => stats.record_sink(&result),
                Err(err) => {
                    stats.sink_error();
                    if err.is_panic() {
                        stats.task_panic();
                        tracing::error!("subtitle sink panicked in clear_source");
                    }
                }
            }
        };
        if let Some(shared) = self.sources.get(source) {
            let mut gate = shared.gate.lock().await;
            gate.reset();
            shared.epoch.store(gate.epoch, Ordering::Release);
            clear.await;
            drop(gate);
        } else {
            clear.await;
        }
    }
}

/// One input step for [`SpeakerRecognizer::process`].
enum RecognizerOp {
    Audio {
        chunk: AudioChunk,
        timestamp_ms: u64,
    },
    Flush {
        timestamp_ms: u64,
    },
}

/// Blocking half of a speaker worker: owns the [`SttStream`] and runs on the blocking pool.
struct SpeakerRecognizer {
    speaker_key: SpeakerKey,
    src_lang: LanguageCode,
    engine: Arc<dyn SttEngine>,
    resampler: Box<dyn Resampler>,
    resampler_factory: ResamplerFactory,
    stats: PipelineStatsHandle,
    discarded: Arc<AtomicBool>,
    stream: Option<Box<dyn SttStream>>,
    /// Last interim hypothesis of the utterance in progress, if any.
    open_utterance: Option<TranscriptEvent>,
    create_failures: u32,
    retry_at: Option<Instant>,
}

impl SpeakerRecognizer {
    fn new(speaker_key: SpeakerKey, deps: &WorkerDeps, discarded: Arc<AtomicBool>) -> Self {
        Self {
            speaker_key,
            src_lang: deps.src_lang.clone(),
            engine: Arc::clone(&deps.stt_engine),
            resampler: (deps.resampler_factory)(),
            resampler_factory: Arc::clone(&deps.resampler_factory),
            stats: deps.stats.clone(),
            discarded,
            stream: None,
            open_utterance: None,
            create_failures: 0,
            retry_at: None,
        }
    }

    const fn has_state(&self) -> bool {
        self.stream.is_some() || self.open_utterance.is_some()
    }

    /// Runs `ops` in order; with `close`, finally flushes, finalizes and drops the stream.
    fn process(
        &mut self,
        ops: Vec<RecognizerOp>,
        close_at: Option<u64>,
        now: Instant,
    ) -> Vec<TranscriptEvent> {
        let mut out = Vec::new();
        for op in ops {
            if self.discarded.load(Ordering::Acquire) {
                self.stream = None;
                self.open_utterance = None;
                return Vec::new();
            }
            match op {
                RecognizerOp::Audio {
                    chunk,
                    timestamp_ms,
                } => self.accept(&chunk, timestamp_ms, now, &mut out),
                RecognizerOp::Flush { timestamp_ms } => self.flush(timestamp_ms, &mut out),
            }
        }
        if let Some(timestamp_ms) = close_at {
            self.flush(timestamp_ms, &mut out);
            self.stream = None;
        }
        out
    }

    fn accept(
        &mut self,
        chunk: &AudioChunk,
        timestamp_ms: u64,
        now: Instant,
        out: &mut Vec<TranscriptEvent>,
    ) {
        let resampler = &mut self.resampler;
        let samples = match catch_unwind(AssertUnwindSafe(|| resampler.process(chunk))) {
            Ok(Ok(samples)) => samples,
            Ok(Err(err)) => {
                // Resample failures are bounded to the chunk.
                self.stats.dropped_chunk();
                tracing::debug!(
                    error_kind = err.kind(),
                    "audio chunk dropped: resample failed"
                );
                return;
            }
            Err(_) => {
                self.stats.task_panic();
                self.stats.dropped_chunk();
                // The filter state may be inconsistent after a panic: start afresh.
                self.resampler = (self.resampler_factory)();
                tracing::error!("resampler panicked; audio chunk dropped");
                return;
            }
        };
        if samples.is_empty() {
            return;
        }
        if !self.ensure_stream(now) {
            self.stats.dropped_chunk();
            return;
        }
        let Some(stream) = self.stream.as_mut() else {
            return;
        };
        match catch_unwind(AssertUnwindSafe(|| {
            stream.accept_audio(&samples, timestamp_ms)
        })) {
            Ok(Ok(events)) => self.track(events, out),
            Ok(Err(err)) => {
                self.stats.stt_error();
                self.stats.dropped_chunk();
                if err.scope().at_most(ErrorScope::Speaker) > ErrorScope::Chunk {
                    tracing::warn!(
                        source = %self.speaker_key.source,
                        user_id = self.speaker_key.user_id,
                        error_kind = err.kind(),
                        "STT accept_audio failed; speaker stream reset"
                    );
                    self.reset_stream(timestamp_ms, out);
                }
            }
            Err(_) => {
                self.stats.task_panic();
                self.stats.dropped_chunk();
                tracing::error!(
                    source = %self.speaker_key.source,
                    user_id = self.speaker_key.user_id,
                    "STT accept_audio panicked; speaker stream reset"
                );
                self.reset_stream(timestamp_ms, out);
            }
        }
    }

    fn flush(&mut self, timestamp_ms: u64, out: &mut Vec<TranscriptEvent>) {
        if let Some(stream) = self.stream.as_mut() {
            match catch_unwind(AssertUnwindSafe(|| stream.flush(timestamp_ms))) {
                Ok(Ok(events)) => self.track(events, out),
                Ok(Err(err)) => {
                    self.stats.stt_error();
                    tracing::warn!(
                        source = %self.speaker_key.source,
                        user_id = self.speaker_key.user_id,
                        error_kind = err.kind(),
                        "STT flush failed; speaker stream reset"
                    );
                    self.reset_stream(timestamp_ms, out);
                }
                Err(_) => {
                    self.stats.task_panic();
                    tracing::error!(
                        source = %self.speaker_key.source,
                        user_id = self.speaker_key.user_id,
                        "STT flush panicked; speaker stream reset"
                    );
                    self.reset_stream(timestamp_ms, out);
                }
            }
        }
        // A flush must leave no utterance open, whatever the engine returned.
        self.finalize_open(timestamp_ms, out);
    }

    /// Creates the stream if needed, honouring the `create_stream` back-off. Returns `true` when
    /// a stream is available.
    fn ensure_stream(&mut self, now: Instant) -> bool {
        if self.stream.is_some() {
            return true;
        }
        if self.retry_at.is_some_and(|at| now < at) {
            return false;
        }
        let engine = Arc::clone(&self.engine);
        let created = catch_unwind(AssertUnwindSafe(|| {
            engine.create_stream(self.speaker_key.clone(), &self.src_lang)
        }));
        let error_kind = match created {
            Ok(Ok(stream)) => {
                self.stream = Some(stream);
                self.create_failures = 0;
                self.retry_at = None;
                return true;
            }
            Ok(Err(err)) => err.kind(),
            Err(_) => {
                self.stats.task_panic();
                "panic"
            }
        };
        self.stats.stt_error();
        self.create_failures = self.create_failures.saturating_add(1);
        let backoff = CREATE_STREAM_BACKOFF_INITIAL
            .saturating_mul(1_u32 << self.create_failures.saturating_sub(1).min(16))
            .min(CREATE_STREAM_BACKOFF_MAX);
        self.retry_at = Some(now + backoff);
        tracing::warn!(
            source = %self.speaker_key.source,
            user_id = self.speaker_key.user_id,
            error_kind,
            consecutive_failures = self.create_failures,
            retry_in_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
            "STT create_stream failed; backing off"
        );
        false
    }

    fn track(&mut self, events: Vec<TranscriptEvent>, out: &mut Vec<TranscriptEvent>) {
        for event in events {
            if event.is_final {
                if self
                    .open_utterance
                    .as_ref()
                    .is_some_and(|open| open.utterance_id == event.utterance_id)
                {
                    self.open_utterance = None;
                }
            } else {
                self.open_utterance = Some(event.clone());
            }
            out.push(event);
        }
    }

    fn reset_stream(&mut self, timestamp_ms: u64, out: &mut Vec<TranscriptEvent>) {
        if self.stream.take().is_some() {
            self.stats.stt_stream_reset();
        }
        self.finalize_open(timestamp_ms, out);
    }

    /// Emits the open utterance's last interim text as its final so the card never stays stuck.
    fn finalize_open(&mut self, timestamp_ms: u64, out: &mut Vec<TranscriptEvent>) {
        if let Some(mut event) = self.open_utterance.take() {
            event.is_final = true;
            event.timestamp_ms = timestamp_ms;
            self.stats.synthesized_final();
            out.push(event);
        }
    }
}

/// Async half of a speaker worker: receives routed input, drives the recognizer on the blocking
/// pool, owns the idle timeout, and publishes results under the source gate.
struct SpeakerWorker {
    speaker_key: SpeakerKey,
    speaker_name: String,
    speaker_color: String,
    epoch: u64,
    source: Arc<SourceShared>,
    discarded: Arc<AtomicBool>,
    deps: Arc<WorkerDeps>,
    /// `None` only while it is lent to a blocking call.
    recognizer: Option<SpeakerRecognizer>,
}

impl SpeakerWorker {
    async fn run(mut self, mut rx: mpsc::Receiver<WorkerInput>) {
        loop {
            match tokio::time::timeout(self.deps.idle_timeout, rx.recv()).await {
                Ok(Some(first)) => {
                    let mut batch = vec![first];
                    while batch.len() < MAX_STT_BATCH {
                        match rx.try_recv() {
                            Ok(input) => batch.push(input),
                            Err(_) => break,
                        }
                    }
                    self.step(batch, false).await;
                }
                Ok(None) => {
                    // The router closed the queue: removal, reset (discarded) or shutdown.
                    self.step(Vec::new(), true).await;
                    return;
                }
                Err(_elapsed) => {
                    // Idle: stop accepting input, process whatever raced in, then finalize.
                    rx.close();
                    let mut batch = Vec::new();
                    while let Ok(input) = rx.try_recv() {
                        batch.push(input);
                    }
                    if batch.is_empty()
                        && self
                            .recognizer
                            .as_ref()
                            .is_some_and(SpeakerRecognizer::has_state)
                    {
                        self.deps.stats.idle_flush();
                        tracing::debug!(
                            source = %self.speaker_key.source,
                            user_id = self.speaker_key.user_id,
                            "speaker idle; stream finalized and dropped"
                        );
                    }
                    self.step(batch, true).await;
                    return;
                }
            }
        }
    }

    async fn step(&mut self, batch: Vec<WorkerInput>, close: bool) {
        if self.discarded.load(Ordering::Acquire) {
            return;
        }
        let mut ops = Vec::with_capacity(batch.len());
        for input in batch {
            match input {
                WorkerInput::Audio {
                    chunk,
                    timestamp_ms,
                    speaker_name,
                } => {
                    self.speaker_name = speaker_name;
                    ops.push(RecognizerOp::Audio {
                        chunk,
                        timestamp_ms,
                    });
                }
                WorkerInput::EndOfSpeech {
                    timestamp_ms,
                    speaker_name,
                } => {
                    self.speaker_name = speaker_name;
                    ops.push(RecognizerOp::Flush { timestamp_ms });
                }
            }
        }
        let close_at = close.then(|| self.deps.clock.now_ms());
        if ops.is_empty()
            && !self
                .recognizer
                .as_ref()
                .is_some_and(SpeakerRecognizer::has_state)
        {
            return;
        }
        let Some(mut recognizer) = self.recognizer.take() else {
            return;
        };
        let now = Instant::now();
        let outcome = tokio::task::spawn_blocking(move || {
            let events = recognizer.process(ops, close_at, now);
            (recognizer, events)
        })
        .await;
        let events = match outcome {
            Ok((recognizer, events)) => {
                self.recognizer = Some(recognizer);
                events
            }
            Err(err) => {
                // Backstop: adapter panics are caught inside `process`; anything else loses the
                // speaker's stream only.
                if err.is_panic() {
                    self.deps.stats.task_panic();
                    self.deps.stats.stt_stream_reset();
                    tracing::error!(
                        source = %self.speaker_key.source,
                        user_id = self.speaker_key.user_id,
                        "speaker STT task panicked; stream dropped"
                    );
                }
                self.recognizer = Some(SpeakerRecognizer::new(
                    self.speaker_key.clone(),
                    &self.deps,
                    Arc::clone(&self.discarded),
                ));
                Vec::new()
            }
        };
        self.publish(events).await;
    }

    /// Emits Interim / Final cards and enqueues finals for translation, each under the source
    /// gate so that a concurrent `SourceReset` either precedes the emit (suppressing it) or
    /// follows it (clearing it).
    ///
    /// Takes `&mut self` only so that the future is `Send`: `&Self` is not, because the owned
    /// `SttStream` is `Send` but not `Sync`.
    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "`&Self` is not `Send` (see above)"
    )]
    async fn publish(&mut self, events: Vec<TranscriptEvent>) {
        let deps = &self.deps;
        for event in events {
            if self.discarded.load(Ordering::Acquire) {
                return;
            }
            let mut gate = self.source.gate.lock().await;
            if gate.epoch != self.epoch {
                return;
            }
            let stage = if event.is_final {
                SubtitleStage::FinalPendingTranslation
            } else {
                SubtitleStage::Interim
            };
            let entry = SubtitleEntry {
                utterance_id: event.utterance_id,
                speaker_key: event.speaker_key.clone(),
                speaker_name: self.speaker_name.clone(),
                speaker_color: self.speaker_color.clone(),
                src_text: event.text.clone(),
                translated_text: None,
                stage,
                failure_reason: None,
                timestamp_ms: event.timestamp_ms,
            };
            emit_contained(&deps.sink, entry, &deps.stats).await;

            if !event.is_final || !deps.pre_filter.should_translate(&event.text) {
                continue;
            }
            let seq = gate.reserve_seq();
            let context = gate.context.snapshot();
            drop(gate);

            let job = TranslationJob {
                source: Arc::clone(&self.source),
                epoch: self.epoch,
                seq,
                req: TranslationRequest {
                    utterance_id: event.utterance_id,
                    speaker_key: event.speaker_key,
                    speaker_name: self.speaker_name.clone(),
                    text: event.text,
                    src_lang: deps.src_lang.clone(),
                    tgt_lang: deps.tgt_lang.clone(),
                    context,
                },
                speaker_name: self.speaker_name.clone(),
                speaker_color: self.speaker_color.clone(),
                timestamp_ms: event.timestamp_ms,
                finalized_at: Instant::now(),
            };
            if let Err(mpsc::error::SendError(job)) = deps.jobs.send(job).await {
                // The dispatcher is gone; keep the context window's commit order moving.
                let mut gate = job.source.gate.lock().await;
                if gate.epoch == job.epoch {
                    gate.complete(job.seq, None);
                }
            }
        }
    }
}

/// One finalized utterance waiting for translation.
#[derive(Clone)]
struct TranslationJob {
    source: Arc<SourceShared>,
    epoch: u64,
    seq: u64,
    req: TranslationRequest,
    speaker_name: String,
    speaker_color: String,
    timestamp_ms: u64,
    finalized_at: Instant,
}

impl TranslationJob {
    fn is_stale(&self) -> bool {
        self.source.is_stale(self.epoch)
    }
}

/// How a [`TranslationJob`] ended.
enum JobOutcome {
    Translated(String),
    Failed(&'static str),
    /// Not translated within the deadline.
    Skipped,
}

/// Bounded, deadline-aware translation scheduler (see the module docs).
struct TranslationDispatcher {
    translator_manager: Arc<TranslatorManager>,
    sink: Arc<dyn SubtitleSink>,
    stats: PipelineStatsHandle,
    max_concurrent: usize,
    max_pending: usize,
    deadline: Duration,
}

impl TranslationDispatcher {
    /// Translates jobs from `rx` until it closes and every job has resolved, or until `shutdown`
    /// fires: then every job not resolved yet is reported as skipped and this returns at once.
    async fn run(
        self,
        mut rx: mpsc::Receiver<TranslationJob>,
        mut shutdown: oneshot::Receiver<()>,
    ) {
        let mut pending: VecDeque<TranslationJob> = VecDeque::new();
        let mut in_flight: JoinSet<()> = JoinSet::new();
        // Copies of the in-flight jobs, so that a panicking task still releases its commit slot
        // and a shutdown timeout can still report the job as skipped.
        let mut in_flight_jobs: HashMap<tokio::task::Id, TranslationJob> = HashMap::new();
        let mut input_open = true;
        let mut shutdown_armed = true;

        loop {
            self.expire_late(&mut pending).await;
            while in_flight.len() < self.max_concurrent {
                let Some(job) = pending.pop_front() else {
                    break;
                };
                if job.is_stale() {
                    continue;
                }
                let Some(remaining) = self
                    .deadline
                    .checked_sub(job.finalized_at.elapsed())
                    .filter(|d| !d.is_zero())
                else {
                    self.skip(job).await;
                    continue;
                };
                let meta = job.clone();
                let handle = in_flight.spawn(translate_job(
                    job,
                    remaining,
                    Arc::clone(&self.translator_manager),
                    Arc::clone(&self.sink),
                    self.stats.clone(),
                ));
                in_flight_jobs.insert(handle.id(), meta);
            }
            self.stats
                .set_pending_translations(pending.len() + in_flight.len());

            if !input_open && pending.is_empty() && in_flight.is_empty() {
                return;
            }

            let next_expiry = pending
                .iter()
                .map(|job| job.finalized_at + self.deadline)
                .min();
            tokio::select! {
                biased;
                signal = &mut shutdown, if shutdown_armed => {
                    if signal.is_ok() {
                        self.resolve_all_as_skipped(
                            &mut rx,
                            std::mem::take(&mut pending),
                            &mut in_flight_jobs,
                        )
                            .await;
                        in_flight.abort_all();
                        self.stats.set_pending_translations(0);
                        return;
                    }
                    // The signal can no longer arrive; keep draining normally.
                    shutdown_armed = false;
                }
                job = rx.recv(), if input_open => match job {
                    Some(job) => {
                        pending.push_back(job);
                        while pending.len() > self.max_pending {
                            if let Some(oldest) = pending.pop_front() {
                                self.skip(oldest).await;
                            }
                        }
                        self.stats
                            .set_pending_translations(pending.len() + in_flight.len());
                    }
                    None => input_open = false,
                },
                Some(res) = in_flight.join_next_with_id(), if !in_flight.is_empty() => {
                    match res {
                        Ok((id, ())) => {
                            in_flight_jobs.remove(&id);
                        }
                        Err(err) => {
                            if err.is_panic() {
                                self.stats.task_panic();
                                tracing::error!("translation task panicked");
                            }
                            if let Some(meta) = in_flight_jobs.remove(&err.id()) {
                                let mut gate = meta.source.gate.lock().await;
                                if gate.epoch == meta.epoch {
                                    gate.complete(meta.seq, None);
                                }
                            }
                        }
                    }
                }
                () = tokio::time::sleep_until(next_expiry.unwrap_or_else(Instant::now)),
                    if next_expiry.is_some() => {}
            }
        }
    }

    /// Reports every job that has not resolved yet (still in `rx`, queued, or in flight) as
    /// skipped, for a shutdown that timed out.
    ///
    /// Each report goes through [`finish_job`] under the source gate, so a reset source stays
    /// cleared and a job whose translation task resolves it concurrently is reported only once.
    async fn resolve_all_as_skipped(
        &self,
        rx: &mut mpsc::Receiver<TranslationJob>,
        mut pending: VecDeque<TranslationJob>,
        in_flight_jobs: &mut HashMap<tokio::task::Id, TranslationJob>,
    ) {
        while let Ok(job) = rx.try_recv() {
            pending.push_back(job);
        }
        for (_, job) in in_flight_jobs.drain() {
            self.skip(job).await;
        }
        for job in pending {
            self.skip(job).await;
        }
    }

    /// Skips every queued job whose deadline has passed.
    async fn expire_late(&self, pending: &mut VecDeque<TranslationJob>) {
        let now = Instant::now();
        if !pending
            .iter()
            .any(|job| job.finalized_at + self.deadline <= now)
        {
            return;
        }
        let (late, on_time): (VecDeque<_>, VecDeque<_>) = pending
            .drain(..)
            .partition(|job| job.finalized_at + self.deadline <= now);
        *pending = on_time;
        for job in late {
            self.skip(job).await;
        }
    }

    /// Reports `job` as skipped; contained so that a panicking sink cannot stop the dispatcher.
    async fn skip(&self, job: TranslationJob) {
        if job.is_stale() {
            return;
        }
        let sink = Arc::clone(&self.sink);
        let stats = self.stats.clone();
        if let Err(err) =
            tokio::spawn(async move { finish_job(job, JobOutcome::Skipped, &sink, &stats).await })
                .await
        {
            if err.is_panic() {
                self.stats.task_panic();
                tracing::error!("subtitle sink panicked while reporting a skipped translation");
            }
        }
    }
}

/// Aborts the wrapped task when dropped, so a cancelled job does not leave its translator call
/// running in the background.
struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Translates one utterance within `remaining` and emits Phase C.
///
/// Translator errors and panics are bounded to [`ErrorScope::Utterance`]: whatever variant the
/// adapter returns, only this utterance fails.
async fn translate_job(
    job: TranslationJob,
    remaining: Duration,
    translator_manager: Arc<TranslatorManager>,
    sink: Arc<dyn SubtitleSink>,
    stats: PipelineStatsHandle,
) {
    // Run the adapter call in its own task so that a panicking translator only fails this
    // utterance instead of losing the card in a `JoinError` without an utterance id.
    let req = job.req.clone();
    let handle = tokio::spawn(async move { translator_manager.translate(req).await });
    let _abort_guard = AbortOnDrop(handle.abort_handle());
    let outcome = match tokio::time::timeout(remaining, handle).await {
        Ok(Ok(Ok(result))) => JobOutcome::Translated(result.translated_text),
        Ok(Ok(Err(err))) => {
            let reason = err.translation_failure_reason();
            tracing::warn!(
                utterance_id = job.req.utterance_id.0,
                error_kind = err.kind(),
                reason,
                "translation failed"
            );
            JobOutcome::Failed(reason)
        }
        Ok(Err(join_err)) => {
            if join_err.is_panic() {
                stats.task_panic();
                tracing::error!(utterance_id = job.req.utterance_id.0, "translator panicked");
            }
            JobOutcome::Failed(TRANSLATOR_CRASHED_REASON)
        }
        Err(_elapsed) => JobOutcome::Skipped,
    };
    finish_job(job, outcome, &sink, &stats).await;
}

/// Emits [`SubtitleStage::Translated`] or [`SubtitleStage::TranslationFailed`] for `job` and
/// records the turn in the context window in finalization order, all under the source gate.
///
/// Both stages carry the request's `utterance_id`, whatever the adapter returned.
async fn finish_job(
    job: TranslationJob,
    outcome: JobOutcome,
    sink: &Arc<dyn SubtitleSink>,
    stats: &PipelineStatsHandle,
) {
    let TranslationJob {
        source,
        epoch,
        seq,
        req,
        speaker_name,
        speaker_color,
        timestamp_ms,
        finalized_at: _,
    } = job;
    let mut gate = source.gate.lock().await;
    // A reset source is never revived, and a final resolves only once: at shutdown the
    // dispatcher may report a job as skipped while its translation task is still running.
    if gate.epoch != epoch || gate.is_resolved(seq) {
        return;
    }
    let (turn, translated_text, stage, failure_reason) = match outcome {
        JobOutcome::Translated(text) => (
            Some(HistoryEntry {
                speaker_name: speaker_name.clone(),
                original_text: req.text.clone(),
                translated_text: text.clone(),
            }),
            Some(text),
            SubtitleStage::Translated,
            None,
        ),
        JobOutcome::Failed(reason) => {
            stats.translation_failure();
            (None, None, SubtitleStage::TranslationFailed, Some(reason))
        }
        JobOutcome::Skipped => {
            stats.translation_skipped();
            tracing::info!(
                utterance_id = req.utterance_id.0,
                "translation skipped (too late)"
            );
            (
                None,
                None,
                SubtitleStage::TranslationFailed,
                Some(TRANSLATION_SKIPPED_REASON),
            )
        }
    };
    // Only successful turns enter the context window, in finalization order.
    gate.complete(seq, turn);
    let entry = SubtitleEntry {
        utterance_id: req.utterance_id,
        speaker_key: req.speaker_key,
        speaker_name,
        speaker_color,
        src_text: req.text,
        translated_text,
        stage,
        failure_reason: failure_reason.map(str::to_owned),
        timestamp_ms,
    };
    emit_contained(sink, entry, stats).await;
    drop(gate);
}

/// Polls an [`Unpin`] future inside `catch_unwind`; resolves to `None` if a poll panicked.
struct CatchUnwind<F>(F);

impl<F: Future + Unpin> Future for CatchUnwind<F> {
    type Output = Option<F::Output>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = &mut self.0;
        match catch_unwind(AssertUnwindSafe(|| Pin::new(inner).poll(cx))) {
            Ok(Poll::Ready(output)) => Poll::Ready(Some(output)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(_) => Poll::Ready(None),
        }
    }
}

/// Calls [`SubtitleSink::emit`] so that neither an error nor a panic escapes: both lose this
/// update only (see "Error policy"). Runs in place, so a caller holding the source gate keeps it.
async fn emit_contained(
    sink: &Arc<dyn SubtitleSink>,
    entry: SubtitleEntry,
    stats: &PipelineStatsHandle,
) {
    if let Some(result) = CatchUnwind(sink.emit(entry)).await {
        stats.record_sink(&result);
    } else {
        stats.sink_error();
        stats.task_panic();
        tracing::error!("subtitle sink panicked in emit");
    }
}
