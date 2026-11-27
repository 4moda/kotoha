//! Integration tests for `kotoha-core` OS-native IPC server (`src/infrastructure/ipc_server.rs`).

#![allow(clippy::expect_used)]

mod common;

use std::time::Duration;

use common::test_endpoint;
use kotoha_core::domain::AudioInputEvent;
use kotoha_core::infrastructure::{connect_local_socket, IpcServerAdapter};
use kotoha_protocol::{encode_frame, Message};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot, watch};

#[tokio::test]
async fn ipc_server_rejects_non_hello_first_frame_and_malformed_bytes() {
    let endpoint = test_endpoint("bad");
    let server = IpcServerAdapter::new(endpoint.clone());
    let (event_tx, mut event_rx) = mpsc::channel::<AudioInputEvent>(16);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (ready_tx, ready_rx) = oneshot::channel();

    let server_task =
        tokio::spawn(async move { server.run(event_tx, shutdown_rx, Some(ready_tx)).await });
    ready_rx.await.expect("IPC server ready");

    // 1. Send Heartbeat before Hello -> server disconnects without emitting SourceConnected
    let mut bad_client = connect_local_socket(&endpoint)
        .await
        .expect("connect bad client");
    bad_client
        .write_all(&encode_frame(&Message::Heartbeat).expect("encode hb"))
        .await
        .expect("write hb");
    drop(bad_client);

    // 2. Send garbage magic -> server disconnects immediately
    let mut garbage_client = connect_local_socket(&endpoint)
        .await
        .expect("connect garbage client");
    garbage_client
        .write_all(&[0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00])
        .await
        .expect("write garbage");
    drop(garbage_client);

    // 3. Valid client connects afterwards -> server stays healthy and accepts it
    let mut good_client = connect_local_socket(&endpoint)
        .await
        .expect("connect good client");
    good_client
        .write_all(
            &encode_frame(&Message::Hello {
                app_name: "ts3".to_owned(),
                plugin_version: "1.0.0".to_owned(),
            })
            .expect("encode hello"),
        )
        .await
        .expect("write hello");
    // Duplicate Hello mid-session -> must disconnect cleanly
    good_client
        .write_all(
            &encode_frame(&Message::Hello {
                app_name: "ts3".to_owned(),
                plugin_version: "1.0.0".to_owned(),
            })
            .expect("encode dup hello"),
        )
        .await
        .expect("write dup hello");
    drop(good_client);

    let first = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
        .await
        .expect("recv first event")
        .expect("channel open");
    assert!(matches!(
        first,
        AudioInputEvent::SourceConnected { ref source, .. } if source == "ts3"
    ));

    let second = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
        .await
        .expect("recv disconnect event")
        .expect("channel open");
    assert!(matches!(
        second,
        AudioInputEvent::SourceDisconnected { ref source, .. } if source == "ts3"
    ));

    let _ = shutdown_tx.send(true);
    server_task.await.expect("server join").expect("server ok");
}

#[tokio::test]
async fn ipc_server_handles_shutdown_signal_and_stale_symlink() {
    let endpoint = test_endpoint("shutdown");

    #[cfg(unix)]
    {
        // Create a dangling symlink at the socket path to verify symlink_metadata removes it cleanly
        let _ = std::fs::remove_file(&endpoint);
        std::os::unix::fs::symlink("/tmp/kotoha_nonexistent_target", &endpoint)
            .expect("create dangling symlink");
    }

    let server = IpcServerAdapter::new(endpoint.clone());
    let (event_tx, _event_rx) = mpsc::channel::<AudioInputEvent>(8);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (ready_tx, ready_rx) = oneshot::channel();

    let server_task =
        tokio::spawn(async move { server.run(event_tx, shutdown_rx, Some(ready_tx)).await });
    ready_rx
        .await
        .expect("server bound cleanly over dangling symlink");

    // Connect client and leave it idle, then trigger shutdown (verifying wait_for terminates
    // handle_connection promptly without hanging)
    let _idle_client = connect_local_socket(&endpoint)
        .await
        .expect("connect idle client");
    shutdown_tx.send(true).expect("send shutdown");

    tokio::time::timeout(Duration::from_secs(2), server_task)
        .await
        .expect("server shutdown must not deadlock")
        .expect("join ok")
        .expect("server ok");
}
