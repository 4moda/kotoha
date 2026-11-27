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

#[cfg(unix)]
mod liveness {
    //! Liveness: Hello timeout, idle timeout, superseded connections, connection limit and
    //! disconnect reasons (DESIGN.md Section 3.2).
    #![allow(clippy::panic, clippy::unwrap_used)]

    use std::time::Duration;

    use kotoha_core::domain::{AudioInputEvent, DisconnectReason};
    use kotoha_core::infrastructure::{connect_local_socket, IpcServerAdapter, IpcServerConfig};
    use kotoha_protocol::{encode_frame, Message};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::{mpsc, oneshot, watch};
    use tokio::task::JoinHandle;

    struct Harness {
        endpoint: String,
        events: mpsc::Receiver<AudioInputEvent>,
        shutdown: watch::Sender<bool>,
        task: JoinHandle<()>,
    }

    impl Harness {
        async fn start(tag: &str, config: IpcServerConfig) -> Self {
            Self::start_with_capacity(tag, config, 64).await
        }

        async fn start_with_capacity(tag: &str, config: IpcServerConfig, capacity: usize) -> Self {
            let endpoint = super::common::test_endpoint(&format!("live_{tag}"));
            let server = IpcServerAdapter::with_config(endpoint.clone(), config);
            let (tx, events) = mpsc::channel(capacity);
            let (shutdown, shutdown_rx) = watch::channel(false);
            let (ready_tx, ready_rx) = oneshot::channel();
            let task = tokio::spawn(async move {
                server
                    .run(tx, shutdown_rx, Some(ready_tx))
                    .await
                    .expect("server run");
            });
            ready_rx.await.expect("ready");
            Self {
                endpoint,
                events,
                shutdown,
                task,
            }
        }

        async fn next_event(&mut self) -> AudioInputEvent {
            tokio::time::timeout(Duration::from_secs(5), self.events.recv())
                .await
                .expect("event timeout")
                .expect("event channel closed")
        }

        async fn stop(self) {
            let _ = self.shutdown.send(true);
            self.task.await.expect("server task");
        }
    }

    const fn config(hello_ms: u64, idle_ms: u64, max: usize) -> IpcServerConfig {
        IpcServerConfig {
            hello_timeout: Duration::from_millis(hello_ms),
            idle_timeout: Duration::from_millis(idle_ms),
            max_connections: max,
        }
    }

    fn hello(name: &str) -> Vec<u8> {
        encode_frame(&Message::Hello {
            app_name: name.to_owned(),
            plugin_version: "t".to_owned(),
        })
        .expect("encode hello")
    }

