//! Unit and pipeline integration tests for `kotoha-stt-sherpa` engine (`src/engine.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{
    generate_speech_burst_wav_fixture, sample_zipformer_config, unique_temp_dir,
    MockRecognizerBackend,
};
use kotoha_core::application::{PipelineConfig, PipelineService, TranslatorManager};
use kotoha_core::domain::{
    AudioChunk, AudioInputEvent, LanguageCode, MergedGlossary, SpeakerKey, SttDecodingMethod,
    SttEngine, SttHotwordsConfig, SubtitleStage, TranslationProviderConfig, UtteranceId,
};
use kotoha_core::infrastructure::{AudioPreprocessor, CollectingSubtitleSink, MockTranslator};
use kotoha_stt_sherpa::SherpaSttEngine;

#[test]
fn streaming_wav_fixture_interim_endpoint_utterance_id_and_flush() {
    let (sample_rate, samples) = generate_speech_burst_wav_fixture();
    assert_eq!(sample_rate, 16_000);
    assert_eq!(samples.len(), 8_320);

    let backend = Arc::new(MockRecognizerBackend::scripted(0.05));
    let engine = SherpaSttEngine::with_backend(sample_zipformer_config(), backend.clone()).unwrap();
    assert_eq!(engine.name(), "sherpa-onnx");
    assert!(engine.hotwords_supported());

    let spk = SpeakerKey::new("mumble", 42);
    let mut stream = engine.create_typed_stream(spk.clone()).unwrap();
    assert_eq!(engine.active_stream_count(), 1);
    assert_eq!(backend.live_stream_count(), 1);

    // Split WAV into 100ms (1600-sample) chunks:
    // chunks 0..2: burst 1 (440Hz, 3200 samples) -> interim events with UtteranceId(1)
    // chunk 2: 1600 silence samples -> endpoint finalized for UtteranceId(1)
    // chunk 3..5: 320 silence + 3200 burst 2 (660Hz) -> UtteranceId(2) interim
    let chunks: Vec<&[f32]> = samples.chunks(1_600).collect();
    assert_eq!(chunks.len(), 6);

    let e0 = stream.push_audio(chunks[0], 100).unwrap();
    assert_eq!(e0.len(), 1);
    assert_eq!(e0[0].utterance_id, UtteranceId(1));
    assert!(!e0[0].is_final);
    assert_eq!(e0[0].speaker_key, spk);

    let e1 = stream.push_audio(chunks[1], 200).unwrap();
    assert_eq!(e1.len(), 1);
    assert_eq!(e1[0].utterance_id, UtteranceId(1));
    assert!(!e1[0].is_final);

    // Silence chunk triggers endpoint on utterance 1
    let e2 = stream.push_audio(chunks[2], 300).unwrap();
    assert_eq!(e2.len(), 1);
    assert_eq!(e2[0].utterance_id, UtteranceId(1));
    assert!(e2[0].is_final);
    assert_eq!(e2[0].text, e1[0].text);

    // Burst 2 starts new utterance with incremented UtteranceId(2)
    let e3 = stream.push_audio(chunks[3], 400).unwrap();
    assert_eq!(e3.len(), 1);
    assert_eq!(e3[0].utterance_id, UtteranceId(2));
    assert!(!e3[0].is_final);

    let e4 = stream.push_audio(chunks[4], 500).unwrap();
    assert_eq!(e4.len(), 1);
    assert_eq!(e4[0].utterance_id, UtteranceId(2));
    assert!(!e4[0].is_final);

    // Disconnect / talking stop -> flush() emits final utterance 2 and drops underlying stream
    let flushed = stream.flush_utterance(600).unwrap().expect("final event");
    assert_eq!(flushed.utterance_id, UtteranceId(2));
    assert!(flushed.is_final);
    assert_eq!(flushed.text, e4[0].text);
    assert_eq!(backend.live_stream_count(), 0);

    // Subsequent flush when buffer is empty emits None
    assert!(stream.flush_utterance(700).unwrap().is_none());

    // Continuing push_audio -> flush on the SAME SherpaSttStream allocates a fresh stream cleanly
    let e5 = stream.push_audio(chunks[0], 800).unwrap();
    assert_eq!(e5.len(), 1);
    assert_eq!(e5[0].utterance_id, UtteranceId(3));
    assert!(!e5[0].is_final);
    assert_eq!(backend.live_stream_count(), 1);

    let flushed2 = stream.flush_utterance(900).unwrap().expect("final event 3");
    assert_eq!(flushed2.utterance_id, UtteranceId(3));
    assert!(flushed2.is_final);
    assert_eq!(flushed2.text, e5[0].text);
    assert_eq!(backend.live_stream_count(), 0);

    drop(stream);
    assert_eq!(engine.active_stream_count(), 0);
}

