//! Comprehensive unit and integration tests for `kotoha-core`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::too_many_lines
)]

use std::collections::HashSet;
use std::f32::consts::PI;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kotoha_core::application::{
    PipelineConfig, PipelineService, SeqStatus, SessionAction, SourceSessionManager,
    TranslatorManager,
};
use kotoha_core::domain::{
    default_model_catalog, AudioChunk, AudioInputEvent, ComputeDevice, ContextWindow, CoreError,
    GlossaryEntry, HistoryEntry, LanguageCode, PreFilter, SpeakerKey, SubtitleStage,
    TranslationProviderConfig, TranslationRequest, TranslationResult, Translator,
    TranslatorFactory, UtteranceId,
};
use kotoha_core::infrastructure::{
    connect_local_socket, AudioPreprocessor, CollectingSubtitleSink, DefaultTranslatorFactory,
    IpcServerAdapter, MockSttEngine, MockTranslator, TARGET_SAMPLE_RATE,
};
use kotoha_protocol::{encode_frame, Message, TalkingStateKind};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot, watch};

fn test_endpoint(tag: &str) -> String {
    let pid = std::process::id();
    if cfg!(windows) {
        format!(r"\\.\pipe\kt_core_{pid}_{tag}")
    } else {
        // Keep path well under macOS 104-byte sun_path limit.
        format!("/tmp/kt_{pid}_{tag}.sock")
    }
}

#[test]
fn domain_speaker_key_color_is_deterministic_and_hex() {
    let a1 = SpeakerKey::new("mumble", 42);
    let a2 = SpeakerKey::new("mumble", 42);
    let b = SpeakerKey::new("ts3", 42);

    assert_eq!(a1.color_hex(), a2.color_hex());
    assert!(a1.color_hex().starts_with('#'));
    assert_eq!(a1.color_hex().len(), 7);
    assert!(b.color_hex().starts_with('#'));
}

#[test]
fn domain_language_code_validation() {
    assert!(LanguageCode::new("en").is_ok());
    assert!(LanguageCode::new("ja").is_ok());
    assert!(LanguageCode::new("en-US").is_ok());
    assert!(LanguageCode::new("auto").is_ok());

    assert!(LanguageCode::new("").is_err());
    assert!(LanguageCode::new("a").is_err());
    assert!(LanguageCode::new(" en").is_err());
    assert!(LanguageCode::new("en_US").is_err());
    assert!(LanguageCode::new("en--US").is_err());
}

#[test]
fn domain_prefilter_and_context_window() {
    let filter = PreFilter::default();
    assert!(!filter.should_translate(""));
    assert!(!filter.should_translate("   \t\n  "));
    assert!(!filter.should_translate("...!?"));
    assert!(!filter.should_translate("uh"));
    assert!(!filter.should_translate("Um... hmm!"));
    assert!(filter.should_translate("Hello world"));
    assert!(filter.should_translate("uh wait a second"));

    let mut ctx = ContextWindow::new(3);
    ctx.set_glossary(vec![GlossaryEntry {
        term: "GG".to_owned(),
        translation: "ナイスゲーム".to_owned(),
    }]);
    for i in 1..=5 {
        ctx.push_turn(HistoryEntry {
            speaker_name: "Alice".to_owned(),
            original_text: format!("src-{i}"),
            translated_text: format!("tgt-{i}"),
        });
    }

    let snap = ctx.snapshot();
    assert_eq!(snap.glossary.len(), 1);
    assert_eq!(snap.recent_history.len(), 3);
    assert_eq!(snap.recent_history[0].original_text, "src-3");
    assert_eq!(snap.recent_history[2].original_text, "src-5");
}

#[test]
fn domain_model_catalog_contains_design_profiles() {
    let catalog = default_model_catalog();
    let ids: HashSet<&str> = catalog.iter().map(|m| m.model_id.as_str()).collect();
    assert!(ids.contains("fugumt-en-ja-int8"));
    assert!(ids.contains("opus-mt-en-ja-int8"));
    assert!(ids.contains("nllb-200-distilled-600m-int8"));
    assert!(ids.contains("nllb-200-1.3b-fp16"));
}

