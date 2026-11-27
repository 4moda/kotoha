//! Shared HTTP client helpers, URL/timeout/SSRF validation, bounded reads, and status mapping.

use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use kotoha_core::domain::CoreError;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{redirect, Client, Response, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::Deserialize;

/// Default HTTP request timeout in milliseconds (`15_000` ms).
pub const DEFAULT_HTTP_TIMEOUT_MS: u64 = 15_000;

/// Minimum permitted HTTP request timeout in milliseconds (`100` ms).
pub const MIN_TIMEOUT_MS: u64 = 100;

/// Maximum permitted HTTP request timeout in milliseconds (`120_000` ms).
pub const MAX_TIMEOUT_MS: u64 = 120_000;

/// Maximum redirects allowed for [`GasWebhookTranslator`](crate::GasWebhookTranslator)
/// (`302 Found` redirect from Google Apps Script Web Apps).
pub const DEFAULT_MAX_REDIRECTS: usize = 5;

/// Hard upper bound on HTTP response body bytes (`1 MiB`) to prevent OOM denial-of-service.
pub const MAX_HTTP_RESPONSE_BYTES: usize = 1024 * 1024;

/// Redirect policy mode for HTTP translator clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectMode {
    /// Reject all redirects (used by API-key-bearing endpoints so secrets never leak).
    None,
    /// Allow up to `max_hops` safe redirects (used by GAS Webhook `302 Found`).
    SafeGas {
        /// Maximum number of redirects permitted.
        max_hops: usize,
        /// Whether the initial configured endpoint was explicitly loopback (`127.0.0.1` / `::1` / `localhost`).
        allow_loopback_targets: bool,
    },
}

/// Builds a `reqwest::Client` using `rustls` TLS with bounded timeout (`100..=120_000` ms) and
/// explicit [`RedirectMode`].
pub fn build_http_client(
    timeout_ms: u64,
    redirect_mode: RedirectMode,
) -> Result<Client, CoreError> {
    if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
        return Err(CoreError::NotConfigured(format!(
            "HTTP translator timeout_ms must be within {MIN_TIMEOUT_MS}..={MAX_TIMEOUT_MS} ms, got {timeout_ms}"
        )));
    }

    let policy = match redirect_mode {
        RedirectMode::None => redirect::Policy::none(),
        RedirectMode::SafeGas {
            max_hops,
            allow_loopback_targets,
        } => {
            if max_hops == 0 {
                redirect::Policy::none()
            } else {
                let hops = Arc::new(AtomicUsize::new(0));
                redirect::Policy::custom(move |attempt| {
                    let count = hops.fetch_add(1, Ordering::Relaxed) + 1;
                    if count > max_hops || attempt.previous().len() > max_hops {
                        return attempt.error("too many redirects");
                    }

                    let next_url = attempt.url();
                    match next_url.scheme() {
                        "https" => {}
                        "http" => {
                            let downgraded_from_https = attempt
                                .previous()
                                .iter()
                                .any(|prev| prev.scheme().eq_ignore_ascii_case("https"));
                            if downgraded_from_https {
                                return attempt
                                    .error("refusing https:// to http:// redirect downgrade");
                            }
                            if !(allow_loopback_targets && is_loopback_url(next_url)) {
                                return attempt.error(
                                    "refusing non-https redirect to non-loopback destination",
                                );
                            }
                        }
                        _ => {
                            return attempt.error("refusing redirect to non-http(s) scheme");
                        }
                    }

                    if let Err(reason) =
                        check_redirect_destination(next_url, allow_loopback_targets)
                    {
                        return attempt.error(reason);
                    }

                    attempt.follow()
                })
            }
        }
    };

    Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .redirect(policy)
        .use_rustls_tls()
        .build()
        .map_err(|err| CoreError::Translation(format!("failed to initialize HTTP client: {err}")))
}

