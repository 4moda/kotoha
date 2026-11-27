//! Unit tests for `kotoha-translate-local` configuration, device resolution, and SHA-256 checks (`src/config.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;

use common::{create_model_dir, unique_temp_dir};
use kotoha_core::domain::{default_model_catalog, ComputeDevice, CoreError};
use kotoha_translate_local::{
    resolve_compute_device_with_caps, validate_model_dir_layout, validate_model_id,
    verify_file_sha256, HardwareCapabilities, ResolvedDevice,
};

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
