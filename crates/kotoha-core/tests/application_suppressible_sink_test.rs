//! Integration tests for `src/application/suppressible_sink.rs`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use common::DelayByUtteranceTranslator;
use kotoha_core::application::{
    PipelineConfig, PipelineService, SuppressibleSubtitleSink, TranslatorManager,
};
use kotoha_core::domain::{
    AudioChunk, AudioInputEvent, CoreError, LanguageCode, SpeakerKey, SubtitleEntry, SubtitleSink,
    SubtitleStage, TranslationProviderConfig, UtteranceId,
};
use kotoha_core::infrastructure::{AudioPreprocessor, CollectingSubtitleSink, MockSttEngine};
use tokio::sync::mpsc;

fn entry(source: &str, utterance_id: u64) -> SubtitleEntry {
    SubtitleEntry {
        utterance_id: UtteranceId(utterance_id),
        speaker_key: SpeakerKey::new(source, 1),
        speaker_name: "speaker".to_owned(),
        speaker_color: "#000000".to_owned(),
        src_text: "hello".to_owned(),
        translated_text: None,
        stage: SubtitleStage::Interim,
        failure_reason: None,
        timestamp_ms: 0,
    }
}

#[tokio::test]
async fn suppressed_sources_are_dropped_and_others_pass_through() {
    let inner = Arc::new(CollectingSubtitleSink::new());
    let sink = SuppressibleSubtitleSink::new(inner.clone());

    sink.emit(entry("mumble", 1)).await.unwrap();
    sink.suppress_source("mumble").await;
    assert!(sink.is_suppressed("mumble").await);
    assert!(!sink.is_suppressed("ts3").await);
    sink.emit(entry("mumble", 2)).await.unwrap();
    sink.emit(entry("ts3", 3)).await.unwrap();
    sink.clear_source("mumble").await.unwrap();

    let ids: Vec<u64> = inner
        .entries()
        .await
        .iter()
        .map(|e| e.utterance_id.0)
        .collect();
    assert_eq!(ids, vec![1, 3]);
    assert_eq!(inner.cleared_sources().await, vec!["mumble".to_owned()]);
}

/// Inner sink whose `emit` takes 100 ms and records when it has finished.
#[derive(Default)]
struct SlowSink {
    started: AtomicBool,
    finished: AtomicBool,
}

#[async_trait]
impl SubtitleSink for SlowSink {
    async fn emit(&self, _entry: SubtitleEntry) -> Result<(), CoreError> {
        self.started.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.finished.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn suppress_source_waits_for_an_emit_in_progress() {
    let inner = Arc::new(SlowSink::default());
    let sink = Arc::new(SuppressibleSubtitleSink::new(inner.clone()));
    let emitting = Arc::clone(&sink);
    let task = tokio::spawn(async move { emitting.emit(entry("mumble", 1)).await });
    while !inner.started.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }

    sink.suppress_source("mumble").await;
    assert!(
        inner.finished.load(Ordering::SeqCst),
        "an emit that passed the check must finish before suppress_source returns"
    );
    task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn retiring_pipeline_cannot_revive_a_source_cleared_after_the_switch() {
    // Models a language change (#54): the old pipeline is retired (its input closes) while a
    // translation is in flight, then the source is reset and the new pipeline clears its cards.
    // The old pipeline never sees that reset, so only the suppression keeps its late
    // translation off the screen.
    let inner = Arc::new(CollectingSubtitleSink::new());
    let sink = Arc::new(SuppressibleSubtitleSink::new(inner.clone()));
    let translator = Arc::new(DelayByUtteranceTranslator::new(|_| {
        Duration::from_millis(200)
    }));
    let captured = Arc::clone(&translator.captured_requests);
    let mut retiring = PipelineService::new(
        PipelineConfig::new(
            LanguageCode::new("en").unwrap(),
            LanguageCode::new("ja").unwrap(),
        ),
        Arc::new(MockSttEngine::new(0.005, 1)),
        Arc::new(TranslatorManager::new(
            translator,
            TranslationProviderConfig::Mock {
                prefix: String::new(),
            },
        )),
        Arc::clone(&sink) as Arc<dyn SubtitleSink>,
        AudioPreprocessor::resampler_factory(),
    );
    let (tx, rx) = mpsc::channel(16);
    let task = tokio::spawn(async move { retiring.run(rx).await });
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
    while captured.lock().unwrap().is_empty() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    // Retire it, then clear the source as the bridge does for a reset seen after the switch.
    drop(tx);
    sink.suppress_source("mumble").await;
    inner.clear_source("mumble").await.unwrap();
    task.await.expect("join").expect("run");

    let entries = inner.entries().await;
    assert!(
        entries
            .iter()
            .any(|e| e.stage == SubtitleStage::FinalPendingTranslation),
        "the final card was shown before the switch"
    );
    assert!(
        entries.iter().all(|e| !matches!(
            e.stage,
            SubtitleStage::Translated | SubtitleStage::TranslationFailed
        )),
        "the retiring pipeline must not update a cleared source"
    );
}
