//! Single-source cross-language IPC v1 E2E test (`kotoha_mock_client` <-> `kotoha-ipc-monitor`).

#![allow(clippy::expect_used)]

use kotoha_e2e::{
    assert_single_source_lifecycle, E2eBinaries, MockClientArgs, MonitorSession, UniqueIpcEndpoint,
};

#[tokio::test]
async fn single_source_mumble_round_trip() {
    // Arrange
    let bins = E2eBinaries::discover().expect("discover kotoha_mock_client and kotoha-ipc-monitor");
    let endpoint = UniqueIpcEndpoint::new("s1").expect("create unique IPC endpoint");
    let mut monitor = MonitorSession::start(&bins.ipc_monitor, &endpoint, Some(32))
        .await
        .expect("start kotoha-ipc-monitor");

    // Act
    MockClientArgs::new(&endpoint)
        .source("mumble")
        .user(42, "Alice")
        .frames(5)
        .with_heartbeat()
        .run(&bins.mock_client)
        .await
        .expect("run kotoha_mock_client");

    monitor
        .wait_for_sources_disconnected(&["mumble"])
        .await
        .expect("wait for mumble disconnect");
    let events = monitor
        .finish_and_collect()
        .await
        .expect("finish monitor and collect NDJSON events");

    // Assert
    assert_single_source_lifecycle(&events, "mumble", 42, "Alice", 5, true, 0.1)
        .expect("validate single-source mumble lifecycle");

    if let Some(sock_path) = endpoint.unix_path() {
        assert!(
            !sock_path.exists(),
            "monitor left stale Unix socket file: {}",
            sock_path.display()
        );
    }
}
