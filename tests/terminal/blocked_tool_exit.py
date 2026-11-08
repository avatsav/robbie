#!/usr/bin/env python3
"""Full-screen PTY regressions: python3 tests/terminal/blocked_tool_exit.py.

No model calls. Exercise completed tool output and a real read_file blocked on a
FIFO, including scrolling, drag-to-copy, resizing, and terminal restoration.
Clipboard requests are captured as bytes, never sent to a real host clipboard.
"""

import argparse
import base64
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

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = (
    "fullscreen::tests::terminal::completed_read_terminal_fixture",
    "fullscreen::tests::terminal::blocked_tool_terminal_fixture",
)


def select_test_binary(artifacts, package_id, source_path):
    binaries = [
        item["executable"]
        for item in artifacts
        if item.get("reason") == "compiler-artifact"
        and item.get("package_id") == package_id
        and item.get("target", {}).get("name") == "robbie"
        and item.get("target", {}).get("kind") == ["bin"]
        and item.get("target", {}).get("src_path") == source_path
        and item.get("profile", {}).get("test") is True
        and item.get("executable")
    ]
    assert len(binaries) == 1, f"Expected one robbie-cli bin test artifact, got {len(binaries)}"
    return binaries[0]


def build_test_binary():
    metadata = subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version=1"],
        cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True,
    )
    packages = [item for item in json.loads(metadata.stdout)["packages"] if item["name"] == "robbie-cli"]
    assert len(packages) == 1, "Expected the robbie-cli package"
    package = packages[0]
    targets = [item for item in package["targets"] if item["name"] == "robbie" and item["kind"] == ["bin"]]
    assert len(targets) == 1, "Expected the robbie bin target"
    build = subprocess.run(
        ["cargo", "test", "--locked", "-p", "robbie-cli", "--bin", "robbie", "--no-run", "--message-format=json"],
        cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True,
    )
    artifacts = [json.loads(line) for line in build.stdout.splitlines()]
    binary = select_test_binary(artifacts, package["id"], targets[0]["src_path"])
    assert Path(binary).is_file() and os.access(binary, os.X_OK), f"Test binary is not executable: {binary}"
    return binary


def verify_fixture_selectors(binary):
    for fixture in FIXTURES:
        listed = subprocess.run(
            [binary, "--exact", fixture, "--ignored", "--list"],
            cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True,
        )
        matches = [line for line in listed.stdout.splitlines() if line.endswith(": test")]
        assert matches == [f"{fixture}: test"], f"Expected exactly one ignored fixture: {fixture}; got {matches}"


CLIPBOARD = re.compile(rb"\x1b\]52;c;([A-Za-z0-9+/=]+)\x1b\\")