#[test]
fn endpoint_and_flush_finalize_with_last_interim_text_when_recognizer_returns_empty() {
    let backend = Arc::new(MockRecognizerBackend::scripted(0.05));
    let engine = SherpaSttEngine::with_backend(sample_zipformer_config(), backend.clone()).unwrap();
    let mut stream = engine
        .create_typed_stream(SpeakerKey::new("mumble", 99))
        .unwrap();

    let active_chunk = vec![0.3_f32; 1_600];
    let silence_chunk = vec![0.0_f32; 1_600];

    // 1. Interim -> recognizer returns empty at endpoint -> must finalize using last_interim_text
    let interim1 = stream.push_audio(&active_chunk, 100).unwrap();
    assert_eq!(interim1.len(), 1);
    assert!(!interim1[0].is_final);

    backend.set_empty_result_on_endpoint_or_flush(true);
    let final1 = stream.push_audio(&silence_chunk, 200).unwrap();
    assert_eq!(final1.len(), 1);
    assert!(final1[0].is_final);
    assert_eq!(final1[0].utterance_id, interim1[0].utterance_id);
    assert_eq!(final1[0].text, interim1[0].text);

    // 2. Interim -> recognizer returns empty on flush() -> must finalize using last_interim_text
    backend.set_empty_result_on_endpoint_or_flush(false);
    let interim2 = stream.push_audio(&active_chunk, 300).unwrap();
    assert_eq!(interim2.len(), 1);
    assert!(!interim2[0].is_final);
    assert_ne!(interim2[0].utterance_id, interim1[0].utterance_id);

    backend.set_empty_result_on_endpoint_or_flush(true);
    let final2 = stream
        .flush_utterance(400)
        .unwrap()
        .expect("flush must emit final using cached interim");
    assert!(final2.is_final);
    assert_eq!(final2.utterance_id, interim2[0].utterance_id);
    assert_eq!(final2.text, interim2[0].text);
}

