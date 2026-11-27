//! Streaming HTTPS model downloader with SSRF protection, progress reporting, and fail-closed SHA-256 verification.

use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use reqwest::{redirect, Client, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::SetupError;

/// Default maximum HTTP redirects permitted while downloading model artifacts.
pub const DEFAULT_MAX_DOWNLOAD_REDIRECTS: usize = 5;

/// Default HTTP request timeout in seconds for model downloads (`300` seconds).
pub const DEFAULT_DOWNLOAD_TIMEOUT_SECS: u64 = 300;

/// Placeholder SHA-256 sentinel used in unreleased catalogs; rejected unless explicitly allowed.
pub const PENDING_RELEASE_MANIFEST_SHA256: &str = "pending-release-manifest";

/// Phase of a model download operation emitted to progress callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadPhase {
    /// Validating URL, opening connection, and awaiting response headers.
    Connecting,
    /// Streaming response chunks into the `.part` temporary file.
    Downloading,
    /// Comparing computed SHA-256 digest and committing the atomic rename.
    Verifying,
    /// Download and verification finished successfully.
    Completed,
}

/// Progress snapshot emitted while downloading a model file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadProgress {
    /// Logical model or artifact identifier associated with the download.
    pub model_id: String,
    /// Total bytes written to `.part` so far.
    pub bytes_downloaded: u64,
    /// Expected total byte size from HTTP `Content-Length`, if provided by the server.
    pub total_bytes: Option<u64>,
    /// Current lifecycle phase of the download.
    pub phase: DownloadPhase,
}

/// Configuration for [`ModelDownloader`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDownloaderConfig {
    /// Request timeout (applied via `reqwest::ClientBuilder::timeout`).
    pub timeout: Duration,
    /// Maximum number of HTTP redirects to follow (must not exceed safe bounds; default `5`).
    pub max_redirects: usize,
    /// Explicit development override allowing [`PENDING_RELEASE_MANIFEST_SHA256`] without checksum
    /// verification. **MUST default to `false` (fail-closed)!**
    pub allow_unverified_in_dev: bool,
}

impl Default for ModelDownloaderConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(DEFAULT_DOWNLOAD_TIMEOUT_SECS),
            max_redirects: DEFAULT_MAX_DOWNLOAD_REDIRECTS,
            allow_unverified_in_dev: false,
        }
    }
}

/// Result metadata returned when [`ModelDownloader::download_file`] succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadedArtifact {
    /// Final destination path after atomic rename from `.part`.
    pub path: PathBuf,
    /// Total bytes written.
    pub bytes_downloaded: u64,
    /// Computed lowercase hex SHA-256 digest of the downloaded file.
    pub sha256: String,
    /// Whether SHA-256 was verified against an explicit 64-hex-digit digest.
    pub verified: bool,
}

/// Streaming model downloader enforcing HTTPS, SSRF protections, safe redirects, and SHA-256 checks.
#[derive(Debug, Clone)]
pub struct ModelDownloader {
    config: ModelDownloaderConfig,
}

