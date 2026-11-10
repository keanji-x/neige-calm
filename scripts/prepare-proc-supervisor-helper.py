#!/usr/bin/env python3
"""Build the real calm-proc-supervisor ELF for calm-server library tests.

The one builder for both test runners (#3482). nextest runs each test in its own
process, so its `proc-supervisor-helper` setup script (.config/nextest.toml) runs
this once before any test and hands the path to the matching tests. libtest runs a
whole test binary in one process; test_support::execution runs this once there.
The path is Cargo's reported `executable`; never infer the target layout.
"""
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
PREPARED = "NEIGE_TEST_PROC_SUPERVISOR_BIN"
USAGE = "usage: scripts/prepare-proc-supervisor-helper.py {--nextest-env|--print}"


def executable(stdout):
    found = []
    for line in filter(None, stdout.splitlines()):
        message = json.loads(line)
        if not isinstance(message, dict):
            raise ValueError(f"Cargo message is not an object: {line!r}")
        target = message.get("target") or {}
        if not isinstance(target, dict):
            raise ValueError(f"Cargo target is not an object: {target!r}")
        if (message.get("reason") == "compiler-artifact"
                and target.get("name") == "calm-proc-supervisor"
                and "bin" in target.get("kind", [])):
            found.append(message.get("executable"))
    if len(found) != 1 or not isinstance(found[0], str) or not os.path.isabs(found[0]):
        raise ValueError(f"Cargo must report one absolute calm-proc-supervisor executable, got {found!r}")
    path = found[0]
    # The nextest env-file protocol is one UTF-8 KEY=VALUE per line. Reject
    # paths that cannot round-trip through it before writing any environment.
    if any(character in path for character in ("\n", "\r", "\0")):
        raise ValueError(f"helper executable cannot be represented in nextest env: {path!r}")
    path.encode("utf-8")
    return path


def main():
    if sys.argv[1:] not in (["--nextest-env"], ["--print"]):
        print(USAGE, file=sys.stderr)
        return 2
    destination = os.environ.get("NEXTEST_ENV") if sys.argv[1] == "--nextest-env" else None
    if sys.argv[1] == "--nextest-env" and not destination:
        print("--nextest-env requires NEXTEST_ENV from a nextest setup script", file=sys.stderr)
        return 2
    env = dict(os.environ)
    env.pop("NEIGE_CODEX_BIN", None)
    env["RUSTC_WRAPPER"] = ""
    try:
        built = subprocess.run(
            ["cargo", "build", "--locked", "-p", "calm-proc-supervisor", "--bin",
             "calm-proc-supervisor", "--message-format=json-render-diagnostics", "-j", "4"],
            cwd=ROOT, env=env, check=True, stdout=subprocess.PIPE, text=True)
        path = executable(built.stdout)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"calm-proc-supervisor helper build failed: {error}", file=sys.stderr)
        return 1
    if destination is None:
        print(path)
        return 0
    with open(destination, "a", encoding="utf-8") as stream:
        stream.write(f"{PREPARED}={path}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
