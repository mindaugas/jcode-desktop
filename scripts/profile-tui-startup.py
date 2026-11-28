#!/usr/bin/env python3
"""Measure installed Jcode's rendered PTY startup, without live credentials.

Requires Linux and pyte==0.8.2 (target/tui-startup-venv/bin/pip install pyte==0.8.2).
Based on jcode/scripts/bench_startup_visible_ready.py, correcting spawn timing,
isolation, bounded split terminal queries, UTF-8 decoding and daemon cleanup.
Every sample uses a fresh private home, working directory, and daemon. This is
process-cold, not filesystem-cache-cold. No Enter is sent and no inference runs.
Timing starts BEFORE Popen and includes spawn. A terminal emulator observes
first text and the actual input probe, not a log marker or terminal local echo.
First text may be the CLI's "Starting server...", not the completed TUI frame.
Probes repeat every 25ms to tolerate bytes consumed by capability discovery.
If first-run onboarding appears, native Escape dismisses it without logging in.
Both the retry and dismissal costs remain included in input_ready_ms.
The emulator adds observation overhead and is not a physical screen scanout.
"""
from __future__ import annotations

import argparse
import codecs
import errno
import fcntl
import json
import math
import os
from pathlib import Path
import platform
import pty
import select
import shutil
import signal
import statistics
import struct
import subprocess
import tempfile
import termios
import time

PROBE = "jqx92"
ROWS, COLS = 24, 80
QUERIES = (
    (b"\x1b[?u", b"\x1b[?0u"),
    (b"\x1b[>c", b"\x1b[>0;0;0c"),
    (b"\x1b[6n", b"\x1b[1;1R"),
    (b"\x1b[c", b"\x1b[?62;c"),
    (b"\x1b[14t", b"\x1b[4;384;640t"),
    (b"\x1b[16t", b"\x1b[6;16;8t"),
    (b"\x1b[18t", b"\x1b[8;24;80t"),
) + tuple((f"\x1b]{color};?{end}".encode(),
           f"\x1b]{color};rgb:{rgb}{end}".encode())
          for color, rgb in ((10, "ffff/ffff/ffff"), (11, "0000/0000/0000"))
          for end in ("\x07", "\x1b\\")) + tuple(
    (f"\x1b[?{mode}$p".encode(), f"\x1b[?{mode};2$y".encode())
    for mode in (1016, 2027, 2031, 1004, 2004, 2026))


def isolated_env(root: Path) -> dict[str, str]:
    """Allowlist, never copy the caller's credentials or session environment."""
    env = {"PATH": "/usr/local/bin:/usr/bin:/bin", "LANG": "C.UTF-8",
           "TERM": "xterm-256color", "COLORTERM": "truecolor",
           "JCODE_COMPANION_AUTOSTART": "0",
           "JCODE_NO_TELEMETRY": "1", "JCODE_WAKE_MODE": "external"}
    for key, directory in (("HOME", "home"), ("JCODE_HOME", "jcode"),
                           ("JCODE_RUNTIME_DIR", "runtime"),
                           ("XDG_RUNTIME_DIR", "runtime"),
                           ("XDG_CONFIG_HOME", "config"),
                           ("XDG_CACHE_HOME", "cache"),
                           ("XDG_DATA_HOME", "data"), ("TMPDIR", "tmp")):
        path = root / directory
        path.mkdir(mode=0o700, exist_ok=True, parents=True)
        env[key] = str(path)
    env["JCODE_SOCKET"] = str(root / "runtime/jcode.sock")
    return env


def configure_pty(fd: int) -> None:
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    attrs = termios.tcgetattr(fd)
    attrs[3] &= ~(termios.ECHO | termios.ECHONL | termios.ICANON)
    attrs[0] &= ~(termios.ICRNL | termios.IXON)
    termios.tcsetattr(fd, termios.TCSANOW, attrs)


def reply_queries(fd: int, pending: bytes) -> bytes:
    for query, reply in QUERIES:
        while query in pending:
            os.write(fd, reply)
            pending = pending.replace(query, b"", 1)
    # Retain incomplete query prefixes only. Do not grow a transcript forever.
    longest = 0
    for query, _ in QUERIES:
        for size in range(1, len(query)):
            if pending.endswith(query[:size]):
                longest = max(longest, size)
    return pending[-longest:] if longest else b""


