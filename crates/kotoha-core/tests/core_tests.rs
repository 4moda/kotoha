//! Comprehensive unit and integration tests for `kotoha-core`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::too_many_lines
)]

use std::collections::{BTreeMap, HashSet};
use std::f32::consts::PI;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use kotoha_core::application::{
    PipelineConfig, PipelineService, SeqStatus, SessionAction, SourceSessionManager,
    TranslatorManager,
};
use kotoha_core::domain::{
    default_model_catalog, protect_terms, restore_terms, AudioChunk, AudioInputEvent,
    ComputeDevice, ContextWindow, CoreError, GlossaryEntry, GlossaryPlaceholderGuard,
    GlossaryProfile, GlossarySettings, HistoryEntry, LanguageCode, MergedGlossary, PreFilter,
    SpeakerKey, SttDecodingMethod, SttEngine, SttHotwordsConfig, SttStream, SubtitleStage,
    TalkingState, TranslationProviderConfig, TranslationRequest, TranslationResult, Translator,
    TranslatorFactory, UtteranceId, DEFAULT_HOTWORDS_SCORE,
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
    assert_eq!(
        snap.glossary.get("GG").map(String::as_str),
        Some("ナイスゲーム")
    );
    assert_eq!(snap.recent_history.len(), 3);
    assert_eq!(snap.recent_history[0].original_text, "src-3");
    assert_eq!(snap.recent_history[2].original_text, "src-5");
}

#[test]
fn domain_glossary_profile_merge_language_filter_and_hotword_keys() {
    let en = LanguageCode::new("en").unwrap();
    let ja = LanguageCode::new("ja").unwrap();
    let ko = LanguageCode::new("ko").unwrap();

    let base_profile = GlossaryProfile::new(
        "general",
        en.clone(),
        ja.clone(),
        BTreeMap::from([
            ("GG".to_owned(), "ナイスゲーム".to_owned()),
            ("M W D".to_owned(), "マイクロワープドライブ".to_owned()),
            ("Ice Beam".to_owned(), "冷凍ビーム".to_owned()),
        ]),
    );
    let game_override = GlossaryProfile::new(
        "eve",
        en.clone(),
        ja.clone(),
        BTreeMap::from([
            ("GG".to_owned(), "お疲れ様".to_owned()),
            ("Tackle".to_owned(), "タックル".to_owned()),
        ]),
    );
    let wrong_lang = GlossaryProfile::new(
        "eve-ko",
        en.clone(),
        ko,
        BTreeMap::from([("Tackle".to_owned(), "다른".to_owned())]),
    );

    let profiles = vec![base_profile, game_override, wrong_lang];

    // Direct merge by pair: later overrides earlier, mismatched language ignored
    let raw_merged = GlossaryProfile::merge(&profiles, &en, &ja);
    assert_eq!(raw_merged.get("GG").map(String::as_str), Some("お疲れ様"));
    assert_eq!(
        raw_merged.get("Tackle").map(String::as_str),
        Some("タックル")
    );
    assert_eq!(raw_merged.len(), 4);

    // Priority merge using GlossarySettings.enabled_profiles
    let settings = GlossarySettings {
        enabled_profiles: vec!["eve".to_owned(), "general".to_owned(), "eve-ko".to_owned()],
        glossary_dir: None,
    };
    let merged =
        MergedGlossary::from_enabled_profiles(&profiles, &settings.enabled_profiles, &en, &ja);
    // "general" came after "eve" in enabled_profiles so "GG" reverts to general's translation
    assert_eq!(
        merged.terms().get("GG").map(String::as_str),
        Some("ナイスゲーム")
    );
    assert_eq!(
        merged.terms().get("Tackle").map(String::as_str),
        Some("タックル")
    );

    // Raw keys including space-separated subword hints ("M W D") are preserved as-is for STT
    let hotwords = merged.hotword_keys();
    assert_eq!(
        hotwords,
        vec![
            "GG".to_owned(),
            "Ice Beam".to_owned(),
            "M W D".to_owned(),
            "Tackle".to_owned()
        ]
    );

    let hw_cfg = merged.to_stt_hotwords_config(DEFAULT_HOTWORDS_SCORE);
    assert_eq!(
        hw_cfg.decoding_method,
        SttDecodingMethod::ModifiedBeamSearch
    );
    assert!((hw_cfg.hotwords_score - 1.5).abs() < f32::EPSILON);
    assert_eq!(hw_cfg.hotwords, hotwords);

    let empty_cfg = MergedGlossary::default().to_stt_hotwords_config(DEFAULT_HOTWORDS_SCORE);
    assert_eq!(empty_cfg.decoding_method, SttDecodingMethod::GreedySearch);
    assert_eq!(empty_cfg.hotwords, Vec::<String>::new());
}

#[test]
fn domain_glossary_placeholder_guard_longest_match_and_restore() {
    let terms = BTreeMap::from([
        ("Ice".to_owned(), "氷".to_owned()),
        ("Ice Beam".to_owned(), "冷凍ビーム".to_owned()),
        ("Beam".to_owned(), "光線".to_owned()),
        ("MWD".to_owned(), "マイクロワープドライブ".to_owned()),
    ]);

    let input = "Use Ice Beam then MWD and Ice Beam again, not Ice!";
    let (protected, guard) = protect_terms(input, &terms);
    assert_eq!(
        protected,
        "Use __TERM_0__ then __TERM_1__ and __TERM_0__ again, not __TERM_2__!"
    );
    assert_eq!(guard.len(), 3);

    // Simulate NMT translating surrounding words while keeping placeholders intact
    let translated_with_placeholders =
        "__TERM_0__ を使ってから __TERM_1__ と __TERM_0__ を再び、__TERM_2__ ではなく！";
    let restored = restore_terms(translated_with_placeholders, &guard);
    assert_eq!(
        restored,
        "冷凍ビーム を使ってから マイクロワープドライブ と 冷凍ビーム を再び、氷 ではなく！"
    );

    // Also test direct guard API with empty matches
    let (untouched, empty_guard) =
        GlossaryPlaceholderGuard::protect("nothing matches here", &terms);
    assert_eq!(untouched, "nothing matches here");
    assert!(empty_guard.is_empty());
    assert_eq!(empty_guard.restore("そのまま"), "そのまま");
}

