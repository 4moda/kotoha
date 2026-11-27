//! Comprehensive unit and pipeline integration tests for `kotoha-stt-sherpa`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::too_many_lines
)]

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::f32::consts::PI;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use kotoha_core::application::{PipelineConfig, PipelineService, TranslatorManager};
use kotoha_core::domain::{
    AudioChunk, AudioInputEvent, ComputeDevice, CoreError, LanguageCode, MergedGlossary,
    SpeakerKey, SttDecodingMethod, SttEngine, SttHotwordsConfig, SubtitleStage,
    TranslationProviderConfig, UtteranceId,
};
use kotoha_core::infrastructure::{AudioPreprocessor, CollectingSubtitleSink, MockTranslator};
use kotoha_stt_sherpa::{
    compute_device_to_provider, format_hotwords_text, format_hotwords_text_with_scores,
    sanitize_hotwords, HotwordsSnapshot, RecognizerBackend, RecognizerHypothesis,
    RecognizerStreamBackend, SherpaEndpointConfig, SherpaModelFamily, SherpaModelingUnit,
    SherpaSttConfig, SherpaSttEngine,
};

fn unique_temp_dir(prefix: &str) -> PathBuf {
    static NEXT_DIR_ID: AtomicU64 = AtomicU64::new(1);
    let seq = NEXT_DIR_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}_{}_{seq}", std::process::id()))
}

// ---------------------------------------------------------------------------
// Pure-Rust 16 kHz Mono PCM-16 WAV Test Fixture Encoder / Decoder
// ---------------------------------------------------------------------------

fn encode_wav_pcm16(sample_rate: u32, samples: &[f32]) -> Vec<u8> {
    let num_samples = u32::try_from(samples.len()).expect("sample count fits u32");
    let data_len = num_samples * 2;
    let riff_len = 36 + data_len;

    let mut bytes = Vec::with_capacity(usize::try_from(44 + data_len).unwrap_or(44));
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&riff_len.to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM format = 1
    bytes.extend_from_slice(&1u16.to_le_bytes()); // channels = 1 (mono)
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte_rate
    bytes.extend_from_slice(&2u16.to_le_bytes()); // block_align
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits_per_sample = 16
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());

    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        #[allow(clippy::cast_possible_truncation)]
        let pcm = (clamped * 32767.0).round() as i16;
        bytes.extend_from_slice(&pcm.to_le_bytes());
    }
    bytes
}

fn decode_wav_pcm16(bytes: &[u8]) -> (u32, Vec<f32>) {
    assert!(bytes.len() >= 44, "WAV too short");
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(&bytes[12..16], b"fmt ");
    let format_tag = u16::from_le_bytes([bytes[20], bytes[21]]);
    let channels = u16::from_le_bytes([bytes[22], bytes[23]]);
    let sample_rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    let bits_per_sample = u16::from_le_bytes([bytes[34], bytes[35]]);
    assert_eq!(format_tag, 1, "expected PCM");
    assert_eq!(channels, 1, "expected mono");
    assert_eq!(bits_per_sample, 16, "expected 16-bit PCM");

    assert_eq!(&bytes[36..40], b"data");
    let data_len = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]) as usize;
    let payload = &bytes[44..44 + data_len];
    let samples = payload
        .chunks_exact(2)
        .map(|chunk| {
            let v = i16::from_le_bytes([chunk[0], chunk[1]]);
            f32::from(v) / 32768.0
        })
        .collect();
    (sample_rate, samples)
}

fn generate_speech_burst_wav_fixture() -> (u32, Vec<f32>) {
    // 16 kHz mono: 200ms burst1 (440Hz) + 120ms silence + 200ms burst2 (660Hz)
    let sample_rate = 16_000u32;
    let mut raw = Vec::with_capacity(8_320);

    for i in 0..3_200 {
        let t = (i as f32) / (sample_rate as f32);
        raw.push(0.45 * (2.0 * PI * 440.0 * t).sin());
    }
    raw.resize(3_200 + 1_920, 0.0);
    for i in 0..3_200 {
        let t = (i as f32) / (sample_rate as f32);
        raw.push(0.45 * (2.0 * PI * 660.0 * t).sin());
    }

    let wav = encode_wav_pcm16(sample_rate, &raw);
    decode_wav_pcm16(&wav)
}

