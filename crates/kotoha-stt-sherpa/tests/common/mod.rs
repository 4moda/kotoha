//! Shared WAV fixture helpers, sample configs, and mock recognizer backend for `kotoha-stt-sherpa` tests.

#![allow(
    dead_code,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_precision_loss
)]

use std::collections::VecDeque;
use std::f32::consts::PI;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use kotoha_core::domain::{CoreError, SttDecodingMethod};
use kotoha_core::infrastructure::AudioPreprocessor;
use kotoha_stt_sherpa::{
    HotwordsSnapshot, RecognizerBackend, RecognizerHypothesis, RecognizerStreamBackend,
    SherpaModelFamily, SherpaModelingUnit, SherpaSttConfig,
};

pub fn unique_temp_dir(prefix: &str) -> PathBuf {
    static NEXT_DIR_ID: AtomicU64 = AtomicU64::new(1);
    let seq = NEXT_DIR_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}_{}_{seq}", std::process::id()))
}

// ---------------------------------------------------------------------------
// Pure-Rust 16 kHz Mono PCM-16 WAV Test Fixture Encoder / Decoder
// ---------------------------------------------------------------------------

pub fn encode_wav_pcm16(sample_rate: u32, samples: &[f32]) -> Vec<u8> {
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

pub fn decode_wav_pcm16(bytes: &[u8]) -> (u32, Vec<f32>) {
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

pub fn generate_speech_burst_wav_fixture() -> (u32, Vec<f32>) {
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
pub struct RecordedStreamCreation {
    pub generation: u64,
    pub decoding_method: SttDecodingMethod,
    pub hotwords_score: f32,
    pub formatted_text: String,
}

#[derive(Debug, Default)]
pub struct MockBackendState {
    pub created_streams: Vec<RecordedStreamCreation>,
    pub hotword_reload_calls: Vec<(SttDecodingMethod, f32, u64)>,
    pub live_stream_count: AtomicUsize,
}

pub struct MockRecognizerBackend {
    state: Arc<StdMutex<MockBackendState>>,
    silence_rms_threshold: f32,
    min_active_chunks_for_endpoint: usize,
    use_scripted_outcomes: bool,
    fail_hotword_reload: AtomicBool,
    empty_result_on_endpoint_or_flush: AtomicBool,
}

impl MockRecognizerBackend {
    pub fn new(silence_rms_threshold: f32, min_active_chunks_for_endpoint: usize) -> Self {
        Self {
            state: Arc::new(StdMutex::new(MockBackendState::default())),
            silence_rms_threshold,
            min_active_chunks_for_endpoint,
            use_scripted_outcomes: false,
            fail_hotword_reload: AtomicBool::new(false),
            empty_result_on_endpoint_or_flush: AtomicBool::new(false),
        }
    }

    pub fn scripted(silence_rms_threshold: f32) -> Self {
        Self {
            state: Arc::new(StdMutex::new(MockBackendState::default())),
            silence_rms_threshold,
            min_active_chunks_for_endpoint: usize::MAX,
            use_scripted_outcomes: true,
            fail_hotword_reload: AtomicBool::new(false),
            empty_result_on_endpoint_or_flush: AtomicBool::new(false),
        }
    }

    pub fn set_fail_hotword_reload(&self, fail: bool) {
        self.fail_hotword_reload.store(fail, Ordering::SeqCst);
    }

    pub fn set_empty_result_on_endpoint_or_flush(&self, empty: bool) {
        self.empty_result_on_endpoint_or_flush
            .store(empty, Ordering::SeqCst);
    }

    pub fn recorded_streams(&self) -> Vec<RecordedStreamCreation> {
        self.state.lock().unwrap().created_streams.clone()
    }

    pub fn hotword_reload_calls(&self) -> Vec<(SttDecodingMethod, f32, u64)> {
        self.state.lock().unwrap().hotword_reload_calls.clone()
    }

    pub fn live_stream_count(&self) -> usize {
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

pub struct MockRecognizerStream {
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

pub fn sample_zipformer_config() -> SherpaSttConfig {
    SherpaSttConfig::new(SherpaModelFamily::ZipformerTransducer {
        encoder: PathBuf::from("/models/encoder.int8.onnx"),
        decoder: PathBuf::from("/models/decoder.onnx"),
        joiner: PathBuf::from("/models/joiner.int8.onnx"),
        tokens: PathBuf::from("/models/tokens.txt"),
        bpe_vocab: Some(PathBuf::from("/models/bpe.vocab")),
        modeling_unit: Some(SherpaModelingUnit::Bpe),
    })
}

pub fn sample_paraformer_config() -> SherpaSttConfig {
    SherpaSttConfig::new(SherpaModelFamily::Paraformer {
        encoder: PathBuf::from("/models/paraformer-encoder.onnx"),
        decoder: PathBuf::from("/models/paraformer-decoder.onnx"),
        tokens: PathBuf::from("/models/tokens.txt"),
    })
}
