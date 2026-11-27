//! Unit tests for `kotoha-protocol` cross-platform IPC endpoint resolution.

use kotoha_protocol::{
    extract_windows_pipe_name, resolve_endpoint, resolve_endpoint_with, ProtocolError, TargetOs,
    DEFAULT_UNIX_FALLBACK_ENDPOINT, DEFAULT_WINDOWS_ENDPOINT, ENV_IPC_ENDPOINT,
};

#[test]
fn endpoint_resolution_rules_and_windows_pipe_normalization() {
    // 1. KOTOHA_IPC_ENDPOINT non-empty override wins on both Windows and Unix
    assert_eq!(
        resolve_endpoint_with(
            TargetOs::Windows,
            Some(r"\\.\pipe\custom_kotoha"),
            Some("/run/user/1000")
        ),
        r"\\.\pipe\custom_kotoha"
    );
    assert_eq!(
        resolve_endpoint_with(
            TargetOs::Unix,
            Some("/tmp/kotoha-test-123.sock"),
            Some("/run/user/1000")
        ),
        "/tmp/kotoha-test-123.sock"
    );

    // Empty override is ignored
    assert_eq!(
        resolve_endpoint_with(TargetOs::Windows, Some(""), None),
        DEFAULT_WINDOWS_ENDPOINT
    );
    assert_eq!(
        resolve_endpoint_with(TargetOs::Unix, Some(""), Some("/run/user/1000")),
        "/run/user/1000/kotoha_v1.sock"
    );

    // 2. Unix XDG_RUNTIME_DIR trailing slash normalization and fallback
    assert_eq!(
        resolve_endpoint_with(TargetOs::Unix, None, Some("/run/user/1000/")),
        "/run/user/1000/kotoha_v1.sock"
    );
    assert_eq!(
        resolve_endpoint_with(TargetOs::Unix, None, Some("///")),
        "/kotoha_v1.sock"
    );
    assert_eq!(
        resolve_endpoint_with(TargetOs::Unix, None, Some("")),
        DEFAULT_UNIX_FALLBACK_ENDPOINT
    );
    assert_eq!(
        resolve_endpoint_with(TargetOs::Unix, None, None),
        DEFAULT_UNIX_FALLBACK_ENDPOINT
    );

    // 3. Windows pipe name extraction (standard prefix case-insensitive + bare name)
    assert_eq!(
        extract_windows_pipe_name(r"\\.\pipe\kotoha_v1"),
        Ok("kotoha_v1")
    );
    assert_eq!(
        extract_windows_pipe_name(r"\\.\PIPE\kotoha_test_99"),
        Ok("kotoha_test_99")
    );
    assert_eq!(extract_windows_pipe_name("kotoha_v1"), Ok("kotoha_v1"));

    // Invalid Windows pipe endpoints
    assert!(matches!(
        extract_windows_pipe_name(""),
        Err(ProtocolError::InvalidEndpoint(_))
    ));
    assert!(matches!(
        extract_windows_pipe_name(r"\\.\pipe\"),
        Err(ProtocolError::InvalidEndpoint(_))
    ));
    assert!(matches!(
        extract_windows_pipe_name(r"\\.\pipe\nested\name"),
        Err(ProtocolError::InvalidEndpoint(_))
    ));
    assert!(matches!(
        extract_windows_pipe_name("bad/name"),
        Err(ProtocolError::InvalidEndpoint(_))
    ));
    assert!(matches!(
        extract_windows_pipe_name("bad\0name"),
        Err(ProtocolError::InvalidEndpoint(_))
    ));

    // Also verify live resolve_endpoint() returns a non-empty string consistent with current OS
    let live = resolve_endpoint();
    assert_ne!(live, "");
    assert_eq!(ENV_IPC_ENDPOINT, "KOTOHA_IPC_ENDPOINT");
}
