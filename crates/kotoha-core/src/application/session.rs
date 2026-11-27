//! Source session ownership, speaker metadata, and `AudioFrame` sequence tracking.

use std::collections::{HashMap, HashSet};

use crate::domain::{AudioChunk, AudioInputEvent, SpeakerKey, TalkingState};

/// Outcome of applying an [`AudioInputEvent`] to [`SourceSessionManager`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionAction {
    /// No speaker cleanup required.
    None,
    /// A new connection succeeded `Hello` (optionally superseding an older connection ID).
    Connected {
        /// Source name.
        source: String,
        /// Newly active connection ID.
        conn_id: u64,
        /// Previous owning connection ID, if superseded.
        superseded_conn_id: Option<u64>,
    },
    /// Owning connection requested `SourceReset` or disconnected; remove listed speakers.
    ClearSpeakers {
        /// Source name being cleared.
        source: String,
        /// Speakers previously associated with `source` that must be flushed/removed.
        removed_speakers: Vec<SpeakerKey>,
    },
    /// A single speaker was removed via `UserRemoved`.
    RemoveSpeaker(SpeakerKey),
    /// An `AudioFrame` sequence check completed.
    AudioSequenceChecked(SeqStatus),
}

/// Sequence verification result for an incoming `AudioFrame`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqStatus {
    /// First `AudioFrame` received on the connection (lazy initialization of `expected_seq`).
    Initialized {
        /// Sequence number of the first frame.
        seq: u32,
    },
    /// Frame arrived strictly in order (`seq == expected_seq`).
    InOrder,
    /// One or more intermediate frames were dropped (`1 <= delta < 0x8000_0000`).
    Dropped {
        /// Number of missing frames detected.
        dropped_frames: u32,
    },
    /// Extreme regression / duplicate / sequence reset (`delta >= 0x8000_0000`); resynchronized.
    SequenceAnomaly {
        /// Previous `expected_seq`.
        expected_seq: u32,
        /// Observed out-of-range `seq`.
        observed_seq: u32,
    },
}

/// Per-source connection and drop statistics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceStats {
    /// Total `AudioFrame` messages received from this source.
    pub frames_received: u64,
    /// Total frames inferred dropped via monotonic `seq` gaps.
    pub frames_dropped: u64,
    /// Total sequence regression/duplicate anomalies detected (`delta >= 0x8000_0000`).
    pub sequence_anomalies: u64,
    /// Total heartbeats received.
    pub heartbeats: u64,
    /// Last reported plugin version string.
    pub plugin_version: String,
}

#[derive(Debug, Clone)]
struct ActiveConnection {
    conn_id: u64,
    expected_seq: Option<u32>,
    stats: SourceStats,
}

#[derive(Debug, Clone)]
struct SpeakerMetadata {
    display_name: Option<String>,
    talking_state: TalkingState,
}

impl Default for SpeakerMetadata {
    fn default() -> Self {
        Self {
            display_name: None,
            talking_state: TalkingState::Passive,
        }
    }
}

/// Tracks active IPC source connections (`conn_id` ownership), per-speaker state, and `seq` drops.
///
/// Implements the connection ownership invariant from `DESIGN.md` Section 3.2:
/// when a client reconnects using the same `source`, the new `conn_id` immediately supersedes the
/// old one in `active_sources`. Delayed `SourceDisconnected` / `SourceReset` events carrying the
/// superseded `conn_id` are ignored so they never wipe out active speaker streams.
#[derive(Debug, Default)]
pub struct SourceSessionManager {
    active_sources: HashMap<String, ActiveConnection>,
    source_speakers: HashMap<String, HashSet<SpeakerKey>>,
    speakers: HashMap<SpeakerKey, SpeakerMetadata>,
}