// ---------------------------------------------------------------------------
// Scriptable Mock RecognizerBackend for deterministic state-machine testing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct RecordedStreamCreation {
    generation: u64,
    decoding_method: SttDecodingMethod,
    hotwords_score: f32,
    formatted_text: String,
}

#[derive(Debug, Default)]
struct MockBackendState {
    created_streams: Vec<RecordedStreamCreation>,
    hotword_reload_calls: Vec<(SttDecodingMethod, f32, u64)>,
    live_stream_count: AtomicUsize,
}

struct MockRecognizerBackend {
    state: Arc<StdMutex<MockBackendState>>,
    silence_rms_threshold: f32,
    min_active_chunks_for_endpoint: usize,
    use_scripted_outcomes: bool,
    fail_hotword_reload: AtomicBool,
    empty_result_on_endpoint_or_flush: AtomicBool,
}

impl MockRecognizerBackend {
    fn new(silence_rms_threshold: f32, min_active_chunks_for_endpoint: usize) -> Self {
        Self {
            state: Arc::new(StdMutex::new(MockBackendState::default())),
            silence_rms_threshold,
            min_active_chunks_for_endpoint,
            use_scripted_outcomes: false,
            fail_hotword_reload: AtomicBool::new(false),
            empty_result_on_endpoint_or_flush: AtomicBool::new(false),
        }
    }

    fn scripted(silence_rms_threshold: f32) -> Self {
        Self {
            state: Arc::new(StdMutex::new(MockBackendState::default())),
            silence_rms_threshold,
            min_active_chunks_for_endpoint: usize::MAX,
            use_scripted_outcomes: true,
            fail_hotword_reload: AtomicBool::new(false),
            empty_result_on_endpoint_or_flush: AtomicBool::new(false),
        }
    }

    fn set_fail_hotword_reload(&self, fail: bool) {
        self.fail_hotword_reload.store(fail, Ordering::SeqCst);
    }

    fn set_empty_result_on_endpoint_or_flush(&self, empty: bool) {
        self.empty_result_on_endpoint_or_flush
            .store(empty, Ordering::SeqCst);
    }

    fn recorded_streams(&self) -> Vec<RecordedStreamCreation> {
        self.state.lock().unwrap().created_streams.clone()
    }

    fn hotword_reload_calls(&self) -> Vec<(SttDecodingMethod, f32, u64)> {
        self.state.lock().unwrap().hotword_reload_calls.clone()
    }

    fn live_stream_count(&self) -> usize {
        self.state
            .lock()
            .unwrap()
            .live_stream_count
            .load(Ordering::SeqCst)
    }
}

impl RecognizerBackend for MockRecognizerBackend {
    fn create_stream(
        &self,
        hotwords: &HotwordsSnapshot,
    ) -> Result<Box<dyn RecognizerStreamBackend>, CoreError> {
        let state = self.state.lock().unwrap();
        state.live_stream_count.fetch_add(1, Ordering::SeqCst);
        drop(state);

        self.state
            .lock()
            .unwrap()
            .created_streams
            .push(RecordedStreamCreation {
                generation: hotwords.generation,
                decoding_method: hotwords.decoding_method,
                hotwords_score: hotwords.hotwords_score,
                formatted_text: hotwords.formatted_text.clone(),
            });

        Ok(Box::new(MockRecognizerStream {
            state: Arc::clone(&self.state),
            hotwords_snapshot: hotwords.clone(),
            silence_rms_threshold: self.silence_rms_threshold,
            min_active_chunks_for_endpoint: self.min_active_chunks_for_endpoint,
            outcome_mode: if self.use_scripted_outcomes {
                MockOutcomeMode::Scripted
            } else {
                MockOutcomeMode::Default
            },
            empty_result_on_endpoint_or_flush: self
                .empty_result_on_endpoint_or_flush
                .load(Ordering::SeqCst),
            pending_chunks: VecDeque::new(),
            active_chunks: 0,
            current_text: String::new(),
            endpoint_reached: false,
            input_finished: false,
            reset_calls: 0,
            dropped: Arc::new(AtomicBool::new(false)),
        }))
    }

