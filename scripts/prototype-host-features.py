#!/usr/bin/env python3
"""Scan known Linux APIs and generate a standalone Cargo cfg demonstration.

Run from the repository root with an unused output directory as the argument.
The generated crate demonstrates cfg selection using compile-time assertions.
Its version labels describe upstream Linux introductions.
"""

import argparse
import re
from pathlib import Path


RULES = (
    ("openat2", "5.6", r"\bopenat2\s*\("),
    ("pidfd-open", "5.3", r"\blibc::syscall\s*\(\s*libc::SYS_pidfd_open\s*,"),
    ("close-range", "5.9", r"\blibc::syscall\s*\(\s*libc::SYS_close_range\s*,[^(),]+,[^(),]+,\s*0\s*,?\s*\)"),
    ("close-range-cloexec", "5.11", r"\blibc::syscall\s*\(\s*libc::SYS_close_range\s*,[^(),]+,[^(),]+,\s*libc::CLOSE_RANGE_CLOEXEC\s*,?\s*\)"),
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    output = parser.parse_args().output
    features = {}
    for path in sorted(Path("crates").glob("*/src/**/*.rs")):
        source = re.sub(r"(?m)//[^\n]*", "", path.read_text())
        for name, version, pattern in RULES:
            for match in re.finditer(pattern, source):
                line = source.count("\n", 0, match.start()) + 1
                print(f"{path}:{line}: {name} (Linux {version})")
                features[name] = version
    if not features:
        parser.error("no known calls found in crates/*/src; run from the repository root")

    output.mkdir()
    (output / "src").mkdir()
    manifest = [
        "[package]",
        'name = "host-feature-demo"',
        'version = "0.1.0"',
        'edition = "2021"',
        "",
        "[features]",
    ]
    code = ["fn main() {"]
    for name, version in sorted(features.items()):
        manifest.append(f"{name} = [] # Linux {version}")
        condition = f'all(target_os = "linux", feature = "{name}")'
        code.extend([
            "    {",
            f"        #[cfg({condition})]",
            "        const ENABLED: bool = true;",
            f"        #[cfg(not({condition}))]",
            "        const ENABLED: bool = false;",
            '        #[cfg(target_os = "macos")]',
            "        const _: () = assert!(!ENABLED);",
            f'        println!("{name}={{ENABLED}}");',
            "    }",
        ])
    code.append("}")
    (output / "Cargo.toml").write_text("\n".join(manifest) + "\n")
    (output / "src/main.rs").write_text("\n".join(code) + "\n")


if __name__ == "__main__":
    main()