#[test]
fn audio_preprocessor_16k_mono_passthrough_and_48k_stereo_resample() {
    // 1. 16 kHz mono fast-path exact preservation
    let mono_16k: Vec<f32> = (0..320).map(|i| ((i as f32) * 0.01).sin()).collect();
    let out_fast =
        AudioPreprocessor::process_interleaved(&mono_16k, 16_000, 1).expect("16k mono passthrough");
    assert_eq!(out_fast, mono_16k);

    // 2. 48 kHz stereo 100ms (4800 stereo frames = 9600 samples) -> 1600 mono 16k samples
    //    440 Hz sine wave with opposite-phase second channel test vs identical stereo channels
    let src_rate = 48_000u32;
    let src_frames = 4_800usize;
    let freq_hz = 440.0f32;
    let mut stereo = Vec::with_capacity(src_frames * 2);
    for i in 0..src_frames {
        let t = (i as f32) / (src_rate as f32);
        let sample = 0.6 * (2.0 * PI * freq_hz * t).sin();
        stereo.push(sample);
        stereo.push(sample);
    }

    let resampled =
        AudioPreprocessor::process_interleaved(&stereo, src_rate, 2).expect("48k stereo resample");
    assert_eq!(resampled.len(), 1_600);
    assert!(resampled
        .iter()
        .all(|s| s.is_finite() && (-1.0..=1.0).contains(s)));

    // Verify dominant frequency via zero-crossing count (~2 * 440 Hz * 0.1 s = 88 crossings)
    let zero_crossings = resampled
        .windows(2)
        .filter(|w| (w[0] < 0.0 && w[1] >= 0.0) || (w[0] >= 0.0 && w[1] < 0.0))
        .count();
    assert!(
        (84..=92).contains(&zero_crossings),
        "expected ~88 zero crossings for 440Hz/100ms, got {zero_crossings}"
    );
    assert_eq!(TARGET_SAMPLE_RATE, 16_000);
}

#[test]
fn audio_preprocessor_rejects_invalid_inputs() {
    assert!(AudioPreprocessor::process_interleaved(&[0.0], 7_999, 1).is_err());
    assert!(AudioPreprocessor::process_interleaved(&[0.0], 192_001, 1).is_err());
    assert!(AudioPreprocessor::process_interleaved(&[0.0], 48_000, 0).is_err());
    assert!(AudioPreprocessor::process_interleaved(&[0.0], 48_000, 9).is_err());
    assert!(AudioPreprocessor::process_interleaved(&[0.0], 48_000, 2).is_err());
}

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

    // Delayed disconnect from Conn 1 MUST NOT clear speakers owned by Conn 2
    assert_eq!(
        mgr.handle_event(&AudioInputEvent::SourceDisconnected {
            conn_id: 1,
            source: "mumble".to_owned(),
        }),
        SessionAction::None
    );
    assert_eq!(mgr.owning_conn_id("mumble"), Some(2));
    assert_eq!(mgr.active_speaker_count(), 1);

    // Lazy seq init starting at u32::MAX - 1 (non-zero reconnect offset)
    let make_chunk = |seq: u32| {
        AudioInputEvent::AudioFrame(AudioChunk {
            speaker_key: spk1.clone(),
            seq,
            sample_rate: 16_000,
            channels: 1,
            is_local_user: false,
            samples: vec![0.1; 160],
        })
    };

    assert_eq!(
        mgr.handle_event(&make_chunk(u32::MAX - 1)),
        SessionAction::AudioSequenceChecked(SeqStatus::Initialized { seq: u32::MAX - 1 })
    );
    assert_eq!(
        mgr.handle_event(&make_chunk(u32::MAX)),
        SessionAction::AudioSequenceChecked(SeqStatus::InOrder)
    );
    // Wrapping across u32::MAX -> 0 -> 1 is in order; jump to 4 drops 2 frames (2, 3)
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

