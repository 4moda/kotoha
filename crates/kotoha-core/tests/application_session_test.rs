//! Unit tests for `kotoha-core` session manager (`src/application/session.rs`).

#![allow(clippy::expect_used, clippy::too_many_lines)]

use kotoha_core::application::{SeqStatus, SessionAction, SourceSessionManager};
use kotoha_core::domain::{
    AudioChunk, AudioInputEvent, DisconnectReason, SpeakerKey, TalkingState,
};

#[test]
fn session_manager_conn_id_supersede_and_seq_tracking() {
    let mut mgr = SourceSessionManager::new();

    // Conn 1 connects
    assert_eq!(
        mgr.handle_event(&AudioInputEvent::SourceConnected {
            conn_id: 1,
            source: "mumble".to_owned(),
            plugin_version: "0.1.0".to_owned(),
        }),
        SessionAction::Connected {
            source: "mumble".to_owned(),
            conn_id: 1,
            superseded_conn_id: None,
        }
    );

    let spk1 = SpeakerKey::new("mumble", 10);
    mgr.handle_event(&AudioInputEvent::UserInfoUpdated {
        conn_id: 1,
        speaker_key: spk1.clone(),
        name: "Alice".to_owned(),
    });
    assert_eq!(mgr.speaker_display_name(&spk1), "Alice");

    // Reconnect: Conn 2 supersedes Conn 1 for "mumble"
    assert_eq!(
        mgr.handle_event(&AudioInputEvent::SourceConnected {
            conn_id: 2,
            source: "mumble".to_owned(),
            plugin_version: "0.1.0".to_owned(),
        }),
        SessionAction::Connected {
            source: "mumble".to_owned(),
            conn_id: 2,
            superseded_conn_id: Some(1),
        }
    );
    assert_eq!(mgr.owning_conn_id("mumble"), Some(2));

    // Delayed events from superseded Conn 1 MUST NOT alter Conn 2 state or speakers
    assert_eq!(
        mgr.handle_event(&AudioInputEvent::SourceDisconnected {
            conn_id: 1,
            source: "mumble".to_owned(),
            reason: DisconnectReason::Superseded,
        }),
        SessionAction::None
    );
    assert_eq!(
        mgr.handle_event(&AudioInputEvent::UserInfoUpdated {
            conn_id: 1,
            speaker_key: spk1.clone(),
            name: "StaleAlice".to_owned(),
        }),
        SessionAction::None
    );
    assert_eq!(mgr.speaker_display_name(&spk1), "Alice");
    assert_eq!(
        mgr.handle_event(&AudioInputEvent::TalkingStateChanged {
            conn_id: 1,
            speaker_key: spk1.clone(),
            state: TalkingState::Shouting,
        }),
        SessionAction::None
    );
    assert_eq!(mgr.speaker_talking_state(&spk1), TalkingState::Passive);
    assert_eq!(
        mgr.handle_event(&AudioInputEvent::UserRemoved {
            conn_id: 1,
            speaker_key: spk1.clone(),
        }),
        SessionAction::None
    );
    assert_eq!(mgr.active_speaker_count(), 1);
    assert_eq!(
        mgr.handle_event(&AudioInputEvent::AudioFrame {
            conn_id: 1,
            chunk: AudioChunk {
                speaker_key: spk1.clone(),
                seq: 999,
                sample_rate: 16_000,
                channels: 1,
                is_local_user: false,
                samples: vec![0.1; 160],
            },
        }),
        SessionAction::None
    );
    assert_eq!(
        mgr.source_stats("mumble").expect("stats").frames_received,
        0
    );

    // Lazy seq init starting at u32::MAX - 1 on owning Conn 2
    let make_chunk = |seq: u32| AudioInputEvent::AudioFrame {
        conn_id: 2,
        chunk: AudioChunk {
            speaker_key: spk1.clone(),
            seq,
            sample_rate: 16_000,
            channels: 1,
            is_local_user: false,
            samples: vec![0.1; 160],
        },
    };

    assert_eq!(
        mgr.handle_event(&make_chunk(u32::MAX - 1)),
        SessionAction::AudioSequenceChecked(SeqStatus::Initialized { seq: u32::MAX - 1 })
    );
    assert_eq!(
        mgr.handle_event(&make_chunk(u32::MAX)),
        SessionAction::AudioSequenceChecked(SeqStatus::InOrder)
    );
    // Wrapping across u32::MAX -> 0 -> 1 is in order; jump to 3 drops 2 frames (1, 2)
    assert_eq!(
        mgr.handle_event(&make_chunk(0)),
        SessionAction::AudioSequenceChecked(SeqStatus::InOrder)
    );
    assert_eq!(
        mgr.handle_event(&make_chunk(3)),
        SessionAction::AudioSequenceChecked(SeqStatus::Dropped { dropped_frames: 2 })
    );
    // Regression / duplicate seq (e.g. jump backward to 1 -> delta >= 0x8000_0000) is an anomaly
    assert_eq!(
        mgr.handle_event(&make_chunk(1)),
        SessionAction::AudioSequenceChecked(SeqStatus::SequenceAnomaly {
            expected_seq: 4,
            observed_seq: 1,
        })
    );
    let stats = mgr.source_stats("mumble").expect("mumble stats exist");
    assert_eq!(stats.frames_received, 5);
    assert_eq!(stats.frames_dropped, 2);
    assert_eq!(stats.sequence_anomalies, 1);

    // SourceReset from owning Conn 2 clears speakers but preserves active connection and expected_seq
    let reset_action = mgr.handle_event(&AudioInputEvent::SourceReset {
        conn_id: 2,
        source: "mumble".to_owned(),
    });
    assert_eq!(
        reset_action,
        SessionAction::ClearSpeakers {
            source: "mumble".to_owned(),
            removed_speakers: vec![spk1.clone()],
        }
    );
    assert_eq!(mgr.owning_conn_id("mumble"), Some(2));
    assert_eq!(mgr.active_speaker_count(), 0);

    // Next frame seq=2 continues monotonically after SourceReset
    assert_eq!(
        mgr.handle_event(&make_chunk(2)),
        SessionAction::AudioSequenceChecked(SeqStatus::InOrder)
    );

    // Disconnecting owning Conn 2 clears active source and speakers
    let disc_action = mgr.handle_event(&AudioInputEvent::SourceDisconnected {
        conn_id: 2,
        source: "mumble".to_owned(),
        reason: DisconnectReason::Eof,
    });
    assert_eq!(
        disc_action,
        SessionAction::ClearSpeakers {
            source: "mumble".to_owned(),
            removed_speakers: vec![spk1],
        }
    );
    assert_eq!(mgr.owning_conn_id("mumble"), None);
}