#[test]
fn dynamic_hotwords_update_generation_reversibility_and_transactional_rollback() {
    let temp_dir = unique_temp_dir("kotoha_sherpa_hw_test");
    let _ = std::fs::remove_dir_all(&temp_dir);

    let backend = Arc::new(MockRecognizerBackend::scripted(0.05));
    let engine = SherpaSttEngine::with_backend_and_hotwords_dir(
        sample_zipformer_config(),
        backend.clone(),
        &temp_dir,
    )
    .unwrap();

    // Empty hotwords must NOT eagerly materialize an empty file on disk
    let initial_snap = engine.hotwords_snapshot().unwrap();
    let hw_file = initial_snap.file_path.expect("managed path configured");
    assert!(
        !hw_file.exists(),
        "empty hotwords should not materialize an empty file on disk"
    );

    assert_eq!(engine.hotwords_generation(), 0);
    assert_eq!(
        engine.effective_decoding_method().unwrap(),
        SttDecodingMethod::GreedySearch
    );

    let mut stream = engine
        .create_typed_stream(SpeakerKey::new("mumble", 1))
        .unwrap();
    assert_eq!(stream.hotwords_generation(), 0);

    // Start utterance 1 before hotwords update (gen 0)
    let active_chunk = vec![0.3_f32; 1_600];
    let u1_interim = stream.push_audio(&active_chunk, 10).unwrap();
    assert_eq!(u1_interim.len(), 1);
    assert!(u1_interim[0].text.contains("[none]"));

    // Update hotwords mid-utterance -> generation increments, managed file updates, backend notified
    let hw_cfg = SttHotwordsConfig {
        decoding_method: SttDecodingMethod::GreedySearch, // promoted to ModifiedBeamSearch
        hotwords_score: 2.0,
        hotwords: vec!["M W D".to_owned(), "Logi".to_owned()],
    };
    engine.update_hotwords(&hw_cfg).unwrap();
    assert_eq!(engine.hotwords_generation(), 1);
    assert_eq!(
        engine.effective_decoding_method().unwrap(),
        SttDecodingMethod::ModifiedBeamSearch
    );

    let snap = engine.hotwords_snapshot().unwrap();
    assert_eq!(snap.formatted_text, "Logi\nM W D\n");
    assert_eq!(std::fs::read_to_string(&hw_file).unwrap(), "Logi\nM W D\n");

    let reloads = backend.hotword_reload_calls();
    assert_eq!(reloads.len(), 1);
    assert_eq!(reloads[0].0, SttDecodingMethod::ModifiedBeamSearch);
    assert!((reloads[0].1 - 2.0).abs() < f32::EPSILON);
    assert_eq!(reloads[0].2, 1);

    // Mid-utterance stream stays on gen 0 until utterance 1 finishes...
    assert_eq!(stream.hotwords_generation(), 0);
    let u1_final = stream.flush_utterance(20).unwrap().unwrap();
    assert!(u1_final.is_final);
    assert!(u1_final.text.contains("[none]"));
    assert_eq!(stream.hotwords_generation(), 1);

    // Next utterance picks up gen 1 cleanly
    let u2 = stream.push_audio(&active_chunk, 30).unwrap();
    assert_eq!(u2.len(), 1);
    assert!(u2[0].text.contains("Logi,M W D"));
    assert!(stream.flush_utterance(40).unwrap().is_some());

    // Transactional rollback when backend::on_hotwords_updated fails
    backend.set_fail_hotword_reload(true);
    let failing_hw = SttHotwordsConfig {
        decoding_method: SttDecodingMethod::ModifiedBeamSearch,
        hotwords_score: 3.5,
        hotwords: vec!["Broken".to_owned()],
    };
    assert!(engine.update_hotwords(&failing_hw).is_err());
    assert_eq!(engine.hotwords_generation(), 1);
    assert_eq!(
        engine.hotwords_snapshot().unwrap().formatted_text,
        "Logi\nM W D\n"
    );
    assert!((engine.config().unwrap().hotwords_score - 2.0).abs() < f32::EPSILON);
    assert_eq!(
        std::fs::read_to_string(&hw_file).unwrap(),
        "Logi\nM W D\n",
        "disk file must roll back to previous hotwords on backend error"
    );
    backend.set_fail_hotword_reload(false);

    // Clearing hotwords must REVERT decoding_method back to base GreedySearch and remove file
    let clear_hw = SttHotwordsConfig {
        decoding_method: SttDecodingMethod::GreedySearch,
        hotwords_score: 1.5,
        hotwords: Vec::new(),
    };
    engine.update_hotwords(&clear_hw).unwrap();
    assert_eq!(engine.hotwords_generation(), 2);
    assert_eq!(
        engine.effective_decoding_method().unwrap(),
        SttDecodingMethod::GreedySearch
    );
    assert!(
        !hw_file.exists(),
        "cleared hotwords should remove managed file"
    );

    // Invalid hotwords_score rejected without mutating generation or decoding_method
    let bad_hw = SttHotwordsConfig {
        decoding_method: SttDecodingMethod::ModifiedBeamSearch,
        hotwords_score: 0.0,
        hotwords: vec!["Test".to_owned()],
    };
    assert!(engine.update_hotwords(&bad_hw).is_err());
    assert_eq!(engine.hotwords_generation(), 2);

    let recorded = backend.recorded_streams();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].generation, 0);
    assert_eq!(recorded[1].generation, 1);
    assert!((recorded[1].hotwords_score - 2.0).abs() < f32::EPSILON);
    assert_eq!(recorded[1].formatted_text, "Logi\nM W D\n");
    assert_eq!(
        recorded[1].decoding_method,
        SttDecodingMethod::ModifiedBeamSearch
    );

    drop(stream);
    drop(engine);
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn idle_stream_reaping_drops_backend_immediately_and_cleans_on_drop() {
    let mut cfg = sample_zipformer_config();
    cfg.idle_timeout = Duration::from_secs(30);

    let backend = Arc::new(MockRecognizerBackend::new(0.05, 2));
    let engine = SherpaSttEngine::with_backend(cfg, backend.clone()).unwrap();

    let spk_idle = SpeakerKey::new("mumble", 10);
    let spk_active = SpeakerKey::new("mumble", 11);
    let mut stream_idle = engine.create_typed_stream(spk_idle.clone()).unwrap();
    let stream_active = engine.create_typed_stream(spk_active).unwrap();
    assert_eq!(engine.active_stream_count(), 2);
    assert_eq!(backend.live_stream_count(), 2);

    let now = Instant::now();
    engine.set_stream_last_activity_for_test(
        &spk_idle,
        now.checked_sub(Duration::from_secs(31))
            .expect("instant sub"),
    );

    let reaped = engine.reap_idle_streams_at(now).unwrap();
    assert_eq!(reaped, 1);
    assert!(stream_idle.is_reaped());
    assert!(!stream_active.is_reaped());
    assert_eq!(engine.active_stream_count(), 1);
    // Underlying backend stream MUST be dropped immediately on reap, before dropping `stream_idle`!
    assert_eq!(backend.live_stream_count(), 1);

    // If reaped stream receives new audio later, it recovers cleanly with a fresh stream
    let recovered = stream_idle.push_audio(&vec![0.3_f32; 1_600], 500).unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(engine.active_stream_count(), 2);
    assert_eq!(backend.live_stream_count(), 2);

    drop(stream_idle);
    drop(stream_active);
    assert_eq!(engine.active_stream_count(), 0);
    assert_eq!(backend.live_stream_count(), 0);
}