impl Default for ModelDownloader {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelDownloader {
    /// Creates a [`ModelDownloader`] with fail-closed security defaults (`allow_unverified_in_dev = false`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: ModelDownloaderConfig::default(),
        }
    }

    /// Creates a [`ModelDownloader`] with explicit [`ModelDownloaderConfig`].
    #[must_use]
    pub const fn with_config(config: ModelDownloaderConfig) -> Self {
        Self { config }
    }

    /// Returns the downloader configuration.
    #[must_use]
    pub const fn config(&self) -> &ModelDownloaderConfig {
        &self.config
    }

    /// Validates `raw_url` against Kotoha's download security policy (HTTPS for non-loopback hosts,
    /// no embedded credentials, no private/unspecified/link-local/metadata IPs) and parses it into
    /// a [`Url`].
    pub fn validate_download_url(raw_url: &str) -> Result<Url, SetupError> {
        let trimmed = raw_url.trim();
        if trimmed.is_empty() {
            return Err(SetupError::InvalidConfig(
                "download URL must not be empty".to_owned(),
            ));
        }
        let parsed = Url::parse(trimmed)
            .map_err(|err| SetupError::InvalidConfig(format!("invalid download URL: {err}")))?;

        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(SetupError::SecurityViolation(
                "download URL must not contain embedded userinfo (user:pass@)".to_owned(),
            ));
        }

        let Some(host) = parsed.host_str() else {
            return Err(SetupError::InvalidConfig(
                "download URL must include a host".to_owned(),
            ));
        };

        if is_link_local_or_metadata_host(host) {
            return Err(SetupError::SecurityViolation(format!(
                "refusing download from link-local/metadata host '{host}'"
            )));
        }

        if is_non_loopback_private_or_unspecified_host(host) {
            return Err(SetupError::SecurityViolation(format!(
                "refusing download from private or unspecified IP host '{host}'"
            )));
        }

        match parsed.scheme() {
            "https" => Ok(parsed),
            "http" => {
                if is_loopback_host(host) {
                    Ok(parsed)
                } else {
                    Err(SetupError::SecurityViolation(format!(
                        "plain http:// download is forbidden for non-loopback host '{host}'; https:// is required"
                    )))
                }
            }
            other => Err(SetupError::SecurityViolation(format!(
                "unsupported download URL scheme '{other}'; only https:// (or loopback http://) is allowed"
            ))),
        }
    }

    /// Downloads `url` to `dest_path`, verifying `expected_sha256` before atomically renaming
    /// `<dest>.part` into `dest_path`.
    ///
    /// Uses `dest_path`'s file name as `DownloadProgress::model_id` unless
    /// [`Self::download_file_for_model`] is used directly.
    pub async fn download_file<F>(
        &self,
        url: &str,
        dest_path: &Path,
        expected_sha256: &str,
        progress_cb: F,
    ) -> Result<DownloadedArtifact, SetupError>
    where
        F: FnMut(DownloadProgress),
    {
        let fallback_id = dest_path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("model")
            .to_owned();
        self.download_file_for_model(&fallback_id, url, dest_path, expected_sha256, progress_cb)
            .await
    }

    /// Downloads `url` for `model_id` into `dest_path`, streaming chunks to `<dest>.part`,
    /// reporting [`DownloadProgress`] updates, verifying SHA-256, and atomically renaming `.part`
    /// to `dest_path` (or deleting `.part` on any error or mismatch).
    pub async fn download_file_for_model<F>(
        &self,
        model_id: &str,
        url: &str,
        dest_path: &Path,
        expected_sha256: &str,
        mut progress_cb: F,
    ) -> Result<DownloadedArtifact, SetupError>
    where
        F: FnMut(DownloadProgress),
    {
        let validated_url = Self::validate_download_url(url)?;
        let expected_digest =
            validate_expected_sha256(expected_sha256, self.config.allow_unverified_in_dev)?;

        if dest_path.as_os_str().is_empty() || dest_path.file_name().is_none() {
            return Err(SetupError::InvalidConfig(format!(
                "invalid model destination path '{}'",
                dest_path.display()
            )));
        }

        let initial_is_loopback = validated_url.host_str().is_some_and(is_loopback_host);
        let client = build_download_client(&self.config, initial_is_loopback)?;

        progress_cb(DownloadProgress {
            model_id: model_id.to_owned(),
            bytes_downloaded: 0,
            total_bytes: None,
            phase: DownloadPhase::Connecting,
        });

        let mut response = client
            .get(validated_url.clone())
            .send()
            .await
            .map_err(|err| map_download_reqwest_error(&err))?;

        let status = response.status();
        if !status.is_success() {
            let reason = status.canonical_reason().unwrap_or("HTTP Error");
            return Err(SetupError::Network(format!(
                "download failed for '{}' with HTTP {} {reason}",
                redact_url_for_error(response.url()),
                status.as_u16()
            )));
        }

        if let Some(parent) = dest_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let part_path = unique_part_path_for(dest_path);
        let mut part_guard = PartFileCleanupGuard::new(part_path.clone());
        let (bytes_downloaded, actual_sha256, verified) = stream_and_verify_download(
            model_id,
            &mut response,
            dest_path,
            &part_path,
            expected_digest.as_deref(),
            &mut progress_cb,
        )
        .await?;

        fs::rename(&part_path, dest_path)?;
        part_guard.disarm();

        progress_cb(DownloadProgress {
            model_id: model_id.to_owned(),
            bytes_downloaded,
            total_bytes: response.content_length(),
            phase: DownloadPhase::Completed,
        });

        Ok(DownloadedArtifact {
            path: dest_path.to_path_buf(),
            bytes_downloaded,
            sha256: actual_sha256,
            verified,
        })
    }
}

