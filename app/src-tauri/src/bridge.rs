//! Tauri event bridge (`SubtitleSink` + IPC connection monitor + background pipeline runner).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use kotoha_core::application::{PipelineConfig, PipelineService, SourceSessionManager};
use kotoha_core::domain::{
    AudioInputEvent, CoreError, LanguageCode, SpeakerKey, SttEngine, SubtitleEntry, SubtitleSink,
};
use kotoha_core::infrastructure::{AudioPreprocessor, IpcServerAdapter, MockSttEngine};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, watch};

pub use crate::state::EVENT_OVERLAY_STYLE_CHANGED;
use crate::state::{
    AppState, ConnectionState, ConnectionStatusDto, OverlayDisplayOptionsDto,
    EVENT_CONNECTION_STATUS, EVENT_SUBTITLE,
};

/// Emits `kotoha://overlay-style-changed` with the updated [`OverlayDisplayOptionsDto`] to all
/// Tauri windows so separate `WebViews` (e.g. `overlay`) synchronize styling immediately.
pub fn emit_overlay_style_changed(
    app: &AppHandle,
    style: &OverlayDisplayOptionsDto,
) -> Result<(), String> {
    app.emit(EVENT_OVERLAY_STYLE_CHANGED, style)
        .map_err(|err| format!("failed to emit '{EVENT_OVERLAY_STYLE_CHANGED}': {err}"))
}

/// [`SubtitleSink`] adapter forwarding 3-stage subtitle cards to Tauri windows and recording
/// recent entries in [`AppState`].
pub struct TauriSubtitleSink {
    app: AppHandle,
    state: Arc<AppState>,
}

impl TauriSubtitleSink {
    /// Creates a new [`TauriSubtitleSink`].
    #[must_use]
    pub const fn new(app: AppHandle, state: Arc<AppState>) -> Self {
        Self { app, state }
    }
}

#[async_trait]
impl SubtitleSink for TauriSubtitleSink {
    async fn emit(&self, entry: SubtitleEntry) -> Result<(), CoreError> {
        self.state.record_subtitle(entry.clone()).await;
        if let Err(err) = self.app.emit(EVENT_SUBTITLE, &entry) {
            return Err(CoreError::Sink(format!(
                "failed to emit '{EVENT_SUBTITLE}': {err}"
            )));
        }
        Ok(())
    }

    async fn clear_source(&self, source: &str) -> Result<(), CoreError> {
        self.state.clear_subtitles_for_source(source).await;
        Ok(())
    }
}

/// Tracks IPC connection status and heartbeats from [`AudioInputEvent`] stream and emits
/// `kotoha://connection-status` events whenever connection state changes.
pub struct ConnectionStatusMonitor {
    endpoint: String,
    session: SourceSessionManager,
    active_source: Option<String>,
    last_activity: Option<Instant>,
    connected: bool,
}

