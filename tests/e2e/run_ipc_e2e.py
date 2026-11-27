#!/usr/bin/env python3
"""Cross-language IPC v1 End-to-End test runner for Kotoha.

Verifies real OS IPC communication between the C++ client (`kotoha_mock_client`)
and the Rust server (`kotoha-ipc-monitor`) on Linux/macOS (Unix Domain Sockets)
and Windows (Named Pipes).

Test scenarios:
  1. Single-source lifecycle (`mumble`): hello, user_info, talking_state
     (talking & passive), >= 5 audio frames with zero drops and non-zero RMS,
     heartbeat, user_removed, source_reset / disconnected.
  2. Multi-source concurrent clients (`mumble` and `ts3` simultaneously)
     connected to the same `kotoha-ipc-monitor` instance, verifying zero
     cross-contamination between `SpeakerKey` (`source`, `user_id`) streams.
"""

from __future__ import annotations

import argparse
import json
import os
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[2]
IS_WINDOWS = os.name == "nt"


def executable_name(stem: str) -> str:
    return f"{stem}.exe" if IS_WINDOWS else stem


def find_binary(
    explicit_path: str | None,
    candidates: list[Path],
    path_names: list[str],
    label: str,
) -> Path:
    if explicit_path:
        path = Path(explicit_path).expanduser().resolve()
        if not path.is_file():
            raise FileNotFoundError(f"Specified {label} binary not found: {path}")
        return path

    existing = [p for p in candidates if p.is_file()]
    if existing:
        # Prefer the most recently modified binary when both Debug and Release exist locally.
        existing.sort(key=lambda p: p.stat().st_mtime, reverse=True)
        return existing[0]

    for name in path_names:
        resolved = shutil.which(name)
        if resolved:
            return Path(resolved).resolve()

    searched = ", ".join(str(p) for p in candidates)
    raise FileNotFoundError(
        f"Could not locate {label} binary. Searched: [{searched}] and PATH ({', '.join(path_names)})."
    )


def resolve_monitor_binary(repo_root: Path, explicit: str | None) -> Path:
    exe = executable_name("kotoha-ipc-monitor")
    candidates = [
        repo_root / "target" / "release" / exe,
        repo_root / "target" / "debug" / exe,
    ]
    return find_binary(explicit, candidates, [exe, "kotoha-ipc-monitor"], "kotoha-ipc-monitor")


def resolve_client_binary(repo_root: Path, explicit: str | None) -> Path:
    exe = executable_name("kotoha_mock_client")
    candidates = [
        repo_root / "plugins" / "build" / "tests" / "Release" / exe,
        repo_root / "plugins" / "build" / "tests" / exe,
        repo_root / "plugins" / "build" / "tests" / "RelWithDebInfo" / exe,
        repo_root / "plugins" / "build" / "tests" / "Debug" / exe,
        repo_root / "plugins" / "build" / "Release" / exe,
        repo_root / "plugins" / "build" / exe,
    ]
    return find_binary(explicit, candidates, [exe, "kotoha_mock_client"], "kotoha_mock_client")


def make_unique_endpoint(tag: str) -> str:
    pid = os.getpid()
    rand_hex = secrets.token_hex(4)
    if IS_WINDOWS:
        return rf"\\.\pipe\kotoha_e2e_{tag}_{pid}_{rand_hex}"

    # Strictly keep Unix socket path < 80 bytes to stay well below macOS 104-byte sun_path limit
    # even when $RUNNER_TEMP is very long.
    base_dir = Path("/tmp") if Path("/tmp").is_dir() else Path(tempfile.gettempdir())
    endpoint = str(base_dir / f"kt_e2e_{tag}_{pid}_{rand_hex}.sock")
    if len(endpoint.encode("utf-8")) >= 80:
        endpoint = f"/tmp/kt_{pid}_{rand_hex}.sock"
    assert len(endpoint.encode("utf-8")) < 80, f"Socket path too long: {endpoint}"
    return endpoint