async fn stream_and_verify_download<F>(
    model_id: &str,
    response: &mut reqwest::Response,
    dest_path: &Path,
    part_path: &Path,
    expected_digest: Option<&str>,
    progress_cb: &mut F,
) -> Result<(u64, String, bool), SetupError>
where
    F: FnMut(DownloadProgress),
{
    let mut part_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(part_path)?;

    let total_bytes = response.content_length();
    let mut bytes_downloaded: u64 = 0;
    let mut hasher = Sha256::new();

    progress_cb(DownloadProgress {
        model_id: model_id.to_owned(),
        bytes_downloaded: 0,
        total_bytes,
        phase: DownloadPhase::Downloading,
    });

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| map_download_reqwest_error(&err))?
    {
        part_file.write_all(&chunk)?;
        hasher.update(&chunk);
        bytes_downloaded = bytes_downloaded.saturating_add(chunk.len() as u64);
        progress_cb(DownloadProgress {
            model_id: model_id.to_owned(),
            bytes_downloaded,
            total_bytes,
            phase: DownloadPhase::Downloading,
        });
    }

    part_file.sync_all()?;
    drop(part_file);

    progress_cb(DownloadProgress {
        model_id: model_id.to_owned(),
        bytes_downloaded,
        total_bytes,
        phase: DownloadPhase::Verifying,
    });

    let actual_sha256 = bytes_to_lower_hex(&hasher.finalize());
    let verified = if let Some(expected) = expected_digest {
        if actual_sha256 != expected {
            return Err(SetupError::ChecksumMismatch {
                path: dest_path.to_path_buf(),
                expected: expected.to_owned(),
                actual: actual_sha256,
            });
        }
        true
    } else {
        false
    };

    Ok((bytes_downloaded, actual_sha256, verified))
}

/// Computes the `<dest>.part` temporary path for `dest_path`.
#[must_use]
pub fn part_path_for(dest_path: &Path) -> PathBuf {
    let mut os_str = dest_path.as_os_str().to_os_string();
    os_str.push(".part");
    PathBuf::from(os_str)
}

fn unique_part_path_for(dest_path: &Path) -> PathBuf {
    static NEXT_PART_ID: AtomicU64 = AtomicU64::new(1);
    let seq = NEXT_PART_ID.fetch_add(1, Ordering::Relaxed);
    let mut os_str = dest_path.as_os_str().to_os_string();
    os_str.push(format!(".{}.{seq}.part", std::process::id()));
    PathBuf::from(os_str)
}

fn validate_expected_sha256(
    expected_sha256: &str,
    allow_unverified_in_dev: bool,
) -> Result<Option<String>, SetupError> {
    let normalized = expected_sha256.trim().to_ascii_lowercase();
    if normalized == PENDING_RELEASE_MANIFEST_SHA256 {
        if allow_unverified_in_dev {
            return Ok(None);
        }
        return Err(SetupError::InvalidConfig(format!(
            "refusing unverified download with placeholder SHA-256 '{PENDING_RELEASE_MANIFEST_SHA256}' \
             (set allow_unverified_in_dev = true only for explicit development overrides)"
        )));
    }
    if normalized.len() != 64 || !normalized.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(SetupError::InvalidConfig(format!(
            "expected_sha256 must be a 64-character hex SHA-256 digest, got '{expected_sha256}'"
        )));
    }
    Ok(Some(normalized))
}

