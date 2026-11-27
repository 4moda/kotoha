//! Real-HTTP integration tests for `kotoha-setup` model downloader (`src/model_downloader.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]

mod common;

use std::fs;
use std::path::Path;
use std::time::Duration;

use common::{sha256_hex, unique_temp_dir, LocalDownloadServer, ScriptedResponse};
use kotoha_setup::{
    part_path_for, DownloadPhase, DownloadProgress, ModelDownloader, ModelDownloaderConfig,
    SetupError, PENDING_RELEASE_MANIFEST_SHA256,
};

#[tokio::test]
async fn downloads_streams_progress_verifies_sha256_and_renames_atomically() {
    let chunk1 = b"KOTOHA_MODEL_HEADER_".to_vec();
    let chunk2 = b"WEIGHTS_PAYLOAD_0123456789".to_vec();
    let mut full_bytes = Vec::new();
    full_bytes.extend_from_slice(&chunk1);
    full_bytes.extend_from_slice(&chunk2);
    let expected_sha = sha256_hex(&full_bytes);

    let server = LocalDownloadServer::start(move |req| {
        if req.target == "/models/fugumt.bin" {
            ScriptedResponse::ok_chunked(vec![chunk1.clone(), chunk2.clone()])
        } else {
            ScriptedResponse::status("404 Not Found", b"not found")
        }
    })
    .await;

    let root = unique_temp_dir("kotoha_dl_pass");
    let _ = fs::remove_dir_all(&root);
    let dest_path = root.join("fugumt/model.bin");

    let downloader = ModelDownloader::new();
    assert!(!downloader.config().allow_unverified_in_dev);

    let mut events: Vec<DownloadProgress> = Vec::new();
    let artifact = downloader
        .download_file_for_model(
            "fugumt-en-ja-int8",
            &format!("{}/models/fugumt.bin", server.base_url),
            &dest_path,
            &expected_sha.to_uppercase(),
            |progress| events.push(progress),
        )
        .await
        .expect("valid download should succeed");

    assert_eq!(artifact.path, dest_path);
    assert_eq!(artifact.bytes_downloaded, full_bytes.len() as u64);
    assert_eq!(artifact.sha256, expected_sha);
    assert!(artifact.verified);
    assert_eq!(fs::read(&dest_path).unwrap(), full_bytes);
    assert!(
        !part_path_for(&dest_path).exists(),
        ".part temporary file must be moved away on completion"
    );

    // Verify lifecycle phases: Connecting -> Downloading... -> Verifying -> Completed
    assert!(
        events.len() >= 4,
        "expected at least 4 progress events, got {events:?}"
    );
    assert_eq!(events.first().unwrap().phase, DownloadPhase::Connecting);
    assert_eq!(events.first().unwrap().model_id, "fugumt-en-ja-int8");
    assert!(events.iter().any(|p| p.phase == DownloadPhase::Downloading
        && p.bytes_downloaded == full_bytes.len() as u64
        && p.total_bytes == Some(full_bytes.len() as u64)));
    assert!(events.iter().any(|p| p.phase == DownloadPhase::Verifying));
    let last = events.last().unwrap();
    assert_eq!(last.phase, DownloadPhase::Completed);
    assert_eq!(last.bytes_downloaded, full_bytes.len() as u64);

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn deletes_part_file_on_checksum_mismatch_or_http_404_error() {
    let payload = b"CORRUPTED_OR_TAMPERED_WEIGHTS".to_vec();
    let wrong_sha = "f".repeat(64);
    let actual_sha = sha256_hex(&payload);

    let server = LocalDownloadServer::start(move |req| match req.target.as_str() {
        "/models/tampered.bin" => ScriptedResponse::ok_bytes(payload.clone()),
        "/models/missing.bin" => ScriptedResponse::status("404 Not Found", b"missing"),
        "/redir/1" => ScriptedResponse::redirect_302("/redir/2".to_owned()),
        "/redir/2" => ScriptedResponse::redirect_302("/redir/3".to_owned()),
        "/redir/3" => ScriptedResponse::redirect_302("/redir/4".to_owned()),
        "/redir/4" => ScriptedResponse::redirect_302("/redir/5".to_owned()),
        "/redir/5" => ScriptedResponse::redirect_302("/redir/6".to_owned()),
        "/redir/6" => ScriptedResponse::redirect_302("/models/tampered.bin".to_owned()),
        _ => ScriptedResponse::status("404 Not Found", b"unknown"),
    })
    .await;

    let root = unique_temp_dir("kotoha_dl_fail");
    let _ = fs::remove_dir_all(&root);
    let downloader = ModelDownloader::new();

    // 1. Checksum mismatch removes .part and never creates final dest_path
    let mismatch_dest = root.join("mismatch/model.bin");
    let mismatch_err = downloader
        .download_file(
            &format!("{}/models/tampered.bin", server.base_url),
            &mismatch_dest,
            &wrong_sha,
            |_| {},
        )
        .await
        .expect_err("SHA-256 mismatch must fail");

    assert!(
        matches!(
            mismatch_err,
            SetupError::ChecksumMismatch {
                ref path,
                ref expected,
                ref actual,
            } if path == &mismatch_dest && expected == &wrong_sha && actual == &actual_sha
        ),
        "expected ChecksumMismatch, got {mismatch_err:?}"
    );
    assert!(!mismatch_dest.exists());
    assert!(!part_path_for(&mismatch_dest).exists());

    // 2. HTTP 404 returns Network error and leaves no .part or final file
    let not_found_dest = root.join("missing/model.bin");
    let err_404 = downloader
        .download_file(
            &format!("{}/models/missing.bin", server.base_url),
            &not_found_dest,
            &actual_sha,
            |_| {},
        )
        .await
        .expect_err("HTTP 404 must fail");
    assert!(matches!(err_404, SetupError::Network(ref m) if m.contains("404")));
    assert!(!not_found_dest.exists());
    assert!(!part_path_for(&not_found_dest).exists());

    // 3. Exceeding max 5 redirects fails cleanly
    let redir_dest = root.join("redir/model.bin");
    let redir_err = downloader
        .download_file(
            &format!("{}/redir/1", server.base_url),
            &redir_dest,
            &actual_sha,
            |_| {},
        )
        .await
        .expect_err("more than 5 redirects must fail");
    assert!(matches!(redir_err, SetupError::Network(_)));

    // 4. pending-release-manifest is rejected by default (fail-closed)
    let pending_dest = root.join("pending/model.bin");
    let pending_err = downloader
        .download_file(
            &format!("{}/models/tampered.bin", server.base_url),
            &pending_dest,
            PENDING_RELEASE_MANIFEST_SHA256,
            |_| {},
        )
        .await
        .expect_err("pending-release-manifest must be rejected by default");
    assert!(matches!(pending_err, SetupError::InvalidConfig(_)));

    // 5. Explicit allow_unverified_in_dev = true permits pending-release-manifest
    let dev_downloader = ModelDownloader::with_config(ModelDownloaderConfig {
        timeout: Duration::from_secs(30),
        max_redirects: 5,
        allow_unverified_in_dev: true,
    });
    let dev_artifact = dev_downloader
        .download_file(
            &format!("{}/models/tampered.bin", server.base_url),
            &pending_dest,
            PENDING_RELEASE_MANIFEST_SHA256,
            |_| {},
        )
        .await
        .expect("explicit allow_unverified_in_dev should allow pending manifest");
    assert!(!dev_artifact.verified);
    assert_eq!(dev_artifact.sha256, actual_sha);

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn rejects_insecure_non_loopback_http_ssrf_and_downgrade_redirects() {
    let valid_sha = "a".repeat(64);
    let root = unique_temp_dir("kotoha_dl_sec");
    let _ = fs::remove_dir_all(&root);
    let downloader = ModelDownloader::new();

    // Non-loopback plain http:// rejected
    let non_loopback_err = downloader
        .download_file(
            "http://example.com/model.bin",
            &root.join("a.bin"),
            &valid_sha,
            |_| {},
        )
        .await
        .expect_err("plain http on non-loopback host must be rejected");
    assert!(matches!(non_loopback_err, SetupError::SecurityViolation(_)));

    // Link-local / cloud metadata / private / unspecified IPs rejected even on https://
    for bad_url in [
        "http://169.254.169.254/latest/meta-data",
        "https://169.254.169.254/model.bin",
        "https://[fe80::1]/model.bin",
        "https://metadata.google.internal/computeMetadata/v1/",
        "https://10.0.0.1/model.bin",
        "https://172.16.0.1/model.bin",
        "https://172.31.255.254/model.bin",
        "https://192.168.1.10/model.bin",
        "https://0.0.0.0/model.bin",
        "https://[fc00::1]/model.bin",
        "https://[fd12:3456::1]/model.bin",
        "https://[::]/model.bin",
        "https://user:pass@huggingface.co/model.bin",
        "ftp://huggingface.co/model.bin",
    ] {
        let err = downloader
            .download_file(bad_url, &root.join("ssrf.bin"), &valid_sha, |_| {})
            .await
            .expect_err("insecure or SSRF URL must be rejected");
        assert!(
            matches!(err, SetupError::SecurityViolation(_)),
            "URL {bad_url} should fail with SecurityViolation, got {err:?}"
        );
    }

    // Loopback URLs remain permitted for local testing
    assert!(ModelDownloader::validate_download_url("http://127.0.0.1:8080/model.bin").is_ok());
    assert!(ModelDownloader::validate_download_url("http://[::1]:8080/model.bin").is_ok());
    assert!(ModelDownloader::validate_download_url("http://localhost:8080/model.bin").is_ok());

    // Redirect from loopback server to link-local metadata IP is blocked before connecting
    let ssrf_server = LocalDownloadServer::start(|_| {
        ScriptedResponse::redirect_302("http://169.254.169.254/latest/meta-data".to_owned())
    })
    .await;
    let redir_ssrf_err = downloader
        .download_file(
            &format!("{}/model.bin", ssrf_server.base_url),
            &root.join("redir_ssrf.bin"),
            &valid_sha,
            |_| {},
        )
        .await
        .expect_err("redirect to metadata IP must be blocked");
    assert!(matches!(
        redir_ssrf_err,
        SetupError::SecurityViolation(_) | SetupError::Network(_)
    ));

    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[tokio::test]
async fn does_not_follow_preexisting_part_symlink_during_download() {
    use std::os::unix::fs::symlink;

    let payload = b"SAFE_DOWNLOAD_BYTES".to_vec();
    let expected_sha = sha256_hex(&payload);
    let server =
        LocalDownloadServer::start(move |_| ScriptedResponse::ok_bytes(payload.clone())).await;

    let root = unique_temp_dir("kotoha_dl_symlink");
    let _ = fs::remove_dir_all(&root);
    let model_dir = root.join("model");
    fs::create_dir_all(&model_dir).unwrap();

    let victim_file = root.join("victim.txt");
    fs::write(&victim_file, b"untouched").unwrap();

    let dest_path = model_dir.join("model.bin");
    let predictable_part = part_path_for(&dest_path);
    symlink(&victim_file, &predictable_part).unwrap();

    let downloader = ModelDownloader::new();
    let artifact = downloader
        .download_file(
            &format!("{}/model.bin", server.base_url),
            &dest_path,
            &expected_sha,
            |_| {},
        )
        .await
        .expect("unique create_new .part file must succeed without touching symlink");

    assert_eq!(artifact.path, dest_path);
    assert_eq!(fs::read(&victim_file).unwrap(), b"untouched");
    assert!(!has_leftover_part_files(&model_dir));

    let _ = fs::remove_dir_all(&root);
}

fn has_leftover_part_files(dir: &Path) -> bool {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            let path = entry.path();
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("part"))
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name != "model.bin.part")
        })
}
