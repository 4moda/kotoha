//! Tauri event bridge (`SubtitleSink` + IPC connection monitor + background pipeline runner).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use kotoha_core::application::{PipelineConfig, PipelineService, SourceSessionManager};
use kotoha_core::domain::{
    AudioInputEvent, CoreError, LanguageCode, SpeakerKey, SubtitleEntry, SubtitleSink,
};
use kotoha_core::infrastructure::{AudioPreprocessor, IpcServerAdapter, MockSttEngine};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

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

/// Tracks the lifecycle events needed to bring a freshly started pipeline up to date: audio
/// frames are only accepted from sources whose `SourceConnected` the pipeline has seen.
#[derive(Default)]
struct SessionReplayLog {
    connected: HashMap<u64, AudioInputEvent>,
    user_info: HashMap<(u64, SpeakerKey), AudioInputEvent>,
}

impl SessionReplayLog {
    fn observe(&mut self, event: &AudioInputEvent) {
        match event {
            AudioInputEvent::SourceConnected { conn_id, .. } => {
                self.connected.insert(*conn_id, event.clone());
            }
            AudioInputEvent::SourceDisconnected { conn_id, .. } => {
                self.connected.remove(conn_id);
                self.user_info.retain(|(conn, _), _| conn != conn_id);
            }
            AudioInputEvent::UserInfoUpdated {
                conn_id,
                speaker_key,
                ..
            } => {
                self.user_info
                    .insert((*conn_id, speaker_key.clone()), event.clone());
            }
            AudioInputEvent::UserRemoved {
                conn_id,
                speaker_key,
            } => {
                self.user_info.remove(&(*conn_id, speaker_key.clone()));
            }
            _ => {}
        }
    }

    fn replay(&self) -> Vec<AudioInputEvent> {
        let mut conns: Vec<_> = self.connected.iter().collect();
        conns.sort_by_key(|(id, _)| **id);
        conns
            .into_iter()
            .map(|(_, e)| e.clone())
            .chain(self.user_info.values().cloned())
            .collect()
    }
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
async fn start_pipeline(
    app: &AppHandle,
    state: &Arc<AppState>,
    pair: (LanguageCode, LanguageCode),
    replay: Vec<AudioInputEvent>,
) -> RunningPipeline {
    let (src_lang, tgt_lang) = pair;
    let stt_engine = Arc::new(MockSttEngine::default());
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
    for event in replay {
        // The channel is empty and larger than the number of replayed events in practice.
        let _ = tx.try_send(event);
    }
    let handle = tokio::spawn(async move {
        run_pipeline_with_rx(&mut pipeline, rx).await;
    });
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
        let mut pipeline = start_pipeline(&app, &state, current_pair.clone(), Vec::new()).await;
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

async fn run_pipeline_with_rx(
    pipeline: &mut PipelineService,
    mut rx: mpsc::Receiver<AudioInputEvent>,
) {
    let mut translation_tasks = JoinSet::new();
    while let Some(event) = rx.recv().await {
        let _ = pipeline.process_event(event, &mut translation_tasks).await;
        while let Some(_res) = translation_tasks.try_join_next() {}
    }
    while let Some(_res) = translation_tasks.join_next().await {}
}