def wait_for_listener_ready(
    monitor_proc: subprocess.Popen[str],
    endpoint: str,
    timeout_sec: float = 5.0,
) -> None:
    if IS_WINDOWS:
        # Avoid probing \\.\pipe\... with CreateFile/stat because opening a Named Pipe
        # instance can race with ConnectNamedPipe; verify the server stays alive briefly
        # and let kotoha_mock_client's built-in retry loop connect cleanly.
        end_wait = time.monotonic() + min(0.25, timeout_sec)
        while time.monotonic() < end_wait:
            rc = monitor_proc.poll()
            if rc is not None:
                raise RuntimeError(f"kotoha-ipc-monitor exited prematurely with code {rc}")
            time.sleep(0.02)
        return

    deadline = time.monotonic() + timeout_sec
    while time.monotonic() < deadline:
        rc = monitor_proc.poll()
        if rc is not None:
            raise RuntimeError(f"kotoha-ipc-monitor exited prematurely with code {rc}")
        if Path(endpoint).exists():
            return
        time.sleep(0.02)

    raise TimeoutError(f"Timed out waiting for Unix socket listener at {endpoint}")


class NdjsonMonitorReader:
    """Streams NDJSON lines from kotoha-ipc-monitor stdout in real time."""

    def __init__(self, proc: subprocess.Popen[str]) -> None:
        self._proc = proc
        self._lock = threading.Lock()
        self._cond = threading.Condition(self._lock)
        self._events: list[dict[str, Any]] = []
        self._parse_error: AssertionError | None = None
        self._eof = False
        self._thread = threading.Thread(target=self._read_loop, daemon=True)
        self._thread.start()

    def _read_loop(self) -> None:
        assert self._proc.stdout is not None
        try:
            for idx, raw_line in enumerate(self._proc.stdout, start=1):
                line = raw_line.strip()
                if not line:
                    continue
                try:
                    obj = json.loads(line)
                except json.JSONDecodeError as exc:
                    with self._cond:
                        self._parse_error = AssertionError(
                            f"Line {idx} is not valid JSON: {line!r} ({exc})"
                        )
                        self._cond.notify_all()
                    return
                if not isinstance(obj, dict) or "event" not in obj:
                    with self._cond:
                        self._parse_error = AssertionError(
                            f"Line {idx} missing 'event' field: {obj!r}"
                        )
                        self._cond.notify_all()
                    return
                with self._cond:
                    self._events.append(obj)
                    self._cond.notify_all()
        finally:
            with self._cond:
                self._eof = True
                self._cond.notify_all()

    def wait_for_sources_disconnected(self, sources: set[str], timeout_sec: float = 10.0) -> None:
        deadline = time.monotonic() + timeout_sec
        with self._cond:
            while True:
                if self._parse_error is not None:
                    raise self._parse_error
                disconnected = {
                    e.get("source")
                    for e in self._events
                    if e.get("event") == "disconnected"
                }
                if sources.issubset(disconnected):
                    return
                if self._eof:
                    return
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(
                        f"Timed out waiting for disconnected events for {sorted(sources)}; "
                        f"seen events={len(self._events)}, disconnected={sorted(s for s in disconnected if s)}"
                    )
                self._cond.wait(timeout=min(remaining, 0.1))

    def finish_and_collect(self, timeout_sec: float = 5.0) -> tuple[int, list[dict[str, Any]], str]:
        if self._proc.poll() is None:
            if not IS_WINDOWS:
                self._proc.send_signal(signal.SIGINT)
            else:
                # On Windows, terminate() exits the monitor cleanly after all expected events
                # have already been read from stdout.
                self._proc.terminate()

        try:
            rc = self._proc.wait(timeout=timeout_sec)
        except subprocess.TimeoutExpired:
            self._proc.terminate()
            try:
                rc = self._proc.wait(timeout=2.0)
            except subprocess.TimeoutExpired:
                self._proc.kill()
                rc = self._proc.wait(timeout=2.0)

        self._thread.join(timeout=2.0)
        stderr_text = self._proc.stderr.read() if self._proc.stderr is not None else ""

        with self._cond:
            if self._parse_error is not None:
                raise self._parse_error
            return rc, list(self._events), stderr_text