def meaningful_line(lines: list[str]) -> str | None:
    for line in lines:
        line = " ".join(line.split())
        if PROBE not in line and len(line) >= 4 and sum(c.isalnum() for c in line) >= 3:
            return line[:160]
    return None


def private_pids(env: dict[str, str]) -> list[int]:
    """Find detached children using an exact, unique environment marker (Linux)."""
    marker = ("JCODE_HOME=" + env["JCODE_HOME"]).encode()
    found = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            if marker in (entry / "environ").read_bytes().split(b"\0"):
                found.append(int(entry.name))
        except (OSError, PermissionError):
            pass
    return found


def cleanup(proc: subprocess.Popen | None, env: dict[str, str]) -> dict:
    seen = set()
    for sig in (signal.SIGTERM, signal.SIGKILL):
        pids = private_pids(env)
        seen.update(pids)
        for pid in pids:
            try:
                fd = os.pidfd_open(pid)
                try:
                    # Revalidate after acquiring the stable handle to avoid PID reuse.
                    if pid in private_pids(env):
                        signal.pidfd_send_signal(fd, sig)
                finally:
                    os.close(fd)
            except ProcessLookupError:
                pass
        if proc is not None:
            try:
                proc.wait(timeout=1)
            except subprocess.TimeoutExpired:
                pass
        deadline = time.monotonic() + 1
        while private_pids(env) and time.monotonic() < deadline:
            time.sleep(0.01)
    remaining = private_pids(env)
    if remaining:
        raise RuntimeError(f"private daemon cleanup failed: {remaining}")
    return {"private_pids_terminated": sorted(seen), "remaining_private_pids": remaining}


def run_once(binary: Path, root: Path, timeout: float) -> dict:
    import pyte
    env = isolated_env(root)
    cwd = root / "work"
    cwd.mkdir()
    master, slave = pty.openpty()
    configure_pty(slave)
    screen = pyte.Screen(COLS, ROWS)
    stream = pyte.Stream(screen)
    decoder = codecs.getincrementaldecoder("utf-8")("replace")
    argv = [str(binary), "--no-update", "--no-selfdev", "--provider", "jcode"]
    result = {"first_visible_ms": None, "input_ready_ms": None,
              "first_visible_excerpt": None, "probe_sent_ms": None,
              "terminal_echo_disabled": True, "probe": PROBE,
              "probe_attempts": 0, "last_probe_sent_ms": None,
              "onboarding_escape_ms": None,
              "mode": "fresh_private_daemon", "argv": argv}
    proc = None
    pending = b""
    try:
        start = time.perf_counter_ns()
        proc = subprocess.Popen(argv, cwd=cwd, env=env, stdin=slave,
                                stdout=slave, stderr=slave, start_new_session=True)
        result["spawn_return_ms"] = (time.perf_counter_ns() - start) / 1e6
        os.set_blocking(master, False)
        while (time.perf_counter_ns() - start) / 1e9 < timeout:
            readable, _, _ = select.select([master], [], [], 0.005)
            if readable:
                try:
                    chunk = os.read(master, 65536)
                except OSError as exc:
                    if exc.errno != errno.EIO:
                        raise
                    break
                if not chunk:
                    break
                pending = reply_queries(master, pending + chunk)
                stream.feed(decoder.decode(chunk))
            elapsed = (time.perf_counter_ns() - start) / 1e6
            if result["first_visible_ms"] is None:
                excerpt = meaningful_line(screen.display)
                if excerpt:
                    result.update(first_visible_ms=elapsed, first_visible_excerpt=excerpt)
            if termios.tcgetattr(slave)[3] & (termios.ECHO | termios.ECHONL):
                raise RuntimeError("Child enabled local echo, native input cannot be verified")
            # Empty homes can enter the first-run login picker. Escape it through
            # native input, never log in. Include this cost in the ready metric.
            if ("Esc to skip onboarding" in "\n".join(screen.display)
                    and result["onboarding_escape_ms"] is None):
                os.write(master, b"\x1b")
                result["onboarding_escape_ms"] = elapsed
            if result["probe_sent_ms"] is not None and PROBE in "\n".join(screen.display):
                result["input_ready_ms"] = elapsed
                result["probe_rendered_line"] = next(line.strip() for line in screen.display if PROBE in line)
                result["terminal_echo_disabled"] = not bool(termios.tcgetattr(slave)[3] & termios.ECHO)
                break
            # Early bytes can be consumed during terminal capability discovery.
            # Retry on a 25ms cadence, and report attempts rather than censoring
            # these launches. Leave Escape separate from text for key decoding.
            last_input = max(result["last_probe_sent_ms"] or -100,
                             result["onboarding_escape_ms"] or -100)
            if result["first_visible_ms"] is not None and elapsed - last_input >= 25:
                os.write(master, PROBE.encode())  # Never Enter, never inference.
                sent = (time.perf_counter_ns() - start) / 1e6
                if result["probe_sent_ms"] is None:
                    result["probe_sent_ms"] = sent
                result["last_probe_sent_ms"] = sent
                result["probe_attempts"] += 1
            if proc.poll() is not None:
                break
        result["screen"] = screen.display
        result["exit_code_at_observation"] = proc.poll()
    finally:
        try:
            result["cleanup"] = cleanup(proc, env)
        finally:
            os.close(master)
            os.close(slave)
    result["success"] = result["input_ready_ms"] is not None and result["terminal_echo_disabled"]
    return result


