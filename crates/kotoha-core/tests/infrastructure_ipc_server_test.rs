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
async fn ipc_server_handles_shutdown_signal() {
    // Symlinks (dangling or not) at the endpoint are refused instead of removed (issue #23);
    // see the `endpoint_security` module below.
    let endpoint = test_endpoint("shutdown");

    let server = IpcServerAdapter::new(endpoint.clone());
    let (event_tx, _event_rx) = mpsc::channel::<AudioInputEvent>(8);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (ready_tx, ready_rx) = oneshot::channel();

    let server_task =
        tokio::spawn(async move { server.run(event_tx, shutdown_rx, Some(ready_tx)).await });
    ready_rx.await.expect("server bound");

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

mod config {
    //! `IpcServerConfig` parsing and validation (environment variables / CLI flags).
    #![allow(clippy::panic, clippy::unwrap_used)]

    use std::collections::HashMap;
    use std::time::Duration;

    use kotoha_core::domain::CoreError;
    use kotoha_core::infrastructure::{
        IpcServerConfig, ENV_IPC_HELLO_TIMEOUT_MS, ENV_IPC_IDLE_TIMEOUT_MS, ENV_IPC_MAX_CONNECTIONS,
    };

    fn parse(vars: &[(&str, &str)]) -> Result<IpcServerConfig, CoreError> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        IpcServerConfig::from_lookup(|name| Ok(map.get(name).cloned()))
    }

    fn message(res: Result<IpcServerConfig, CoreError>) -> String {
        match res {
            Err(CoreError::Ipc(msg)) => msg,
            other => panic!("expected an IPC config error, got {other:?}"),
        }
    }

    #[test]
    fn unset_and_empty_variables_keep_defaults() {
        assert_eq!(parse(&[]).unwrap(), IpcServerConfig::default());
        let empty = parse(&[
            (ENV_IPC_HELLO_TIMEOUT_MS, ""),
            (ENV_IPC_IDLE_TIMEOUT_MS, "  "),
            (ENV_IPC_MAX_CONNECTIONS, ""),
        ]);
        assert_eq!(empty.unwrap(), IpcServerConfig::default());
    }

    #[test]
    fn variables_override_individual_limits() {
        let config = parse(&[
            (ENV_IPC_HELLO_TIMEOUT_MS, "500"),
            (ENV_IPC_IDLE_TIMEOUT_MS, " 7500 "),
            (ENV_IPC_MAX_CONNECTIONS, "4"),
        ])
        .unwrap();
        assert_eq!(config.hello_timeout, Duration::from_millis(500));
        assert_eq!(config.idle_timeout, Duration::from_millis(7500));
        assert_eq!(config.max_connections, 4);

        let partial = parse(&[(ENV_IPC_MAX_CONNECTIONS, "1")]).unwrap();
        assert_eq!(
            partial.hello_timeout,
            IpcServerConfig::default().hello_timeout
        );
        assert_eq!(partial.max_connections, 1);
    }

    #[test]
    fn invalid_values_are_rejected_with_the_variable_name() {
        for (name, bad) in [
            (ENV_IPC_HELLO_TIMEOUT_MS, "abc"),
            (ENV_IPC_IDLE_TIMEOUT_MS, "-5"),
            (ENV_IPC_IDLE_TIMEOUT_MS, "1.5"),
            (ENV_IPC_MAX_CONNECTIONS, "many"),
            (ENV_IPC_HELLO_TIMEOUT_MS, "99999999999999999999999"),
        ] {
            let msg = message(parse(&[(name, bad)]));
            assert!(msg.contains(name), "'{msg}' should name {name}");
        }
    }

    #[test]
    fn out_of_range_values_are_rejected() {
        for (name, bad, field) in [
            (ENV_IPC_HELLO_TIMEOUT_MS, "0", "hello_timeout"),
            (ENV_IPC_IDLE_TIMEOUT_MS, "0", "idle_timeout"),
            (ENV_IPC_IDLE_TIMEOUT_MS, "1999", "idle_timeout"),
            (ENV_IPC_IDLE_TIMEOUT_MS, "3600001", "idle_timeout"),
            (ENV_IPC_MAX_CONNECTIONS, "0", "max_connections"),
            (ENV_IPC_MAX_CONNECTIONS, "4097", "max_connections"),
        ] {
            let msg = message(parse(&[(name, bad)]));
            assert!(msg.contains(field), "'{msg}' should mention {field}");
        }
        // Boundary values are accepted.
        parse(&[
            (ENV_IPC_HELLO_TIMEOUT_MS, "1"),
            (ENV_IPC_IDLE_TIMEOUT_MS, "3600000"),
            (ENV_IPC_MAX_CONNECTIONS, "4096"),
        ])
        .unwrap();
        // The idle minimum is twice the plugin heartbeat interval (1000 ms).
        parse(&[(ENV_IPC_IDLE_TIMEOUT_MS, "2000")]).unwrap();
    }

    #[test]
    fn validate_checks_programmatic_configs() {
        assert!(IpcServerConfig::default().validate().is_ok());
        let zero = IpcServerConfig {
            idle_timeout: Duration::ZERO,
            ..IpcServerConfig::default()
        };
        assert!(zero.validate().is_err());
        let no_slots = IpcServerConfig {
            max_connections: 0,
            ..IpcServerConfig::default()
        };
        assert!(no_slots.validate().is_err());
        let below_heartbeat = IpcServerConfig {
            idle_timeout: Duration::from_millis(1999),
            ..IpcServerConfig::default()
        };
        assert!(below_heartbeat.validate().is_err());
    }

    #[test]
    fn adapter_constructors_validate_the_config() {
        use kotoha_core::infrastructure::IpcServerAdapter;
        let bad = IpcServerConfig {
            max_connections: 0,
            ..IpcServerConfig::default()
        };
        assert!(IpcServerAdapter::with_config("/tmp/x.sock", bad).is_err());
        assert!(IpcServerAdapter::from_env_with_config(bad).is_err());
        let short_idle = IpcServerConfig {
            idle_timeout: Duration::from_millis(500),
            ..IpcServerConfig::default()
        };
        assert!(IpcServerAdapter::with_config("/tmp/x.sock", short_idle).is_err());
        assert!(IpcServerAdapter::with_config("/tmp/x.sock", IpcServerConfig::default()).is_ok());
    }
}

