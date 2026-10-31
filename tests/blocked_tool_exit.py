#!/usr/bin/env python3
"""Unix PTY regression: python3 tests/blocked_tool_exit.py (no model calls).

Builds the Rust test fixture, runs the production busy UI loop with a real
read_file blocked on a FIFO, then verifies drafting, scrolling and Ctrl+C exit.
"""
import fcntl
import json
import os
import pty
import select
import struct
import subprocess
import tempfile
import termios
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
build = subprocess.run(
    ["cargo", "test", "--no-run", "--message-format=json"],
    cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True,
)
binaries = [
    item["executable"]
    for line in build.stdout.splitlines()
    if (item := json.loads(line)).get("reason") == "compiler-artifact"
    and item.get("executable") and item.get("profile", {}).get("test")
]
assert len(binaries) == 1, f"Expected one test binary, got {len(binaries)}"

with tempfile.TemporaryDirectory(prefix=".robbie-pty-", dir=ROOT) as directory:
    fifo = Path(directory) / "pipe"
    os.mkfifo(fifo)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
    original = termios.tcgetattr(slave)
    child = subprocess.Popen(
        [binaries[0], "--exact", "ui::tests::blocked_tool_terminal_fixture",
         "--ignored", "--nocapture"],
        cwd=ROOT, stdin=slave, stdout=slave, stderr=slave,
        env={**os.environ, "TERM": "xterm-256color", "ROBBIE_TEST_FIFO": str(fifo)},
    )
    output = bytearray()

    def collect_until(predicate, timeout=3):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if predicate():
                return
            if select.select([master], [], [], 0.05)[0]:
                output.extend(os.read(master, 65536))
        raise AssertionError("Timed out waiting for terminal response")

    try:
        collect_until(lambda: b"Blocked read test" in output)
        # Let the FIFO read start. There is deliberately never a FIFO writer.
        time.sleep(0.2)
        output.clear()
        os.write(master, b"Z")
        collect_until(lambda: b"Z" in output)
        # Discard remaining redraw output before checking the scroll response.
        while select.select([master], [], [], 0.05)[0]:
            os.read(master, 65536)
        output.clear()
        os.write(master, b"\x1b[5~")  # Page Up
        collect_until(lambda: bool(output))
        os.write(master, b"\x03")  # Ctrl+C in raw mode
        collect_until(lambda: b"\x1b[?1049l" in output)
        assert child.wait(timeout=3) == 0, "Fixture failed"
        assert b"\x1b[?1006l" in output, "Mouse reporting not disabled"
        restored = termios.tcgetattr(slave)
        assert restored == original, "Terminal attributes were not restored"
        print("PASS: blocked read allows drafting, scrolling, Ctrl+C, terminal restoration and process exit.")
    finally:
        if child.poll() is None:
            child.kill()
            child.wait()
        os.close(master)
        os.close(slave)
