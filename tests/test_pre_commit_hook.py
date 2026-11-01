"""Exercise the real Git hook in temporary repositories, without compiling Rust."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
HOOKS = ROOT / ".githooks"
EXPECTED = [
    "cargo fmt --all -- --check",
    "cargo clippy --locked --all-targets --all-features -- -D warnings",
    "ruff format --check tests",
    "ruff check tests",
]


class PreCommitHookTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="robbie-hook-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.log = self.root / "checks.log"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for tool in ("cargo", "ruff"):
            executable = self.bin / tool
            executable.write_text(
                "#!/bin/sh\n"
                "tool=${0##*/}\n"
                'printf "%s\\n" "$tool $*" >> "$CHECK_LOG"\n'
                'if [ "$tool $1" = "${FAIL_STEP:-}" ]; then exit 1; fi\n'
            )
            executable.chmod(0o755)
        self.env = {
            **os.environ,
            "PATH": f"{self.bin}{os.pathsep}{os.environ['PATH']}",
            "CHECK_LOG": str(self.log),
            "FAIL_STEP": "",
        }
        self.git("init", "--quiet")
        self.git("config", "core.hooksPath", str(HOOKS))
        self.git("config", "user.name", "Hook test")
        self.git("config", "user.email", "hook@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        # Keep all mock tooling outside the index. The hook must not stage it.
        (self.root / "example.txt").write_text("staged content\n")
        self.git("add", "example.txt")

    def git(self, *args, check=True):
        return subprocess.run(
            ["git", *args],
            cwd=self.root,
            env=self.env,
            text=True,
            capture_output=True,
            check=check,
        )

    def test_checks_run_before_a_real_commit_without_changing_the_index(self):
        self.assertTrue((HOOKS / "pre-commit").is_file())
        self.assertTrue(os.access(HOOKS / "pre-commit", os.X_OK))
        result = self.git("commit", "--quiet", "-m", "Hook success")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(self.log.read_text().splitlines(), EXPECTED)
        self.assertEqual(
            self.git("ls-tree", "--name-only", "HEAD").stdout.splitlines(),
            ["example.txt"],
        )
        self.assertEqual(
            self.git("show", "HEAD:example.txt").stdout, "staged content\n"
        )

    def test_each_failure_blocks_the_commit_and_stops_later_checks(self):
        index_before = self.git("write-tree").stdout
        for number, step in enumerate(
            ("cargo fmt", "cargo clippy", "ruff format", "ruff check")
        ):
            with self.subTest(step=step):
                self.log.unlink(missing_ok=True)
                self.env["FAIL_STEP"] = step
                result = self.git("commit", "--quiet", "-m", "Must fail", check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(
                    self.log.read_text().splitlines(), EXPECTED[: number + 1]
                )
                self.assertNotEqual(
                    self.git("rev-parse", "--verify", "HEAD", check=False).returncode, 0
                )
                self.assertEqual(self.git("write-tree").stdout, index_before)
                self.assertEqual(
                    (self.root / "example.txt").read_text(), "staged content\n"
                )

    def test_direct_invocation_works_from_a_subdirectory(self):
        nested = self.root / "nested"
        nested.mkdir()
        result = subprocess.run(
            [str(HOOKS / "pre-commit")], cwd=nested, env=self.env, check=False
        )
        self.assertEqual(result.returncode, 0)
        self.assertEqual(self.log.read_text().splitlines(), EXPECTED)


if __name__ == "__main__":
    unittest.main()
