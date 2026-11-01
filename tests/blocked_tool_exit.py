#!/usr/bin/env python3
"""Unix PTY regressions: python3 tests/blocked_tool_exit.py (no model calls).

Exercises native read/response rendering and both production busy loops with a
real read_file blocked on a FIFO. No model calls. Native mode must not enter the
alternate screen or capture the mouse.
"""
import fcntl
import json
import os
import pty
import re
import select
import signal
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


def check_frontend(frontend, completed_read=False):
    native = frontend == "ui"
    fixture = "completed_read_terminal_fixture" if completed_read else "blocked_tool_terminal_fixture"
    with tempfile.TemporaryDirectory(prefix=".robbie-pty-", dir=ROOT) as directory:
        fifo = Path(directory) / "pipe"
        os.mkfifo(fifo)
        master, slave = pty.openpty()
        rows, columns = 24, 100
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        original = termios.tcgetattr(slave)
        child = subprocess.Popen(
            [binaries[0], "--exact", f"{frontend}::tests::{fixture}",
             "--ignored", "--nocapture"],
            cwd=ROOT, stdin=slave, stdout=slave, stderr=slave,
            env={**os.environ, "TERM": "xterm-256color", "ROBBIE_TEST_FIFO": str(fifo)},
        )
        output = bytearray()
        answered_queries = 0

        def collect_until(predicate, timeout=3):
            nonlocal answered_queries
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if predicate():
                    return
                if select.select([master], [], [], 0.05)[0]:
                    output.extend(os.read(master, 65536))
                    # Inline viewports query cursor position. A PTY is not a
                    # terminal emulator, so answer using the last explicit move.
                    queries = output.count(b"\x1b[6n")
                    while answered_queries < queries:
                        positions = re.findall(rb"\x1b\[(\d+);(\d+)H", output)
                        row, column = map(int, positions[-1]) if positions else (1, 1)
                        os.write(master, f"\x1b[{min(row, rows)};{min(column, columns)}R".encode())
                        answered_queries += 1
            raise AssertionError(f"{frontend}: timed out waiting for terminal response")

        try:
            if completed_read:
                response = b"Dependency summary rendered successfully" if native else b"Dependency-summary-rendered-successfully"
                collect_until(lambda: response in output or b"normal duration" in output, timeout=5)
                assert b"normal duration" not in output, "Native read/response hit cursor-position timeout"
                collect_until(lambda: b"markdown_fixture" in output and b"Quoted-fixture" in output)
                plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", output)
                assert b"[read] Cargo.toml" in plain
                assert b"Dependencies" in plain, "Markdown heading missing"
                assert b"## Dependencies" not in plain, "Raw heading syntax leaked"
                assert b"**Dependency" not in plain, "Raw emphasis syntax leaked"
                assert b"```" not in plain, "Code-fence presentation markers leaked"
                assert "│".encode() in plain, "Quote gutter missing"
                assert b"> Quoted-fixture" not in plain, "Raw quote marker leaked"
            else:
                # A single-word marker survives Ratatui's skipped-space diffs.
                marker = b"Blocked read test" if native else b"Blocked-read-test"
                collect_until(lambda: marker in output)
                # No FIFO writer exists for the entire lifetime of the child.
                time.sleep(0.2)
            for mode in (1000, 1002, 1003):
                assert f"\x1b[?{mode}h".encode() not in output, "Mouse capture blocks ordinary terminal drag selection"
            os.write(master, b"Z")
            collect_until(lambda: b"Z" in output)
            if completed_read:
                for marker in (b"1", b"2", b"3"):
                    mark = len(output)
                    # Cycling is silent; a typed character confirms input still works.
                    os.write(master, b"\x0f" + marker)
                    collect_until(lambda marker=marker, mark=mark: marker in output[mark:])
                assert b"tools:" not in output, "Output mode leaked into prompt area"
                assert b"[dependencies]" not in output, "Mode change reprinted historical tool output"
                assert b"normal duration" not in output, "Cycling tool output mode hit cursor-position timeout"
            if native:
                if not completed_read:
                    assert b"history 00" in output and b"history 59" in output
                assert b"\x1b[?1049h" not in output, "Native UI entered alternate screen"
                assert b"\x1b[?1006h" not in output, "Native UI captured the mouse"
                # Shrinking must not clear the existing terminal transcript.
                rows, columns = 20, 60
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
                child.send_signal(signal.SIGWINCH)
                mark = len(output)
                os.write(master, b"Q")
                collect_until(lambda: b"Q" in output[mark:])
                assert b"\x1b[2J" not in output and b"\x1b[3J" not in output, "Native UI erased history"
            else:
                mark = len(output)
                os.write(master, b"\x1b[5~")  # Page Up
                collect_until(lambda: len(output) > mark)
                # Test F2 in both the idle loop and the blocked-tool busy loop.
                mark = len(output)
                os.write(master, b"\x1bOQ")  # F2: opt into mouse scrolling
                collect_until(lambda: b"\x1b[?1006h" in output[mark:])
                if not completed_read:
                    mark = len(output)
                    os.write(master, b"\x1b[<64;10;10M")  # SGR wheel up
                    collect_until(lambda: len(output) > mark)
                mark = len(output)
                os.write(master, b"\x1bOQ")  # F2: return to selection
                collect_until(lambda: b"\x1b[?1006l" in output[mark:])
                mark = len(output)
                os.write(master, b"\x1bOQ")  # Leave capture ON to exercise cleanup
                collect_until(lambda: b"\x1b[?1006h" in output[mark:])
            shutdown_mark = len(output)
            os.write(master, b"\x03")  # Ctrl+C in raw mode
            collect_until(lambda: (b"\x1b[?2004l" if native else b"\x1b[?1049l") in output)
            code = child.wait(timeout=3)
            while select.select([master], [], [], 0.1)[0]:
                output.extend(os.read(master, 65536))
            assert code == 0, f"{frontend}: fixture failed ({code}): {bytes(output[-3000:])!r}"
            if native:
                assert b"\x1b[?1049l" not in output, "Native exit restored a stale alternate-screen cursor; shell redraw can erase the last turn"
                assert b"\x1b[?2004l" in output, "Paste mode not disabled"
            else:
                assert b"\x1b[?1006l" in output[shutdown_mark:], "Mouse reporting not disabled on exit"
            assert termios.tcgetattr(slave) == original, "Terminal attributes not restored"
            scenario = "completed read/response" if completed_read else "blocked read"
            print(f"PASS {frontend}: responsive during {scenario}; Ctrl+C restores terminal and exits.")
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            os.close(master)
            os.close(slave)


for frontend in ("ui", "fullscreen"):
    check_frontend(frontend, completed_read=True)
    check_frontend(frontend)
