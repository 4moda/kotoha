//! Integration tests for `kotoha-core` pipeline orchestration (`src/application/pipeline.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use common::{
    test_endpoint, CapturingTranslator, DelayedFailingOrTaggedTranslator, FailingSubtitleSink,
    FlakySttEngine, HotwordTrackingSttEngine, ScriptedTranslator, TranslatorScript,
};
use kotoha_core::application::{PipelineConfig, PipelineService, TranslatorManager};
use kotoha_core::domain::{
    AudioChunk, AudioInputEvent, CoreError, LanguageCode, MergedGlossary, SpeakerKey,
    SttDecodingMethod, SubtitleStage, TranslationProviderConfig, UtteranceId,
};
use kotoha_core::infrastructure::{
    connect_local_socket, AudioPreprocessor, CollectingSubtitleSink, IpcServerAdapter,
    MockSttEngine, MockTranslator,
};
use kotoha_protocol::{encode_frame, Message, TalkingStateKind};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot, watch};

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

    // Wait for both utterances to finish all 3 subtitle stages before sending SourceReset
    // (which advances the source epoch and clears subtitles).
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if sink.entries().await.len() >= 6 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("two utterances should complete 3-stage subtitles before SourceReset");

    client
        .write_all(&send_msg(Message::SourceReset))
        .await
        .expect("send source reset");

    drop(client);

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if sink.cleared_sources().await.contains(&"mumble".to_owned()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("SourceReset should invoke clear_source");

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
async fn pipeline_passes_active_glossary_to_translation_context_and_stt_hotwords() {
    let stt = Arc::new(HotwordTrackingSttEngine::new());
    let captured_requests = Arc::new(StdMutex::new(Vec::new()));
    let translator = Arc::new(CapturingTranslator {
        captured_requests: Arc::clone(&captured_requests),
    });
    let translator_mgr = Arc::new(TranslatorManager::new(
        translator,
        TranslationProviderConfig::Mock {
            prefix: "[ja] ".to_owned(),
        },
    ));
    let sink = Arc::new(CollectingSubtitleSink::new());
    let resample_fn = Arc::new(AudioPreprocessor::process_chunk);

    let mut pipeline = PipelineService::new(
        PipelineConfig::new(
            LanguageCode::new("en").unwrap(),
            LanguageCode::new("ja").unwrap(),
        ),
        stt.clone(),
        translator_mgr,
        sink,
        resample_fn,
    );

    let merged = MergedGlossary::new(BTreeMap::from([
        ("M W D".to_owned(), "マイクロワープドライブ".to_owned()),
        ("Tackle".to_owned(), "タックル".to_owned()),
    ]));
    pipeline
        .set_glossary(merged.clone())
        .await
        .expect("set_glossary");

    // Verify STT engine received raw hotword keys verbatim with ModifiedBeamSearch
    {
        let updates = stt.hotword_updates.lock().expect("lock");
        assert_eq!(updates.len(), 1);
        assert_eq!(
            updates[0].decoding_method,
            SttDecodingMethod::ModifiedBeamSearch
        );
        assert_eq!(
            updates[0].hotwords,
            vec!["M W D".to_owned(), "Tackle".to_owned()]
        );
        drop(updates);
    }

    // Drive an audio frame through the pipeline and verify TranslationRequest.context.glossary
    let mut tasks = tokio::task::JoinSet::new();
    pipeline
        .process_event(
            AudioInputEvent::SourceConnected {
                conn_id: 1,
                source: "mumble".to_owned(),
                plugin_version: "0.1.0".to_owned(),
            },
            &mut tasks,
        )
        .await
        .expect("connect");

    pipeline
        .process_event(
            AudioInputEvent::AudioFrame {
                conn_id: 1,
                chunk: AudioChunk {
                    speaker_key: SpeakerKey::new("mumble", 5),
                    seq: 0,
                    sample_rate: 16_000,
                    channels: 1,
                    is_local_user: false,
                    samples: vec![0.2; 320],
                },
            },
            &mut tasks,
        )
        .await
        .expect("audio frame");

    while let Some(res) = tasks.join_next().await {
        res.expect("task join").expect("task ok");
    }

    let reqs = captured_requests.lock().expect("lock");
    assert_eq!(reqs.len(), 1);
    assert_eq!(&reqs[0].context.glossary, merged.terms());
    drop(reqs);
}

#[tokio::test]
async fn pipeline_suppresses_stale_translations_after_source_reset_and_reports_task_errors() {
    // 1. Stale translation suppression across SourceReset (epoch increment)
    let stt = Arc::new(MockSttEngine::new(0.01, 1));
    let translator_mgr = Arc::new(TranslatorManager::new(
        Arc::new(DelayedFailingOrTaggedTranslator {
            delay: Duration::from_millis(40),
            should_fail: false,
        }),
        TranslationProviderConfig::Mock {
            prefix: "[ja] ".to_owned(),
        },
    ));
    let sink = Arc::new(CollectingSubtitleSink::new());
    let mut pipeline = PipelineService::new(
        PipelineConfig::new(
            LanguageCode::new("en").unwrap(),
            LanguageCode::new("ja").unwrap(),
        ),
        stt,
        translator_mgr,
        sink.clone(),
        Arc::new(AudioPreprocessor::process_chunk),
    );

    let (tx, rx) = mpsc::channel(16);
    tx.send(AudioInputEvent::SourceConnected {
        conn_id: 1,
        source: "mumble".to_owned(),
        plugin_version: "0.1.0".to_owned(),
    })
    .await
    .unwrap();
    tx.send(AudioInputEvent::AudioFrame {
        conn_id: 1,
        chunk: AudioChunk {
            speaker_key: SpeakerKey::new("mumble", 1),
            seq: 0,
            sample_rate: 16_000,
            channels: 1,
            is_local_user: false,
            samples: vec![0.2; 320],
        },
    })
    .await
    .unwrap();
    // Immediately send SourceReset before the 40ms translation completes
    tx.send(AudioInputEvent::SourceReset {
        conn_id: 1,
        source: "mumble".to_owned(),
    })
    .await
    .unwrap();
    drop(tx);

    pipeline.run(rx).await.expect("pipeline finishes cleanly");

    let entries = sink.entries().await;
    // Phase B FinalPendingTranslation happened before SourceReset, but Phase C Translated MUST be
    // suppressed because the source epoch incremented!
    assert!(
        entries.iter().all(|e| e.stage != SubtitleStage::Translated),
        "stale post-reset translation must not be emitted"
    );

    // 2. A failing translation task during run() is reported per utterance, not propagated
    let stt_err = Arc::new(MockSttEngine::new(0.01, 1));
    let failing_mgr = Arc::new(TranslatorManager::new(
        Arc::new(DelayedFailingOrTaggedTranslator {
            delay: Duration::ZERO,
            should_fail: true,
        }),
        TranslationProviderConfig::Mock {
            prefix: String::new(),
        },
    ));
    let sink_err = Arc::new(CollectingSubtitleSink::new());
    let mut failing_pipeline = PipelineService::new(
        PipelineConfig::new(
            LanguageCode::new("en").unwrap(),
            LanguageCode::new("ja").unwrap(),
        ),
        stt_err,
        failing_mgr,
        sink_err.clone(),
        Arc::new(AudioPreprocessor::process_chunk),
    );

    let (tx2, rx2) = mpsc::channel(16);
    tx2.send(AudioInputEvent::SourceConnected {
        conn_id: 1,
        source: "mumble".to_owned(),
        plugin_version: "0.1.0".to_owned(),
    })
    .await
    .unwrap();
    tx2.send(AudioInputEvent::AudioFrame {
        conn_id: 1,
        chunk: AudioChunk {
            speaker_key: SpeakerKey::new("mumble", 1),
            seq: 0,
            sample_rate: 16_000,
            channels: 1,
            is_local_user: false,
            samples: vec![0.2; 320],
        },
    })
    .await
    .unwrap();
    // Keep tx2 alive briefly so try_join_next in the event loop observes the task error
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        drop(tx2);
    });

    failing_pipeline
        .run(rx2)
        .await
        .expect("a translation failure must not end the pipeline");
    let failed: Vec<_> = sink_err
        .entries()
        .await
        .into_iter()
        .filter(|e| e.stage == SubtitleStage::TranslationFailed)
        .collect();
    assert_eq!(
        failed.len(),
        1,
        "the failed utterance is reported to the UI"
    );
    assert_eq!(
        failed[0].failure_reason.as_deref(),
        Some("translation provider error")
    );
    assert_eq!(failing_pipeline.stats().translation_failures, 1);
}

