//! Comprehensive unit and integration tests for `kotoha-translate-local` and
//! `CompositeTranslatorFactory`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use kotoha_core::application::TranslatorManager;
use kotoha_core::domain::{
    default_model_catalog, ComputeDevice, CoreError, LanguageCode, ModelBackend, ModelProfile,
    SpeakerKey, TranslateContext, TranslationProviderConfig, TranslationRequest, TranslatorFactory,
    UtteranceId,
};
use kotoha_translate_http::HttpTranslatorFactory;
use kotoha_translate_local::{
    compute_file_sha256_hex, detect_model_family, resolve_compute_device_with_caps, to_flores200,
    validate_model_dir_layout, validate_model_id, verify_file_sha256, CompositeTranslatorFactory,
    HardwareCapabilities, LocalNmtBackend, LocalNmtBackendLoader, LocalNmtConfig, LocalNmtFamily,
    LocalNmtTranslator, LocalNmtTranslatorFactory, LocalTranslateParams, ResolvedDevice,
};

fn unique_temp_dir(prefix: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}_{}_{seq}", std::process::id()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedCall {
    model_id: String,
    device: ResolvedDevice,
    threads: usize,
    text: String,
    source_prefix: Option<String>,
    target_prefix: Option<String>,
}

struct MockLocalBackend {
    model_id: String,
    device: ResolvedDevice,
    threads: usize,
    calls: Arc<Mutex<Vec<RecordedCall>>>,
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

struct MockLocalLoader {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
}

impl MockLocalLoader {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().expect("lock").clone()
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

fn create_model_dir(root: &Path, model_id: &str, weights: &[u8], tokenizer_kind: &str) -> String {
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

fn sample_request(text: &str, src: &str, tgt: &str) -> TranslationRequest {
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

#[test]
fn compute_device_resolution_handles_auto_cuda_metal_and_cpu() {
    let catalog = default_model_catalog();
    let fugu = catalog
        .iter()
        .find(|p| p.model_id == "fugumt-en-ja-int8")
        .unwrap();
    let nllb_600m = catalog
        .iter()
        .find(|p| p.model_id == "nllb-200-distilled-600m-int8")
        .unwrap();
    let nllb_1_3b = catalog
        .iter()
        .find(|p| p.model_id == "nllb-200-1.3b-fp16")
        .unwrap();

    let cpu_only = HardwareCapabilities {
        cuda_available: false,
        metal_available: false,
    };
    let cuda_host = HardwareCapabilities {
        cuda_available: true,
        metal_available: false,
    };
    let metal_host = HardwareCapabilities {
        cuda_available: false,
        metal_available: true,
    };

    // Auto respects profile recommended_device & host capabilities.
    // Because ModelBackend::CTranslate2 runs on CPU on macOS (Accelerate/Ruy), Auto resolves to
    // Cpu even when metal_available=true (Minor 15).
    assert_eq!(
        resolve_compute_device_with_caps(ComputeDevice::Auto, fugu, cuda_host),
        ResolvedDevice::Cpu
    );
    assert_eq!(
        resolve_compute_device_with_caps(ComputeDevice::Auto, nllb_600m, cpu_only),
        ResolvedDevice::Cpu
    );
    assert_eq!(
        resolve_compute_device_with_caps(ComputeDevice::Auto, nllb_600m, cuda_host),
        ResolvedDevice::Cuda
    );
    assert_eq!(
        resolve_compute_device_with_caps(ComputeDevice::Auto, nllb_600m, metal_host),
        ResolvedDevice::Cpu
    );
    assert_eq!(
        resolve_compute_device_with_caps(ComputeDevice::Auto, nllb_1_3b, cpu_only),
        ResolvedDevice::Cpu
    );
    assert_eq!(
        resolve_compute_device_with_caps(ComputeDevice::Auto, nllb_1_3b, cuda_host),
        ResolvedDevice::Cuda
    );

    // Explicit device selections pass through cleanly
    assert_eq!(
        resolve_compute_device_with_caps(ComputeDevice::Cuda, fugu, cpu_only),
        ResolvedDevice::Cuda
    );
    assert_eq!(
        resolve_compute_device_with_caps(ComputeDevice::Metal, fugu, cpu_only),
        ResolvedDevice::Metal
    );
    assert_eq!(
        resolve_compute_device_with_caps(ComputeDevice::Cpu, nllb_1_3b, cuda_host),
        ResolvedDevice::Cpu
    );
}

#[test]
fn flores200_mapping_covers_iso_and_bcp47_codes() {
    assert_eq!(
        to_flores200(&LanguageCode::new("en").unwrap()).unwrap(),
        "eng_Latn"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("en-US").unwrap()).unwrap(),
        "eng_Latn"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("ja").unwrap()).unwrap(),
        "jpn_Jpan"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("zh").unwrap()).unwrap(),
        "zho_Hans"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("zh-TW").unwrap()).unwrap(),
        "zho_Hant"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("zh-Hant-TW").unwrap()).unwrap(),
        "zho_Hant"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("zh-Hans-CN").unwrap()).unwrap(),
        "zho_Hans"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("ko").unwrap()).unwrap(),
        "kor_Hang"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("fr").unwrap()).unwrap(),
        "fra_Latn"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("de").unwrap()).unwrap(),
        "deu_Latn"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("es").unwrap()).unwrap(),
        "spa_Latn"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("ru").unwrap()).unwrap(),
        "rus_Cyrl"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("pt-BR").unwrap()).unwrap(),
        "por_Latn"
    );
    assert_eq!(
        to_flores200(&LanguageCode::new("it").unwrap()).unwrap(),
        "ita_Latn"
    );

    assert!(to_flores200(&LanguageCode::new("auto").unwrap()).is_err());
    assert!(to_flores200(&LanguageCode::new("zz").unwrap()).is_err());
}

