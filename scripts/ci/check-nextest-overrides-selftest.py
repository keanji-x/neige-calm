#!/usr/bin/env python3
"""Exercise guard contracts with fixed nextest outputs, never a filter evaluator."""
import contextlib
import copy
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tomllib
import unittest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "guard", Path(__file__).with_name("check-nextest-overrides.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


def fixture(identities):
    suites = {}
    for package, binary, name in identities:
        suite = suites.setdefault(binary, {"package-name": package, "binary-id": binary,
                                          "status": "listed", "testcases": {}})
        suite["testcases"][name] = {"ignored": False, "filter-match": {"status": "matches"}}
    return json.dumps({"rust-suites": suites})


def config():
    with (guard.ROOT / ".config/nextest.toml").open("rb") as stream:
        return tomllib.load(stream)


class GuardTests(unittest.TestCase):
    def run_check(self, outputs, cfg=None):
        cfg = config() if cfg is None else cfg
        commands = []
        def query(command):
            commands.append(command)
            return outputs[len(commands) - 1]
        diagnostics = io.StringIO()
        with contextlib.redirect_stderr(diagnostics), contextlib.redirect_stdout(io.StringIO()):
            passed = guard.check(cfg, ["--workspace", "--locked", "--features",
                                      "calm-server/codex-e2e"], query)
        return passed, diagnostics.getvalue(), commands

    def outputs(self):
        return [fixture(guard.REVIEWED[slot]) for slot in sorted(guard.REVIEWED)]

    def test_exact_sets_and_original_expression(self):
        passed, _, commands = self.run_check(self.outputs())
        self.assertTrue(passed)
        for index, command in enumerate(commands):
            self.assertEqual(command[-1], config()["profile"]["ci"]["overrides"][index]["filter"])
            self.assertNotIn("--partition", command)
            self.assertIn("calm-server/codex-e2e", command)

    def test_empty(self):
        outputs = self.outputs()
        outputs[1] = fixture([])
        passed, diagnostic, _ = self.run_check(outputs)
        self.assertFalse(passed)
        self.assertIn("empty=True", diagnostic)
        self.assertIn(next(iter(guard.REVIEWED[("ci", 1)]))[2], diagnostic)

    def test_complete_missing_and_unexpected(self):
        outputs = self.outputs()
        extra = [("extra", "extra::suite", "one"), ("extra", "extra::suite", "two")]
        outputs[0] = fixture(extra)
        passed, diagnostic, _ = self.run_check(outputs)
        self.assertFalse(passed)
        for identity in guard.REVIEWED[("ci", 0)] | set(extra):
            self.assertIn(identity[2], diagnostic)

    def test_bad_output(self):
        for bad in ("not json", "[]", "{}", '{"rust-suites": []}',
                    '{"rust-suites": {"x": {}}}',
                    '{"rust-suites": {"x": null}}'):
            with self.subTest(bad=bad), self.assertRaises((ValueError, KeyError, TypeError)):
                self.run_check([bad])
        data = json.loads(self.outputs()[1])
        suite = next(iter(data["rust-suites"].values()))
        next(iter(suite["testcases"].values()))["filter-match"]["status"] = "unknown"
        with self.assertRaises(ValueError):
            guard.runnable(json.dumps(data))

    def test_ignored_is_not_runnable(self):
        data = json.loads(self.outputs()[1])
        test = next(iter(next(iter(data["rust-suites"].values()))["testcases"].values()))
        test["ignored"] = True
        self.assertEqual(guard.runnable(json.dumps(data)), set())

    def test_nextest_error(self):
        def fail(command):
            raise subprocess.CalledProcessError(42, command)
        with self.assertRaises(subprocess.CalledProcessError):
            guard.check(config(), [], fail)

    def test_unregistered_and_deleted(self):
        for profile in ("ci", "default"):
            cfg = copy.deepcopy(config())
            cfg["profile"][profile].setdefault("overrides", []).append({"filter": "all()"})
            with self.assertRaisesRegex(ValueError, "unregistered"):
                self.run_check(self.outputs(), cfg)
        cfg = config()
        cfg["profile"]["ci"]["overrides"].pop()
        with self.assertRaisesRegex(ValueError, "missing registrations"):
            self.run_check(self.outputs(), cfg)


if __name__ == "__main__":
    if sys.argv[1:2] == ["--emit-fixture"]:
        # The shell-entry selftest stubs only the nextest boundary. Select a fixed
        # response by exact argv text; this is not an implementation of its DSL.
        expression = sys.argv[sys.argv.index("-E") + 1]
        slots = config()["profile"]["ci"]["overrides"]
        index = next(i for i, row in enumerate(slots) if row["filter"] == expression)
        print(fixture(guard.REVIEWED[("ci", index)]))
    else:
        unittest.main()