impl ConnectionStatusMonitor {
    /// Creates a new [`ConnectionStatusMonitor`] bound to `endpoint`.
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            session: SourceSessionManager::new(),
            active_source: None,
            last_activity: None,
            connected: false,
        }
    }

    /// Processes an [`AudioInputEvent`] and returns `Some(ConnectionStatusDto)` if an update
    /// should be pushed to UI listeners.
    pub fn observe(&mut self, event: &AudioInputEvent) -> Option<ConnectionStatusDto> {
        let _action = self.session.handle_event(event);
        match event {
            AudioInputEvent::SourceConnected { source, .. } => {
                self.active_source = Some(source.clone());
                self.last_activity = Some(Instant::now());
                self.connected = true;
                Some(self.build_dto(ConnectionState::Connected))
            }
            AudioInputEvent::Heartbeat { source, .. } => {
                if self.session.owning_conn_id(source).is_some() {
                    self.active_source = Some(source.clone());
                    self.last_activity = Some(Instant::now());
                    self.connected = true;
                    Some(self.build_dto(ConnectionState::Connected))
                } else {
                    None
                }
            }
            AudioInputEvent::SourceDisconnected { .. } => {
                let remaining_owner = self
                    .active_source
                    .as_deref()
                    .and_then(|active| self.session.owning_conn_id(active));
                if remaining_owner.is_some() {
                    None
                } else {
                    self.connected = false;
                    self.last_activity = None;
                    Some(self.build_dto(ConnectionState::Disconnected))
                }
            }
            AudioInputEvent::SourceReset { source, .. }
                if self.session.owning_conn_id(source).is_some() =>
            {
                self.last_activity = Some(Instant::now());
                Some(self.build_dto(ConnectionState::Connected))
            }
            AudioInputEvent::UserInfoUpdated { .. }
            | AudioInputEvent::UserRemoved { .. }
            | AudioInputEvent::AudioFrame { .. } => {
                self.last_activity = Some(Instant::now());
                None
            }
            _ => None,
        }
    }

    /// Returns a snapshot [`ConnectionStatusDto`] representing the current monitor state.
    #[must_use]
    pub fn snapshot(&self) -> ConnectionStatusDto {
        let state = if self.connected {
            ConnectionState::Connected
        } else {
            ConnectionState::Disconnected
        };
        self.build_dto(state)
    }

    fn build_dto(&self, state: ConnectionState) -> ConnectionStatusDto {
        let source = self
            .active_source
            .clone()
            .unwrap_or_else(|| "mumble".to_owned());
        let src_stats = self.session.source_stats(&source);
        let plugin_version = src_stats
            .map(|s| s.plugin_version.clone())
            .filter(|v| !v.is_empty());
        let dropped_frames = src_stats.map_or(0, |s| s.frames_dropped);
        let last_heartbeat_ms = if state == ConnectionState::Connected {
            Some(self.last_activity.map_or(0, |t| {
                u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX)
            }))
        } else {
            None
        };

        ConnectionStatusDto {
            state,
            source,
            plugin_version,
            endpoint: self.endpoint.clone(),
            active_speakers: self.session.active_speaker_count(),
            dropped_frames,
            last_heartbeat_ms,
        }
    }
}

/// Mirrors the ownership rules of `SourceSessionManager` for the events a restarted pipeline
/// needs: audio frames are only accepted from the *current owner* of a source, so a new pipeline
/// must see exactly that owner's `SourceConnected` (and the display names registered since).
#[derive(Default)]
struct SessionReplayLog {
    /// Current owning `SourceConnected` event per source (a later connect supersedes).
    owners: HashMap<String, (u64, AudioInputEvent)>,
    /// Latest `UserInfoUpdated` per speaker, accepted from the owning connection only.
    user_info: HashMap<SpeakerKey, AudioInputEvent>,
}

impl SessionReplayLog {
    fn is_owner(&self, conn_id: u64, source: &str) -> bool {
        self.owners
            .get(source)
            .is_some_and(|(id, _)| *id == conn_id)
    }

    fn clear_source(&mut self, source: &str) {
        self.user_info.retain(|key, _| key.source != source);
    }

    fn observe(&mut self, event: &AudioInputEvent) {
        match event {
            AudioInputEvent::SourceConnected {
                conn_id, source, ..
            } => {
                self.owners
                    .insert(source.clone(), (*conn_id, event.clone()));
            }
            AudioInputEvent::SourceDisconnected {
                conn_id, source, ..
            } => {
                if self.is_owner(*conn_id, source) {
                    self.owners.remove(source);
                    self.clear_source(source);
                }
            }
            AudioInputEvent::SourceReset { conn_id, source } => {
                if self.is_owner(*conn_id, source) {
                    self.clear_source(source);
                }
            }
            AudioInputEvent::UserInfoUpdated {
                conn_id,
                speaker_key,
                ..
            } => {
                if self.is_owner(*conn_id, &speaker_key.source) {
                    self.user_info.insert(speaker_key.clone(), event.clone());
                }
            }
            AudioInputEvent::UserRemoved {
                conn_id,
                speaker_key,
            } if self.is_owner(*conn_id, &speaker_key.source) => {
                self.user_info.remove(speaker_key);
            }
            _ => {}
        }
    }

    fn replay(&self) -> Vec<AudioInputEvent> {
        let mut owners: Vec<_> = self.owners.values().collect();
        owners.sort_by_key(|(id, _)| *id);
        owners
            .into_iter()
            .map(|(_, e)| e.clone())
            .chain(self.user_info.values().cloned())
            .collect()
    }
}

/// Creates the STT engine shared by every pipeline instance of one application run.
#[must_use]
pub fn new_shared_stt_engine() -> Arc<dyn SttEngine> {
    Arc::new(MockSttEngine::default())
}

/// A running pipeline task together with the sender feeding it.
struct RunningPipeline {
    tx: mpsc::Sender<AudioInputEvent>,
    handle: tokio::task::JoinHandle<()>,
}