def check_screen(binary, completed_read=False):
    selector = FIXTURES[0 if completed_read else 1]
    fixture = selector.rsplit("::", 1)[1]
    with tempfile.TemporaryDirectory(prefix=".robbie-pty-", dir=ROOT) as directory:
        fifo = Path(directory) / "pipe"
        os.mkfifo(fifo)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
        original = termios.tcgetattr(slave)
        child = subprocess.Popen(
            [binary, "--exact", selector, "--ignored", "--nocapture"],
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
            raise AssertionError(f"{fixture}: timed out waiting for terminal response; exit={child.poll()}, tail={bytes(output[-2000:])!r}")

        def type_and_wait(data, marker):
            mark = len(output)
            os.write(master, data)
            collect_until(lambda: marker in output[mark:])

        try:
            if completed_read:
                collect_until(lambda: b"Dependency-summary-rendered-successfully" in output, timeout=5)
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
                collect_until(lambda: b"Blocked-read-test" in output)
                collect_until(lambda: "Working…".encode() in output)
                mark = len(output)
                spinners = [glyph.encode() for glyph in "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"]
                collect_until(lambda: any(glyph in output[mark:] for glyph in spinners))
                # The FIFO never gets a writer; activity must still animate.

            assert b"\x1b[?1049h" in output, "Full-screen mode not entered"
            assert b"\x1b[?1000h" in output, "Mouse reporting not enabled"
            assert b"\x1b[?1006h" in output, "SGR mouse reporting not enabled"
            type_and_wait(b"Z", b"Z")
            if completed_read:
                for marker in (b"1", b"2", b"3"):
                    type_and_wait(b"\x0f" + marker, marker)  # Ctrl+O cycles silently.
                assert b"Ctrl+O" not in output, "Mode hint should stay hidden"
                assert b"[dependencies]" not in output, "Mode change replayed tool output"

            # Reach the start regardless of footer height or terminal wrapping.
            # Typing confirms processing even when another Page Up is a no-op.
            type_and_wait(b"\x1b[5~" * 20 + b"P", b"P")
            mark = len(output)
            os.write(master, b"\x1b[<0;1;1M\x1b[<32;8;1M")  # Left down, drag.
            collect_until(lambda: b"48;2;50;75;110" in output[mark:])
            assert not CLIPBOARD.search(output), "Copied before mouse release"
            os.write(master, b"\x1b[<0;8;1m")  # Left release.
            collect_until(lambda: CLIPBOARD.search(output) is not None)
            copied = CLIPBOARD.search(output)
            assert copied is not None
            text = base64.b64decode(copied.group(1)).decode()
            assert text == ("▌ robbie" if completed_read else "history "), repr(text)
            type_and_wait(b"K", b"K")
            assert len(CLIPBOARD.findall(output)) == 1, "Redraw copied the selection again"

            # A plain click must not overwrite the clipboard. Wheel input remains
            # active after selection; there is no scrolling/selection mode toggle.
            type_and_wait(b"\x1b[<0;1;1M\x1b[<0;1;1mC", b"C")
            assert len(CLIPBOARD.findall(output)) == 1, "A click copied text"
            for report, marker in [(b"\x1b[<65;10;10M", b"R"), (b"\x1b[<64;10;10M", b"U")]:
                mark = len(output)
                type_and_wait(report + marker, marker)
                if not completed_read:
                    # Unchanged spaces need not be repainted by Ratatui's diff.
                    assert b"history" in output[mark:], "Wheel input did not scroll the transcript"

            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 20, 60, 0, 0))
            child.send_signal(signal.SIGWINCH)
            type_and_wait(b"@", b"@")  # Unique acknowledgement, unlike Q in Quoted-fixture.
            assert b"\x1b[6n" not in output, "Obsolete cursor-position query returned"
            assert b"F2" not in output, "Obsolete mouse-mode toggle returned"

            shutdown_mark = len(output)
            os.write(master, b"\x03")  # Ctrl+C still exits.
            collect_until(lambda: b"\x1b[?1049l" in output[shutdown_mark:])
            code = child.wait(timeout=3)
            while select.select([master], [], [], 0.1)[0]:
                output.extend(os.read(master, 65536))
            assert code == 0, f"Fixture failed ({code}): {bytes(output[-3000:])!r}"
            for mode in (1000, 1002, 1003, 1006, 2004):
                assert f"\x1b[?{mode}l".encode() in output[shutdown_mark:], f"Mode {mode} not restored"
            assert termios.tcgetattr(slave) == original, "Terminal attributes not restored"
            scenario = "completed read/response" if completed_read else "blocked read"
            print(f"PASS fullscreen: {scenario}, drag-to-copy, scrolling, resize, Ctrl+C and restoration.")
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            os.close(master)
            os.close(slave)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check-discovery", action="store_true", help="verify Cargo target and exact fixture selectors without running PTY scenarios")
    args = parser.parse_args()
    binary = build_test_binary()
    verify_fixture_selectors(binary)
    if args.check_discovery:
        print("PASS discovery: robbie-cli test binary and both exact ignored fixture selectors; PTY scenarios not run.")
        return
    check_screen(binary, completed_read=True)
    check_screen(binary)


if __name__ == "__main__":
    main()