struct RecordingFactory;

struct TaggedTranslator {
    id: String,
    prefix: String,
}

#[async_trait]
impl Translator for TaggedTranslator {
    fn provider_id(&self) -> &str {
        &self.id
    }

    async fn translate(&self, req: TranslationRequest) -> Result<TranslationResult, CoreError> {
        Ok(TranslationResult {
            utterance_id: req.utterance_id,
            original_text: req.text.clone(),
            translated_text: format!("{}{}", self.prefix, req.text),
            provider_id: self.id.clone(),
            latency_ms: 0,
        })
    }
}

impl TranslatorFactory for RecordingFactory {
    fn create(&self, config: &TranslationProviderConfig) -> Result<Arc<dyn Translator>, CoreError> {
        match config {
            TranslationProviderConfig::Mock { prefix } => Ok(Arc::new(TaggedTranslator {
                id: "mock".to_owned(),
                prefix: prefix.clone(),
            })),
            TranslationProviderConfig::LocalModel { model_id, .. } => {
                Ok(Arc::new(TaggedTranslator {
                    id: format!("local:{model_id}"),
                    prefix: format!("[{model_id}] "),
                }))
            }
            TranslationProviderConfig::OpenAiCompatible { model, .. } => {
                Ok(Arc::new(TaggedTranslator {
                    id: format!("openai:{model}"),
                    prefix: format!("[{model}] "),
                }))
            }
            _ => DefaultTranslatorFactory.create(config),
        }
    }
}

#[tokio::test]
async fn translator_manager_runtime_hot_swap_and_validation() {
    let factory = RecordingFactory;
    let mgr = TranslatorManager::from_config(
        TranslationProviderConfig::Mock {
            prefix: "[mock] ".to_owned(),
        },
        &factory,
    )
    .expect("create manager");

    assert_eq!(mgr.current_provider_id().await, "mock");
    assert_eq!(mgr.available_models().len(), 4);

    // Reject unknown model_id before mutating active provider
    let err = mgr
        .switch_provider(
            TranslationProviderConfig::LocalModel {
                model_id: "non-existent-model".to_owned(),
                device: ComputeDevice::Auto,
                threads: None,
            },
            &factory,
        )
        .await
        .expect_err("unknown model profile must be rejected");
    assert!(matches!(err, CoreError::UnknownModelProfile(_)));
    assert_eq!(mgr.current_provider_id().await, "mock");

    // Hot-swap to known catalog model profile
    mgr.switch_provider(
        TranslationProviderConfig::LocalModel {
            model_id: "fugumt-en-ja-int8".to_owned(),
            device: ComputeDevice::Cpu,
            threads: Some(4),
        },
        &factory,
    )
    .await
    .expect("switch to fugumt");
    assert_eq!(mgr.current_provider_id().await, "local:fugumt-en-ja-int8");

    // Hot-swap to OpenAI-compatible LLM
    mgr.switch_provider(
        TranslationProviderConfig::OpenAiCompatible {
            base_url: "http://localhost:11434/v1".to_owned(),
            model: "qwen2.5:7b".to_owned(),
            api_key: None,
            timeout_ms: 5_000,
        },
        &factory,
    )
    .await
    .expect("switch to local ollama");
    assert_eq!(mgr.current_provider_id().await, "openai:qwen2.5:7b");

    // DefaultTranslatorFactory returns NotConfigured for empty DeepL key
    let empty_deepl = DefaultTranslatorFactory.create(&TranslationProviderConfig::DeepL {
        api_key: String::new(),
        use_free_endpoint: true,
        timeout_ms: 5_000,
    });
    assert!(matches!(empty_deepl, Err(CoreError::NotConfigured(_))));
}