/// Validates and normalizes an HTTP/HTTPS endpoint URL.
///
/// Rejects embedded userinfo (`user:pass@`), `?query`, `#fragment`, link-local metadata addresses,
/// and non-loopback plain `http://` URLs whenever `has_credentials` is `true`.
pub fn validate_http_url(
    label: &str,
    raw_url: &str,
    has_credentials: bool,
) -> Result<String, CoreError> {
    let trimmed = raw_url.trim();
    if trimmed.is_empty() {
        return Err(CoreError::NotConfigured(format!(
            "{label} must not be empty"
        )));
    }
    let parsed = Url::parse(trimmed)
        .map_err(|err| CoreError::NotConfigured(format!("{label} is not a valid URL: {err}")))?;

    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(CoreError::NotConfigured(format!(
                "{label} must use http:// or https:// scheme, got '{other}'"
            )));
        }
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(CoreError::NotConfigured(format!(
            "{label} must not contain embedded userinfo (user:pass@)"
        )));
    }
    if parsed.query().is_some() {
        return Err(CoreError::NotConfigured(format!(
            "{label} must not contain a query string (?query)"
        )));
    }
    if parsed.fragment().is_some() {
        return Err(CoreError::NotConfigured(format!(
            "{label} must not contain a fragment (#fragment)"
        )));
    }

    let Some(host) = parsed.host_str() else {
        return Err(CoreError::NotConfigured(format!(
            "{label} must contain a host name"
        )));
    };
    if is_link_local_host(host) {
        return Err(CoreError::NotConfigured(format!(
            "{label} must not target link-local metadata host '{host}'"
        )));
    }

    if has_credentials && parsed.scheme().eq_ignore_ascii_case("http") && !is_loopback_url(&parsed)
    {
        return Err(CoreError::NotConfigured(format!(
            "{label} must use https:// when credentials are configured for non-loopback host '{host}'"
        )));
    }

    Ok(trimmed.trim_end_matches('/').to_owned())
}

/// Returns `true` when `url` targets a loopback host (`localhost`, `127.0.0.0/8`, `::1`).
#[must_use]
pub fn is_loopback_url(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    is_loopback_host(host)
}

/// Validates and constructs a sensitive HTTP header value (marking [`HeaderValue::set_sensitive(true)`]).
pub fn validate_sensitive_header_value(field: &str, value: &str) -> Result<HeaderValue, CoreError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(CoreError::NotConfigured(format!(
            "{field} must not be empty"
        )));
    }
    let mut header_value = HeaderValue::from_str(trimmed).map_err(|err| {
        CoreError::NotConfigured(format!("{field} contains invalid HTTP header bytes: {err}"))
    })?;
    header_value.set_sensitive(true);
    Ok(header_value)
}

/// Inserts a pre-validated sensitive [`HeaderValue`] into `headers`.
pub fn insert_sensitive_header(
    headers: &mut HeaderMap,
    name: reqwest::header::HeaderName,
    value: HeaderValue,
) {
    headers.insert(name, value);
}

/// Maps `reqwest::Error` (redacting any embedded URL via `without_url`) into [`CoreError::Translation`].
#[must_use]
pub fn map_reqwest_error(provider: &str, err: &reqwest::Error) -> CoreError {
    let is_timeout = err.is_timeout();
    let is_connect = err.is_connect();
    let is_redirect = err.is_redirect();
    let sanitized = err.to_string();
    let detail = if let Some(url) = err.url() {
        sanitized.replace(url.as_str(), &redact_url_for_error(url))
    } else {
        sanitized
    };

    if is_timeout {
        CoreError::Translation(format!("{provider} HTTP request timed out: {detail}"))
    } else if is_connect {
        CoreError::Translation(format!("{provider} HTTP connection failed: {detail}"))
    } else if is_redirect {
        CoreError::Translation(format!("{provider} HTTP redirect blocked: {detail}"))
    } else {
        CoreError::Translation(format!("{provider} HTTP transport error: {detail}"))
    }
}

fn redact_url_for_error(url: &Url) -> String {
    let host = url.host_str().unwrap_or("<unknown>");
    url.port().map_or_else(
        || format!("{}://{host}/...", url.scheme()),
        |port| format!("{}://{host}:{port}/...", url.scheme()),
    )
}

#[derive(Debug, Deserialize)]
struct GenericErrorEnvelope {
    #[serde(default)]
    error: Option<serde_json::Value>,
    #[serde(default)]
    message: Option<String>,
}

