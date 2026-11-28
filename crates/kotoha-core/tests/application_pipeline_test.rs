//! Integration tests for `kotoha-core` pipeline orchestration (`src/application/pipeline.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use common::{
    test_endpoint, CapturingTranslator, DelayByUtteranceTranslator,
    DelayedFailingOrTaggedTranslator, FailingSubtitleSink, FlakySttEngine,
    HotwordTrackingSttEngine, RendezvousSttEngine, ScriptedSttEngine, ScriptedTranslator,
    SttScript, TranslatorScript, UnavailableSttEngine,
};
use kotoha_core::application::{
    PipelineConfig, PipelineService, TranslatorManager, TRANSLATION_SKIPPED_REASON,
};
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
    let events = vec![
        AudioInputEvent::SourceConnected {
            conn_id: 1,
            source: "mumble".to_owned(),
            plugin_version: "0.1.0".to_owned(),
        },
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
    ];
    let (_pipeline, result) = run_with_events(pipeline, events).await;
    result.expect("run");

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
    // 100 finals arrive in one burst; a queue that holds them all keeps every one translatable
    // (overflow skipping is covered by `slow_translator_keeps_pending_bounded_...`).
    let pipeline = pipeline_with_config(
        Arc::new(MockSttEngine::new(0.005, 2)),
        Arc::new(ScriptedTranslator::new(TranslatorScript::AlwaysFail)),
        sink.clone(),
        |c| c.max_pending_translations = 100,
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
    let stats = pipeline.stats_handle();

    let (tx, rx) = mpsc::channel(16);
    let task = tokio::spawn(async move { pipeline.run(rx).await });
    tx.send(connected(1)).await.unwrap();
    // 10 utterances of 2 frames each; settle each translation before the next utterance so each
    // request's context snapshot is deterministic.
    for utterance in 0..10_u32 {
        tx.send(sine_frame(1, utterance * 2)).await.unwrap();
        tx.send(sine_frame(1, utterance * 2 + 1)).await.unwrap();
        wait_for_phase_c(&sink, usize::try_from(utterance).unwrap() + 1).await;
    }
    drop(tx);
    task.await.expect("join").expect("run");

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
    assert_eq!(stats.snapshot().translation_failures, 5);
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

    // Stream A: frames 0-1 (one final), frame 2 fails. Stream B: frames 3-9 (three finals plus
    // the trailing interim of frame 9, finalized by the shutdown flush).
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
    assert_eq!(finals, 5);
    assert_eq!(translated, 5);

    let stats = pipeline.stats();
    assert_eq!(stats.stt_errors, 1);
    assert_eq!(stats.stt_stream_resets, 1);
    assert_eq!(stats.dropped_chunks, 1);
    assert_eq!(
        stats.synthesized_finals, 0,
        "the shutdown flush finalized normally"
    );
    assert_eq!(
        pipeline.active_stream_count(),
        0,
        "workers stop when run() returns"
    );
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
    // The second half (after the reset) always emits 5 Interim + 5 Final; the first half may be
    // discarded by the reset before its worker has processed it.
    assert!(emits >= 10, "emit attempts: {emits}");
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
            if chunk.seq.is_multiple_of(2) {
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

// ---------------------------------------------------------------------------------------------
// Execution model (#27): per-speaker workers, bounded deadline-aware translation, idle timeout.
// ---------------------------------------------------------------------------------------------

/// Waits until `count` utterances have reached Phase C (Translated or `TranslationFailed`).
async fn wait_for_phase_c(sink: &CollectingSubtitleSink, count: usize) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let done = sink
                .entries()
                .await
                .iter()
                .filter(|e| {
                    matches!(
                        e.stage,
                        SubtitleStage::Translated | SubtitleStage::TranslationFailed
                    )
                })
                .count();
            if done >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("phase C entries");
}

fn pipeline_with_config(
    stt: Arc<dyn kotoha_core::domain::SttEngine>,
    translator: Arc<dyn kotoha_core::domain::Translator>,
    sink: Arc<dyn kotoha_core::domain::SubtitleSink>,
    configure: impl FnOnce(&mut PipelineConfig),
) -> PipelineService {
    let mut config = PipelineConfig::new(
        LanguageCode::new("en").unwrap(),
        LanguageCode::new("ja").unwrap(),
    );
    configure(&mut config);
    PipelineService::new(
        config,
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

/// 20 ms of 440 Hz sine at 48 kHz mono for `user_id`.
fn speech_frame(conn_id: u64, user_id: u32, seq: u32) -> AudioInputEvent {
    let AudioInputEvent::AudioFrame { chunk, .. } = sine_frame(conn_id, seq) else {
        unreachable!("sine_frame builds an AudioFrame");
    };
    AudioInputEvent::AudioFrame {
        conn_id,
        chunk: AudioChunk {
            speaker_key: SpeakerKey::new("mumble", user_id),
            ..chunk
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_simultaneous_speakers_decode_in_parallel() {
    // Structural check instead of wall-clock latency: every speaker's first `accept_audio` waits
    // until all three speakers are inside `accept_audio` at the same time. Only per-speaker
    // workers can get there; a serial pipeline times out the rendezvous and fails the test.
    const SPEAKERS: u32 = 3;
    const FRAMES: u32 = 5;
    let stt = Arc::new(RendezvousSttEngine::new(
        // Never finalize: every chunk yields exactly one Interim card.
        MockSttEngine::new(0.005, 1_000_000),
        SPEAKERS as usize,
        Duration::from_secs(10),
    ));
    let timed_out = Arc::clone(&stt.rendezvous_timed_out);
    let peak_in_flight = Arc::clone(&stt.peak_in_flight);
    let sink = Arc::new(CollectingSubtitleSink::new());
    let mut pipeline = pipeline_with(
        stt,
        Arc::new(MockTranslator::new("mock", "[ja] ")),
        sink.clone(),
    );
    let (tx, rx) = mpsc::channel(64);
    let task = tokio::spawn(async move { pipeline.run(rx).await });
    tx.send(connected(1)).await.unwrap();
    for seq in 0..FRAMES {
        for user in 1..=SPEAKERS {
            tx.send(speech_frame(1, user, seq)).await.unwrap();
        }
    }
    drop(tx);
    task.await.expect("join").expect("run");

    assert!(
        !timed_out.load(AtomicOrdering::SeqCst),
        "the speakers' STT calls never overlapped (decoding is serial)"
    );
    let peak = peak_in_flight.load(AtomicOrdering::SeqCst);
    assert!(
        peak >= 2,
        "peak concurrent accept_audio calls across speakers: {peak}"
    );
    let entries = sink.entries().await;
    for user in 1..=SPEAKERS {
        let interims = entries
            .iter()
            .filter(|e| e.speaker_key.user_id == user && e.stage == SubtitleStage::Interim)
            .count();
        assert_eq!(
            interims, FRAMES as usize,
            "speaker {user}: one card per chunk"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn slow_translator_keeps_pending_bounded_and_skips_late_finals() {
    let translator = Arc::new(DelayByUtteranceTranslator::new(|_| Duration::from_secs(5)));
    let peak_in_flight = Arc::clone(&translator.peak_in_flight);
    let sink = Arc::new(CollectingSubtitleSink::new());
    let pipeline = pipeline_with_config(
        Arc::new(MockSttEngine::new(0.005, 1)),
        translator,
        sink.clone(),
        |c| {
            c.max_concurrent_translations = 2;
            c.max_pending_translations = 4;
            c.translation_deadline = Duration::from_secs(6);
        },
    );
    let stats = pipeline.stats_handle();
    let started = tokio::time::Instant::now();

    let mut events = vec![connected(1)];
    events.extend((0..20).map(|seq| sine_frame(1, seq)));
    let (_pipeline, result) = run_with_events(pipeline, events).await;
    result.expect("run");
    let elapsed = started.elapsed();

    let entries = sink.entries().await;
    let finals: Vec<u64> = entries
        .iter()
        .filter(|e| e.stage == SubtitleStage::FinalPendingTranslation)
        .map(|e| e.utterance_id.0)
        .collect();
    assert_eq!(finals.len(), 20);
    let mut translated: Vec<u64> = entries
        .iter()
        .filter(|e| e.stage == SubtitleStage::Translated)
        .map(|e| e.utterance_id.0)
        .collect();
    translated.sort_unstable();
    assert_eq!(
        translated,
        vec![1, 2],
        "only the first two fit in the deadline"
    );
    for id in &finals {
        let phase_c: Vec<_> = entries
            .iter()
            .filter(|e| {
                e.utterance_id.0 == *id
                    && matches!(
                        e.stage,
                        SubtitleStage::Translated | SubtitleStage::TranslationFailed
                    )
            })
            .collect();
        assert_eq!(phase_c.len(), 1, "utterance {id} resolves exactly once");
        if phase_c[0].stage == SubtitleStage::TranslationFailed {
            assert_eq!(
                phase_c[0].failure_reason.as_deref(),
                Some(TRANSLATION_SKIPPED_REASON)
            );
        }
    }

    let snapshot = stats.snapshot();
    println!(
        "slow translator: peak_pending={} peak_in_flight={} skipped={} elapsed={elapsed:?}",
        snapshot.peak_pending_translations,
        peak_in_flight.load(AtomicOrdering::SeqCst),
        snapshot.translations_skipped
    );
    assert!(snapshot.peak_pending_translations <= 4 + 2);
    assert!(peak_in_flight.load(AtomicOrdering::SeqCst) <= 2);
    assert_eq!(snapshot.translations_skipped, 18);
    assert_eq!(snapshot.translation_failures, 0);
    assert_eq!(snapshot.pending_translations, 0);
    assert!(
        elapsed <= Duration::from_millis(6_500),
        "every final resolves within the deadline, took {elapsed:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn idle_speaker_mid_utterance_gets_final_card() {
    // `ForgetOnFlush` models an engine that loses the open hypothesis on flush; the pipeline must
    // still finalize the card from the last interim text.
    let stt = Arc::new(ScriptedSttEngine::new(
        MockSttEngine::new(0.005, 4),
        SttScript::ForgetOnFlush,
    ));
    let streams_created = Arc::clone(&stt.streams_created);
    let sink = Arc::new(CollectingSubtitleSink::new());
    let mut pipeline = pipeline_with_config(
        stt,
        Arc::new(MockTranslator::new("mock", "[ja] ")),
        sink.clone(),
        |c| c.speaker_idle_timeout = Duration::from_secs(2),
    );
    let stats = pipeline.stats_handle();
    let (tx, rx) = mpsc::channel(16);
    let task = tokio::spawn(async move { pipeline.run(rx).await });

    tx.send(connected(1)).await.unwrap();
    tx.send(sine_frame(1, 0)).await.unwrap();
    // No TalkingState / UserRemoved: the speaker just goes silent mid-utterance.
    tokio::time::sleep(Duration::from_secs(3)).await;
    wait_for_phase_c(&sink, 1).await;

    let entries = sink.entries().await;
    let stages: Vec<_> = entries
        .iter()
        .map(|e| (e.utterance_id.0, e.stage))
        .collect();
    assert_eq!(
        stages,
        vec![
            (1, SubtitleStage::Interim),
            (1, SubtitleStage::FinalPendingTranslation),
            (1, SubtitleStage::Translated),
        ]
    );
    assert_eq!(entries[1].src_text, entries[0].src_text);
    let snapshot = stats.snapshot();
    assert_eq!(snapshot.idle_flushes, 1);
    assert_eq!(snapshot.synthesized_finals, 1);
    assert_eq!(snapshot.active_speaker_workers, 0, "the idle worker exited");

    // The speaker comes back: a fresh worker and stream take over.
    tx.send(sine_frame(1, 1)).await.unwrap();
    drop(tx);
    task.await.expect("join").expect("run");
    assert_eq!(streams_created.load(AtomicOrdering::SeqCst), 2);
}

#[tokio::test]
async fn panicking_stt_stream_does_not_end_run() {
    // Call 4 is frame 3, which would finalize utterance 2 (opened by frame 2).
    let stt = Arc::new(ScriptedSttEngine::new(
        MockSttEngine::new(0.005, 2),
        SttScript::PanicOnCall(4),
    ));
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
    result.expect("an STT panic must not end run()");

    let entries = sink.entries().await;
    let mut interim_ids: Vec<u64> = entries
        .iter()
        .filter(|e| e.stage == SubtitleStage::Interim)
        .map(|e| e.utterance_id.0)
        .collect();
    interim_ids.dedup();
    let final_ids: Vec<u64> = entries
        .iter()
        .filter(|e| e.stage == SubtitleStage::FinalPendingTranslation)
        .map(|e| e.utterance_id.0)
        .collect();
    assert_eq!(final_ids, vec![1, 2, 3, 4, 5]);
    assert_eq!(
        interim_ids, final_ids,
        "no Interim card is left without a final"
    );
    let translated = entries
        .iter()
        .filter(|e| e.stage == SubtitleStage::Translated)
        .count();
    assert_eq!(translated, 5);

    let stats = pipeline.stats();
    assert_eq!(stats.task_panics, 1);
    assert_eq!(stats.stt_stream_resets, 1);
    assert_eq!(stats.synthesized_finals, 1);
    assert_eq!(streams_created.load(AtomicOrdering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn context_window_follows_finalization_order_and_request_utterance_ids() {
    // Utterances 1, 3 and 5 are slow, so translations complete out of order.
    let translator = Arc::new(DelayByUtteranceTranslator::new(|id| match id {
        1 => Duration::from_millis(300),
        3 => Duration::from_millis(200),
        5 => Duration::from_millis(100),
        _ => Duration::from_millis(10),
    }));
    let captured = Arc::clone(&translator.captured_requests);
    let sink = Arc::new(CollectingSubtitleSink::new());
    let mut pipeline = pipeline_with_config(
        Arc::new(MockSttEngine::new(0.005, 1)),
        translator,
        sink.clone(),
        |c| c.max_concurrent_translations = 4,
    );
    let (tx, rx) = mpsc::channel(16);
    let task = tokio::spawn(async move { pipeline.run(rx).await });
    tx.send(connected(1)).await.unwrap();
    for seq in 0..6 {
        // One speaker per utterance so that every turn has a distinct text and speaker name.
        tx.send(speech_frame(1, seq + 1, seq)).await.unwrap();
    }
    wait_for_phase_c(&sink, 6).await;
    tx.send(speech_frame(1, 7, 6)).await.unwrap();
    drop(tx);
    task.await.expect("join").expect("run");

    let entries = sink.entries().await;
    let completion_order: Vec<u64> = entries
        .iter()
        .filter(|e| e.stage == SubtitleStage::Translated)
        .map(|e| e.utterance_id.0)
        .collect();
    assert_ne!(
        completion_order[..6],
        [1, 2, 3, 4, 5, 6],
        "the scenario must complete translations out of order"
    );
    let mut sorted = completion_order;
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        vec![1, 2, 3, 4, 5, 6, 7],
        "Translated uses the request's utterance id, not the adapter's"
    );

    let final_texts: Vec<String> = entries
        .iter()
        .filter(|e| e.stage == SubtitleStage::FinalPendingTranslation)
        .take(6)
        .map(|e| e.src_text.clone())
        .collect();
    let distinct: std::collections::HashSet<_> = final_texts.iter().collect();
    assert_eq!(distinct.len(), 6, "turns must be distinguishable");
    let requests = captured.lock().unwrap().clone();
    let last = requests
        .iter()
        .find(|r| r.utterance_id == UtteranceId(7))
        .expect("request for utterance 7");
    let history: Vec<String> = last
        .context
        .recent_history
        .iter()
        .map(|turn| turn.original_text.clone())
        .collect();
    assert_eq!(
        history, final_texts,
        "context order equals finalization order"
    );
}

#[tokio::test(start_paused = true)]
async fn failing_create_stream_backs_off() {
    let stt = Arc::new(UnavailableSttEngine::default());
    let attempts = Arc::clone(&stt.attempts);
    let sink = Arc::new(CollectingSubtitleSink::new());
    let mut pipeline = pipeline_with(
        stt,
        Arc::new(MockTranslator::new("mock", "[ja] ")),
        sink.clone(),
    );
    let stats = pipeline.stats_handle();
    let (tx, rx) = mpsc::channel(16);
    let task = tokio::spawn(async move { pipeline.run(rx).await });
    tx.send(connected(1)).await.unwrap();
    // 1 s of audio at 20 ms per frame.
    for seq in 0..50 {
        tx.send(sine_frame(1, seq)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(tx);
    task.await.expect("join").expect("run");

    // Back-off 100 + 200 + 400 ms allows at most ~4 attempts within 1 s instead of 50.
    let attempts = attempts.load(AtomicOrdering::SeqCst);
    assert!(
        (2..=5).contains(&attempts),
        "create_stream attempts: {attempts}"
    );
    let snapshot = stats.snapshot();
    assert_eq!(snapshot.dropped_chunks, 50);
    assert_eq!(snapshot.stt_errors, u64::try_from(attempts).unwrap());
    assert!(sink.entries().await.is_empty());
}
