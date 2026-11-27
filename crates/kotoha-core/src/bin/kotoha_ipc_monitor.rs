//! Phase 1 Presentation CLI (`kotoha-ipc-monitor`) for cross-language IPC diagnostics and E2E.

use std::process::ExitCode;

use kotoha_core::application::{SeqStatus, SessionAction, SourceSessionManager};
use kotoha_core::domain::AudioInputEvent;
use kotoha_core::infrastructure::{AudioPreprocessor, IpcServerAdapter};
use kotoha_protocol::resolve_endpoint;
use serde::Serialize;
use tokio::sync::{mpsc, oneshot, watch};

struct CliArgs {
    endpoint: String,
    max_events: Option<usize>,
    json: bool,
}

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum MonitorEvent<'a> {
    Hello {
        conn_id: u64,
        source: &'a str,
        plugin_version: &'a str,
        superseded_conn_id: Option<u64>,
    },
    Disconnected {
        conn_id: u64,
        source: &'a str,
        cleared_speakers: usize,
        reason: String,
    },
    SourceReset {
        conn_id: u64,
        source: &'a str,
        cleared_speakers: usize,
    },
    UserInfo {
        source: &'a str,
        user_id: u32,
        name: &'a str,
    },
    UserRemoved {
        source: &'a str,
        user_id: u32,
    },
    TalkingState {
        source: &'a str,
        user_id: u32,
        state: &'static str,
    },
    AudioFrame {
        source: &'a str,
        user_id: u32,
        seq: u32,
        sample_rate: u32,
        channels: u16,
        is_local_user: bool,
        samples: usize,
        samples_16k_mono: usize,
        rms: f32,
        seq_status: &'static str,
        dropped_frames: u32,
        total_dropped: u64,
    },
    Heartbeat {
        conn_id: u64,
        source: &'a str,
    },
}

fn parse_args() -> Result<CliArgs, String> {
    let mut endpoint: Option<String> = None;
    let mut max_events: Option<usize> = None;
    let mut json = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--endpoint" => {
                let val = args
                    .next()
                    .ok_or_else(|| "--endpoint requires a path/pipe value".to_owned())?;
                endpoint = Some(val);
            }
            "--max-events" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "--max-events requires a positive integer".to_owned())?;
                let parsed = raw
                    .parse::<usize>()
                    .map_err(|_| format!("invalid --max-events value '{raw}'"))?;
                max_events = Some(parsed);
            }
            "--json" => json = true,
            "-h" | "--help" => {
                return Err(
                    "Usage: kotoha-ipc-monitor [--endpoint <path>] [--max-events <n>] [--json]"
                        .to_owned(),
                );
            }
            other => return Err(format!("unknown argument '{other}'")),
        }
    }

    Ok(CliArgs {
        endpoint: endpoint.unwrap_or_else(resolve_endpoint),
        max_events,
        json,
    })
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match parse_args() {
        Ok(cfg) => cfg,
        Err(msg) => {
            eprintln!("{msg}");
            return if msg.starts_with("Usage:") {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            };
        }
    };

    let server = IpcServerAdapter::new(cli.endpoint.clone());
    let (event_tx, mut event_rx) = mpsc::channel::<AudioInputEvent>(256);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (ready_tx, ready_rx) = oneshot::channel();

    let server_handle =
        tokio::spawn(async move { server.run(event_tx, shutdown_rx, Some(ready_tx)).await });

    if ready_rx.await.is_err() {
        eprintln!("IPC server failed before becoming ready");
        return ExitCode::FAILURE;
    }

    if !cli.json {
        eprintln!("kotoha-ipc-monitor listening on {}", cli.endpoint);
    }

    let shutdown_ctrl = shutdown_tx.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = shutdown_ctrl.send(true);
        }
    });

    let mut session = SourceSessionManager::new();
    let mut processed = 0usize;

    while let Some(event) = event_rx.recv().await {
        let action = session.handle_event(&event);
        emit_event(&event, &action, &session, cli.json);
        processed += 1;

        if let Some(limit) = cli.max_events {
            if processed >= limit {
                let _ = shutdown_tx.send(true);
                break;
            }
        }
    }

    let _ = shutdown_tx.send(true);
    match server_handle.await {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(err)) => {
            eprintln!("IPC server error: {err}");
            ExitCode::FAILURE
        }
        Err(join_err) => {
            eprintln!("IPC server task error: {join_err}");
            ExitCode::FAILURE
        }
    }
}