    fn on_hotwords_updated(
        &self,
        config: &SherpaSttConfig,
        hotwords: &HotwordsSnapshot,
    ) -> Result<(), CoreError> {
        if self.fail_hotword_reload.load(Ordering::SeqCst) {
            return Err(CoreError::Stt(
                "simulated recognizer reload failure".to_owned(),
            ));
        }
        self.state.lock().unwrap().hotword_reload_calls.push((
            config.effective_decoding_method(!hotwords.is_empty()),
            hotwords.hotwords_score,
            hotwords.generation,
        ));
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MockOutcomeMode {
    Default,
    Scripted,
}

struct MockRecognizerStream {
    state: Arc<StdMutex<MockBackendState>>,
    hotwords_snapshot: HotwordsSnapshot,
    silence_rms_threshold: f32,
    min_active_chunks_for_endpoint: usize,
    outcome_mode: MockOutcomeMode,
    empty_result_on_endpoint_or_flush: bool,
    pending_chunks: VecDeque<Vec<f32>>,
    active_chunks: usize,
    current_text: String,
    endpoint_reached: bool,
    input_finished: bool,
    reset_calls: usize,
    dropped: Arc<AtomicBool>,
}

impl Drop for MockRecognizerStream {
    fn drop(&mut self) {
        if !self.dropped.swap(true, Ordering::SeqCst) {
            let state = self.state.lock().unwrap();
            state.live_stream_count.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl RecognizerStreamBackend for MockRecognizerStream {
    fn accept_waveform(&mut self, _sample_rate: i32, samples: &[f32]) -> Result<(), CoreError> {
        assert!(
            !self.input_finished,
            "accept_waveform must never be called after input_finished() on same stream"
        );
        self.pending_chunks.push_back(samples.to_vec());
        Ok(())
    }

    fn input_finished(&mut self) -> Result<(), CoreError> {
        self.input_finished = true;
        Ok(())
    }

    fn is_ready(&mut self) -> Result<bool, CoreError> {
        Ok(!self.pending_chunks.is_empty())
    }

    fn decode(&mut self) -> Result<(), CoreError> {
        let Some(chunk) = self.pending_chunks.pop_front() else {
            return Ok(());
        };
        let rms = AudioPreprocessor::compute_rms(&chunk);
        let bias_tag = if self.hotwords_snapshot.phrases.is_empty() {
            "none".to_owned()
        } else {
            self.hotwords_snapshot.phrases.join(",")
        };

        if rms >= self.silence_rms_threshold {
            self.active_chunks += 1;
            match self.outcome_mode {
                MockOutcomeMode::Scripted => {
                    self.current_text = if self.active_chunks == 1 {
                        format!("Activate [{bias_tag}]")
                    } else {
                        format!("Activate [{bias_tag}] chunk {}", self.active_chunks)
                    };
                }
                MockOutcomeMode::Default => {
                    self.current_text =
                        format!("Utterance({}) [hw:{bias_tag}]", self.active_chunks);
                    if self.active_chunks >= self.min_active_chunks_for_endpoint {
                        self.endpoint_reached = true;
                    }
                }
            }
        } else if self.active_chunks > 0 {
            self.endpoint_reached = true;
        }

        Ok(())
    }

    fn get_result(&mut self) -> Result<Option<RecognizerHypothesis>, CoreError> {
        if self.empty_result_on_endpoint_or_flush && (self.endpoint_reached || self.input_finished)
        {
            return Ok(None);
        }
        if self.current_text.is_empty() {
            return Ok(None);
        }
        Ok(Some(RecognizerHypothesis {
            text: self.current_text.clone(),
            is_final: false,
        }))
    }

    fn is_endpoint(&mut self) -> Result<bool, CoreError> {
        Ok(self.endpoint_reached)
    }

    fn reset(&mut self) -> Result<(), CoreError> {
        self.reset_calls += 1;
        self.pending_chunks.clear();
        self.active_chunks = 0;
        self.current_text.clear();
        self.endpoint_reached = false;
        self.input_finished = false;
        Ok(())
    }
}

fn sample_zipformer_config() -> SherpaSttConfig {
    SherpaSttConfig::new(SherpaModelFamily::ZipformerTransducer {
        encoder: PathBuf::from("/models/encoder.int8.onnx"),
        decoder: PathBuf::from("/models/decoder.onnx"),
        joiner: PathBuf::from("/models/joiner.int8.onnx"),
        tokens: PathBuf::from("/models/tokens.txt"),
        bpe_vocab: Some(PathBuf::from("/models/bpe.vocab")),
        modeling_unit: Some(SherpaModelingUnit::Bpe),
    })
}

fn sample_paraformer_config() -> SherpaSttConfig {
    SherpaSttConfig::new(SherpaModelFamily::Paraformer {
        encoder: PathBuf::from("/models/paraformer-encoder.onnx"),
        decoder: PathBuf::from("/models/paraformer-decoder.onnx"),
        tokens: PathBuf::from("/models/tokens.txt"),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn config_validation_provider_mapping_and_decoding_method() {
    assert_eq!(compute_device_to_provider(ComputeDevice::Auto), "cpu");
    assert_eq!(compute_device_to_provider(ComputeDevice::Cpu), "cpu");
    assert_eq!(compute_device_to_provider(ComputeDevice::Cuda), "cuda");
    assert_eq!(compute_device_to_provider(ComputeDevice::Metal), "coreml");

    let mut cfg = sample_zipformer_config();
    assert!(cfg.validate().is_ok());
    assert_eq!(cfg.provider_name(), "cpu");
    assert_eq!(
        cfg.effective_decoding_method(false),
        SttDecodingMethod::GreedySearch
    );
    assert_eq!(
        cfg.effective_decoding_method(true),
        SttDecodingMethod::ModifiedBeamSearch
    );
    assert_eq!(
        SherpaSttConfig::decoding_method_str(SttDecodingMethod::ModifiedBeamSearch),
        "modified_beam_search"
    );

    cfg.device = ComputeDevice::Metal;
    assert_eq!(cfg.provider_name(), "coreml");

    let paraformer = sample_paraformer_config();
    assert!(paraformer.validate().is_ok());
    assert!(!paraformer.model.supports_hotwords());
    assert_eq!(
        paraformer.effective_decoding_method(true),
        SttDecodingMethod::GreedySearch
    );

    // Invalid numeric parameters rejected
    let mut bad = cfg.clone();
    bad.num_threads = 0;
    assert!(bad.validate().is_err());

    bad = cfg.clone();
    bad.max_active_paths = 0;
    assert!(bad.validate().is_err());

    bad = cfg.clone();
    bad.hotwords_score = 0.0;
    assert!(bad.validate().is_err());

    bad = cfg.clone();
    bad.hotwords_score = f32::NAN;
    assert!(bad.validate().is_err());

    bad = cfg.clone();
    bad.hotwords_score = f32::INFINITY;
    assert!(bad.validate().is_err());

    bad = cfg.clone();
    bad.idle_timeout = Duration::ZERO;
    assert!(bad.validate().is_err());

    bad = cfg.clone();
    bad.endpoint = SherpaEndpointConfig {
        enable: true,
        rule1_min_trailing_silence: -0.1,
        ..Default::default()
    };
    assert!(bad.validate().is_err());

    // Empty path rejected
    let empty_path_cfg = SherpaSttConfig::new(SherpaModelFamily::ZipformerTransducer {
        encoder: PathBuf::from(""),
        decoder: PathBuf::from("/models/decoder.onnx"),
        joiner: PathBuf::from("/models/joiner.int8.onnx"),
        tokens: PathBuf::from("/models/tokens.txt"),
        bpe_vocab: None,
        modeling_unit: None,
    });
    assert!(empty_path_cfg.validate().is_err());

    // BPE / CjkCharPlusBpe modeling_unit requires bpe_vocab
    for unit in [SherpaModelingUnit::Bpe, SherpaModelingUnit::CjkCharPlusBpe] {
        let missing_vocab_cfg = SherpaSttConfig::new(SherpaModelFamily::ZipformerTransducer {
            encoder: PathBuf::from("/models/encoder.int8.onnx"),
            decoder: PathBuf::from("/models/decoder.onnx"),
            joiner: PathBuf::from("/models/joiner.int8.onnx"),
            tokens: PathBuf::from("/models/tokens.txt"),
            bpe_vocab: None,
            modeling_unit: Some(unit),
        });
        assert!(missing_vocab_cfg.validate().is_err());
    }

    // CjkChar modeling_unit does NOT require bpe_vocab
    let cjk_cfg = SherpaSttConfig::new(SherpaModelFamily::ZipformerTransducer {
        encoder: PathBuf::from("/models/encoder.int8.onnx"),
        decoder: PathBuf::from("/models/decoder.onnx"),
        joiner: PathBuf::from("/models/joiner.int8.onnx"),
        tokens: PathBuf::from("/models/tokens.txt"),
        bpe_vocab: None,
        modeling_unit: Some(SherpaModelingUnit::CjkChar),
    });
    assert!(cjk_cfg.validate().is_ok());

    // Non-existent files fail validate_files_exist()
    assert!(cfg.validate_files_exist().is_err());
}

#[test]
fn hotwords_formatting_sanitizes_deduplicates_and_guards_sherpa_syntax() {
    let raw = vec![
        "  M W D  ".to_owned(),
        "M W D\r\n".to_owned(),
        "Ferox\0Injected".to_owned(),
        "#commented_out".to_owned(),
        "##   double_comment".to_owned(),
        "foo :9.9".to_owned(),
        "###".to_owned(),
        "   ".to_owned(),
        "\t\n".to_owned(),
        "Logi".to_owned(),
        "Logi".to_owned(),
    ];

    let sanitized = sanitize_hotwords(&raw);
    assert_eq!(
        sanitized,
        vec![
            "FeroxInjected".to_owned(),
            "Logi".to_owned(),
            "M W D".to_owned(),
            "commented_out".to_owned(),
            "double_comment".to_owned(),
            "foo  9.9".to_owned(),
        ]
    );

    let hw_cfg = SttHotwordsConfig::from_hotwords(vec![
        "M W D".to_owned(),
        "Anchor".to_owned(),
        "M W D\n".to_owned(),
        "#Fleet:9.5".to_owned(),
    ]);
    let formatted = format_hotwords_text(&hw_cfg).expect("format hotwords");
    assert_eq!(formatted, "Anchor\nFleet 9.5\nM W D\n");

    let scored = format_hotwords_text_with_scores(&hw_cfg, Some(2.25)).expect("scored format");
    assert_eq!(scored, "Anchor :2.25\nFleet 9.5 :2.25\nM W D :2.25\n");

    for bad_score in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let invalid = SttHotwordsConfig {
            decoding_method: SttDecodingMethod::ModifiedBeamSearch,
            hotwords_score: bad_score,
            hotwords: vec!["Logi".to_owned()],
        };
        assert!(format_hotwords_text(&invalid).is_err());
    }
}

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

#[cfg(feature = "native-sherpa")]
#[test]
fn native_sherpa_config_mapping_populates_transducer_paraformer_and_hotwords() {
    use kotoha_stt_sherpa::build_native_config;

    let mut cfg = sample_zipformer_config();
    cfg.device = ComputeDevice::Metal;
    let hw_snap = HotwordsSnapshot {
        generation: 1,
        decoding_method: SttDecodingMethod::ModifiedBeamSearch,
        hotwords_score: 2.25,
        phrases: vec!["M W D".to_owned()],
        formatted_text: "M W D\n".to_owned(),
        file_path: None,
    };
    let native = build_native_config(&cfg, &hw_snap);
    assert_eq!(native.feat_config.sample_rate, 16_000);
    assert_eq!(native.model_config.provider.as_deref(), Some("coreml"));
    assert_eq!(
        native.model_config.transducer.encoder.as_deref(),
        Some("/models/encoder.int8.onnx")
    );
    assert_eq!(native.model_config.modeling_unit.as_deref(), Some("bpe"));
    assert_eq!(
        native.decoding_method.as_deref(),
        Some("modified_beam_search")
    );
    assert!((native.hotwords_score - 2.25).abs() < f32::EPSILON);
    assert_eq!(native.hotwords_buf.as_deref(), Some(b"M W D\n".as_slice()));
    assert!(native.enable_endpoint);

    let paraformer = sample_paraformer_config();
    let native_p = build_native_config(&paraformer, &hw_snap);
    assert_eq!(
        native_p.model_config.paraformer.encoder.as_deref(),
        Some("/models/paraformer-encoder.onnx")
    );
    assert_eq!(native_p.decoding_method.as_deref(), Some("greedy_search"));
    assert!(native_p.hotwords_buf.is_none());
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