/// Builds a [`PipelineService`] for `(src_lang, tgt_lang)` and runs it on its own task.
///
/// The language pair is fixed for the lifetime of a `PipelineService`, so a language change is
/// applied by starting a new pipeline (see [`spawn_background_pipeline`]); `replay` re-introduces
/// the sources that are already connected.
///
/// `stt_engine` is created once by the caller and shared by every restarted pipeline: it owns the
/// utterance-id counter, and ids must stay unique because [`AppState::record_subtitle`] and the
/// UI upsert subtitle cards by `utterance_id`.
async fn start_pipeline(
    app: &AppHandle,
    state: &Arc<AppState>,
    pair: (LanguageCode, LanguageCode),
    stt_engine: Arc<dyn SttEngine>,
    replay: Vec<AudioInputEvent>,
) -> RunningPipeline {
    let (src_lang, tgt_lang) = pair;
    let translator_manager = Arc::clone(state.translator_manager());
    let sink: Arc<dyn SubtitleSink> =
        Arc::new(TauriSubtitleSink::new(app.clone(), Arc::clone(state)));
    let resample_fn = Arc::new(AudioPreprocessor::process_chunk);

    let mut pipeline = PipelineService::new(
        PipelineConfig::new(src_lang, tgt_lang),
        stt_engine,
        translator_manager,
        sink,
        resample_fn,
    );

    {
        let glossary_mgr = state.glossary_manager().read().await;
        let _ = glossary_mgr.sync_pipeline(&mut pipeline).await;
        drop(glossary_mgr);
    }

    let (tx, rx) = mpsc::channel::<AudioInputEvent>(256);
    let handle = tokio::spawn(async move {
        run_pipeline_with_rx(&mut pipeline, rx).await;
    });
    // Sent after the task is running so more events than the channel capacity cannot deadlock.
    for event in replay {
        if tx.send(event).await.is_err() {
            break;
        }
    }
    RunningPipeline { tx, handle }
}

/// Spawns the background IPC server and audio-to-subtitle pipeline onto Tauri's async runtime.
///
/// Language changes take effect without a restart: when the configured language pair changes the
/// current pipeline is retired (its sender is dropped so it drains in-flight work) and a new one
/// is started with the new pair. Subtitles in flight at that moment finish with the old pair.
pub fn spawn_background_pipeline(app: AppHandle, shutdown_rx: watch::Receiver<bool>) {
    let state = app.state::<Arc<AppState>>().inner().clone();

    tauri::async_runtime::spawn(async move {
        let ipc_server = IpcServerAdapter::from_env();
        let endpoint = ipc_server.endpoint().to_owned();
        let mut monitor = ConnectionStatusMonitor::new(endpoint.clone());

        state.set_connection_status(monitor.snapshot()).await;
        let _ = app.emit(EVENT_CONNECTION_STATUS, monitor.snapshot());

        let (raw_tx, mut raw_rx) = mpsc::channel::<AudioInputEvent>(256);

        let ipc_shutdown = shutdown_rx.clone();
        let ipc_handle = tokio::spawn(async move {
            let _ = ipc_server.run(raw_tx, ipc_shutdown, None).await;
        });

        let mut lang_rx = state.subscribe_language_pair();
        let mut current_pair = lang_rx.borrow_and_update().clone();
        let mut replay_log = SessionReplayLog::default();
        let stt_engine = new_shared_stt_engine();
        let mut pipeline = start_pipeline(
            &app,
            &state,
            current_pair.clone(),
            Arc::clone(&stt_engine),
            Vec::new(),
        )
        .await;
        let mut retired: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        let mut forward_shutdown = shutdown_rx;
        loop {
            let shutdown_fut = wait_for_shutdown(&mut forward_shutdown);
            tokio::select! {
                biased;
                () = shutdown_fut => {
                    break;
                }
                changed = lang_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let next_pair = lang_rx.borrow_and_update().clone();
                    if next_pair != current_pair {
                        current_pair = next_pair;
                        let next = start_pipeline(
                            &app,
                            &state,
                            current_pair.clone(),
                            Arc::clone(&stt_engine),
                            replay_log.replay(),
                        )
                        .await;
                        let old = std::mem::replace(&mut pipeline, next);
                        drop(old.tx);
                        retired.push(old.handle);
                        retired.retain(|h| !h.is_finished());
                    }
                }
                maybe_event = raw_rx.recv() => {
                    let Some(event) = maybe_event else {
                        break;
                    };
                    replay_log.observe(&event);
                    if let Some(status) = monitor.observe(&event) {
                        state.set_connection_status(status.clone()).await;
                        let _ = app.emit(EVENT_CONNECTION_STATUS, &status);
                    }
                    if pipeline.tx.send(event).await.is_err() {
                        break;
                    }
                }
            }
        }

        drop(pipeline.tx);
        let _ = pipeline.handle.await;
        for handle in retired {
            let _ = handle.await;
        }
        let _ = ipc_handle.await;
    });
}