fn emit_event(
    event: &AudioInputEvent,
    action: &SessionAction,
    session: &SourceSessionManager,
    json: bool,
) {
    match event {
        AudioInputEvent::SourceConnected {
            conn_id,
            source,
            plugin_version,
        } => emit_hello(*conn_id, source, plugin_version, action, json),
        AudioInputEvent::SourceDisconnected {
            conn_id,
            source,
            reason,
        } => {
            emit_cleared("DISCONNECT", *conn_id, source, action, json, Some(reason));
        }
        AudioInputEvent::SourceReset { conn_id, source } => {
            emit_cleared("SOURCE_RESET", *conn_id, source, action, json, None);
        }
        AudioInputEvent::UserInfoUpdated {
            conn_id: _,
            speaker_key,
            name,
        } => {
            if json {
                print_json(&MonitorEvent::UserInfo {
                    source: &speaker_key.source,
                    user_id: speaker_key.user_id,
                    name,
                });
            } else {
                println!(
                    "[USER_INFO] source={} user_id={} name={name:?}",
                    speaker_key.source, speaker_key.user_id
                );
            }
        }
        AudioInputEvent::UserRemoved {
            conn_id: _,
            speaker_key,
        } => {
            if json {
                print_json(&MonitorEvent::UserRemoved {
                    source: &speaker_key.source,
                    user_id: speaker_key.user_id,
                });
            } else {
                println!(
                    "[USER_REMOVED] source={} user_id={}",
                    speaker_key.source, speaker_key.user_id
                );
            }
        }
        AudioInputEvent::TalkingStateChanged {
            conn_id: _,
            speaker_key,
            state,
        } => {
            let state_str = match state {
                kotoha_core::domain::TalkingState::Passive => "passive",
                kotoha_core::domain::TalkingState::Talking => "talking",
                kotoha_core::domain::TalkingState::Whispering => "whispering",
                kotoha_core::domain::TalkingState::Shouting => "shouting",
                kotoha_core::domain::TalkingState::TalkingMuted => "talking_muted",
            };
            if json {
                print_json(&MonitorEvent::TalkingState {
                    source: &speaker_key.source,
                    user_id: speaker_key.user_id,
                    state: state_str,
                });
            } else {
                println!(
                    "[TALKING] source={} user_id={} state={state_str}",
                    speaker_key.source, speaker_key.user_id
                );
            }
        }
        AudioInputEvent::AudioFrame { conn_id: _, chunk } => {
            emit_audio_frame(chunk, action, session, json);
        }
        AudioInputEvent::Heartbeat { conn_id, source } => {
            if json {
                print_json(&MonitorEvent::Heartbeat {
                    conn_id: *conn_id,
                    source,
                });
            } else {
                println!("[HEARTBEAT] conn_id={conn_id} source={source}");
            }
        }
    }
}

fn emit_hello(
    conn_id: u64,
    source: &str,
    plugin_version: &str,
    action: &SessionAction,
    json: bool,
) {
    let superseded_conn_id = match action {
        SessionAction::Connected {
            superseded_conn_id, ..
        } => *superseded_conn_id,
        _ => None,
    };
    if json {
        print_json(&MonitorEvent::Hello {
            conn_id,
            source,
            plugin_version,
            superseded_conn_id,
        });
    } else {
        println!(
            "[HELLO] conn_id={conn_id} source={source} plugin_version={plugin_version} superseded={superseded_conn_id:?}"
        );
    }
}