    /// Waits until the server closes the stream (EOF or reset) within `limit`.
    async fn expect_closed(stream: &mut (impl AsyncReadExt + Unpin), limit: Duration) {
        let mut buf = [0u8; 16];
        let res = tokio::time::timeout(limit, async {
            loop {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        })
        .await;
        assert!(res.is_ok(), "connection was not closed within {limit:?}");
    }

    #[tokio::test]
    async fn slow_consumer_does_not_cause_idle_timeout() {
        // Channel capacity 1 and a consumer that stalls for 800ms (> idle_timeout 300ms) while the
        // client keeps sending Heartbeats every 50ms: time blocked on the consumer is not idle time.
        let mut h = Harness::start_with_capacity("slow", config(1000, 300, 16), 1).await;
        let mut c = connect_local_socket(&h.endpoint).await.expect("connect");
        c.write_all(&hello("mumble")).await.expect("hello");
        let hb = encode_frame(&Message::Heartbeat).expect("hb");
        let writer = tokio::spawn(async move {
            for _ in 0..24 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                c.write_all(&hb).await.expect("heartbeat");
            }
            c
        });
        tokio::time::sleep(Duration::from_millis(800)).await;
        let mut seen = 0;
        while let Ok(Some(ev)) =
            tokio::time::timeout(Duration::from_millis(250), h.events.recv()).await
        {
            assert!(
                !matches!(ev, AudioInputEvent::SourceDisconnected { .. }),
                "unexpected disconnect: {ev:?}"
            );
            seen += 1;
        }
        assert!(seen > 2, "consumer should have received the backlog");
        let _client = writer.await.expect("writer");
        h.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn near_simultaneous_hello_leaves_a_live_owner() {
        use kotoha_core::application::SourceSessionManager;

        let mut h = Harness::start("race", config(2000, 5000, 256)).await;
        let mut mgr = SourceSessionManager::new();
        for i in 0..1000u32 {
            let src = format!("race{i}");
            let (mut a, mut b) = (
                connect_local_socket(&h.endpoint).await.expect("a"),
                connect_local_socket(&h.endpoint).await.expect("b"),
            );
            let frame = hello(&src);
            let (ra, rb) = tokio::join!(a.write_all(&frame), b.write_all(&frame));
            ra.expect("hello a");
            rb.expect("hello b");

            let (mut connected, mut superseded) = (0, None);
            while connected < 2 || superseded.is_none() {
                let ev = h.next_event().await;
                let ours = match &ev {
                    AudioInputEvent::SourceConnected { source, .. }
                    | AudioInputEvent::SourceDisconnected { source, .. } => source == &src,
                    _ => false,
                };
                if !ours {
                    continue; // leftover EOF from a previous iteration
                }
                match &ev {
                    AudioInputEvent::SourceConnected { .. } => connected += 1,
                    AudioInputEvent::SourceDisconnected {
                        conn_id, reason, ..
                    } => {
                        assert_eq!(*reason, DisconnectReason::Superseded);
                        superseded = Some(*conn_id);
                    }
                    _ => {}
                }
                mgr.handle_event(&ev);
            }
            let loser = superseded.expect("superseded");
            let owner = mgr.owning_conn_id(&src);
            assert!(owner.is_some(), "iteration {i}: source has no owner");
            assert_ne!(
                owner,
                Some(loser),
                "iteration {i}: superseded conn owns source"
            );
        }
        h.stop().await;
    }

    #[tokio::test]
    async fn closes_connection_without_hello() {
        let h = Harness::start("nohello", config(150, 5000, 16)).await;
        let mut c = connect_local_socket(&h.endpoint).await.expect("connect");
        expect_closed(&mut c, Duration::from_secs(2)).await;
        h.stop().await;
    }

    #[tokio::test]
    async fn idle_client_is_disconnected_with_idle_timeout_reason() {
        let mut h = Harness::start("idle", config(1000, 200, 16)).await;
        let mut c = connect_local_socket(&h.endpoint).await.expect("connect");
        c.write_all(&hello("mumble")).await.expect("hello");
        assert!(matches!(
            h.next_event().await,
            AudioInputEvent::SourceConnected { .. }
        ));
        match h.next_event().await {
            AudioInputEvent::SourceDisconnected { source, reason, .. } => {
                assert_eq!(source, "mumble");
                assert_eq!(reason, DisconnectReason::IdleTimeout);
            }
            other => panic!("unexpected event {other:?}"),
        }
        expect_closed(&mut c, Duration::from_secs(2)).await;
        h.stop().await;
    }

    #[tokio::test]
    async fn heartbeats_keep_connection_alive() {
        let mut h = Harness::start("hb", config(1000, 300, 16)).await;
        let mut c = connect_local_socket(&h.endpoint).await.expect("connect");
        c.write_all(&hello("mumble")).await.expect("hello");
        let hb = encode_frame(&Message::Heartbeat).expect("hb");
        for _ in 0..8 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            c.write_all(&hb).await.expect("heartbeat");
        }
        // 800ms elapsed (> idle_timeout) but only Connected + Heartbeat events, no disconnect.
        let mut disconnects = 0;
        while let Ok(ev) = h.events.try_recv() {
            if matches!(ev, AudioInputEvent::SourceDisconnected { .. }) {
                disconnects += 1;
            }
        }
        assert_eq!(disconnects, 0);
        h.stop().await;
    }

