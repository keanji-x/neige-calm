#!/usr/bin/env python3
"""Render nextest JUnit failure metadata as bounded GitHub error annotations.

Only validated nextest identities and fixed structural failure kinds are public.
Never read failure/error attributes, bodies or system-out. A reporter failure
cannot turn the preceding nextest step green; CI invokes this only after it failed.
"""
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

MAX_INPUT = 16 * 1024 * 1024
# GitHub Actions accepts at most ten error annotations per step.
MAX_ANNOTATIONS = 10
MAX_OUTPUT = 32 * 1024
MARKER = "nextest failure: "


# Closed nextest/Rust identity protocol: ASCII identifiers joined by ::, with
# hyphens additionally allowed in binary names. Unsupported identities are skipped,
# never normalized into another identity. See the plugin's opt-in annotation marker.
def valid_identity(value, binary=False):
    segment = r"[A-Za-z_][A-Za-z0-9_-]*" if binary else r"[A-Za-z_][A-Za-z0-9_]*"
    if not re.fullmatch(segment + r"(?:::" + segment + r")*", value):
        return False
    # Credential-looking identifiers and workflow command words are outside this
    # public protocol, even when they happen to fit Rust's identifier grammar.
    return not any(part.lower() in {"authorization", "bearer", "token", "password", "secret", "api_key", "error", "warning"}
                   or any(prefix in part.lower() for prefix in ("ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"))
                   for part in value.split("::"))


def escape(value, property_value=False):
    value = value.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")
    if property_value:
        value = value.replace(":", "%3A").replace(",", "%2C")
    return value


def command(level, title, message):
    return f"::{level} title={escape(title, True)}::{escape(message)}\n"


def diagnostic(message):
    return command("warning", "nextest JUnit diagnostics", message)


def render(path):
    try:
        with Path(path).open("rb") as stream:
            data = stream.read(MAX_INPUT + 1)
        if len(data) > MAX_INPUT:
            raise ValueError("JUnit exceeds input limit")
        if b"<!DOCTYPE" in data.upper() or b"<!ENTITY" in data.upper():
            raise ValueError("DTD/entity declarations are unsupported")
        root = ET.fromstring(data)
        if root.tag not in ("testsuites", "testsuite"):
            raise ValueError("unsupported JUnit root")
    except (OSError, ValueError, ET.ParseError):
        return diagnostic("JUnit missing, malformed, unsupported, or exceeds input limit; test names not collected"), 1
    output = []
    used = count = 0
    incomplete = False
    for case in root.iter("testcase"):
        failures = [child for child in case if child.tag in ("failure", "error")]
        if not failures:
            continue
        binary, name = case.get("classname", ""), case.get("name", "")
        title = MARKER + binary + "::" + name
        if not valid_identity(binary, True) or not valid_identity(name) or len(title) > 240:
            incomplete = True
            continue
        # Multiple failure/error elements still identify one failed testcase.
        kind = "failure" if any(f.tag == "failure" for f in failures) else "error"
        summary = f"nextest testcase failed (JUnit {kind}); see job log"
        line = command("error", title, summary)
        size = len(line.encode("utf-8"))
        if count >= MAX_ANNOTATIONS or used + size > MAX_OUTPUT - 512:
            incomplete = True
            continue
        output.append(line)
        used += size
        count += 1
    if incomplete:
        output.append(diagnostic("truncated: annotation limits or incomplete testcase identity; some test names not collected"))
    if not count:
        output.append(diagnostic("No failure/error testcase names collected; nextest status remains authoritative"))
    return "".join(output), 0


def main():
    output, status = render(sys.argv[1] if len(sys.argv) > 1 else "target/nextest/ci/junit.xml")
    sys.stdout.write(output)
    return status


if __name__ == "__main__":
    sys.exit(main())