#[cfg(unix)]
mod liveness {
    //! Liveness: Hello timeout, idle timeout, superseded connections, connection limit and
    //! disconnect reasons (DESIGN.md Section 3.2).
    #![allow(clippy::panic, clippy::unwrap_used)]

    use std::time::Duration;

    use kotoha_core::domain::{AudioInputEvent, DisconnectReason};
    use std::sync::{Arc, Mutex};

    use kotoha_core::infrastructure::{
        connect_local_socket, IpcDiagnostic, IpcServerAdapter, IpcServerConfig,
    };
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
            // Unchecked: these tests deliberately use idle timeouts below the production minimum.
            let server = IpcServerAdapter::with_config_unchecked(endpoint.clone(), config);
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
        // Channel capacity 1 and a consumer that stalls for 1000ms (> idle_timeout 600ms) while the
        // client keeps sending Heartbeats every 50ms: time blocked on the consumer is not idle time.
        // This only exercises select ordering; the deadline reset itself is covered by
        // `idle_deadline_restarts_after_a_blocked_event_send`. Real socket I/O cannot run under
        // `tokio::time::pause` (auto-advance would fire timers while I/O is pending), so the
        // timing tests use real time with generous margins instead.
        let mut h = Harness::start_with_capacity("slow", config(1000, 600, 16), 1).await;
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
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut seen = 0;
        while let Ok(Some(ev)) =
            tokio::time::timeout(Duration::from_millis(500), h.events.recv()).await
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