    #[tokio::test]
    async fn superseded_connection_is_closed_actively() {
        let mut h = Harness::start("super", config(1000, 5000, 16)).await;
        let mut a = connect_local_socket(&h.endpoint).await.expect("connect a");
        a.write_all(&hello("mumble")).await.expect("hello a");
        let AudioInputEvent::SourceConnected { conn_id: id_a, .. } = h.next_event().await else {
            panic!("expected connect A");
        };
        let mut b = connect_local_socket(&h.endpoint).await.expect("connect b");
        b.write_all(&hello("mumble")).await.expect("hello b");
        let AudioInputEvent::SourceConnected { conn_id: id_b, .. } = h.next_event().await else {
            panic!("expected connect B");
        };
        assert_ne!(id_a, id_b);

        // A observes EOF even though it never closed or sent anything.
        expect_closed(&mut a, Duration::from_secs(2)).await;
        match h.next_event().await {
            AudioInputEvent::SourceDisconnected {
                conn_id, reason, ..
            } => {
                assert_eq!(conn_id, id_a);
                assert_eq!(reason, DisconnectReason::Superseded);
            }
            other => panic!("unexpected event {other:?}"),
        }
        // B stays open: a heartbeat is still accepted.
        b.write_all(&encode_frame(&Message::Heartbeat).expect("hb"))
            .await
            .expect("b heartbeat");
        assert!(matches!(
            h.next_event().await,
            AudioInputEvent::Heartbeat { conn_id, .. } if conn_id == id_b
        ));
        h.stop().await;
    }

    #[tokio::test]
    async fn connections_beyond_limit_are_dropped() {
        let h = Harness::start("max", config(5000, 5000, 2)).await;
        let mut c1 = connect_local_socket(&h.endpoint).await.expect("c1");
        let mut c2 = connect_local_socket(&h.endpoint).await.expect("c2");
        // Let the server register the first two before the third arrives.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut c3 = connect_local_socket(&h.endpoint).await.expect("c3");
        expect_closed(&mut c3, Duration::from_secs(2)).await;
        // The first two remain open: nothing to read within a short window.
        let mut buf = [0u8; 1];
        assert!(
            tokio::time::timeout(Duration::from_millis(200), c1.read(&mut buf))
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(200), c2.read(&mut buf))
                .await
                .is_err()
        );
        h.stop().await;
    }

    #[tokio::test]
    async fn protocol_error_reports_reason() {
        let mut h = Harness::start("proto", config(1000, 5000, 16)).await;
        let mut c = connect_local_socket(&h.endpoint).await.expect("connect");
        c.write_all(&hello("mumble")).await.expect("hello");
        assert!(matches!(
            h.next_event().await,
            AudioInputEvent::SourceConnected { .. }
        ));
        c.write_all(&[0, 0, 1, 0, 0, 0, 0, 0]).await.expect("junk");
        match h.next_event().await {
            AudioInputEvent::SourceDisconnected { reason, .. } => {
                assert!(
                    matches!(reason, DisconnectReason::Protocol(_)),
                    "got {reason:?}"
                );
            }
            other => panic!("unexpected event {other:?}"),
        }
        h.stop().await;
    }

    #[tokio::test]
    async fn clean_close_reports_eof() {
        let mut h = Harness::start("eof", config(1000, 5000, 16)).await;
        let mut c = connect_local_socket(&h.endpoint).await.expect("connect");
        c.write_all(&hello("mumble")).await.expect("hello");
        let _ = h.next_event().await;
        drop(c);
        match h.next_event().await {
            AudioInputEvent::SourceDisconnected { reason, .. } => {
                assert_eq!(reason, DisconnectReason::Eof);
            }
            other => panic!("unexpected event {other:?}"),
        }
        h.stop().await;
    }
}