fn speaker() -> SpeakerKey {
    SpeakerKey::new("mumble", 1)
}

fn connected(conn_id: u64) -> AudioInputEvent {
    AudioInputEvent::SourceConnected {
        conn_id,
        source: "mumble".to_owned(),
        plugin_version: "0.1.0".to_owned(),
    }
}

/// 20 ms of a 440 Hz sine at 48 kHz mono, loud enough for `MockSttEngine` to treat as speech.
fn sine_frame(conn_id: u64, seq: u32) -> AudioInputEvent {
    let samples = (0..960_u16)
        .map(|i| 0.3 * (2.0 * std::f32::consts::PI * 440.0 * f32::from(i) / 48_000.0).sin())
        .collect();
    AudioInputEvent::AudioFrame {
        conn_id,
        chunk: AudioChunk {
            speaker_key: speaker(),
            seq,
            sample_rate: 48_000,
            channels: 1,
            is_local_user: false,
            samples,
        },
    }
}

fn pipeline_with(
    stt: Arc<dyn kotoha_core::domain::SttEngine>,
    translator: Arc<dyn kotoha_core::domain::Translator>,
    sink: Arc<dyn kotoha_core::domain::SubtitleSink>,
) -> PipelineService {
    PipelineService::new(
        PipelineConfig::new(
            LanguageCode::new("en").unwrap(),
            LanguageCode::new("ja").unwrap(),
        ),
        stt,
        Arc::new(TranslatorManager::new(
            translator,
            TranslationProviderConfig::Mock {
                prefix: String::new(),
            },
        )),
        sink,
        Arc::new(AudioPreprocessor::process_chunk),
    )
}