#[test]
fn concurrent_multi_speaker_streams_emit_unique_utterance_ids_without_cross_contamination() {
    let backend = Arc::new(MockRecognizerBackend::new(0.05, 2));
    let engine =
        Arc::new(SherpaSttEngine::with_backend(sample_zipformer_config(), backend).unwrap());

    let mut handles = Vec::new();
    for speaker_idx in 1..=4u32 {
        let eng = Arc::clone(&engine);
        handles.push(std::thread::spawn(move || {
            let spk = SpeakerKey::new("mumble", speaker_idx);
            let mut stream = eng.create_typed_stream(spk.clone()).unwrap();
            let active = vec![0.25_f32; 1_600];
            let mut ids = Vec::new();
            for utt in 0..5u64 {
                let ts = utt * 200;
                let ev1 = stream.push_audio(&active, ts).unwrap();
                assert_eq!(ev1.len(), 1);
                assert!(!ev1[0].is_final);
                assert_eq!(ev1[0].speaker_key, spk);

                let ev2 = stream.push_audio(&active, ts + 100).unwrap();
                assert_eq!(ev2.len(), 1);
                assert!(ev2[0].is_final);
                assert_eq!(ev2[0].speaker_key, spk);
                assert_eq!(ev1[0].utterance_id, ev2[0].utterance_id);
                ids.push(ev2[0].utterance_id);
            }
            ids
        }));
    }

    let mut all_ids = HashSet::new();
    for handle in handles {
        let ids = handle.join().expect("speaker thread join");
        assert_eq!(ids.len(), 5);
        for id in ids {
            assert!(
                all_ids.insert(id),
                "duplicate UtteranceId across speakers: {id:?}"
            );
        }
    }
    assert_eq!(all_ids.len(), 20);
    assert_eq!(engine.active_stream_count(), 0);
}

