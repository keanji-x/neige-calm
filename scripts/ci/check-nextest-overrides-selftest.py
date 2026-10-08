#!/usr/bin/env python3
"""Exercise the guard with synthetic nextest protocol outputs, never a DSL evaluator."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "guard", Path(__file__).with_name("check-nextest-overrides.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


def fixture(names=("example",), *, ignored=False, status="matches"):
    return json.dumps({"rust-suites": {"sample::suite": {
        "package-name": "sample", "binary-id": "sample::suite", "status": "listed",
        "testcases": {name: {"ignored": ignored, "filter-match": {"status": status}}
                      for name in names}}}})


def config():
    return {"profile": {"renamed": {"overrides": [{"filter": "test(example)"}]}}}


class GuardTests(unittest.TestCase):
    def run_check(self, outputs, cfg=None):
        commands = []
        def query(command):
            commands.append(command)
            return outputs[len(commands) - 1]
        diagnostics = io.StringIO()
        with contextlib.redirect_stderr(diagnostics), contextlib.redirect_stdout(io.StringIO()):
            passed = guard.check(config() if cfg is None else cfg,
                                 ["--archive-file", "sample.tar.zst"], query)
        return passed, diagnostics.getvalue(), commands

    def test_nonempty_expansion_and_rename(self):
        for names in (("renamed-test",), ("one", "two", "three")):
            with self.subTest(names=names):
                self.assertTrue(self.run_check([fixture(names)])[0])

    def test_new_profiles_and_overrides_need_no_registration(self):
        cfg = config()
        cfg["profile"]["renamed"]["overrides"].append({"filter": "all()"})
        cfg["profile"]["new"] = {"overrides": [{"filter": "package(sample)"}]}
        cfg["profile"]["without-overrides"] = {}
        passed, _, commands = self.run_check([fixture()] * 3, cfg)
        self.assertTrue(passed)
        self.assertEqual(len(commands), 3)
        rows = [(profile, row["filter"]) for profile, settings in cfg["profile"].items()
                for row in settings.get("overrides", [])]
        for command, (profile, expression) in zip(commands, rows):
            self.assertEqual(command, ["cargo", "nextest", "list", "--archive-file",
                                      "sample.tar.zst", "--profile", profile,
                                      "--message-format", "json", "-E", expression])

    def test_empty_ignored_and_mismatch(self):
        for output in (fixture(()), fixture(ignored=True), fixture(status="mismatch")):
            with self.subTest(output=output):
                passed, diagnostic, _ = self.run_check([output])
                self.assertFalse(passed)
                self.assertIn("no runnable matches", diagnostic)
                self.assertIn("test(example)", diagnostic)
        cfg = config()
        cfg["profile"]["renamed"]["overrides"].append({"filter": "all()"})
        passed, _, commands = self.run_check([fixture(()), fixture()], cfg)
        self.assertFalse(passed)
        self.assertEqual(len(commands), 2)

    def test_bad_protocol_and_filter(self):
        for bad in ("not json", "[]", "{}", '{"rust-suites": []}',
                    '{"rust-suites": {"x": {}}}', fixture(status="unknown")):
            with self.subTest(bad=bad), self.assertRaises((ValueError, KeyError, TypeError)):
                self.run_check([bad])
        cfg = config()
        cfg["profile"]["renamed"]["overrides"][0]["filter"] = ""
        with self.assertRaisesRegex(ValueError, "invalid filter"):
            self.run_check([], cfg)

    def test_main_nonzero_on_empty_and_nextest_error(self):
        # Exercise the production CLI error conversion, not only the helper.
        for response in (fixture(()), subprocess.CalledProcessError(42, ["cargo", "nextest", "list"])):
            kwargs = ({"side_effect": response} if isinstance(response, Exception)
                      else {"return_value": subprocess.CompletedProcess([], 0, response)})
            with self.subTest(response=response), patch.object(sys, "argv", ["guard"]), \
                    patch.object(guard.subprocess, "run", **kwargs) as query, \
                    contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(guard.main(), 1)
                self.assertNotIn("NEIGE_CODEX_BIN", query.call_args.kwargs["env"])


if __name__ == "__main__":
    unittest.main()
