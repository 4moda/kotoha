//! Multi-source concurrent cross-language IPC v1 E2E test verifying `SpeakerKey` isolation.

#![allow(clippy::expect_used)]

use std::time::Duration;

use kotoha_e2e::harness::await_mock_client;
use kotoha_e2e::{
    assert_multi_source_isolation, E2eBinaries, MockClientArgs, MonitorSession, UniqueIpcEndpoint,
};

#[tokio::test]
async fn concurrent_multi_source_isolates_speaker_keys() {
    // Arrange
    let Some(bins) = E2eBinaries::discover_or_skip_local()
        .expect("discover kotoha_mock_client and kotoha-ipc-monitor")
    else {
        return;
    };
    let endpoint = UniqueIpcEndpoint::new("m2").expect("create unique IPC endpoint");
    let mut monitor = MonitorSession::start(&bins.ipc_monitor, &endpoint, Some(64))
        .await
        .expect("start kotoha-ipc-monitor");

    // Deliberately reuse user_id=42 across BOTH sources with distinct display names ("Alice" vs
    // "Bob") and overlapping streaming to prove SpeakerKey { source, user_id } isolation.
    let mumble_child = MockClientArgs::new(&endpoint)
        .source("mumble")
        .user(42, "Alice")
        .frames(6)
        .duration_ms(120)
        .with_heartbeat()
        .spawn(&bins.mock_client)
        .expect("spawn mumble mock client");

    let ts3_child = MockClientArgs::new(&endpoint)
        .source("ts3")
        .user(42, "Bob")
        .frames(6)
        .duration_ms(120)
        .with_heartbeat()
        .spawn(&bins.mock_client)
        .expect("spawn ts3 mock client");

    // Act
    let timeout = Duration::from_secs(15);
    let (mumble_res, ts3_res) = tokio::join!(
        await_mock_client(mumble_child, timeout),
        await_mock_client(ts3_child, timeout)
    );
    mumble_res.expect("mumble kotoha_mock_client succeeded");
    ts3_res.expect("ts3 kotoha_mock_client succeeded");

    monitor
        .wait_for_sources_disconnected(&["mumble", "ts3"])
        .await
        .expect("wait for mumble and ts3 disconnects");
    let events = monitor
        .finish_and_collect()
        .await
        .expect("finish monitor and collect NDJSON events");

    // Assert
    assert_multi_source_isolation(
        &events,
        ("mumble", 42, "Alice", 6),
        ("ts3", 42, "Bob", 6),
        true,
        0.1,
    )
    .expect("validate concurrent multi-source isolation");

    if let Some(sock_path) = endpoint.unix_path() {
        assert!(
            !sock_path.exists(),
            "monitor left stale Unix socket file: {}",
            sock_path.display()
        );
    }
}
