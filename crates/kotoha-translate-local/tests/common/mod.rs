//! Shared mock local NMT backend, temporary model directory, and request fixtures for `kotoha-translate-local`.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use kotoha_core::domain::{
    CoreError, LanguageCode, SpeakerKey, TranslateContext, TranslationRequest, UtteranceId,
};
use kotoha_translate_local::{
    compute_file_sha256_hex, LocalNmtBackend, LocalNmtBackendLoader, LocalTranslateParams,
    ResolvedDevice,
};

pub fn unique_temp_dir(prefix: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}_{}_{seq}", std::process::id()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCall {
    pub model_id: String,
    pub device: ResolvedDevice,
    pub threads: usize,
    pub text: String,
    pub source_prefix: Option<String>,
    pub target_prefix: Option<String>,
}

pub struct MockLocalBackend {
    pub model_id: String,
    pub device: ResolvedDevice,
    pub threads: usize,
    pub calls: Arc<Mutex<Vec<RecordedCall>>>,
}

impl LocalNmtBackend for MockLocalBackend {
    fn translate(&self, params: LocalTranslateParams<'_>) -> Result<String, CoreError> {
        self.calls.lock().expect("lock").push(RecordedCall {
            model_id: self.model_id.clone(),
            device: self.device,
            threads: self.threads,
            text: params.text.to_owned(),
            source_prefix: params.source_prefix.map(str::to_owned),
            target_prefix: params.target_prefix.map(str::to_owned),
        });
        let prefix = match (params.source_prefix, params.target_prefix) {
            (Some(s), Some(t)) => format!("[{}:{s}->{t}] ", self.model_id),
            _ => format!("[{}] ", self.model_id),
        };
        Ok(format!("{prefix}{}", params.text))
    }
}

pub struct MockLocalLoader {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
}

impl MockLocalLoader {
    pub fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().expect("lock").clone()
    }
}

impl Default for MockLocalLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalNmtBackendLoader for MockLocalLoader {
    fn load(
        &self,
        model_id: &str,
        _model_dir: &Path,
        device: ResolvedDevice,
        threads: usize,
    ) -> Result<Arc<dyn LocalNmtBackend>, CoreError> {
        Ok(Arc::new(MockLocalBackend {
            model_id: model_id.to_owned(),
            device,
            threads,
            calls: Arc::clone(&self.calls),
        }))
    }
}

pub fn create_model_dir(
    root: &Path,
    model_id: &str,
    weights: &[u8],
    tokenizer_kind: &str,
) -> String {
    let dir = root.join(model_id);
    fs::create_dir_all(&dir).expect("create model dir");
    let model_bin = dir.join("model.bin");
    fs::write(&model_bin, weights).expect("write model.bin");
    match tokenizer_kind {
        "hf" => fs::write(dir.join("tokenizer.json"), b"{}").expect("write tokenizer.json"),
        "spm" => {
            fs::write(dir.join("source.spm"), b"spm").expect("write source.spm");
            fs::write(dir.join("target.spm"), b"spm").expect("write target.spm");
        }
        "spm_bpe" => {
            fs::write(dir.join("sentencepiece.bpe.model"), b"spm").expect("write spm.bpe.model");
        }
        _ => {}
    }
    compute_file_sha256_hex(&model_bin).expect("compute sha256")
}

pub fn sample_request(text: &str, src: &str, tgt: &str) -> TranslationRequest {
    TranslationRequest {
        utterance_id: UtteranceId(101),
        speaker_key: SpeakerKey::new("mumble", 9),
        speaker_name: "Bob".to_owned(),
        text: text.to_owned(),
        src_lang: LanguageCode::new(src).unwrap(),
        tgt_lang: LanguageCode::new(tgt).unwrap(),
        context: TranslateContext {
            recent_history: Vec::new(),
            glossary: BTreeMap::from([
                ("M W D".to_owned(), "MWD".to_owned()),
                ("Ferox".to_owned(), "フェロックス".to_owned()),
            ]),
        },
    }
}
