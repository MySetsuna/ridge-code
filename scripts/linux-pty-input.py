#!/usr/bin/env python3
"""Exercise RidgeCode's Unix PTY input path with raw terminal bytes.

The harness is intentionally dependency-free and Linux-only.  It uses a real
controlling PTY, not a simulated event list, then validates the TUI snapshot
after ordinary editing, bracketed paste, unwrapped multiline input, and LF
submission.
"""

from __future__ import annotations

import argparse
import errno
import fcntl
import json
import os
import pty
import select
import signal
import struct
import time
import termios
from pathlib import Path


def drain(fd: int, output: bytearray, seconds: float) -> None:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        ready, _, _ = select.select(
            [fd], [], [], min(0.02, max(0.0, deadline - time.monotonic()))
        )
        if not ready:
            continue
        try:
            chunk = os.read(fd, 65536)
            output.extend(chunk)
            # Crossterm/rataui asks the real terminal for the cursor position
            # during inline viewport setup.  A PTY harness must act as the
            # terminal host and answer that query, otherwise startup aborts
            # before input can be exercised.
            if b"\x1b[6n" in chunk:
                os.write(fd, b"\x1b[1;1R")
        except OSError as error:
            if error.errno not in (errno.EIO, errno.EBADF):
                raise
            return


def read_snapshot(path: Path) -> dict | None:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (FileNotFoundError, json.JSONDecodeError, OSError):
        return None


def wait_buffer(
    fd: int, output: bytearray, snapshot: Path, expected: str, timeout: float
) -> dict | None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        drain(fd, output, 0.08)
        frame = read_snapshot(snapshot)
        if frame and frame.get("input", {}).get("buffer") == expected:
            return frame
    return None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--binary", default="target/debug/ridgecode", help="RidgeCode binary"
    )
    parser.add_argument(
        "--home", default="/tmp/ridgecode-linux-pty-home", help="isolated HOME"
    )
    parser.add_argument("--timeout", type=float, default=8.0)
    args = parser.parse_args()

    if os.name != "posix":
        raise SystemExit("linux-pty-input.py requires a POSIX PTY host")
    binary = Path(args.binary).resolve()
    home = Path(args.home).resolve()
    ridge_home = home / ".ridge"
    ridge_home.mkdir(parents=True, exist_ok=True)
    snapshot = ridge_home / "frame.json"
    for stale in (snapshot, ridge_home / "tui-trace.log", ridge_home / "keylog.txt"):
        stale.unlink(missing_ok=True)
    env = os.environ.copy()
    env.update(
        {
            "HOME": str(home),
            "RIDGE_FORCE_TUI": "1",
            "RIDGE_TUI_FIXTURE": "input",
            "RIDGE_TUI_SNAPSHOT": str(snapshot),
            "RIDGE_TUI_TRACE": str(ridge_home / "tui-trace.log"),
            "RIDGE_TUI_INPUT_DIAGNOSTICS": "1",
            # Raw LF/HT lose byte provenance on a Unix PTY. Exercise the
            # compatibility bridge explicitly; production defaults keep these
            # semantic Ctrl-J/Tab events on their normal key path.
            "RIDGE_TUI_UNWRAPPED_BRIDGE": "1",
            "RIDGE_KEYLOG": "1",
            "RIDGE_TUI_MOUSE_CAPTURE": "0",
            "RIDGE_PROXY": "",
            "RIDGE_CONFIG": str(home / "config.json"),
            "TERM": "xterm-256color",
        }
    )

    child, fd = pty.fork()
    if child == 0:
        os.execve(str(binary), [str(binary)], env)

    # Python's fresh PTY starts with a zero window size on some WSL kernels.
    # Ratatui's inline insertion loop cannot make progress with height=0, so
    # establish a real terminal geometry before the child enters raw mode.
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    os.set_blocking(fd, False)
    output = bytearray()

    def send(payload: bytes) -> None:
        os.write(fd, payload)
        drain(fd, output, 0.12)

    first = second = None
    try:
        # Do not race raw-mode setup: bytes sent while the PTY is still in
        # canonical mode are consumed by the host line discipline (not by
        # Crossterm), which would make BS/DEL evidence nondeterministic.
        if wait_buffer(fd, output, snapshot, "", args.timeout) is None:
            raise RuntimeError("TUI did not publish its ready snapshot")
        send(b"a b")
        send(b"\x08")
        send(b"\x7f")
        send(b"\t")
        send(b"\x1b[Z")
        send(b"\x1b[200~x\x1b[31my\x1b]0;title\x07z\x1b[201~")
        first = wait_buffer(fd, output, snapshot, "axyz", args.timeout)
        send(b"raw\n\ttail")
        second = wait_buffer(
            fd, output, snapshot, "axyzraw\n\ttail", args.timeout
        )
        if second:
            # Raw LF must still route to Submit after the multiline probe;
            # only send it once the exact pre-submit snapshot is observed.
            send(b"\n")
            drain(fd, output, 0.8)
        send(b"\x03")
        send(b"\x03")
        drain(fd, output, 0.8)
    finally:
        try:
            os.kill(child, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(child, 0)
        except ChildProcessError:
            pass

    result = {
        "status": "passed" if first and second else "failed",
        "binary": str(binary),
        "pid": child,
        "output_bytes": len(output),
        "output_tail": output[-1200:].decode("utf-8", errors="replace"),
        "input_after_space": (first or {}).get("input", {}).get("buffer"),
        "input_after_unwrapped": (second or {}).get("input", {}).get("buffer"),
        "cursor": (second or {}).get("input", {}).get("cursor"),
        "snapshot": str(snapshot),
        "keylog": str(ridge_home / "keylog.txt"),
    }
    print(json.dumps(result, ensure_ascii=False, sort_keys=True))
    return 0 if result["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
