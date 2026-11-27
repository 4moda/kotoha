//! Unit tests for `kotoha-protocol` cross-platform IPC endpoint resolution (DESIGN.md Section
//! 3.1), including the shared Rust/C++ parity fixture `tests/fixtures/endpoint_resolution_cases.tsv`.

#![allow(clippy::expect_used, clippy::panic)]

use kotoha_protocol::{
    extract_windows_pipe_name, resolve_endpoint, resolve_endpoint_details_with,
    resolve_endpoint_with, EndpointEnv, ProtocolError, ResolvedEndpoint, TargetOs,
    DEFAULT_WINDOWS_ENDPOINT, ENV_IPC_ENDPOINT, MAX_UNIX_ENDPOINT_LEN,
};

const PARITY_CASES: &str = include_str!("../../../tests/fixtures/endpoint_resolution_cases.tsv");

fn opt(field: &str) -> Option<&str> {
    if field == "-" {
        None
    } else {
        Some(field)
    }
}

#[test]
fn shared_parity_fixture_matches_rust_resolver() {
    let mut checked = 0usize;
    for (line_no, line) in PARITY_CASES.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        assert_eq!(
            cols.len(),
            6,
            "fixture line {} must have 6 columns",
            line_no + 1
        );
        let env = EndpointEnv {
            endpoint_override: opt(cols[0]),
            xdg_runtime_dir: opt(cols[1]),
            tmpdir: opt(cols[2]),
            euid: cols[3].parse().expect("fixture euid is a u32"),
        };
        let expected = ResolvedEndpoint {
            endpoint: cols[4].to_owned(),
            private_dir: opt(cols[5]).map(str::to_owned),
        };
        assert_eq!(
            resolve_endpoint_details_with(TargetOs::Unix, &env),
            expected,
            "fixture line {}",
            line_no + 1
        );
        checked += 1;
    }
    assert!(checked >= 10, "fixture should contain the full case table");
}

#[test]
fn empty_values_behave_like_unset() {
    let unset = EndpointEnv {
        euid: 1000,
        ..EndpointEnv::default()
    };
    let empty = EndpointEnv {
        endpoint_override: Some(""),
        xdg_runtime_dir: Some(""),
        tmpdir: Some(""),
        euid: 1000,
    };
    assert_eq!(
        resolve_endpoint_details_with(TargetOs::Unix, &unset),
        resolve_endpoint_details_with(TargetOs::Unix, &empty)
    );
    assert_eq!(
        resolve_endpoint_with(TargetOs::Unix, &empty),
        "/tmp/kotoha-1000/kotoha_v1.sock"
    );
}

#[test]
fn no_fallback_directly_in_world_writable_tmp() {
    for tmpdir in [None, Some("/"), Some("rel"), Some("/tmp"), Some("/tmp/")] {
        let resolved = resolve_endpoint_details_with(
            TargetOs::Unix,
            &EndpointEnv {
                tmpdir,
                euid: 42,
                ..EndpointEnv::default()
            },
        );
        assert_ne!(resolved.endpoint, "/tmp/kotoha_v1.sock");
        assert_eq!(resolved.endpoint, "/tmp/kotoha-42/kotoha_v1.sock");
        assert_eq!(resolved.private_dir.as_deref(), Some("/tmp/kotoha-42"));
    }
}

#[test]
fn derived_private_endpoint_never_exceeds_length_limit() {
    for len in 1..200 {
        let tmpdir = format!("/{}", "x".repeat(len));
        let endpoint = resolve_endpoint_with(
            TargetOs::Unix,
            &EndpointEnv {
                tmpdir: Some(&tmpdir),
                euid: u32::MAX,
                ..EndpointEnv::default()
            },
        );
        assert!(
            endpoint.len() <= MAX_UNIX_ENDPOINT_LEN,
            "len {len}: '{endpoint}' too long"
        );
    }
}

#[test]
fn windows_resolution_and_pipe_normalization() {
    assert_eq!(
        resolve_endpoint_with(
            TargetOs::Windows,
            &EndpointEnv {
                endpoint_override: Some(r"\\.\pipe\custom_kotoha"),
                xdg_runtime_dir: Some("/run/user/1000"),
                ..EndpointEnv::default()
            }
        ),
        r"\\.\pipe\custom_kotoha"
    );
    let resolved = resolve_endpoint_details_with(
        TargetOs::Windows,
        &EndpointEnv {
            endpoint_override: Some(""),
            tmpdir: Some("/var/tmp"),
            ..EndpointEnv::default()
        },
    );
    assert_eq!(resolved.endpoint, DEFAULT_WINDOWS_ENDPOINT);
    assert_eq!(resolved.private_dir, None);

    assert_eq!(
        extract_windows_pipe_name(r"\\.\pipe\kotoha_v1"),
        Ok("kotoha_v1")
    );
    assert_eq!(
        extract_windows_pipe_name(r"\\.\PIPE\kotoha_test_99"),
        Ok("kotoha_test_99")
    );
    assert_eq!(extract_windows_pipe_name("kotoha_v1"), Ok("kotoha_v1"));
    for bad in [
        "",
        r"\\.\pipe\",
        r"\\.\pipe\nested\name",
        "bad/name",
        "bad\0name",
    ] {
        assert!(
            matches!(
                extract_windows_pipe_name(bad),
                Err(ProtocolError::InvalidEndpoint(_))
            ),
            "{bad:?} must be rejected"
        );
    }
}

#[test]
fn live_resolution_is_non_empty() {
    assert_ne!(resolve_endpoint(), "");
    assert_eq!(ENV_IPC_ENDPOINT, "KOTOHA_IPC_ENDPOINT");
}

#[cfg(unix)]
#[test]
fn current_euid_matches_owner_of_newly_created_file() {
    use std::os::unix::fs::MetadataExt as _;
    let path = std::env::temp_dir().join(format!("kotoha-euid-probe-{}", std::process::id()));
    std::fs::write(&path, b"x").expect("create probe file");
    let owner = std::fs::metadata(&path).expect("stat probe file").uid();
    let _ = std::fs::remove_file(&path);
    assert_eq!(kotoha_protocol::current_euid(), owner);
}