#[test]
fn domain_glossary_placeholder_guard_respects_ascii_word_boundaries() {
    let terms = BTreeMap::from([
        ("Logi".to_owned(), "ロジ".to_owned()),
        ("M W D".to_owned(), "MWD".to_owned()),
        ("シールド".to_owned(), "Shield".to_owned()),
    ]);

    // "logical", "my_Logi", "Logi2" must NOT be corrupted; standalone "Logi", "M W D", and CJK
    // substring "シールド" MUST match.
    let input = "This logical my_Logi Logi2 needs Logi and M W D with シールド強化";
    let (protected, guard) = protect_terms(input, &terms);
    assert_eq!(
        protected,
        "This logical my_Logi Logi2 needs __TERM_0__ and __TERM_1__ with __TERM_2__強化"
    );
    let restored = restore_terms(&protected, &guard);
    assert_eq!(
        restored,
        "This logical my_Logi Logi2 needs ロジ and MWD with Shield強化"
    );
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

struct HotwordTrackingSttEngine {
    inner: MockSttEngine,
    hotword_updates: Arc<StdMutex<Vec<SttHotwordsConfig>>>,
}

impl HotwordTrackingSttEngine {
    fn new() -> Self {
        Self {
            inner: MockSttEngine::new(0.01, 1),
            hotword_updates: Arc::new(StdMutex::new(Vec::new())),
        }
    }
}

impl SttEngine for HotwordTrackingSttEngine {
    fn name(&self) -> &'static str {
        "hotword-tracking-mock"
    }

    fn hotwords_supported(&self) -> bool {
        true
    }

    fn update_hotwords(&self, config: &SttHotwordsConfig) -> Result<(), CoreError> {
        self.hotword_updates
            .lock()
            .expect("lock")
            .push(config.clone());
        Ok(())
    }

    fn create_stream(
        &self,
        speaker_key: SpeakerKey,
        src_lang: &LanguageCode,
    ) -> Result<Box<dyn SttStream>, CoreError> {
        self.inner.create_stream(speaker_key, src_lang)
    }
}

struct CapturingTranslator {
    captured_requests: Arc<StdMutex<Vec<TranslationRequest>>>,
}

#[async_trait]
impl Translator for CapturingTranslator {
    fn provider_id(&self) -> &'static str {
        "capturing-mock"
    }

    async fn translate(&self, req: TranslationRequest) -> Result<TranslationResult, CoreError> {
        self.captured_requests
            .lock()
            .expect("lock")
            .push(req.clone());
        Ok(TranslationResult {
            utterance_id: req.utterance_id,
            original_text: req.text.clone(),
            translated_text: format!("[ja] {}", req.text),
            provider_id: "capturing-mock".to_owned(),
            latency_ms: 0,
        })
    }
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
}

struct DelayedFailingOrTaggedTranslator {
    delay: Duration,
    should_fail: bool,
}

#[async_trait]
impl Translator for DelayedFailingOrTaggedTranslator {
    fn provider_id(&self) -> &'static str {
        "delayed"
    }

    async fn translate(&self, req: TranslationRequest) -> Result<TranslationResult, CoreError> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        if self.should_fail {
            return Err(CoreError::Translation("simulated failure".to_owned()));
        }
        Ok(TranslationResult {
            utterance_id: req.utterance_id,
            original_text: req.text.clone(),
            translated_text: format!("[ja] {}", req.text),
            provider_id: "delayed".to_owned(),
            latency_ms: 0,
        })
    }
}

#[tokio::test]
async fn pipeline_suppresses_stale_translations_after_source_reset_and_propagates_task_errors() {
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

    // 2. Failing spawned translation task during run() must propagate Ok(Err(err))
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
        sink_err,
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

    let err = failing_pipeline
        .run(rx2)
        .await
        .expect_err("translation task error must propagate");
    assert!(matches!(err, CoreError::Translation(_)));
}

#[tokio::test]
async fn ipc_server_handles_shutdown_signal_and_stale_symlink() {
    let endpoint = test_endpoint("shutdown");

    #[cfg(unix)]
    {
        // Create a dangling symlink at the socket path to verify symlink_metadata removes it cleanly
        let _ = std::fs::remove_file(&endpoint);
        std::os::unix::fs::symlink("/tmp/kotoha_nonexistent_target", &endpoint)
            .expect("create dangling symlink");
    }

    let server = IpcServerAdapter::new(endpoint.clone());
    let (event_tx, _event_rx) = mpsc::channel::<AudioInputEvent>(8);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (ready_tx, ready_rx) = oneshot::channel();

    let server_task =
        tokio::spawn(async move { server.run(event_tx, shutdown_rx, Some(ready_tx)).await });
    ready_rx
        .await
        .expect("server bound cleanly over dangling symlink");

    // Connect client and leave it idle, then trigger shutdown (verifying wait_for terminates
    // handle_connection promptly without hanging)
    let _idle_client = connect_local_socket(&endpoint)
        .await
        .expect("connect idle client");
    shutdown_tx.send(true).expect("send shutdown");

    tokio::time::timeout(Duration::from_secs(2), server_task)
        .await
        .expect("server shutdown must not deadlock")
        .expect("join ok")
        .expect("server ok");
}