/// Feeds `events` through `run()` on a spawned task and returns the pipeline and its result.
async fn run_with_events(
    mut pipeline: PipelineService,
    events: Vec<AudioInputEvent>,
) -> (PipelineService, Result<(), CoreError>) {
    let (tx, rx) = mpsc::channel(16);
    let task = tokio::spawn(async move {
        let result = pipeline.run(rx).await;
        (pipeline, result)
    });
    for (i, event) in events.into_iter().enumerate() {
        assert!(
            tx.send(event).await.is_ok(),
            "pipeline stopped receiving at event {i}"
        );
    }
    drop(tx);
    task.await.expect("pipeline task")
}

#[tokio::test]
async fn pipeline_keeps_streaming_stt_subtitles_when_every_translation_fails() {
    let sink = Arc::new(CollectingSubtitleSink::new());
    let pipeline = pipeline_with(
        Arc::new(MockSttEngine::new(0.005, 2)),
        Arc::new(ScriptedTranslator::new(TranslatorScript::AlwaysFail)),
        sink.clone(),
    );

    let mut events = vec![connected(1)];
    events.extend((0..200).map(|seq| sine_frame(1, seq)));
    let (pipeline, result) = run_with_events(pipeline, events).await;
    result.expect("run() returns Ok once the channel closes");

    let entries = sink.entries().await;
    let count = |stage| entries.iter().filter(|e| e.stage == stage).count();
    assert_eq!(count(SubtitleStage::Interim), 100);
    assert_eq!(count(SubtitleStage::FinalPendingTranslation), 100);
    assert_eq!(count(SubtitleStage::Translated), 0);
    assert_eq!(count(SubtitleStage::TranslationFailed), 100);

    for final_entry in entries
        .iter()
        .filter(|e| e.stage == SubtitleStage::FinalPendingTranslation)
    {
        let failed = entries
            .iter()
            .find(|e| {
                e.stage == SubtitleStage::TranslationFailed
                    && e.utterance_id == final_entry.utterance_id
            })
            .expect("every final utterance gets a TranslationFailed entry");
        assert_eq!(failed.src_text, final_entry.src_text);
        assert_eq!(failed.translated_text, None);
        let reason = failed.failure_reason.as_deref().expect("failure reason");
        assert_eq!(reason, "translation provider error");
        assert!(
            !reason.contains("secret-detail"),
            "raw provider messages must not reach the UI"
        );
    }

    let stats = pipeline.stats();
    assert_eq!(stats.translation_failures, 100);
    assert_eq!(stats.task_panics, 0);
    assert_eq!(stats.dropped_chunks, 0);
}