#[test]
fn validate_model_id_rejects_path_traversal_and_accepts_normal_names() {
    assert_eq!(
        validate_model_id("  fugumt-en-ja-int8  ").unwrap(),
        "fugumt-en-ja-int8"
    );
    for bad in [
        "",
        "   ",
        "../escape",
        "/etc",
        "foo/bar",
        r"foo\bar",
        "..",
        ".",
        "C:drive",
        "bad\0id",
    ] {
        assert!(
            matches!(validate_model_id(bad), Err(CoreError::NotConfigured(_))),
            "model_id '{bad}' must be rejected"
        );
    }
}

#[test]
fn sha256_verification_and_model_layout_checks() {
    let root = unique_temp_dir("kotoha_nmt_sha");
    let _ = fs::remove_dir_all(&root);

    let sha_fugu = create_model_dir(&root, "fugumt-en-ja-int8", b"fugu-weights-v1", "spm");
    let model_dir = root.join("fugumt-en-ja-int8");
    let model_bin = validate_model_dir_layout(&model_dir).expect("valid spm layout");

    // Exact match (case-insensitive) passes
    assert!(verify_file_sha256(&model_bin, &sha_fugu.to_uppercase()).is_ok());

    // sentencepiece.bpe.model tokenizer layout passes (Minor 12)
    let _ = create_model_dir(&root, "spm-bpe-model", b"weights", "spm_bpe");
    assert!(validate_model_dir_layout(&root.join("spm-bpe-model")).is_ok());

    // Tampered SHA fails with CoreError::Translation
    let bad_sha = "0".repeat(64);
    let err = verify_file_sha256(&model_bin, &bad_sha).expect_err("sha mismatch");
    assert!(matches!(err, CoreError::Translation(ref m) if m.contains("SHA-256")));

    // Malformed digest string fails with CoreError::NotConfigured
    let malformed = verify_file_sha256(&model_bin, "pending-release-manifest")
        .expect_err("non-hex digest rejected");
    assert!(matches!(malformed, CoreError::NotConfigured(_)));

    // Directory missing tokenizer fails layout check
    let _ = create_model_dir(&root, "no-tokenizer", b"weights", "none");
    let missing_tok =
        validate_model_dir_layout(&root.join("no-tokenizer")).expect_err("missing tokenizer");
    assert!(matches!(missing_tok, CoreError::NotConfigured(_)));

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn model_granular_switching_flores_prefixes_and_placeholder_guard() {
    let root = unique_temp_dir("kotoha_nmt_models");
    let _ = fs::remove_dir_all(&root);

    let fugu_sha = create_model_dir(&root, "fugumt-en-ja-int8", b"fugu-ct2-weights", "spm");
    let nllb_sha = create_model_dir(
        &root,
        "nllb-200-distilled-600m-int8",
        b"nllb-ct2-weights",
        "hf",
    );

    // Override catalog profiles with real test SHA-256 digests
    let custom_profiles: Vec<ModelProfile> = default_model_catalog()
        .into_iter()
        .map(|mut p| {
            if p.model_id == "fugumt-en-ja-int8" {
                p.sha256 = fugu_sha.clone();
            } else if p.model_id == "nllb-200-distilled-600m-int8" {
                p.sha256 = nllb_sha.clone();
            }
            p
        })
        .collect();

    assert_eq!(
        detect_model_family(&custom_profiles[0]),
        LocalNmtFamily::MarianOrOpus
    );
    assert_eq!(
        detect_model_family(&custom_profiles[2]),
        LocalNmtFamily::Nllb200
    );

    let loader = Arc::new(MockLocalLoader::new());
    let local_factory = LocalNmtTranslatorFactory::with_loader(&root, loader.clone())
        .with_custom_profiles(custom_profiles.clone())
        .with_capabilities(HardwareCapabilities {
            cuda_available: true,
            metal_available: false,
        });

    let composite: Arc<dyn TranslatorFactory> = Arc::new(CompositeTranslatorFactory::new(
        local_factory,
        HttpTranslatorFactory::new(),
    ));
    let mgr = TranslatorManager::from_config(
        TranslationProviderConfig::LocalModel {
            model_id: "fugumt-en-ja-int8".to_owned(),
            device: ComputeDevice::Auto,
            threads: Some(4),
        },
        composite.as_ref(),
    )
    .expect("initialize with fugumt");

    assert_eq!(mgr.current_provider_id().await, "local:fugumt-en-ja-int8");
    let res1 = mgr
        .translate(sample_request("Engage M W D on Ferox now", "en", "ja"))
        .await
        .expect("fugu translate");
    assert_eq!(
        res1.translated_text,
        "[fugumt-en-ja-int8] Engage MWD on フェロックス now"
    );

    // Bilingual Marian/OPUS-MT rejects unsupported pair and 'auto' (Minor 13)
    let unsupported_pair = mgr
        .translate(sample_request("Bonjour", "fr", "de"))
        .await
        .expect_err("fugu en->ja must reject fr->de");
    assert!(matches!(
        unsupported_pair,
        CoreError::InvalidLanguageCode(_)
    ));
    let auto_pair = mgr
        .translate(sample_request("Hello", "auto", "ja"))
        .await
        .expect_err("fugu en->ja must reject auto");
    assert!(matches!(auto_pair, CoreError::InvalidLanguageCode(_)));

    // Hot-swap model profile to NLLB-200 distilled 600M -> uses CUDA + Flores-200 prefix tags
    mgr.switch_provider(
        TranslationProviderConfig::LocalModel {
            model_id: "nllb-200-distilled-600m-int8".to_owned(),
            device: ComputeDevice::Auto,
            threads: Some(2),
        },
        Arc::clone(&composite),
    )
    .await
    .expect("switch to nllb-200");
    assert_eq!(
        mgr.current_provider_id().await,
        "local:nllb-200-distilled-600m-int8"
    );

    let res2 = mgr
        .translate(sample_request("Engage M W D on Ferox now", "en", "ja"))
        .await
        .expect("nllb translate");
    assert_eq!(
        res2.translated_text,
        "[nllb-200-distilled-600m-int8:eng_Latn->jpn_Jpan] Engage MWD on フェロックス now"
    );

    let calls = loader.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].model_id, "fugumt-en-ja-int8");
    assert_eq!(calls[0].device, ResolvedDevice::Cpu);
    assert_eq!(calls[0].threads, 4);
    assert_eq!(calls[0].source_prefix, None);
    assert_eq!(calls[0].target_prefix, None);
    assert_eq!(calls[0].text, "Engage __TERM_0__ on __TERM_1__ now");

    assert_eq!(calls[1].model_id, "nllb-200-distilled-600m-int8");
    assert_eq!(calls[1].device, ResolvedDevice::Cuda);
    assert_eq!(calls[1].threads, 2);
    assert_eq!(calls[1].source_prefix.as_deref(), Some("eng_Latn"));
    assert_eq!(calls[1].target_prefix.as_deref(), Some("jpn_Jpan"));

    // Verify LocalNmtTranslator direct construction rejects corrupted weights
    fs::write(
        root.join("fugumt-en-ja-int8").join("model.bin"),
        b"corrupted-bytes",
    )
    .unwrap();
    let cfg = LocalNmtConfig::new(&root, "fugumt-en-ja-int8", ComputeDevice::Cpu);
    let corrupted_err = LocalNmtTranslator::with_backend(
        &cfg,
        &custom_profiles,
        HardwareCapabilities::default(),
        Arc::new(MockLocalBackend {
            model_id: "fugumt-en-ja-int8".to_owned(),
            device: ResolvedDevice::Cpu,
            threads: 0,
            calls: Arc::new(Mutex::new(Vec::new())),
        }),
    )
    .expect_err("corrupted weights must fail SHA-256 check");
    assert!(matches!(corrupted_err, CoreError::Translation(_)));

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn composite_factory_switches_across_all_six_providers_in_translator_manager() {
    let root = unique_temp_dir("kotoha_composite_all_six");
    let _ = fs::remove_dir_all(&root);
    let sha = create_model_dir(&root, "opus-mt-en-ja-int8", b"opus-weights", "hf");

    let mut custom = default_model_catalog();
    for p in &mut custom {
        if p.model_id == "opus-mt-en-ja-int8" {
            p.sha256 = sha.clone();
            p.backend = ModelBackend::CTranslate2;
        }
    }

    let loader = Arc::new(MockLocalLoader::new());
    let local_factory =
        LocalNmtTranslatorFactory::with_loader(&root, loader).with_custom_profiles(custom);
    let http_factory = HttpTranslatorFactory::new()
        .with_deepl_base_url("http://127.0.0.1:9999")
        .with_google_cloud_base_url("http://127.0.0.1:9999");
    let composite: Arc<dyn TranslatorFactory> =
        Arc::new(CompositeTranslatorFactory::new(local_factory, http_factory));

    let mgr = TranslatorManager::from_config(
        TranslationProviderConfig::Mock {
            prefix: "[mock] ".to_owned(),
        },
        composite.as_ref(),
    )
    .expect("1. Mock");
    assert_eq!(mgr.current_provider_id().await, "mock");

    mgr.switch_provider(
        TranslationProviderConfig::LocalModel {
            model_id: "opus-mt-en-ja-int8".to_owned(),
            device: ComputeDevice::Cpu,
            threads: Some(2),
        },
        Arc::clone(&composite),
    )
    .await
    .expect("2. LocalModel");
    assert_eq!(mgr.current_provider_id().await, "local:opus-mt-en-ja-int8");

    mgr.switch_provider(
        TranslationProviderConfig::OpenAiCompatible {
            base_url: "http://127.0.0.1:11434/v1".to_owned(),
            model: "qwen2.5:7b".to_owned(),
            api_key: None,
            timeout_ms: 5_000,
        },
        Arc::clone(&composite),
    )
    .await
    .expect("3. OpenAiCompatible");
    assert_eq!(mgr.current_provider_id().await, "openai-compatible");

    mgr.switch_provider(
        TranslationProviderConfig::DeepL {
            api_key: "test-deepl-key".to_owned(),
            use_free_endpoint: true,
            timeout_ms: 5_000,
        },
        Arc::clone(&composite),
    )
    .await
    .expect("4. DeepL");
    assert_eq!(mgr.current_provider_id().await, "deepl");

    mgr.switch_provider(
        TranslationProviderConfig::GoogleCloud {
            api_key: "test-gc-key".to_owned(),
            project_id: Some("proj".to_owned()),
            timeout_ms: 5_000,
        },
        Arc::clone(&composite),
    )
    .await
    .expect("5. GoogleCloud");
    assert_eq!(mgr.current_provider_id().await, "google-cloud");

    mgr.switch_provider(
        TranslationProviderConfig::GasWebhook {
            endpoint_url: "http://127.0.0.1:8080/exec".to_owned(),
            bearer_token: Some("tok".to_owned()),
            timeout_ms: 5_000,
        },
        Arc::clone(&composite),
    )
    .await
    .expect("6. GasWebhook");
    assert_eq!(mgr.current_provider_id().await, "gas-webhook");

    let _ = fs::remove_dir_all(&root);
}