/// Streams the HTTP response body up to [`MAX_HTTP_RESPONSE_BYTES`] (`1 MiB`), checks for non-2xx
/// statuses (including explicit `429` mapping), and deserializes the JSON payload.
pub async fn decode_json_response<T: DeserializeOwned>(
    provider: &str,
    mut response: Response,
) -> Result<T, CoreError> {
    let status = response.status();
    if let Some(content_len) = response.content_length() {
        if content_len > MAX_HTTP_RESPONSE_BYTES as u64 {
            return Err(CoreError::Translation(format!(
                "{provider} response Content-Length ({content_len} bytes) exceeds maximum limit of {MAX_HTTP_RESPONSE_BYTES} bytes"
            )));
        }
    }

    let initial_cap = response
        .content_length()
        .and_then(|len| usize::try_from(len).ok())
        .map_or(4096, |len| len.min(MAX_HTTP_RESPONSE_BYTES));
    let mut bytes = Vec::with_capacity(initial_cap);

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| map_reqwest_error(provider, &err))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_HTTP_RESPONSE_BYTES {
            return Err(CoreError::Translation(format!(
                "{provider} response body exceeds maximum limit of {MAX_HTTP_RESPONSE_BYTES} bytes"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }

    let body = String::from_utf8_lossy(&bytes);

    if !status.is_success() {
        return Err(map_http_status_error(provider, status, &body));
    }

    serde_json::from_str::<T>(&body).map_err(|err| {
        let preview = truncate_body(&body, 256);
        CoreError::Translation(format!(
            "{provider} returned invalid JSON response ({err}); body={preview:?}"
        ))
    })
}

/// Maps HTTP non-2xx responses (with explicit `429 Too Many Requests`, auth, and 5xx handling)
/// into descriptive [`CoreError::Translation`] errors.
#[must_use]
pub fn map_http_status_error(provider: &str, status: StatusCode, body: &str) -> CoreError {
    let detail = extract_error_detail(body);
    let reason = status.canonical_reason().unwrap_or("HTTP Error");
    if status == StatusCode::TOO_MANY_REQUESTS {
        CoreError::Translation(format!(
            "{provider} rate limited (HTTP 429 Too Many Requests): {detail}"
        ))
    } else if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        CoreError::Translation(format!(
            "{provider} authentication failed (HTTP {} {reason}): {detail}",
            status.as_u16()
        ))
    } else {
        CoreError::Translation(format!(
            "{provider} request failed (HTTP {} {reason}): {detail}",
            status.as_u16()
        ))
    }
}

fn check_redirect_destination(url: &Url, allow_loopback_targets: bool) -> Result<(), &'static str> {
    let Some(host) = url.host_str() else {
        return Err("redirect missing host name");
    };
    if !url.username().is_empty() || url.password().is_some() {
        return Err("redirect URL contains embedded credentials");
    }
    if is_link_local_host(host) {
        return Err("refusing redirect to link-local metadata address");
    }
    if !allow_loopback_targets && is_private_or_loopback_host(host) {
        return Err("refusing redirect to loopback or private network address");
    }
    Ok(())
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

fn is_link_local_host(host: &str) -> bool {
    match parse_ip_host(host) {
        Some(IpAddr::V4(v4)) => v4.is_link_local(),
        Some(IpAddr::V6(v6)) => {
            // fe80::/10 unicast link-local; also inspect IPv4-mapped ::ffff:169.254.x.x
            let first16 = v6.segments()[0];
            (first16 & 0xffc0) == 0xfe80 || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_link_local())
        }
        None => false,
    }
}

fn is_private_or_loopback_host(host: &str) -> bool {
    if is_loopback_host(host) || is_link_local_host(host) {
        return true;
    }
    match parse_ip_host(host) {
        Some(IpAddr::V4(v4)) => v4.is_private() || v4.is_unspecified() || v4.is_documentation(),
        Some(IpAddr::V6(v6)) => {
            let seg0 = v6.segments()[0];
            let is_ula = (seg0 & 0xfe00) == 0xfc00;
            v6.is_unspecified()
                || is_ula
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| v4.is_private() || v4.is_loopback() || v4.is_link_local())
        }
        None => false,
    }
}

fn extract_error_detail(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "empty response body".to_owned();
    }
    if let Ok(parsed) = serde_json::from_str::<GenericErrorEnvelope>(trimmed) {
        if let Some(msg) = parsed.message.filter(|m| !m.trim().is_empty()) {
            return truncate_body(&msg, 256);
        }
        if let Some(err_val) = parsed.error {
            if let Some(s) = err_val.as_str() {
                return truncate_body(s, 256);
            }
            if let Some(msg) = err_val
                .get("message")
                .and_then(|v| v.as_str())
                .filter(|m| !m.trim().is_empty())
            {
                return truncate_body(msg, 256);
            }
            return truncate_body(&err_val.to_string(), 256);
        }
    }
    truncate_body(trimmed, 256)
}

fn truncate_body(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in text.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out
}

/// Computes elapsed milliseconds saturated to `u64`.
#[must_use]
pub fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