#[tokio::test]
async fn pipeline_records_only_successful_turns_in_context_window() {
    let translator = Arc::new(ScriptedTranslator::new(TranslatorScript::FailOddUtterances));
    let captured = Arc::clone(&translator.captured_requests);
    let sink = Arc::new(CollectingSubtitleSink::new());
    let mut pipeline = pipeline_with(
        Arc::new(MockSttEngine::new(0.005, 2)),
        translator,
        sink.clone(),
    );

    let mut tasks = tokio::task::JoinSet::new();
    pipeline
        .process_event(connected(1), &mut tasks)
        .await
        .unwrap();
    // 10 utterances of 2 frames each; settle translations between utterances so each request's
    // context snapshot is deterministic.
    for seq in 0..20 {
        pipeline
            .process_event(sine_frame(1, seq), &mut tasks)
            .await
            .expect("frame");
        while let Some(res) = tasks.join_next().await {
            res.expect("join").expect("task");
        }
    }

    let entries = sink.entries().await;
    let translated: Vec<u64> = entries
        .iter()
        .filter(|e| e.stage == SubtitleStage::Translated)
        .map(|e| e.utterance_id.0)
        .collect();
    let failed: Vec<u64> = entries
        .iter()
        .filter(|e| e.stage == SubtitleStage::TranslationFailed)
        .map(|e| e.utterance_id.0)
        .collect();
    assert_eq!(translated, vec![2, 4, 6, 8, 10]);
    assert_eq!(failed, vec![1, 3, 5, 7, 9]);

    let requests = captured.lock().expect("lock").clone();
    assert_eq!(requests.len(), 10);
    for req in &requests {
        let successes_before = usize::try_from((req.utterance_id.0 - 1) / 2).unwrap();
        assert_eq!(
            req.context.recent_history.len(),
            successes_before,
            "utterance {:?} must only see successful turns",
            req.utterance_id
        );
    }
    let last = requests.last().expect("last request");
    assert!(last
        .context
        .recent_history
        .iter()
        .all(|turn| turn.translated_text.starts_with("[ja] ")));
    assert_eq!(pipeline.stats().translation_failures, 5);
}

#[tokio::test]
async fn pipeline_recreates_speaker_stream_after_stt_error() {
    let stt = Arc::new(FlakySttEngine::new(MockSttEngine::new(0.005, 2), 3, 1));
    let streams_created = Arc::clone(&stt.streams_created);
    let sink = Arc::new(CollectingSubtitleSink::new());
    let pipeline = pipeline_with(
        stt,
        Arc::new(MockTranslator::new("mock", "[ja] ")),
        sink.clone(),
    );

    let mut events = vec![connected(1)];
    events.extend((0..10).map(|seq| sine_frame(1, seq)));
    let (pipeline, result) = run_with_events(pipeline, events).await;
    result.expect("an STT error must not end the pipeline");

    // Stream A: frames 0-1 (one final), frame 2 fails. Stream B: frames 3-9 (three finals).
    assert_eq!(streams_created.load(AtomicOrdering::SeqCst), 2);
    let entries = sink.entries().await;
    let finals = entries
        .iter()
        .filter(|e| e.stage == SubtitleStage::FinalPendingTranslation)
        .count();
    let translated = entries
        .iter()
        .filter(|e| e.stage == SubtitleStage::Translated)
        .count();
    assert_eq!(finals, 4);
    assert_eq!(translated, 4);

    let stats = pipeline.stats();
    assert_eq!(stats.stt_errors, 1);
    assert_eq!(stats.stt_stream_resets, 1);
    assert_eq!(stats.dropped_chunks, 1);
    assert_eq!(pipeline.active_stream_count(), 1);
}