fn build_download_client(
    config: &ModelDownloaderConfig,
    allow_loopback_targets: bool,
) -> Result<Client, SetupError> {
    let max_hops = config.max_redirects.min(DEFAULT_MAX_DOWNLOAD_REDIRECTS);
    let policy = if max_hops == 0 {
        redirect::Policy::none()
    } else {
        let hops = Arc::new(AtomicUsize::new(0));
        redirect::Policy::custom(move |attempt| {
            let count = hops.fetch_add(1, Ordering::Relaxed) + 1;
            if count > max_hops || attempt.previous().len() > max_hops {
                return attempt.error("too many redirects");
            }

            let next_url = attempt.url();
            if !next_url.username().is_empty() || next_url.password().is_some() {
                return attempt.error("redirect URL contains embedded credentials");
            }

            let Some(host) = next_url.host_str() else {
                return attempt.error("redirect URL missing host");
            };

            if is_link_local_or_metadata_host(host) {
                return attempt.error("refusing redirect to link-local/metadata host");
            }
            if !allow_loopback_targets && is_private_or_loopback_host(host) {
                return attempt.error("refusing redirect to private/loopback host");
            }

            match next_url.scheme() {
                "https" => attempt.follow(),
                "http" => {
                    let downgraded_from_https = attempt
                        .previous()
                        .iter()
                        .any(|prev| prev.scheme().eq_ignore_ascii_case("https"));
                    if downgraded_from_https {
                        return attempt.error("refusing https:// to http:// redirect downgrade");
                    }
                    if allow_loopback_targets && is_loopback_host(host) {
                        attempt.follow()
                    } else {
                        attempt.error("refusing non-https redirect to non-loopback host")
                    }
                }
                _ => attempt.error("refusing redirect to non-http(s) scheme"),
            }
        })
    };

    Client::builder()
        .timeout(config.timeout)
        .redirect(policy)
        .use_rustls_tls()
        .build()
        .map_err(|err| SetupError::Network(format!("failed to initialize HTTP downloader: {err}")))
}

fn map_download_reqwest_error(err: &reqwest::Error) -> SetupError {
    let detail = err.url().map_or_else(
        || err.to_string(),
        |url| {
            err.to_string()
                .replace(url.as_str(), &redact_url_for_error(url))
        },
    );
    if err.is_redirect() && detail.contains("refusing") {
        return SetupError::SecurityViolation(format!("download redirect blocked: {detail}"));
    }
    if err.is_timeout() {
        SetupError::Network(format!("download timed out: {detail}"))
    } else if err.is_connect() {
        SetupError::Network(format!("download connection failed: {detail}"))
    } else if err.is_redirect() {
        SetupError::Network(format!("download redirect failed: {detail}"))
    } else {
        SetupError::Network(format!("download failed: {detail}"))
    }
}

fn redact_url_for_error(url: &Url) -> String {
    let host = url.host_str().unwrap_or("<unknown>");
    url.port().map_or_else(
        || format!("{}://{host}/...", url.scheme()),
        |port| format!("{}://{host}:{port}/...", url.scheme()),
    )
}

fn parse_ip_host(host: &str) -> Option<IpAddr> {
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    let without_zone = stripped.split('%').next().unwrap_or(stripped);
    without_zone.parse::<IpAddr>().ok()
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return true;
    }
    match parse_ip_host(host) {
        Some(IpAddr::V4(v4)) => v4.is_loopback(),
        Some(IpAddr::V6(v6)) => v6.is_loopback(),
        None => false,
    }
}

fn is_link_local_or_metadata_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("metadata.google.internal") {
        return true;
    }
    match parse_ip_host(host) {
        Some(IpAddr::V4(v4)) => v4.is_link_local(),
        Some(IpAddr::V6(v6)) => {
            let first16 = v6.segments()[0];
            (first16 & 0xffc0) == 0xfe80 || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_link_local())
        }
        None => false,
    }
}

fn is_non_loopback_private_or_unspecified_host(host: &str) -> bool {
    if is_loopback_host(host) {
        return false;
    }
    match parse_ip_host(host) {
        Some(IpAddr::V4(v4)) => v4.is_private() || v4.is_unspecified(),
        Some(IpAddr::V6(v6)) => {
            let seg0 = v6.segments()[0];
            let is_ula = (seg0 & 0xfe00) == 0xfc00;
            v6.is_unspecified()
                || is_ula
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| !v4.is_loopback() && (v4.is_private() || v4.is_unspecified()))
        }
        None => false,
    }
}

fn is_private_or_loopback_host(host: &str) -> bool {
    is_loopback_host(host)
        || is_link_local_or_metadata_host(host)
        || is_non_loopback_private_or_unspecified_host(host)
        || matches!(parse_ip_host(host), Some(IpAddr::V4(v4)) if v4.is_documentation())
}

fn bytes_to_lower_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

struct PartFileCleanupGuard {
    path: PathBuf,
    armed: bool,
}

impl PartFileCleanupGuard {
    const fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    const fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PartFileCleanupGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}