#[tokio::test]
async fn end_to_end_ipc_server_to_pipeline_and_three_stage_subtitles() {
    let endpoint = test_endpoint("e2e");
    let server = IpcServerAdapter::new(endpoint.clone());
    let (event_tx, event_rx) = mpsc::channel::<AudioInputEvent>(64);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (ready_tx, ready_rx) = oneshot::channel();

    let server_task =
        tokio::spawn(async move { server.run(event_tx, shutdown_rx, Some(ready_tx)).await });
    ready_rx.await.expect("IPC server ready");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&endpoint)
            .expect("socket metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "Unix socket permissions must be 0600");
    }

    let translator_mgr = Arc::new(TranslatorManager::new(
        Arc::new(MockTranslator::new("mock", "[JA] ")),
        TranslationProviderConfig::Mock {
            prefix: "[JA] ".to_owned(),
        },
    ));
    let stt_engine = Arc::new(MockSttEngine::new(0.01, 2));
    let sink = Arc::new(CollectingSubtitleSink::new());
    let resample_fn = Arc::new(AudioPreprocessor::process_chunk);

    let mut pipeline = PipelineService::new(
        PipelineConfig::new(
            LanguageCode::new("en").expect("src lang"),
            LanguageCode::new("ja").expect("tgt lang"),
        ),
        stt_engine,
        translator_mgr,
        sink.clone(),
        resample_fn,
    );

    let pipeline_task = tokio::spawn(async move { pipeline.run(event_rx).await });

    // Connect a synthetic client over real OS IPC
    let mut client = connect_local_socket(&endpoint)
        .await
        .expect("connect to IPC server");

    let send_msg = |msg: Message| encode_frame(&msg).expect("encode frame");

    client
        .write_all(&send_msg(Message::Hello {
            app_name: "mumble".to_owned(),
            plugin_version: "0.1.0".to_owned(),
        }))
        .await
        .expect("send hello");

    client
        .write_all(&send_msg(Message::UserInfo {
            user_id: 7,
            name: "Alice".to_owned(),
        }))
        .await
        .expect("send user info");

    client
        .write_all(&send_msg(Message::TalkingState {
            user_id: 7,
            state: TalkingStateKind::Talking,
        }))
        .await
        .expect("send talking");

    // Send 2 non-silent 48 kHz stereo frames -> MockSttEngine emits Interim on frame 1, Final on frame 2
    let speech_frame = vec![0.25_f32; 960 * 2]; // 20ms @ 48kHz stereo
    for seq in 0..2 {
        client
            .write_all(&send_msg(Message::AudioFrame {
                user_id: 7,
                seq,
                sample_rate: 48_000,
                channels: 2,
                flags: 0x0001,
                pcm_samples: speech_frame.clone(),
            }))
            .await
            .expect("send audio frame");
    }

    // Send a 3rd active frame followed by UserRemoved -> flushes active partial into Final + Translated
    client
        .write_all(&send_msg(Message::AudioFrame {
            user_id: 7,
            seq: 2,
            sample_rate: 48_000,
            channels: 2,
            flags: 0x0001,
            pcm_samples: speech_frame,
        }))
        .await
        .expect("send audio frame 3");

    client
        .write_all(&send_msg(Message::Heartbeat))
        .await
        .expect("send heartbeat");

    client
        .write_all(&send_msg(Message::UserRemoved { user_id: 7 }))
        .await
        .expect("send user removed");

    client
        .write_all(&send_msg(Message::SourceReset))
        .await
        .expect("send source reset");

    drop(client);

    // Wait briefly for server to ingest and close channel on shutdown
    tokio::time::sleep(Duration::from_millis(80)).await;
    let _ = shutdown_tx.send(true);
    server_task.await.expect("server join").expect("server ok");
    pipeline_task
        .await
        .expect("pipeline join")
        .expect("pipeline ok");

    #[cfg(unix)]
    assert!(
        !std::path::Path::new(&endpoint).exists(),
        "Unix socket file must be cleaned up on graceful shutdown"
    );

    let entries = sink.entries().await;
    assert!(
        entries.len() >= 6,
        "expected Interim + Final + Translated for 2 utterances, got {}",
        entries.len()
    );

    let utt1_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.utterance_id == UtteranceId(1))
        .collect();
    assert_eq!(utt1_entries.len(), 3);
    assert_eq!(utt1_entries[0].stage, SubtitleStage::Interim);
    assert_eq!(
        utt1_entries[1].stage,
        SubtitleStage::FinalPendingTranslation
    );
    assert_eq!(utt1_entries[2].stage, SubtitleStage::Translated);
    assert_eq!(utt1_entries[0].speaker_name, "Alice");
    assert!(utt1_entries[2]
        .translated_text
        .as_deref()
        .is_some_and(|t| t.starts_with("[JA] ")));

    let utt2_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.utterance_id == UtteranceId(2))
        .collect();
    assert_eq!(utt2_entries.len(), 3);
    assert_eq!(utt2_entries[0].stage, SubtitleStage::Interim);
    assert_eq!(
        utt2_entries[1].stage,
        SubtitleStage::FinalPendingTranslation
    );
    assert_eq!(utt2_entries[2].stage, SubtitleStage::Translated);

    let cleared = sink.cleared_sources().await;
    assert!(cleared.contains(&"mumble".to_owned()));
}