async fn wait_for_shutdown(shutdown_rx: &mut watch::Receiver<bool>) {
    let _ = shutdown_rx.wait_for(|v| *v).await;
}

async fn run_pipeline_with_rx(pipeline: &mut PipelineService, rx: mpsc::Receiver<AudioInputEvent>) {
    // `run` owns the per-speaker STT workers and the bounded translation queue, and recovers
    // from every non-fatal error itself.
    let _ = pipeline.run(rx).await;
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use kotoha_core::domain::DisconnectReason;

    use super::*;

    fn key(source: &str, user_id: u32) -> SpeakerKey {
        SpeakerKey::new(source, user_id)
    }

    fn connected(conn_id: u64, source: &str) -> AudioInputEvent {
        AudioInputEvent::SourceConnected {
            conn_id,
            source: source.to_owned(),
            plugin_version: "1".to_owned(),
        }
    }

    fn user_info(conn_id: u64, source: &str, user_id: u32, name: &str) -> AudioInputEvent {
        AudioInputEvent::UserInfoUpdated {
            conn_id,
            speaker_key: key(source, user_id),
            name: name.to_owned(),
        }
    }

    #[test]
    fn shared_stt_engine_keeps_utterance_ids_unique_across_pipeline_restarts() {
        let engine = new_shared_stt_engine();
        let lang = LanguageCode::new("en").unwrap();
        let loud = vec![0.5_f32; 1600];

        // Each "pipeline" creates its own streams from the same shared engine.
        let mut ids = Vec::new();
        for user_id in [1, 2] {
            let mut stream = engine.create_stream(key("mumble", user_id), &lang).unwrap();
            let events = stream.accept_audio(&loud, 0).unwrap();
            ids.push(events[0].utterance_id);
        }
        assert_ne!(ids[0], ids[1]);

        // A per-restart engine would restart at 1 and collide, which this guards against.
        let fresh = new_shared_stt_engine();
        let mut stream = fresh.create_stream(key("mumble", 1), &lang).unwrap();
        let colliding = stream.accept_audio(&loud, 0).unwrap()[0].utterance_id;
        assert_eq!(colliding, ids[0]);
    }

    #[test]
    fn replay_log_tracks_latest_owner_per_source_not_ascending_conn_id() {
        let mut log = SessionReplayLog::default();
        log.observe(&connected(7, "mumble"));
        log.observe(&connected(3, "mumble")); // takes over the source
        log.observe(&connected(5, "ts3"));
        log.observe(&AudioInputEvent::SourceDisconnected {
            conn_id: 7, // stale, no longer the owner
            source: "mumble".to_owned(),
            reason: DisconnectReason::Eof,
        });

        let replayed = log.replay();
        assert_eq!(replayed.len(), 2);
        assert!(replayed.contains(&connected(3, "mumble")));
        assert!(replayed.contains(&connected(5, "ts3")));
        assert!(!replayed.contains(&connected(7, "mumble")));
    }

    #[test]
    fn replay_log_clears_names_on_reset_and_ignores_non_owner_updates() {
        let mut log = SessionReplayLog::default();
        log.observe(&connected(1, "mumble"));
        log.observe(&user_info(1, "mumble", 10, "Alice"));
        log.observe(&user_info(99, "mumble", 11, "Intruder"));
        assert!(log.replay().contains(&user_info(1, "mumble", 10, "Alice")));
        assert_eq!(log.replay().len(), 2);

        log.observe(&AudioInputEvent::SourceReset {
            conn_id: 1,
            source: "mumble".to_owned(),
        });
        assert_eq!(log.replay(), vec![connected(1, "mumble")]);

        log.observe(&user_info(1, "mumble", 10, "Alice"));
        log.observe(&AudioInputEvent::UserRemoved {
            conn_id: 1,
            speaker_key: key("mumble", 10),
        });
        assert_eq!(log.replay(), vec![connected(1, "mumble")]);
    }
}