impl SourceSessionManager {
    /// Creates an empty [`SourceSessionManager`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Processes an [`AudioInputEvent`] and returns any speaker cleanup or `seq` inspection result.
    ///
    /// Events originating from a superseded or non-owning `conn_id` are ignored and return
    /// [`SessionAction::None`].
    pub fn handle_event(&mut self, event: &AudioInputEvent) -> SessionAction {
        match event {
            AudioInputEvent::SourceConnected {
                conn_id,
                source,
                plugin_version,
            } => self.on_source_connected(*conn_id, source, plugin_version),
            AudioInputEvent::SourceDisconnected { conn_id, source } => {
                self.on_source_disconnected(*conn_id, source)
            }
            AudioInputEvent::SourceReset { conn_id, source } => {
                self.on_source_reset(*conn_id, source)
            }
            AudioInputEvent::UserInfoUpdated {
                conn_id,
                speaker_key,
                name,
            } => {
                if !self.is_owner(*conn_id, &speaker_key.source) {
                    return SessionAction::None;
                }
                self.register_speaker(speaker_key);
                let meta = self.speakers.entry(speaker_key.clone()).or_default();
                meta.display_name = if name.is_empty() {
                    None
                } else {
                    Some(name.clone())
                };
                SessionAction::None
            }
            AudioInputEvent::UserRemoved {
                conn_id,
                speaker_key,
            } => {
                if !self.is_owner(*conn_id, &speaker_key.source) {
                    return SessionAction::None;
                }
                self.remove_speaker(speaker_key);
                SessionAction::RemoveSpeaker(speaker_key.clone())
            }
            AudioInputEvent::TalkingStateChanged {
                conn_id,
                speaker_key,
                state,
            } => {
                if !self.is_owner(*conn_id, &speaker_key.source) {
                    return SessionAction::None;
                }
                self.register_speaker(speaker_key);
                let meta = self.speakers.entry(speaker_key.clone()).or_default();
                meta.talking_state = *state;
                SessionAction::None
            }
            AudioInputEvent::AudioFrame { conn_id, chunk } => self
                .on_audio_frame(*conn_id, chunk)
                .map_or(SessionAction::None, SessionAction::AudioSequenceChecked),
            AudioInputEvent::Heartbeat { conn_id, source } => {
                if let Some(active) = self.active_sources.get_mut(source) {
                    if active.conn_id == *conn_id {
                        active.stats.heartbeats += 1;
                    }
                }
                SessionAction::None
            }
        }
    }

    /// Returns `true` if `conn_id` is currently the active owning connection for `source`.
    #[must_use]
    pub fn is_owner(&self, conn_id: u64, source: &str) -> bool {
        self.active_sources
            .get(source)
            .is_some_and(|c| c.conn_id == conn_id)
    }

    /// Returns the currently owning `conn_id` for `source`, if connected.
    #[must_use]
    pub fn owning_conn_id(&self, source: &str) -> Option<u64> {
        self.active_sources.get(source).map(|c| c.conn_id)
    }

    /// Returns the resolved display name for `speaker_key`, falling back to `"User #<id>"`.
    #[must_use]
    pub fn speaker_display_name(&self, speaker_key: &SpeakerKey) -> String {
        self.speakers
            .get(speaker_key)
            .and_then(|meta| meta.display_name.as_deref())
            .filter(|name| !name.is_empty())
            .map_or_else(|| format!("User #{}", speaker_key.user_id), str::to_owned)
    }

    /// Returns the last known [`TalkingState`] for `speaker_key` (defaults to [`TalkingState::Passive`]).
    #[must_use]
    pub fn speaker_talking_state(&self, speaker_key: &SpeakerKey) -> TalkingState {
        self.speakers
            .get(speaker_key)
            .map_or(TalkingState::Passive, |meta| meta.talking_state)
    }

    /// Returns a snapshot of connection statistics for `source`.
    #[must_use]
    pub fn source_stats(&self, source: &str) -> Option<&SourceStats> {
        self.active_sources.get(source).map(|c| &c.stats)
    }