#[tokio::test]
async fn full_pipeline_integration_three_stage_subtitles_and_glossary_hotwords() {
    let backend = Arc::new(MockRecognizerBackend::new(0.05, 2));
    let stt_engine = Arc::new(
        SherpaSttEngine::with_backend(sample_zipformer_config(), backend.clone()).unwrap(),
    );

    let translator_mgr = Arc::new(TranslatorManager::new(
        Arc::new(MockTranslator::new("mock", "[JA] ")),
        TranslationProviderConfig::Mock {
            prefix: "[JA] ".to_owned(),
        },
    ));
    let sink = Arc::new(CollectingSubtitleSink::new());
    let mut pipeline = PipelineService::new(
        PipelineConfig::new(
            LanguageCode::new("en").unwrap(),
            LanguageCode::new("ja").unwrap(),
        ),
        stt_engine.clone(),
        translator_mgr,
        sink.clone(),
        Arc::new(AudioPreprocessor::process_chunk),
    );

    let glossary = MergedGlossary::new(BTreeMap::from([
        ("M W D".to_owned(), "マイクロワープドライブ".to_owned()),
        ("Anchor".to_owned(), "アンカー".to_owned()),
    ]));
    pipeline.set_glossary(glossary.clone()).await.unwrap();

    assert_eq!(stt_engine.hotwords_generation(), 1);
    let hw_snap = stt_engine.hotwords_snapshot().unwrap();
    assert_eq!(hw_snap.formatted_text, "Anchor\nM W D\n");
    assert_eq!(
        hw_snap.decoding_method,
        SttDecodingMethod::ModifiedBeamSearch
    );

    let (sample_rate, wav_samples) = generate_speech_burst_wav_fixture();
    assert_eq!(sample_rate, 16_000);

    let spk = SpeakerKey::new("mumble", 7);
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
        .unwrap();
    pipeline
        .process_event(
            AudioInputEvent::UserInfoUpdated {
                conn_id: 1,
                speaker_key: spk.clone(),
                name: "FleetCommander".to_owned(),
            },
            &mut tasks,
        )
        .await
        .unwrap();

    // Feed first two 100ms speech chunks from WAV fixture -> Interim (1) then Final + Translated (2)
    for (seq, chunk) in wav_samples[..3_200].chunks(1_600).enumerate() {
        pipeline
            .process_event(
                AudioInputEvent::AudioFrame {
                    conn_id: 1,
                    chunk: AudioChunk {
                        speaker_key: spk.clone(),
                        seq: u32::try_from(seq).unwrap(),
                        sample_rate: 16_000,
                        channels: 1,
                        is_local_user: false,
                        samples: chunk.to_vec(),
                    },
                },
                &mut tasks,
            )
            .await
            .unwrap();
    }

    while let Some(res) = tasks.join_next().await {
        res.unwrap().unwrap();
    }

    let entries = sink.entries().await;
    assert_eq!(entries.len(), 3, "expected 3-stage subtitle lifecycle");
    assert_eq!(entries[0].stage, SubtitleStage::Interim);
    assert_eq!(entries[1].stage, SubtitleStage::FinalPendingTranslation);
    assert_eq!(entries[2].stage, SubtitleStage::Translated);
    assert_eq!(entries[0].utterance_id, entries[1].utterance_id);
    assert_eq!(entries[1].utterance_id, entries[2].utterance_id);
    assert_eq!(entries[0].speaker_name, "FleetCommander");
    assert!(entries[1].src_text.contains("Anchor,M W D"));
    assert_eq!(
        entries[2].translated_text.as_deref(),
        Some(format!("[JA] {}", entries[1].src_text).as_str())
    );
}