def summary(values: list[float | None]) -> dict:
    valid = sorted(value for value in values if value is not None)
    if not valid:
        return {"count": 0, "total": len(values)}
    return {"count": len(valid), "total": len(values), "min_ms": valid[0],
            "median_ms": statistics.median(valid), "p95_ms": valid[math.ceil(len(valid) * .95) - 1],
            "max_ms": valid[-1]}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default=shutil.which("jcode"))
    parser.add_argument("--runs", type=int, default=10)
    parser.add_argument("--timeout", type=float, default=10)
    parser.add_argument("--json-out", type=Path, default=Path("target/tui-startup.json"))
    args = parser.parse_args()
    if args.runs < 1 or args.timeout <= 0 or not args.binary:
        parser.error("positive runs/timeout and an installed Jcode binary are required")
    if platform.system() != "Linux":
        parser.error("Linux /proc is required for verified detached-daemon cleanup")
    binary = Path(args.binary).resolve()
    args.json_out.parent.mkdir(parents=True, exist_ok=True)
    scratch = Path("target").resolve()
    scratch.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="tui-", dir=scratch) as temporary:
        root = Path(temporary)
        version_env = isolated_env(root / "version")
        version = subprocess.run([str(binary), "version"], env=version_env, cwd=root,
                                 capture_output=True, text=True, timeout=10, check=True).stdout.strip()
        result = {"schema_version": 1, "binary": str(binary), "version": version,
                  "mode": "fresh_private_daemon", "terminal": {"rows": ROWS, "cols": COLS},
                  "clock": "perf_counter_ns before Popen", "platform": platform.platform(),
                  "probe_retry_interval_ms": 25,
                  "notes": "No credentials, no Enter, no inference. OS caches are not flushed. First visible may be Starting server, not full TUI. PTY emulator observation, not physical scanout.",
                  "samples": []}
        for i in range(args.runs):
            sample = run_once(binary, root / f"run-{i + 1}", args.timeout)
            result["samples"].append(sample)
            for key in ("first_visible_ms", "input_ready_ms"):
                result[key + "_summary"] = summary([s[key] for s in result["samples"]])
            args.json_out.write_text(json.dumps(result, indent=2) + "\n")
            print(f"{i + 1}/{args.runs}: visible={sample['first_visible_ms']} ms input={sample['input_ready_ms']} ms success={sample['success']}", flush=True)
    print(json.dumps({key: value for key, value in result.items() if key.endswith("_summary")}, indent=2))
    return 0 if all(sample["success"] for sample in result["samples"]) else 1


if __name__ == "__main__":
    raise SystemExit(main())
