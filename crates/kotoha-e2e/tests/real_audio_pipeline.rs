//! Full real-audio E2E pipeline test (`C++ kotoha_mock_client --wav` -> OS IPC ->
//! `AudioPreprocessor` -> native `sherpa-onnx` Zipformer STT with TOML hotwords ->
//! `OpenAiCompatibleTranslator` over local HTTP -> `CollectingSubtitleSink`).

#![cfg(feature = "real-audio-e2e")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines
)]

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kotoha_core::application::{
    GlossaryManager, PipelineConfig, PipelineService, SeqStatus, SessionAction,
    SourceSessionManager, TranslatorManager,
};
use kotoha_core::domain::{
    AudioInputEvent, LanguageCode, SubtitleStage, TalkingState, TranslationProviderConfig,
};
use kotoha_core::infrastructure::{
    AudioPreprocessor, CollectingSubtitleSink, IpcServerAdapter, TomlGlossaryRepository,
};
use kotoha_e2e::harness::await_mock_client;
use kotoha_e2e::{E2eBinaries, MockClientArgs, UniqueIpcEndpoint};
use kotoha_stt_sherpa::{SherpaModelFamily, SherpaModelingUnit, SherpaSttConfig, SherpaSttEngine};
use kotoha_translate_http::{OpenAiCompatibleConfig, OpenAiCompatibleTranslator};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, watch};

const ENV_MODEL_DIR: &str = "KOTOHA_E2E_MODEL_DIR";
const ENV_WAV_PATH: &str = "KOTOHA_E2E_WAV_PATH";
const ENV_EXPECTED_WORDS: &str = "KOTOHA_E2E_EXPECTED_WORDS";
const DEFAULT_EXPECTED_WORDS: &str = "NIGHTFALL,YELLOW,LAMPS";
const JAPANESE_TRANSLATION: &str = "夕暮れの後、黄色いランタンが静かに灯った。";

struct DiscoveredZipformerModel {
    encoder: PathBuf,
    decoder: PathBuf,
    joiner: PathBuf,
    tokens: PathBuf,
    bpe_vocab: Option<PathBuf>,
}

fn discover_zipformer_model(model_dir: &Path) -> DiscoveredZipformerModel {
    assert!(
        model_dir.is_dir(),
        "{ENV_MODEL_DIR} does not exist or is not a directory: {}",
        model_dir.display()
    );

    let mut files: Vec<PathBuf> = fs::read_dir(model_dir)
        .expect("read model_dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    files.sort();

    let encoder = select_onnx_model(&files, "encoder")
        .unwrap_or_else(|| panic!("missing *encoder*.onnx in {}", model_dir.display()));
    let decoder = select_onnx_model(&files, "decoder")
        .unwrap_or_else(|| panic!("missing *decoder*.onnx in {}", model_dir.display()));
    let joiner = select_onnx_model(&files, "joiner")
        .unwrap_or_else(|| panic!("missing *joiner*.onnx in {}", model_dir.display()));

    let tokens = files
        .iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some("tokens.txt"))
        .cloned()
        .unwrap_or_else(|| panic!("missing tokens.txt in {}", model_dir.display()));

    let bpe_vocab = files
        .iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some("bpe.vocab"))
        .or_else(|| {
            files
                .iter()
                .find(|p| p.file_name().and_then(|n| n.to_str()) == Some("bpe.model"))
        })
        .cloned();

    DiscoveredZipformerModel {
        encoder,
        decoder,
        joiner,
        tokens,
        bpe_vocab,
    }
}

fn select_onnx_model(files: &[PathBuf], role: &str) -> Option<PathBuf> {
    let matching: Vec<&PathBuf> = files
        .iter()
        .filter(|path| {
            let is_onnx = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("onnx"));
            let has_role = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains(role));
            is_onnx && has_role
        })
        .collect();

    // Prefer int8 quantized artifacts when both int8 and fp32 exist in the model archive.
    matching
        .iter()
        .find(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains(".int8."))
        })
        .or_else(|| matching.first())
        .map(|p| (*p).clone())
}

#[derive(Debug, Clone)]
struct RecordedHttpRequest {
    method: String,
    target: String,
    body: String,
}