#[tokio::test]
async fn pipeline_keeps_running_when_subtitle_sink_fails() {
    let sink = Arc::new(FailingSubtitleSink::default());
    let pipeline = pipeline_with(
        Arc::new(MockSttEngine::new(0.005, 2)),
        Arc::new(MockTranslator::new("mock", "[ja] ")),
        sink.clone(),
    );

    let mut events = vec![connected(1)];
    events.extend((0..10).map(|seq| sine_frame(1, seq)));
    events.push(AudioInputEvent::SourceReset {
        conn_id: 1,
        source: "mumble".to_owned(),
    });
    events.push(connected(2));
    events.extend((10..20).map(|seq| sine_frame(2, seq)));
    let (pipeline, result) = run_with_events(pipeline, events).await;
    result.expect("sink errors must not end the pipeline");

    let emits = sink.emit_attempts.load(AtomicOrdering::SeqCst);
    let clears = sink.clear_attempts.load(AtomicOrdering::SeqCst);
    // 5 Interim + 5 Final per half are always emitted; translations may be suppressed by the reset.
    assert!(emits >= 20, "emit attempts: {emits}");
    assert_eq!(clears, 1);
    assert_eq!(
        pipeline.stats().sink_errors,
        u64::try_from(emits + clears).unwrap()
    );
}

#[tokio::test]
async fn pipeline_keeps_running_when_translator_panics() {
    let sink = Arc::new(CollectingSubtitleSink::new());
    let pipeline = pipeline_with(
        Arc::new(MockSttEngine::new(0.005, 2)),
        Arc::new(ScriptedTranslator::new(
            TranslatorScript::PanicOnOddUtterances,
        )),
        sink.clone(),
    );

    let mut events = vec![connected(1)];
    events.extend((0..20).map(|seq| sine_frame(1, seq)));
    let (pipeline, result) = run_with_events(pipeline, events).await;
    result.expect("a panicking translator must not end the pipeline");

    let entries = sink.entries().await;
    let mut translated: Vec<u64> = entries
        .iter()
        .filter(|e| e.stage == SubtitleStage::Translated)
        .map(|e| e.utterance_id.0)
        .collect();
    translated.sort_unstable();
    let mut failed: Vec<_> = entries
        .iter()
        .filter(|e| e.stage == SubtitleStage::TranslationFailed)
        .map(|e| (e.utterance_id.0, e.failure_reason.clone()))
        .collect();
    failed.sort_unstable();
    assert_eq!(translated, vec![2, 4, 6, 8, 10]);
    assert_eq!(
        failed,
        [1, 3, 5, 7, 9]
            .into_iter()
            .map(|id| (id, Some("translation engine crashed".to_owned())))
            .collect::<Vec<_>>()
    );

    let stats = pipeline.stats();
    assert_eq!(stats.task_panics, 5);
    assert_eq!(stats.translation_failures, 5);
}

#[tokio::test]
async fn pipeline_drops_chunk_when_resample_fails() {
    let sink = Arc::new(CollectingSubtitleSink::new());
    let mut pipeline = PipelineService::new(
        PipelineConfig::new(
            LanguageCode::new("en").unwrap(),
            LanguageCode::new("ja").unwrap(),
        ),
        Arc::new(MockSttEngine::new(0.005, 2)),
        Arc::new(TranslatorManager::new(
            Arc::new(MockTranslator::new("mock", "[ja] ")),
            TranslationProviderConfig::Mock {
                prefix: "[ja] ".to_owned(),
            },
        )),
        sink.clone(),
        Arc::new(|chunk: &AudioChunk| {
            if chunk.seq % 2 == 0 {
                Err(CoreError::InvalidAudio("simulated bad chunk".to_owned()))
            } else {
                AudioPreprocessor::process_chunk(chunk)
            }
        }),
    );
    let stats = pipeline.stats_handle();

    let (tx, rx) = mpsc::channel(16);
    let task = tokio::spawn(async move { pipeline.run(rx).await });
    tx.send(connected(1)).await.unwrap();
    for seq in 0..8 {
        tx.send(sine_frame(1, seq)).await.unwrap();
    }
    drop(tx);
    task.await.expect("join").expect("run");

    assert_eq!(stats.snapshot().dropped_chunks, 4);
    let finals = sink
        .entries()
        .await
        .iter()
        .filter(|e| e.stage == SubtitleStage::FinalPendingTranslation)
        .count();
    assert_eq!(finals, 2, "the 4 surviving chunks still form 2 utterances");
}