#[tokio::test]
async fn ipc_server_rejects_non_hello_first_frame_and_malformed_bytes() {
    let endpoint = test_endpoint("bad");
    let server = IpcServerAdapter::new(endpoint.clone());
    let (event_tx, mut event_rx) = mpsc::channel::<AudioInputEvent>(16);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (ready_tx, ready_rx) = oneshot::channel();

    let server_task =
        tokio::spawn(async move { server.run(event_tx, shutdown_rx, Some(ready_tx)).await });
    ready_rx.await.expect("IPC server ready");

    // 1. Send Heartbeat before Hello -> server disconnects without emitting SourceConnected
    let mut bad_client = connect_local_socket(&endpoint)
        .await
        .expect("connect bad client");
    bad_client
        .write_all(&encode_frame(&Message::Heartbeat).expect("encode hb"))
        .await
        .expect("write hb");
    drop(bad_client);

    // 2. Send garbage magic -> server disconnects immediately
    let mut garbage_client = connect_local_socket(&endpoint)
        .await
        .expect("connect garbage client");
    garbage_client
        .write_all(&[0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00])
        .await
        .expect("write garbage");
    drop(garbage_client);

    // 3. Valid client connects afterwards -> server stays healthy and accepts it
    let mut good_client = connect_local_socket(&endpoint)
        .await
        .expect("connect good client");
    good_client
        .write_all(
            &encode_frame(&Message::Hello {
                app_name: "ts3".to_owned(),
                plugin_version: "1.0.0".to_owned(),
            })
            .expect("encode hello"),
        )
        .await
        .expect("write hello");
    // Duplicate Hello mid-session -> must disconnect cleanly
    good_client
        .write_all(
            &encode_frame(&Message::Hello {
                app_name: "ts3".to_owned(),
                plugin_version: "1.0.0".to_owned(),
            })
            .expect("encode dup hello"),
        )
        .await
        .expect("write dup hello");
    drop(good_client);

    let first = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
        .await
        .expect("recv first event")
        .expect("channel open");
    assert!(matches!(
        first,
        AudioInputEvent::SourceConnected { ref source, .. } if source == "ts3"
    ));

    let second = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
        .await
        .expect("recv disconnect event")
        .expect("channel open");
    assert!(matches!(
        second,
        AudioInputEvent::SourceDisconnected { ref source, .. } if source == "ts3"
    ));

    let _ = shutdown_tx.send(true);
    server_task.await.expect("server join").expect("server ok");
}