struct LocalOpenAiServer {
    base_url: String,
    recorded: Arc<Mutex<Vec<RecordedHttpRequest>>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl LocalOpenAiServer {
    async fn start(reply_text: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local HTTP translation server");
        let addr = listener.local_addr().expect("local_addr");
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let recorded_bg = Arc::clone(&recorded);
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else {
                            break;
                        };
                        let recorded_conn = Arc::clone(&recorded_bg);
                        tokio::spawn(async move {
                            if let Some(req) = read_http_request(&mut stream).await {
                                recorded_conn.lock().expect("lock").push(req);
                                let body = serde_json::json!({
                                    "choices": [{
                                        "message": {
                                            "content": reply_text
                                        }
                                    }]
                                })
                                .to_string();
                                let mut raw = String::new();
                                let _ = write!(
                                    raw,
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                    body.len(),
                                    body
                                );
                                let _ = stream.write_all(raw.as_bytes()).await;
                                let _ = stream.shutdown().await;
                            }
                        });
                    }
                }
            }
        });

        Self {
            base_url: format!("http://{addr}/v1"),
            recorded,
            shutdown_tx: Some(shutdown_tx),
        }
    }

    fn requests(&self) -> Vec<RecordedHttpRequest> {
        self.recorded.lock().expect("lock").clone()
    }
}

impl Drop for LocalOpenAiServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> Option<RecordedHttpRequest> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 1024];
    let header_end = loop {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
    };

    let header_text = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();

    let mut content_length = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }

    let body_start = header_end + 4;
    let expected_total = body_start.checked_add(content_length)?;
    while buf.len() < expected_total {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body_end = expected_total.min(buf.len());
    let body = String::from_utf8_lossy(&buf[body_start..body_end]).into_owned();

    Some(RecordedHttpRequest {
        method,
        target,
        body,
    })
}

fn write_temp_glossary_dir() -> PathBuf {
    static NEXT_GLOSSARY_SEQ: AtomicU64 = AtomicU64::new(1);
    let seq = NEXT_GLOSSARY_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!("kotoha_e2e_glossary_{}_{seq}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp glossary dir");

    let toml_payload = r#"name = "e2e-real-audio"
src_lang = "en"
tgt_lang = "ja"

[terms]
"NIGHTFALL" = "夕暮れ"
"YELLOW" = "黄色い"
"#;
    fs::write(dir.join("e2e_real_audio.toml"), toml_payload).expect("write TOML glossary");
    dir
}

struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn real_english_wav_through_cpp_ipc_sherpa_stt_and_http_translation() {
    // 1. Locate model artifacts and WAV speech fixture
    let model_dir = PathBuf::from(env::var(ENV_MODEL_DIR).unwrap_or_else(|_| {
        panic!("{ENV_MODEL_DIR} must point to a downloaded Zipformer streaming model directory")
    }));
    let wav_path = env::var(ENV_WAV_PATH)
        .map_or_else(|_| model_dir.join("test_wavs").join("0.wav"), PathBuf::from);
    assert!(
        wav_path.is_file(),
        "real audio WAV fixture does not exist: {}",
        wav_path.display()
    );

    let expected_words_raw =
        env::var(ENV_EXPECTED_WORDS).unwrap_or_else(|_| DEFAULT_EXPECTED_WORDS.to_owned());
    let expected_words: Vec<String> = expected_words_raw
        .split(',')
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_uppercase)
        .collect();
    assert!(
        !expected_words.is_empty(),
        "{ENV_EXPECTED_WORDS} must specify at least one expected keyword"
    );

    let model_files = discover_zipformer_model(&model_dir);
    let has_bpe_vocab = model_files
        .bpe_vocab
        .as_ref()
        .is_some_and(|p| p.file_name().and_then(|n| n.to_str()) == Some("bpe.vocab"));

    // 2. Load TOML glossary via TomlGlossaryRepository + GlossaryManager
    let glossary_dir = write_temp_glossary_dir();
    let _glossary_guard = TempDirGuard(glossary_dir.clone());
    let src_lang = LanguageCode::new("en").expect("en language code");
    let tgt_lang = LanguageCode::new("ja").expect("ja language code");
    let repo = TomlGlossaryRepository::new();
    let glossary_mgr =
        GlossaryManager::from_dir(&repo, &glossary_dir, src_lang.clone(), tgt_lang.clone())
            .expect("load TOML glossary directory");
    assert_eq!(
        glossary_mgr
            .active_merged()
            .terms()
            .get("NIGHTFALL")
            .map(String::as_str),
        Some("夕暮れ")
    );
    assert_eq!(
        glossary_mgr
            .active_merged()
            .terms()
            .get("YELLOW")
            .map(String::as_str),
        Some("黄色い")
    );

    // 3. Instantiate real native sherpa-onnx streaming STT engine
    let mut stt_cfg = SherpaSttConfig::new(SherpaModelFamily::ZipformerTransducer {
        encoder: model_files.encoder.clone(),
        decoder: model_files.decoder.clone(),
        joiner: model_files.joiner.clone(),
        tokens: model_files.tokens.clone(),
        bpe_vocab: if has_bpe_vocab {
            model_files.bpe_vocab.clone()
        } else {
            None
        },
        modeling_unit: has_bpe_vocab.then_some(SherpaModelingUnit::Bpe),
    });
    stt_cfg.hotwords_score = 1.5;

    let stt_engine =
        Arc::new(SherpaSttEngine::new_native(stt_cfg).expect("create native SherpaSttEngine"));

    // 4. Start local OpenAI-compatible HTTP server & wire OpenAiCompatibleTranslator
    let llm_server = LocalOpenAiServer::start(JAPANESE_TRANSLATION).await;
    let translator = Arc::new(
        OpenAiCompatibleTranslator::new(&OpenAiCompatibleConfig {
            base_url: llm_server.base_url.clone(),
            model: "kotoha-e2e-llm".to_owned(),
            api_key: None,
            timeout_ms: 10_000,
        })
        .expect("create OpenAiCompatibleTranslator"),
    );
    let translator_mgr = Arc::new(TranslatorManager::new(
        translator,
        TranslationProviderConfig::OpenAiCompatible {
            base_url: llm_server.base_url.clone(),
            model: "kotoha-e2e-llm".to_owned(),
            timeout_ms: 10_000,
        },
    ));

    // 5. Assemble PipelineService and inject TOML glossary (updates both ContextWindow and STT hotwords)
    let sink = Arc::new(CollectingSubtitleSink::new());
    let mut pipeline = PipelineService::new(
        PipelineConfig::new(src_lang, tgt_lang),
        stt_engine.clone(),
        translator_mgr,
        sink.clone(),
        AudioPreprocessor::resampler_factory(),
    );
    if has_bpe_vocab {
        glossary_mgr
            .sync_pipeline(&mut pipeline)
            .await
            .expect("sync glossary + sherpa BPE hotwords into pipeline");
    } else {
        // Without `bpe.vocab`, `sherpa-onnx` still accepts `sync_pipeline` cleanly (logging a
        // notice for non-BPE-encoded hotwords) while propagating the full TOML glossary to
        // `PipelineService`'s `ContextWindow` and HTTP translation requests.
        let stt_ok = glossary_mgr.sync_pipeline(&mut pipeline).await;
        if let Err(err) = stt_ok {
            eprintln!("note: skipping sherpa hotwords sync without bpe.vocab ({err})");
        }
    }

    // 6. Start real OS IPC server (IpcServerAdapter)
    let client_bin =
        E2eBinaries::discover_mock_client().expect("discover C++ kotoha_mock_client binary");
    let endpoint = UniqueIpcEndpoint::new("realaudio").expect("create unique IPC endpoint");
    let ipc_server = IpcServerAdapter::new(endpoint.as_str());
    let (raw_tx, mut raw_rx) = mpsc::channel::<AudioInputEvent>(512);
    let (pipeline_tx, pipeline_rx) = mpsc::channel::<AudioInputEvent>(512);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (ready_tx, ready_rx) = oneshot::channel();
    let (disconnected_tx, disconnected_rx) = oneshot::channel::<()>();

    let server_handle =
        tokio::spawn(async move { ipc_server.run(raw_tx, shutdown_rx, Some(ready_tx)).await });
    if ready_rx.await.is_err() {
        let server_res = server_handle.await.expect("join failed IPC server task");
        panic!("IPC server failed before becoming ready: {server_res:?}");
    }

    // Tap IPC events through SourceSessionManager to independently track drop-free delivery and
    // passive state transition while forwarding every event into PipelineService.
    let passive_seen = Arc::new(AtomicBool::new(false));
    let passive_seen_bg = Arc::clone(&passive_seen);
    let drop_tracker_handle = tokio::spawn(async move {
        let mut monitor_session = SourceSessionManager::new();
        let mut frames_received = 0u64;
        let mut frames_dropped = 0u64;
        let mut disconnected_seen = false;
        let mut disconnected_notify = Some(disconnected_tx);

        while let Some(event) = raw_rx.recv().await {
            let action = monitor_session.handle_event(&event);
            if let SessionAction::AudioSequenceChecked(SeqStatus::Dropped { dropped_frames }) =
                action
            {
                frames_dropped += u64::from(dropped_frames);
            }
            if matches!(event, AudioInputEvent::AudioFrame { .. }) {
                frames_received += 1;
            }
            let is_passive = matches!(
                event,
                AudioInputEvent::TalkingStateChanged {
                    state: TalkingState::Passive,
                    ..
                }
            );
            let is_disconnect = matches!(event, AudioInputEvent::SourceDisconnected { .. });
            if pipeline_tx.send(event).await.is_err() {
                break;
            }
            if is_passive {
                passive_seen_bg.store(true, Ordering::Release);
            }
            if is_disconnect {
                disconnected_seen = true;
                if let Some(tx) = disconnected_notify.take() {
                    let _ = tx.send(());
                }
            }
        }
        (frames_received, frames_dropped, disconnected_seen)
    });

    let pipeline_handle = tokio::spawn(async move { pipeline.run(pipeline_rx).await });

    // 7. Stream genuine human speech WAV + 1400ms trailing silence via real C++ kotoha_mock_client,
    // holding the IPC connection open via `--wait-stdin-eof` until all WAV/silence frames
    // (`TalkingState::Passive` seen) and async translations finish so `UserRemoved` / `SourceReset`
    // never supersedes an in-flight translation epoch.
    let mut client_child = MockClientArgs::new(&endpoint)
        .source("mumble")
        .user(42, "Alice")
        .wav(&wav_path)
        .tail_silence_ms(1400)
        .with_heartbeat()
        .wait_stdin_eof()
        .spawn(&client_bin)
        .expect("spawn kotoha_mock_client --wav --wait-stdin-eof");
    let client_stdin = client_child
        .stdin
        .take()
        .expect("wait_stdin_eof must pipe client stdin");

    // Poll every 50ms up to 20s until:
    // - `kotoha_mock_client` has NOT exited prematurely, AND
    // - `TalkingState::Passive` has been observed (all WAV + tail-silence frames delivered), AND
    // - at least one `Translated` entry exists with all finalized utterances translated.
    let translation_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(early_status) = client_child
            .try_wait()
            .expect("poll kotoha_mock_client child status")
        {
            let out = await_mock_client(client_child, Duration::from_secs(2)).await;
            panic!(
                "kotoha_mock_client exited prematurely before stdin EOF (status={early_status}): {out:?}"
            );
        }

        let current_entries = sink.entries().await;
        let final_count = current_entries
            .iter()
            .filter(|e| e.stage == SubtitleStage::FinalPendingTranslation)
            .count();
        let translated_count = current_entries
            .iter()
            .filter(|e| e.stage == SubtitleStage::Translated)
            .count();
        if passive_seen.load(Ordering::Acquire)
            && translated_count > 0
            && translated_count >= final_count
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < translation_deadline,
            "timed out after 20s waiting for Translated subtitle entry (passive_seen={}); entries={current_entries:#?}",
            passive_seen.load(Ordering::Acquire)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Signal clean client teardown (`UserRemoved` -> `SourceReset` -> disconnect).
    drop(client_stdin);
    let client_output = await_mock_client(client_child, Duration::from_secs(15))
        .await
        .expect("kotoha_mock_client exited cleanly after stdin EOF");
    assert!(
        client_output.status.success(),
        "kotoha_mock_client exited unexpectedly: {:?}",
        client_output.status
    );

    // Wait for `IpcServerAdapter`'s connection handler to emit `SourceDisconnected` after reading
    // every queued client frame before signalling server shutdown.
    tokio::time::timeout(Duration::from_secs(10), disconnected_rx)
        .await
        .expect("timed out waiting for SourceDisconnected")
        .expect("drop tracker closed before SourceDisconnected");

    // Gracefully shut down IPC server -> closes raw_tx -> closes pipeline_tx -> flushes translations
    let _ = shutdown_tx.send(true);
    server_handle
        .await
        .expect("join IPC server")
        .expect("IPC server clean shutdown");

    if let Some(sock_path) = endpoint.unix_path() {
        assert!(
            !sock_path.exists(),
            "IpcServerAdapter left stale Unix socket file after shutdown: {}",
            sock_path.display()
        );
    }

    let (frames_received, frames_dropped, disconnected_seen) =
        drop_tracker_handle.await.expect("join drop tracker");
    pipeline_handle
        .await
        .expect("join pipeline")
        .expect("pipeline clean shutdown");

    assert!(
        frames_received > 0,
        "expected > 0 IPC audio frames from WAV"
    );
    assert_eq!(frames_dropped, 0, "expected zero dropped IPC audio frames");
    assert!(disconnected_seen, "expected clean SourceDisconnected event");

    // 8. Assert 3-stage SubtitleSink lifecycle & real STT transcription accuracy
    let entries = sink.entries().await;
    assert!(
        !entries.is_empty(),
        "expected CollectingSubtitleSink entries from real audio"
    );

    let has_interim = entries.iter().any(|e| e.stage == SubtitleStage::Interim);
    let final_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.stage == SubtitleStage::FinalPendingTranslation)
        .collect();
    let translated_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.stage == SubtitleStage::Translated)
        .collect();

    assert!(
        has_interim,
        "expected at least one Interim subtitle entry, got {entries:#?}"
    );
    assert!(
        !final_entries.is_empty(),
        "expected at least one FinalPendingTranslation subtitle entry, got {entries:#?}"
    );
    assert_eq!(
        translated_entries.len(),
        final_entries.len(),
        "every finalized utterance should emit a Translated subtitle entry"
    );

    for entry in &entries {
        assert_eq!(entry.speaker_name, "Alice");
        assert_eq!(entry.speaker_key.source, "mumble");
        assert_eq!(entry.speaker_key.user_id, 42);
    }

    let combined_transcript_raw = final_entries
        .iter()
        .map(|e| e.src_text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    println!(
        "[real_audio_pipeline] real sherpa-onnx transcript ({} final utterance(s), {} IPC frames): {:?}",
        final_entries.len(),
        frames_received,
        combined_transcript_raw
    );
    let combined_transcript = combined_transcript_raw.to_ascii_uppercase();
    for word in &expected_words {
        assert!(
            combined_transcript.contains(word),
            "real sherpa-onnx transcript {combined_transcript:?} missing expected word {word:?}"
        );
    }

    for entry in &translated_entries {
        assert_eq!(
            entry.translated_text.as_deref(),
            Some(JAPANESE_TRANSLATION),
            "unexpected Japanese translation in Translated subtitle entry"
        );
    }

    // 9. Assert local OpenAI-compatible HTTP server received real English transcript + TOML glossary
    let http_requests = llm_server.requests();
    assert_eq!(
        http_requests.len(),
        final_entries.len(),
        "HTTP translation server request count mismatch"
    );
    let mut saw_expected_word_in_http = false;
    for req in &http_requests {
        assert_eq!(req.method, "POST");
        assert_eq!(req.target, "/v1/chat/completions");

        let body_json: serde_json::Value =
            serde_json::from_str(&req.body).expect("valid OpenAI JSON request body");
        let user_content = body_json["messages"][1]["content"]
            .as_str()
            .expect("user message content string");
        let inner_json = user_content
            .strip_prefix("<TRANSLATION_INPUT_JSON>\n")
            .and_then(|s| s.strip_suffix("\n</TRANSLATION_INPUT_JSON>"))
            .expect("wrapped <TRANSLATION_INPUT_JSON> payload");
        let payload: serde_json::Value =
            serde_json::from_str(inner_json).expect("valid inner translation JSON");

        assert_eq!(payload["current_speaker"], "Alice");
        assert_eq!(payload["glossary"]["NIGHTFALL"], "夕暮れ");
        assert_eq!(payload["glossary"]["YELLOW"], "黄色い");

        let utterance = payload["utterance"]
            .as_str()
            .expect("utterance string")
            .to_ascii_uppercase();
        if expected_words.iter().any(|w| utterance.contains(w)) {
            saw_expected_word_in_http = true;
        }
    }
    assert!(
        saw_expected_word_in_http,
        "HTTP translation requests did not carry any expected STT keyword from {expected_words:?}"
    );
}