    #[tokio::test]
    async fn idle_deadline_restarts_after_a_blocked_event_send() {
        // The consumer stalls for longer than `idle_timeout` while the server is blocked inside
        // `tx.send(..)` for a Heartbeat. Once the send completes the idle window must restart from
        // that moment: the client was not silent, the server was busy. If the deadline were
        // computed before the send (or not reset), the connection would be dropped immediately
        // after the send, however the `select!` branches are ordered.
        let idle = Duration::from_millis(1500);
        let mut h = Harness::start_with_capacity("reset", config(1000, 1500, 16), 1).await;
        let mut c = connect_local_socket(&h.endpoint).await.expect("connect");
        c.write_all(&hello("mumble")).await.expect("hello");
        // `SourceConnected` now occupies the only channel slot; nobody consumes it yet.
        c.write_all(&encode_frame(&Message::Heartbeat).expect("hb"))
            .await
            .expect("heartbeat");
        // The server reads the Heartbeat and blocks in `send`. Stall past the idle timeout.
        tokio::time::sleep(idle + Duration::from_millis(500)).await;

        assert!(matches!(
            h.next_event().await,
            AudioInputEvent::SourceConnected { .. }
        ));
        assert!(matches!(
            h.next_event().await,
            AudioInputEvent::Heartbeat { .. }
        ));
        // The send has now completed. The client stays silent, but well within a fresh idle
        // window (idle_timeout 1500ms, observed for 500ms) the connection must still be alive.
        let early = tokio::time::timeout(Duration::from_millis(500), h.events.recv()).await;
        assert!(
            early.is_err(),
            "connection dropped right after the blocked send: {early:?}"
        );
        // ...and the restarted window does eventually expire.
        match h.next_event().await {
            AudioInputEvent::SourceDisconnected { reason, .. } => {
                assert_eq!(reason, DisconnectReason::IdleTimeout);
            }
            other => panic!("unexpected event {other:?}"),
        }
        h.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn near_simultaneous_hello_leaves_a_live_owner() {
        use kotoha_core::application::SourceSessionManager;

        let mut h = Harness::start("race", config(2000, 5000, 256)).await;
        let mut mgr = SourceSessionManager::new();
        for i in 0..3000u32 {
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
        let mut h = Harness::start("hb", config(1000, 600, 16)).await;
        let mut c = connect_local_socket(&h.endpoint).await.expect("connect");
        c.write_all(&hello("mumble")).await.expect("hello");
        let hb = encode_frame(&Message::Heartbeat).expect("hb");
        for _ in 0..8 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            c.write_all(&hb).await.expect("heartbeat");
        }
        // 800ms elapsed (> idle_timeout 600ms) but only Connected + Heartbeat events, no disconnect.
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
    async fn panicking_diagnostics_hook_does_not_skip_disconnect_cleanup() {
        let endpoint = super::common::test_endpoint("live_panic_close");
        let server =
            IpcServerAdapter::with_config_unchecked(endpoint.clone(), config(1000, 5000, 4))
                .with_diagnostics(|_| panic!("hook panic"));
        let (tx, mut events) = mpsc::channel(64);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (ready_tx, ready_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            server
                .run(tx, shutdown_rx, Some(ready_tx))
                .await
                .expect("server run");
        });
        ready_rx.await.expect("ready");

        let mut client = connect_local_socket(&endpoint).await.expect("client");
        client.write_all(&hello("mumble")).await.expect("hello");
        assert!(matches!(
            events.recv().await.expect("SourceConnected"),
            AudioInputEvent::SourceConnected { .. }
        ));
        drop(client);
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("SourceDisconnected within 1s")
            .expect("event channel open");
        assert!(matches!(event, AudioInputEvent::SourceDisconnected { .. }));

        // The owner registry was cleaned up: the same source can reconnect and is not superseded.
        let mut again = connect_local_socket(&endpoint).await.expect("again");
        again.write_all(&hello("mumble")).await.expect("hello");
        assert!(matches!(
            events.recv().await.expect("SourceConnected"),
            AudioInputEvent::SourceConnected { .. }
        ));

        let _ = shutdown.send(true);
        task.await.expect("server task");
    }

    #[tokio::test]
    async fn panicking_hook_on_refused_connection_does_not_stop_the_server() {
        let endpoint = super::common::test_endpoint("live_panic_refuse");
        let server =
            IpcServerAdapter::with_config_unchecked(endpoint.clone(), config(5000, 5000, 1))
                .with_diagnostics(|d| {
                    if matches!(d, IpcDiagnostic::ConnectionRefused { .. }) {
                        panic!("hook panic");
                    }
                });
        let (tx, mut events) = mpsc::channel(64);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (ready_tx, ready_rx) = oneshot::channel();
        let task = tokio::spawn(async move { server.run(tx, shutdown_rx, Some(ready_tx)).await });
        ready_rx.await.expect("ready");

        let mut first = connect_local_socket(&endpoint).await.expect("first");
        first.write_all(&hello("mumble")).await.expect("hello");
        let _ = events.recv().await.expect("SourceConnected");
        // Over the limit: refused, and the panicking hook must not kill the accept loop.
        let mut refused = connect_local_socket(&endpoint).await.expect("refused");
        expect_closed(&mut refused, Duration::from_secs(5)).await;
        assert!(!task.is_finished(), "server stopped after hook panic");

        // The server still accepts after a slot frees up.
        drop(first);
        let _ = events.recv().await.expect("SourceDisconnected");
        let mut second = connect_local_socket(&endpoint).await.expect("second");
        second.write_all(&hello("ts3")).await.expect("hello");
        assert!(matches!(
            events.recv().await.expect("SourceConnected"),
            AudioInputEvent::SourceConnected { .. }
        ));

        let _ = shutdown.send(true);
        task.await.expect("task").expect("server run");
    }

    #[tokio::test]
    async fn closed_and_refused_connections_are_reported_to_the_diagnostics_hook() {
        let endpoint = super::common::test_endpoint("live_diag");
        let log: Arc<Mutex<Vec<IpcDiagnostic>>> = Arc::default();
        let sink = Arc::clone(&log);
        let server = IpcServerAdapter::with_config(endpoint.clone(), config(1000, 5000, 2))
            .unwrap()
            .with_diagnostics(move |d| sink.lock().unwrap().push(d.clone()));
        let (tx, mut events) = mpsc::channel(64);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (ready_tx, ready_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            server
                .run(tx, shutdown_rx, Some(ready_tx))
                .await
                .expect("server run");
        });
        ready_rx.await.expect("ready");

        // One connection completes the handshake, one stays silent (hello timeout).
        let mut named = connect_local_socket(&endpoint).await.expect("named");
        named.write_all(&hello("mumble")).await.expect("hello");
        let mut silent = connect_local_socket(&endpoint).await.expect("silent");
        let _ = events.recv().await.expect("SourceConnected");
        // Both slots are taken: the third connection is refused and dropped.
        let mut refused = connect_local_socket(&endpoint).await.expect("refused");
        expect_closed(&mut refused, Duration::from_secs(5)).await;
        expect_closed(&mut silent, Duration::from_secs(5)).await;
        drop(named);

        let snapshot = loop {
            let snapshot = log.lock().unwrap().clone();
            let closed = snapshot
                .iter()
                .filter(|d| matches!(d, IpcDiagnostic::ConnectionClosed { .. }))
                .count();
            if closed >= 2 {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert!(
            snapshot.contains(&IpcDiagnostic::ConnectionRefused {
                active: 2,
                max_connections: 2
            }),
            "missing refusal in {snapshot:?}"
        );
        assert!(
            snapshot.iter().any(|d| matches!(
                d,
                IpcDiagnostic::ConnectionClosed { source: None, reason, .. }
                    if *reason == DisconnectReason::HelloTimeout
            )),
            "missing hello timeout in {snapshot:?}"
        );
        assert!(
            snapshot.iter().any(|d| matches!(
                d,
                IpcDiagnostic::ConnectionClosed { source: Some(s), reason, .. }
                    if s == "mumble" && *reason == DisconnectReason::Eof
            )),
            "missing EOF of the named connection in {snapshot:?}"
        );
        let text = snapshot.iter().map(ToString::to_string).collect::<String>();
        assert!(text.contains("limit of 2") && text.contains("HelloTimeout"));

        let _ = shutdown.send(true);
        task.await.expect("server task");
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

/// Security tests for Unix IPC endpoint handling (issue #23): the server must never delete
/// anything at the endpoint path except a stale socket it owns, and must refuse insecure
/// per-user private directories.
#[cfg(unix)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod endpoint_security {
    use std::os::unix::fs::{DirBuilderExt as _, FileTypeExt as _, PermissionsExt as _};
    use std::os::unix::net::UnixListener as StdUnixListener;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use kotoha_core::domain::{AudioInputEvent, CoreError};
    use kotoha_core::infrastructure::IpcServerAdapter;
    use tokio::sync::{mpsc, oneshot, watch};

    /// Uid used for "owned by someone else" checks when the tests run as root.
    const OTHER_UID: u32 = 65_534;

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Short, unique scratch directory under `/tmp` (keeps socket paths well below `sun_path` limits).
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = PathBuf::from(format!("/tmp/kt-sec-{}-{tag}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&dir)
                .expect("create scratch dir");
            Self(dir)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn running_as_root() -> bool {
        kotoha_protocol::current_euid() == 0
    }

    /// A running server; dropping it without `stop` just leaks the task (tests always stop).
    struct Running {
        shutdown_tx: watch::Sender<bool>,
        task: tokio::task::JoinHandle<Result<(), CoreError>>,
        _event_rx: mpsc::Receiver<AudioInputEvent>,
    }

    impl Running {
        async fn stop(self) {
            let _ = self.shutdown_tx.send(true);
            let res = tokio::time::timeout(Duration::from_secs(5), self.task)
                .await
                .expect("server stops in time")
                .expect("server task joined");
            res.expect("server exits cleanly");
        }
    }

    /// Starts `server`. Returns `Ok(Running)` once it is ready, or the error `run` failed with.
    async fn start(server: IpcServerAdapter) -> Result<Running, CoreError> {
        let (event_tx, event_rx) = mpsc::channel::<AudioInputEvent>(16);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (ready_tx, ready_rx) = oneshot::channel();
        let mut task =
            tokio::spawn(async move { server.run(event_tx, shutdown_rx, Some(ready_tx)).await });

        // `run` drops `ready_tx` without sending when binding fails, so a closed channel means the
        // task has finished (or is about to) with the bind error.
        if ready_rx.await.is_ok() {
            return Ok(Running {
                shutdown_tx,
                task,
                _event_rx: event_rx,
            });
        }
        let res = tokio::time::timeout(Duration::from_secs(5), &mut task)
            .await
            .expect("server task finishes after a failed bind")
            .expect("server task joined");
        match res {
            Ok(()) => panic!("server exited before becoming ready without an error"),
            Err(err) => Err(err),
        }
    }

    fn expect_ipc_error(res: Result<Running, CoreError>, needle: &str) -> String {
        match res {
            Ok(_) => panic!("server unexpectedly started (expected error containing '{needle}')"),
            Err(CoreError::Ipc(msg)) => {
                assert!(
                    msg.contains(needle),
                    "error '{msg}' does not contain '{needle}'"
                );
                msg
            }
            Err(other) => panic!("unexpected error kind: {other:?}"),
        }
    }

    /// Every private-directory refusal must tell the user how to pick another location.
    fn assert_mentions_workaround(msg: &str) {
        assert!(
            msg.contains("KOTOHA_IPC_ENDPOINT") && msg.contains("XDG_RUNTIME_DIR"),
            "error '{msg}' should suggest KOTOHA_IPC_ENDPOINT or XDG_RUNTIME_DIR"
        );
    }

    fn endpoint_str(path: &Path) -> String {
        path.to_str().expect("utf-8 path").to_owned()
    }

    fn is_socket(path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket())
    }

    #[tokio::test]
    async fn refuses_to_delete_regular_file_at_endpoint() {
        let scratch = Scratch::new("file");
        let endpoint = scratch.path("k.sock");
        std::fs::write(&endpoint, b"important user data").unwrap();

        let res = start(IpcServerAdapter::new(endpoint_str(&endpoint))).await;
        expect_ipc_error(res, "existing path is a regular file, not a socket");

        assert_eq!(std::fs::read(&endpoint).unwrap(), b"important user data");
    }

    #[tokio::test]
    async fn refuses_to_delete_directory_at_endpoint() {
        let scratch = Scratch::new("dir");
        let endpoint = scratch.path("k.sock");
        std::fs::create_dir(&endpoint).unwrap();
        std::fs::write(endpoint.join("keep"), b"x").unwrap();

        let res = start(IpcServerAdapter::new(endpoint_str(&endpoint))).await;
        expect_ipc_error(res, "existing path is a directory, not a socket");

        assert!(endpoint.join("keep").exists());
    }

    #[tokio::test]
    async fn refuses_to_delete_symlink_at_endpoint() {
        let scratch = Scratch::new("link");
        let target = scratch.path("target.txt");
        std::fs::write(&target, b"target").unwrap();
        let endpoint = scratch.path("k.sock");
        std::os::unix::fs::symlink(&target, &endpoint).unwrap();

        let res = start(IpcServerAdapter::new(endpoint_str(&endpoint))).await;
        expect_ipc_error(res, "existing path is a symlink, not a socket");

        let meta = std::fs::symlink_metadata(&endpoint).unwrap();
        assert!(meta.file_type().is_symlink());
        assert_eq!(std::fs::read(&target).unwrap(), b"target");
    }

    #[tokio::test]
    async fn refuses_to_delete_dangling_symlink_at_endpoint() {
        let scratch = Scratch::new("dangle");
        let endpoint = scratch.path("k.sock");
        std::os::unix::fs::symlink(scratch.path("missing"), &endpoint).unwrap();

        let res = start(IpcServerAdapter::new(endpoint_str(&endpoint))).await;
        expect_ipc_error(res, "existing path is a symlink, not a socket");
        assert!(std::fs::symlink_metadata(&endpoint).is_ok());
    }

    #[tokio::test]
    async fn reuses_stale_socket_owned_by_current_user() {
        let scratch = Scratch::new("stale");
        let endpoint = scratch.path("k.sock");
        drop(StdUnixListener::bind(&endpoint).unwrap());
        assert!(is_socket(&endpoint), "stale socket file remains after drop");

        let running = start(IpcServerAdapter::new(endpoint_str(&endpoint)))
            .await
            .expect("stale own socket is replaced");
        assert!(is_socket(&endpoint));
        let mode = std::fs::metadata(&endpoint).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        running.stop().await;

        assert!(
            std::fs::symlink_metadata(&endpoint).is_err(),
            "own socket is removed on shutdown"
        );
    }

    #[tokio::test]
    async fn refuses_live_socket_and_leaves_it_in_place() {
        let scratch = Scratch::new("live");
        let endpoint = scratch.path("k.sock");
        let live = StdUnixListener::bind(&endpoint).unwrap();

        let res = start(IpcServerAdapter::new(endpoint_str(&endpoint))).await;
        expect_ipc_error(res, "already in use");
        assert!(is_socket(&endpoint));
        drop(live);
    }

    #[tokio::test]
    async fn refuses_stale_socket_owned_by_another_user() {
        if !running_as_root() {
            eprintln!("skipping: changing socket ownership requires root");
            return;
        }
        let scratch = Scratch::new("foreign");
        let endpoint = scratch.path("k.sock");
        drop(StdUnixListener::bind(&endpoint).unwrap());
        std::os::unix::fs::lchown(&endpoint, Some(OTHER_UID), None).unwrap();

        let res = start(IpcServerAdapter::new(endpoint_str(&endpoint))).await;
        expect_ipc_error(res, "owned by uid 65534");
        assert!(is_socket(&endpoint));
    }

    #[tokio::test]
    async fn shutdown_does_not_delete_a_path_replaced_after_bind() {
        let scratch = Scratch::new("swap");
        let endpoint = scratch.path("k.sock");
        let running = start(IpcServerAdapter::new(endpoint_str(&endpoint)))
            .await
            .expect("server starts");

        std::fs::remove_file(&endpoint).unwrap();
        std::fs::write(&endpoint, b"replacement").unwrap();
        running.stop().await;

        assert_eq!(std::fs::read(&endpoint).unwrap(), b"replacement");
    }

    #[tokio::test]
    async fn private_dir_is_created_with_mode_0700() {
        let scratch = Scratch::new("mk");
        let dir = scratch.path("kotoha-x");
        let endpoint = dir.join("kotoha_v1.sock");

        let running = start(IpcServerAdapter::new(endpoint_str(&endpoint)).with_private_dir(&dir))
            .await
            .expect("server creates private dir");
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o700);
        assert!(is_socket(&endpoint));
        running.stop().await;
    }

    #[tokio::test]
    async fn existing_private_dir_with_0700_is_accepted() {
        let scratch = Scratch::new("ok");
        let dir = scratch.path("kotoha-x");
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let endpoint = dir.join("kotoha_v1.sock");

        let running = start(IpcServerAdapter::new(endpoint_str(&endpoint)).with_private_dir(&dir))
            .await
            .expect("0700 private dir is accepted");
        running.stop().await;
    }

    #[tokio::test]
    async fn private_dir_with_group_or_other_bits_is_refused() {
        for mode in [0o750, 0o705, 0o770, 0o777, 0o1777, 0o710] {
            let scratch = Scratch::new("perm");
            let dir = scratch.path("kotoha-x");
            std::fs::create_dir(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
            let endpoint = dir.join("kotoha_v1.sock");

            let res =
                start(IpcServerAdapter::new(endpoint_str(&endpoint)).with_private_dir(&dir)).await;
            let msg = expect_ipc_error(res, "grant group/other access");
            assert_mentions_workaround(&msg);
            assert!(
                std::fs::symlink_metadata(&endpoint).is_err(),
                "no socket is created in an insecure dir (mode {mode:o})"
            );
            // The server must not "fix" a directory it does not trust.
            let after = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o7777;
            assert_eq!(after, mode);
        }
    }

    #[tokio::test]
    async fn private_dir_that_is_a_symlink_is_refused() {
        let scratch = Scratch::new("dlink");
        let real = scratch.path("real");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&real)
            .unwrap();
        let dir = scratch.path("kotoha-x");
        std::os::unix::fs::symlink(&real, &dir).unwrap();
        let endpoint = dir.join("kotoha_v1.sock");

        let res =
            start(IpcServerAdapter::new(endpoint_str(&endpoint)).with_private_dir(&dir)).await;
        let msg = expect_ipc_error(res, "not a directory");
        assert_mentions_workaround(&msg);
        assert!(std::fs::read_dir(&real).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn private_dir_that_is_a_regular_file_is_refused() {
        let scratch = Scratch::new("dfile");
        let dir = scratch.path("kotoha-x");
        std::fs::write(&dir, b"not a dir").unwrap();
        let endpoint = dir.join("kotoha_v1.sock");

        let res =
            start(IpcServerAdapter::new(endpoint_str(&endpoint)).with_private_dir(&dir)).await;
        let msg = expect_ipc_error(res, "not a directory");
        assert_mentions_workaround(&msg);
        assert_eq!(std::fs::read(&dir).unwrap(), b"not a dir");
    }

    #[tokio::test]
    async fn private_dir_owned_by_another_user_is_refused() {
        if !running_as_root() {
            eprintln!("skipping: changing directory ownership requires root");
            return;
        }
        let scratch = Scratch::new("downer");
        let dir = scratch.path("kotoha-x");
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        std::os::unix::fs::chown(&dir, Some(OTHER_UID), None).unwrap();
        let endpoint = dir.join("kotoha_v1.sock");

        let res =
            start(IpcServerAdapter::new(endpoint_str(&endpoint)).with_private_dir(&dir)).await;
        let msg = expect_ipc_error(res, "owned by uid 65534");
        assert_mentions_workaround(&msg);
    }

    #[test]
    fn from_env_derives_private_dir_only_for_fallback_rule() {
        // Pure check of the adapter wiring without mutating the process environment.
        let resolved = kotoha_protocol::resolve_endpoint_details();
        let server = IpcServerAdapter::from_env();
        assert_eq!(server.endpoint(), resolved.endpoint);
        assert_eq!(
            server.private_dir().map(|p| p.to_str().unwrap().to_owned()),
            resolved.private_dir
        );
        assert!(IpcServerAdapter::new("/tmp/x.sock").private_dir().is_none());

        let cfg_server =
            IpcServerAdapter::from_env_with_config(kotoha_core::infrastructure::IpcServerConfig {
                max_connections: 3,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(cfg_server.endpoint(), resolved.endpoint);
        assert_eq!(
            cfg_server
                .private_dir()
                .map(|p| p.to_str().unwrap().to_owned()),
            resolved.private_dir
        );
    }

    #[tokio::test]
    async fn private_dir_composes_with_explicit_config() {
        let scratch = Scratch::new("cfg");
        let dir = scratch.path("kotoha-x");
        let endpoint = dir.join("kotoha_v1.sock");
        let config = kotoha_core::infrastructure::IpcServerConfig {
            max_connections: 2,
            ..Default::default()
        };
        let server = IpcServerAdapter::with_config(endpoint_str(&endpoint), config)
            .unwrap()
            .with_private_dir(&dir);
        assert_eq!(server.private_dir(), Some(dir.as_path()));

        let running = start(server)
            .await
            .expect("server starts with config + dir");
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o700);
        assert!(is_socket(&endpoint));
        running.stop().await;
    }
}
