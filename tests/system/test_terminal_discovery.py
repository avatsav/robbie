"""Check Cargo artifact/fixture selection without opening a PTY or calling a model."""

import importlib.util
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
PACKAGE = "path+file:///repo/crates/robbie-cli#0.1.0"
SOURCE = str(ROOT / "crates/robbie-cli/src/main.rs")
if os.name == "posix":
    spec = importlib.util.spec_from_file_location(
        "terminal_driver", ROOT / "tests/terminal/blocked_tool_exit.py"
    )
    assert spec is not None and spec.loader is not None
    driver = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(driver)


def artifact():
    return {
        "reason": "compiler-artifact",
        "package_id": PACKAGE,
        "target": {"name": "robbie", "kind": ["bin"], "src_path": SOURCE},
        "profile": {"test": True},
        "executable": "fixture-robbie",
    }


@unittest.skipUnless(os.name == "posix", "Unix PTY discovery")
class TerminalDiscoveryTests(unittest.TestCase):
    def test_selects_only_the_cli_binary_test_artifact(self):
        core = artifact()
        core.update(package_id="robbie-core", target={"name": "robbie_core", "kind": ["lib"]})
        normal = artifact()
        normal["profile"]["test"] = False
        selected = driver.select_test_binary([core, normal, artifact()], PACKAGE, SOURCE)
        self.assertEqual(selected, "fixture-robbie")

    def test_missing_or_ambiguous_artifacts_fail(self):
        for records in ([], [artifact(), artifact()]):
            with self.subTest(records=records), self.assertRaises(AssertionError):
                driver.select_test_binary(records, PACKAGE, SOURCE)

    def test_package_target_profile_and_source_metadata_are_verified(self):
        wrong = []
        for key, value in (
            ("reason", "build-script-executed"),
            ("package_id", "robbie-core"),
            ("target", {"name": "other", "kind": ["bin"], "src_path": SOURCE}),
            ("target", {"name": "robbie", "kind": ["test"], "src_path": SOURCE}),
            ("target", {"name": "robbie", "kind": ["bin"], "src_path": "/other/main.rs"}),
            ("profile", {"test": False}),
            ("executable", None),
        ):
            record = artifact()
            record[key] = value
            wrong.append(record)
        for record in wrong:
            with self.subTest(record=record), self.assertRaises(AssertionError):
                driver.select_test_binary([record], PACKAGE, SOURCE)

    def test_build_explicitly_selects_the_cli_package_and_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "robbie-test"
            binary.touch(mode=0o755)
            record = artifact()
            record["executable"] = str(binary)
            metadata = {
                "packages": [{
                    "name": "robbie-cli", "id": PACKAGE,
                    "targets": [{"name": "robbie", "kind": ["bin"], "src_path": SOURCE}],
                }]
            }
            with patch.object(driver.subprocess, "run", side_effect=[
                SimpleNamespace(stdout=json.dumps(metadata)),
                SimpleNamespace(stdout=json.dumps(record)),
            ]) as run:
                self.assertEqual(driver.build_test_binary(), str(binary))
            self.assertEqual(run.call_args_list[0].args[0], [
                "cargo", "metadata", "--locked", "--no-deps", "--format-version=1",
            ])
            self.assertEqual(run.call_args_list[1].args[0], [
                "cargo", "test", "--locked", "-p", "robbie-cli", "--bin", "robbie",
                "--no-run", "--message-format=json",
            ])
            self.assertTrue(all(call.kwargs["check"] for call in run.call_args_list))

    def test_each_exact_ignored_selector_requires_one_matching_test(self):
        outputs = [SimpleNamespace(stdout=f"{fixture}: test\n\n1 test\n") for fixture in driver.FIXTURES]
        with patch.object(driver.subprocess, "run", side_effect=outputs) as run:
            driver.verify_fixture_selectors("fixture-robbie")
        self.assertEqual([call.args[0] for call in run.call_args_list], [
            ["fixture-robbie", "--exact", fixture, "--ignored", "--list"]
            for fixture in driver.FIXTURES
        ])
        self.assertTrue(all(call.kwargs["stdout"] == subprocess.PIPE for call in run.call_args_list))
        for index, fixture in enumerate(driver.FIXTURES):
            for stdout in ("0 tests\n", f"{fixture}: test\n{fixture}: test\n"):
                bad = outputs.copy()
                bad[index] = SimpleNamespace(stdout=stdout)
                with (
                    self.subTest(fixture=fixture, stdout=stdout),
                    patch.object(driver.subprocess, "run", side_effect=bad),
                    self.assertRaises(AssertionError),
                ):
                    driver.verify_fixture_selectors("fixture-robbie")


if __name__ == "__main__":
    unittest.main()