def cleanup_process_and_socket(proc: subprocess.Popen[str] | None, endpoint: str) -> None:
    if proc is not None and proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=3.0)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=3.0)
    if not IS_WINDOWS:
        sock = Path(endpoint)
        if sock.exists():
            try:
                sock.unlink()
            except OSError:
                pass


def validate_source_stream(
    events: list[dict[str, Any]],
    *,
    source: str,
    user_id: int,
    user_name: str,
    expected_frames: int,
    require_heartbeat: bool,
) -> None:
    src_events = [e for e in events if e.get("source") == source]
    assert src_events, f"No events recorded for source={source!r}"

    hellos = [e for e in src_events if e.get("event") == "hello"]
    assert len(hellos) == 1, f"[{source}] Expected 1 hello event, got {len(hellos)}: {hellos}"
    assert hellos[0].get("plugin_version") == "0.1.0", (
        f"[{source}] Unexpected plugin_version: {hellos[0]}"
    )

    user_infos = [
        e
        for e in src_events
        if e.get("event") == "user_info"
        and e.get("user_id") == user_id
        and e.get("name") == user_name
    ]
    assert user_infos, (
        f"[{source}] Missing UserInfoUpdated for user_id={user_id}, name={user_name!r}. "
        f"Events: {src_events}"
    )

    talking_states = [
        e.get("state")
        for e in src_events
        if e.get("event") == "talking_state" and e.get("user_id") == user_id
    ]
    assert "talking" in talking_states, (
        f"[{source}] Missing Talking state for user_id={user_id}: {talking_states}"
    )
    assert "passive" in talking_states, (
        f"[{source}] Missing Passive state for user_id={user_id}: {talking_states}"
    )

    audio_frames = [
        e
        for e in src_events
        if e.get("event") == "audio_frame" and e.get("user_id") == user_id
    ]
    assert len(audio_frames) >= expected_frames, (
        f"[{source}] Expected >= {expected_frames} AudioFrame events for user_id={user_id}, "
        f"got {len(audio_frames)}"
    )

    for idx, frame in enumerate(audio_frames):
        dropped_frames = frame.get("dropped_frames")
        total_dropped = frame.get("total_dropped")
        seq_status = frame.get("seq_status")
        rms = frame.get("rms")
        samples_16k = frame.get("samples_16k_mono")
        assert dropped_frames == 0, (
            f"[{source}] Frame {idx} had dropped_frames={dropped_frames}: {frame}"
        )
        assert total_dropped == 0, (
            f"[{source}] Frame {idx} had total_dropped={total_dropped}: {frame}"
        )
        assert seq_status in ("init", "ok"), (
            f"[{source}] Frame {idx} unexpected seq_status={seq_status}: {frame}"
        )
        assert isinstance(rms, (int, float)) and rms > 0.1, (
            f"[{source}] Frame {idx} RMS too low (expected > 0.1 for 440Hz sine): {frame}"
        )
        assert isinstance(samples_16k, int) and samples_16k > 0, (
            f"[{source}] Frame {idx} invalid 16k mono sample count: {frame}"
        )

    if require_heartbeat:
        heartbeats = [e for e in src_events if e.get("event") == "heartbeat"]
        assert len(heartbeats) >= 1, f"[{source}] Expected >= 1 Heartbeat event, got {heartbeats}"

    user_removed = [
        e
        for e in src_events
        if e.get("event") == "user_removed" and e.get("user_id") == user_id
    ]
    assert user_removed, f"[{source}] Missing UserRemoved for user_id={user_id}: {src_events}"

    lifecycle_end = [
        e
        for e in src_events
        if e.get("event") in ("source_reset", "disconnected")
    ]
    assert lifecycle_end, f"[{source}] Missing SourceReset/Disconnected event: {src_events}"

    # Verify ordering invariants within this source stream:
    # Hello must be first, UserRemoved after all AudioFrames.
    first_hello_idx = next(i for i, e in enumerate(src_events) if e.get("event") == "hello")
    assert first_hello_idx == 0, f"[{source}] Hello was not the first event: {src_events}"
    last_audio_idx = max(i for i, e in enumerate(src_events) if e.get("event") == "audio_frame")
    user_removed_idx = next(
        i for i, e in enumerate(src_events) if e.get("event") == "user_removed"
    )
    assert user_removed_idx > last_audio_idx, (
        f"[{source}] UserRemoved ({user_removed_idx}) occurred before final AudioFrame ({last_audio_idx})"
    )