    /// Returns all currently tracked speakers across all active sources.
    #[must_use]
    pub fn active_speaker_count(&self) -> usize {
        self.speakers.len()
    }

    fn on_source_connected(
        &mut self,
        conn_id: u64,
        source: &str,
        plugin_version: &str,
    ) -> SessionAction {
        let superseded_conn_id = self.active_sources.get(source).map(|prev| prev.conn_id);
        let stats = SourceStats {
            plugin_version: plugin_version.to_owned(),
            ..Default::default()
        };
        self.active_sources.insert(
            source.to_owned(),
            ActiveConnection {
                conn_id,
                expected_seq: None,
                stats,
            },
        );
        SessionAction::Connected {
            source: source.to_owned(),
            conn_id,
            superseded_conn_id,
        }
    }

    fn on_source_disconnected(&mut self, conn_id: u64, source: &str) -> SessionAction {
        if !self.is_owner(conn_id, source) {
            return SessionAction::None;
        }
        self.active_sources.remove(source);
        let removed_speakers = self.clear_speakers_for_source(source);
        SessionAction::ClearSpeakers {
            source: source.to_owned(),
            removed_speakers,
        }
    }

    fn on_source_reset(&mut self, conn_id: u64, source: &str) -> SessionAction {
        // Per DESIGN.md Section 3.2 & 3.4:
        // - Verify conn_id ownership before cleaning up speaker streams.
        // - Keep active_sources registration and expected_seq intact across SourceReset!
        if !self.is_owner(conn_id, source) {
            return SessionAction::None;
        }
        let removed_speakers = self.clear_speakers_for_source(source);
        SessionAction::ClearSpeakers {
            source: source.to_owned(),
            removed_speakers,
        }
    }

    fn on_audio_frame(&mut self, conn_id: u64, chunk: &AudioChunk) -> Option<SeqStatus> {
        if !self.is_owner(conn_id, &chunk.speaker_key.source) {
            return None;
        }

        self.register_speaker(&chunk.speaker_key);

        let active = self.active_sources.get_mut(&chunk.speaker_key.source)?;
        active.stats.frames_received += 1;

        let status = match active.expected_seq {
            None => {
                active.expected_seq = Some(chunk.seq.wrapping_add(1));
                SeqStatus::Initialized { seq: chunk.seq }
            }
            Some(expected) => {
                let delta = chunk.seq.wrapping_sub(expected);
                active.expected_seq = Some(chunk.seq.wrapping_add(1));
                if delta == 0 {
                    SeqStatus::InOrder
                } else if delta < 0x8000_0000 {
                    active.stats.frames_dropped += u64::from(delta);
                    SeqStatus::Dropped {
                        dropped_frames: delta,
                    }
                } else {
                    active.stats.sequence_anomalies += 1;
                    SeqStatus::SequenceAnomaly {
                        expected_seq: expected,
                        observed_seq: chunk.seq,
                    }
                }
            }
        };

        Some(status)
    }

    fn register_speaker(&mut self, speaker_key: &SpeakerKey) {
        self.source_speakers
            .entry(speaker_key.source.clone())
            .or_default()
            .insert(speaker_key.clone());
        self.speakers.entry(speaker_key.clone()).or_default();
    }

    fn remove_speaker(&mut self, speaker_key: &SpeakerKey) {
        self.speakers.remove(speaker_key);
        if let Some(set) = self.source_speakers.get_mut(&speaker_key.source) {
            set.remove(speaker_key);
            if set.is_empty() {
                self.source_speakers.remove(&speaker_key.source);
            }
        }
    }

    fn clear_speakers_for_source(&mut self, source: &str) -> Vec<SpeakerKey> {
        let Some(keys) = self.source_speakers.remove(source) else {
            return Vec::new();
        };
        let mut removed = Vec::with_capacity(keys.len());
        for key in keys {
            self.speakers.remove(&key);
            removed.push(key);
        }
        removed
    }
}