fn emit_cleared(
    label: &str,
    conn_id: u64,
    source: &str,
    action: &SessionAction,
    json: bool,
    disconnect_reason: Option<&kotoha_core::domain::DisconnectReason>,
) {
    let cleared_speakers = match action {
        SessionAction::ClearSpeakers {
            removed_speakers, ..
        } => removed_speakers.len(),
        _ => 0,
    };
    if json {
        if let Some(reason) = disconnect_reason {
            print_json(&MonitorEvent::Disconnected {
                conn_id,
                source,
                cleared_speakers,
                reason: reason_slug(reason),
            });
        } else {
            print_json(&MonitorEvent::SourceReset {
                conn_id,
                source,
                cleared_speakers,
            });
        }
    } else if let Some(reason) = disconnect_reason {
        println!("[{label}] conn_id={conn_id} source={source} cleared_speakers={cleared_speakers} reason={reason:?}");
    } else {
        println!("[{label}] conn_id={conn_id} source={source} cleared_speakers={cleared_speakers}");
    }
}

fn emit_audio_frame(
    chunk: &kotoha_core::domain::AudioChunk,
    action: &SessionAction,
    session: &SourceSessionManager,
    json: bool,
) {
    let rms = AudioPreprocessor::compute_rms(&chunk.samples);
    let samples_16k_mono =
        AudioPreprocessor::process_chunk(chunk).map_or(0, |resampled| resampled.len());
    let (seq_status, dropped_frames) = match action {
        SessionAction::AudioSequenceChecked(SeqStatus::Initialized { .. }) => ("init", 0),
        SessionAction::AudioSequenceChecked(SeqStatus::InOrder) => ("ok", 0),
        SessionAction::AudioSequenceChecked(SeqStatus::Dropped { dropped_frames }) => {
            ("dropped", *dropped_frames)
        }
        SessionAction::AudioSequenceChecked(SeqStatus::SequenceAnomaly { .. }) => ("anomaly", 0),
        _ => ("unknown", 0),
    };
    let total_dropped = session
        .source_stats(&chunk.speaker_key.source)
        .map_or(0, |s| s.frames_dropped);
    if json {
        print_json(&MonitorEvent::AudioFrame {
            source: &chunk.speaker_key.source,
            user_id: chunk.speaker_key.user_id,
            seq: chunk.seq,
            sample_rate: chunk.sample_rate,
            channels: chunk.channels,
            is_local_user: chunk.is_local_user,
            samples: chunk.samples.len(),
            samples_16k_mono,
            rms,
            seq_status,
            dropped_frames,
            total_dropped,
        });
    } else {
        println!(
            "[AUDIO] source={} user_id={} seq={} {}Hz {}ch local={} samples={} 16k_mono={} rms={rms:.4} seq_status={seq_status} dropped={dropped_frames} total_dropped={total_dropped}",
            chunk.speaker_key.source,
            chunk.speaker_key.user_id,
            chunk.seq,
            chunk.sample_rate,
            chunk.channels,
            chunk.is_local_user,
            chunk.samples.len(),
            samples_16k_mono
        );
    }
}

fn print_json<T: Serialize>(value: &T) {
    if let Ok(line) = serde_json::to_string(value) {
        println!("{line}");
    }
}

/// Stable `snake_case` identifier for a disconnect reason (used in JSON output).
fn reason_slug(reason: &kotoha_core::domain::DisconnectReason) -> String {
    use kotoha_core::domain::DisconnectReason as R;
    match reason {
        R::Eof => "eof".to_owned(),
        R::IoError(_) => "io_error".to_owned(),
        R::Protocol(_) => "protocol".to_owned(),
        R::HelloTimeout => "hello_timeout".to_owned(),
        R::IdleTimeout => "idle_timeout".to_owned(),
        R::Superseded => "superseded".to_owned(),
        R::Shutdown => "shutdown".to_owned(),
    }
}