def run_single_source_test(monitor_bin: Path, client_bin: Path) -> None:
    print("[E2E 1/2] Running single-source IPC test (mumble -> kotoha-ipc-monitor)...")
    endpoint = make_unique_endpoint("s1")
    if not IS_WINDOWS and Path(endpoint).exists():
        Path(endpoint).unlink()

    # Generous upper bound so any extra Heartbeat on slow CI runners cannot truncate SourceReset/Disconnected.
    max_events_ceiling = 32

    monitor_proc: subprocess.Popen[str] | None = None
    client_proc: subprocess.Popen[str] | None = None
    try:
        monitor_proc = subprocess.Popen(
            [
                str(monitor_bin),
                "--endpoint",
                endpoint,
                "--json",
                "--max-events",
                str(max_events_ceiling),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
        )
        reader = NdjsonMonitorReader(monitor_proc)
        wait_for_listener_ready(monitor_proc, endpoint)

        client_proc = subprocess.Popen(
            [
                str(client_bin),
                "--endpoint",
                endpoint,
                "--source",
                "mumble",
                "--user-id",
                "42",
                "--user-name",
                "Alice",
                "--frames",
                "5",
                "--with-heartbeat",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
        )
        client_stdout, client_stderr = client_proc.communicate(timeout=15.0)
        if client_proc.returncode != 0:
            raise AssertionError(
                f"kotoha_mock_client failed (exit {client_proc.returncode}):\n"
                f"stdout: {client_stdout}\nstderr: {client_stderr}"
            )

        reader.wait_for_sources_disconnected({"mumble"}, timeout_sec=10.0)
        mon_rc, events, mon_stderr = reader.finish_and_collect(timeout_sec=5.0)
        if not IS_WINDOWS and mon_rc != 0:
            raise AssertionError(
                f"kotoha-ipc-monitor failed (exit {mon_rc}):\nstderr: {mon_stderr}"
            )

        validate_source_stream(
            events,
            source="mumble",
            user_id=42,
            user_name="Alice",
            expected_frames=5,
            require_heartbeat=True,
        )

        # Ensure socket cleanup on Unix after monitor graceful exit
        if not IS_WINDOWS:
            assert not Path(endpoint).exists(), f"Monitor left stale socket file: {endpoint}"

        print(f"[E2E 1/2] PASS ({len(events)} NDJSON events validated on {endpoint})")
    finally:
        cleanup_process_and_socket(client_proc, endpoint)
        cleanup_process_and_socket(monitor_proc, endpoint)


def run_multi_source_test(monitor_bin: Path, client_bin: Path) -> None:
    print("[E2E 2/2] Running multi-source concurrent IPC test (mumble + ts3 -> kotoha-ipc-monitor)...")
    endpoint = make_unique_endpoint("m2")
    if not IS_WINDOWS and Path(endpoint).exists():
        Path(endpoint).unlink()

    max_events_ceiling = 64

    monitor_proc: subprocess.Popen[str] | None = None
    mumble_proc: subprocess.Popen[str] | None = None
    ts3_proc: subprocess.Popen[str] | None = None
    try:
        monitor_proc = subprocess.Popen(
            [
                str(monitor_bin),
                "--endpoint",
                endpoint,
                "--json",
                "--max-events",
                str(max_events_ceiling),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
        )
        reader = NdjsonMonitorReader(monitor_proc)
        wait_for_listener_ready(monitor_proc, endpoint)

        # Deliberately reuse user_id=42 across BOTH sources with different names ("Alice" vs "Bob")
        # and interleave execution to prove SpeakerKey { source, user_id } isolation!
        mumble_proc = subprocess.Popen(
            [
                str(client_bin),
                "--endpoint",
                endpoint,
                "--source",
                "mumble",
                "--user-id",
                "42",
                "--user-name",
                "Alice",
                "--frames",
                "6",
                "--duration-ms",
                "120",
                "--with-heartbeat",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
        )
        ts3_proc = subprocess.Popen(
            [
                str(client_bin),
                "--endpoint",
                endpoint,
                "--source",
                "ts3",
                "--user-id",
                "42",
                "--user-name",
                "Bob",
                "--frames",
                "6",
                "--duration-ms",
                "120",
                "--with-heartbeat",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
        )

        m_out, m_err = mumble_proc.communicate(timeout=15.0)
        t_out, t_err = ts3_proc.communicate(timeout=15.0)
        if mumble_proc.returncode != 0:
            raise AssertionError(
                f"mumble client failed (exit {mumble_proc.returncode}): {m_out}\n{m_err}"
            )
        if ts3_proc.returncode != 0:
            raise AssertionError(
                f"ts3 client failed (exit {ts3_proc.returncode}): {t_out}\n{t_err}"
            )

        reader.wait_for_sources_disconnected({"mumble", "ts3"}, timeout_sec=10.0)
        mon_rc, events, mon_stderr = reader.finish_and_collect(timeout_sec=5.0)
        if not IS_WINDOWS and mon_rc != 0:
            raise AssertionError(
                f"kotoha-ipc-monitor failed (exit {mon_rc}):\nstderr: {mon_stderr}"
            )

        sources = {e.get("source") for e in events}
        assert sources == {"mumble", "ts3"}, f"Unexpected sources set: {sources}"

        validate_source_stream(
            events,
            source="mumble",
            user_id=42,
            user_name="Alice",
            expected_frames=6,
            require_heartbeat=True,
        )
        validate_source_stream(
            events,
            source="ts3",
            user_id=42,
            user_name="Bob",
            expected_frames=6,
            require_heartbeat=True,
        )

        # Strictly verify zero SpeakerKey cross-contamination:
        # mumble must never report Bob; ts3 must never report Alice; distinct conn_ids assigned.
        mumble_names = {
            e.get("name")
            for e in events
            if e.get("source") == "mumble" and e.get("event") == "user_info"
        }
        ts3_names = {
            e.get("name")
            for e in events
            if e.get("source") == "ts3" and e.get("event") == "user_info"
        }
        assert mumble_names == {"Alice"}, f"mumble source contaminated: {mumble_names}"
        assert ts3_names == {"Bob"}, f"ts3 source contaminated: {ts3_names}"

        mumble_conn = next(
            e["conn_id"]
            for e in events
            if e.get("source") == "mumble" and e.get("event") == "hello"
        )
        ts3_conn = next(
            e["conn_id"]
            for e in events
            if e.get("source") == "ts3" and e.get("event") == "hello"
        )
        assert mumble_conn != ts3_conn, f"Concurrent clients shared conn_id={mumble_conn}"

        if not IS_WINDOWS:
            assert not Path(endpoint).exists(), f"Monitor left stale socket file: {endpoint}"

        print(
            f"[E2E 2/2] PASS ({len(events)} NDJSON events validated across mumble [conn={mumble_conn}] "
            f"and ts3 [conn={ts3_conn}] on {endpoint})"
        )
    finally:
        cleanup_process_and_socket(mumble_proc, endpoint)
        cleanup_process_and_socket(ts3_proc, endpoint)
        cleanup_process_and_socket(monitor_proc, endpoint)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Run Kotoha cross-language IPC E2E tests.")
    parser.add_argument(
        "--monitor-bin",
        default=None,
        help="Path to built kotoha-ipc-monitor executable (auto-detected if omitted).",
    )
    parser.add_argument(
        "--client-bin",
        default=None,
        help="Path to built kotoha_mock_client executable (auto-detected if omitted).",
    )
    args = parser.parse_args(argv)

    monitor_bin = resolve_monitor_binary(REPO_ROOT, args.monitor_bin)
    client_bin = resolve_client_binary(REPO_ROOT, args.client_bin)

    print(f"Using monitor binary: {monitor_bin}")
    print(f"Using client binary:  {client_bin}")

    run_single_source_test(monitor_bin, client_bin)
    run_multi_source_test(monitor_bin, client_bin)
    print("All Kotoha cross-language IPC E2E tests passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
