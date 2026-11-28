//! Process-environment test for `resolve_endpoint_details` (DESIGN.md Section 3.1).
//!
//! Lives in its own test binary (a single test) because it mutates the process environment.

#![cfg(unix)]
#![allow(clippy::expect_used)]

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;

use kotoha_protocol::{current_euid, resolve_endpoint_details};

#[test]
fn non_utf8_environment_values_are_treated_as_unset() {
    let euid = current_euid();
    let fallback = format!("/tmp/kotoha-{euid}");

    std::env::remove_var("KOTOHA_IPC_ENDPOINT");
    std::env::remove_var("XDG_RUNTIME_DIR");

    // Same bytes as the C++ test in plugins/tests/test_ipc_client.cpp: both sides fall back.
    std::env::set_var("TMPDIR", OsString::from_vec(b"/var/\xff\xfe/T".to_vec()));
    let resolved = resolve_endpoint_details();
    assert_eq!(resolved.private_dir.as_deref(), Some(fallback.as_str()));
    assert_eq!(resolved.endpoint, format!("{fallback}/kotoha_v1.sock"));

    // A valid non-ASCII UTF-8 TMPDIR is still honoured.
    std::env::set_var("TMPDIR", "/var/t\u{e9}mp");
    let resolved = resolve_endpoint_details();
    assert_eq!(
        resolved.endpoint,
        format!("/var/t\u{e9}mp/kotoha-{euid}/kotoha_v1.sock")
    );

    // A non-UTF-8 XDG_RUNTIME_DIR is ignored too.
    std::env::remove_var("TMPDIR");
    std::env::set_var("XDG_RUNTIME_DIR", OsString::from_vec(b"/run/\xff".to_vec()));
    assert_eq!(
        resolve_endpoint_details().private_dir.as_deref(),
        Some(fallback.as_str())
    );
}
