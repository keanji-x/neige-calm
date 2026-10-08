#!/usr/bin/env python3
"""Require each TOML override to match at least one runnable test.

Nextest evaluates the original filters; archive listings use private extraction.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]


def runnable(output):
    data = json.loads(output)
    if not isinstance(data, dict):
        raise ValueError("nextest output must be an object")
    suites = data["rust-suites"]
    if not isinstance(suites, dict):
        raise ValueError("rust-suites must be an object")
    matches = set()
    for key, suite in suites.items():
        if not isinstance(suite, dict) or not isinstance(suite.get("testcases"), dict):
            raise ValueError("invalid suite/testcases object")
        package, binary = suite["package-name"], suite["binary-id"]
        if not all(isinstance(x, str) and x for x in (package, binary)) or key != binary:
            raise ValueError("invalid package/binary identity")
        if suite["status"] != "listed":
            raise ValueError(f"unlisted binary: {binary}")
        for name, test in suite["testcases"].items():
            if not isinstance(test, dict) or not isinstance(test.get("filter-match"), dict):
                raise ValueError("invalid testcase/filter-match object")
            status = test["filter-match"]["status"]
            if status not in ("matches", "mismatch") or type(test["ignored"]) is not bool:
                raise ValueError(f"invalid test status: {binary}::{name}")
            if not isinstance(name, str) or not name:
                raise ValueError("invalid test identity")
            if status == "matches" and not test["ignored"]:
                matches.add((package, binary, name))
    return matches


def check(config, source_args, query=None):
    slots = {(profile, index): override
             for profile, settings in config["profile"].items()
             for index, override in enumerate(settings.get("overrides", []))}
    env = dict(os.environ)
    env.pop("NEIGE_CODEX_BIN", None)
    failed = False
    for slot, override in slots.items():
        expression = override["filter"]
        if not isinstance(expression, str) or not expression:
            raise ValueError(f"invalid filter: {slot}")
        command = ["cargo", "nextest", "list", *source_args, "--profile", slot[0],
                   "--message-format", "json", "-E", expression]
        output = (query(command) if query else subprocess.run(
            command, cwd=ROOT, env=env, check=True, stdout=subprocess.PIPE, text=True).stdout)
        actual = runnable(output)
        if not actual:
            failed = True
            print(f"override {slot}: no runnable matches for {expression!r}", file=sys.stderr)
        else:
            print(f"override {slot}: {len(actual)} runnable tests")
    return not failed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive-file")
    args = parser.parse_args()
    source = (["--archive-file", args.archive_file, "--workspace-remap", str(ROOT)]
              if args.archive_file else
              ["--workspace", "--locked", "--features", "calm-server/codex-e2e"])
    try:
        with (ROOT / ".config/nextest.toml").open("rb") as stream:
            config = tomllib.load(stream)
        return 0 if check(config, source) else 1
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        print(f"nextest override guard failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
