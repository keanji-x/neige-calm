# Linux Alpha release: build, install, verify

This runbook covers a **single-owner Linux Alpha** using the new desktop web UI.
It does not publish a GitHub release, create a tag, or upgrade an existing instance.
Use a clean candidate revision containing the fixes selected for the release.
The package release ID (for example `0.1.0-alpha.1`) is distinct from individual
crate versions; `/api/version.buildSha` identifies the compiled source.
An optional release codename (for example **yansong**) belongs in the display
title and release notes; keep the package release ID in the `N.N.N-alpha.N` format.

## 1. Build the distribution (maintainer)

Prerequisites: Linux, Git, Bash, GNU tar/coreutils, Python 3, the pinned Rust
toolchain, Go (the version in `tailnet/go.mod`), and Node.js >=22.12 with npm. The build does not need Codex credentials.

```bash
git clone https://github.com/keanji-x/neige-calm.git
cd neige-calm
# Select the reviewed candidate commit or existing release tag, then:
rustup toolchain install
scripts/release/build-alpha.sh \
  --version 0.1.0-alpha.1 \
  --output-dir /tmp/neige-alpha-artifacts
```

The script refuses a dirty checkout and existing output names. It builds release
binaries with `--locked`, caps Cargo at six jobs, explicitly selects the rustc host target and reads only
that target output (even when CARGO_BUILD_TARGET is set), embeds the exact Git SHA, runs
`npm ci` and the production build for the maintained frontend, and calls the real
`neige-app system package` command. `--target-dir /abs/path` selects a Cargo cache.
Keep output outside the source checkout so the final clean-tree check stays useful.

Outputs are `neige-calm-<version>-linux-<arch>.tar.gz`, its `.sha256`, and a
`.BUILD.json` recording source SHA, compiler, architecture, and build-host libc.
Inside the archive:

```text
neige-calm-0.1.0-alpha.1-linux-x86_64/
├── BUILD.json
├── INSTALL.md
├── docs/                             # installation, usage, and recovery guides
└── release/
    ├── manifest.json
    ├── bin/{neige-app,calm-server,calm-proc-supervisor,
    │        neige-codex-bridge,neige-mcp-stdio-shim,neige}
    └── web/dist/
        └── next/{index.html,assets/...,manifest.webmanifest,icons/...} # new /next/
```

`web/dist/next` is included in the **Web manifest unit and hashes** as the
legacy bundle: a next-only change changes the Web upgrade identity. Do not copy
extra files into `release/` after packaging; integrity verification rejects them.
The outer BUILD.json, INSTALL.md and docs/ are covered by the archive checksum.

This is a native build, not a universal Linux binary. Match CPU architecture and
check BUILD.json's build environment before distributing; release CI should build
on the oldest Linux/libc baseline the release intends to support. On the target,
`release/bin/neige-app --version` and `release/bin/calm-server --version` must run
without a missing loader/library error. Rust and Node are not runtime prerequisites.

## 2. Install and use the archive (user)

Follow [Start with one repository](getting-started.md)
([简体中文](getting-started.zh-CN.md)). From the extracted archive:

```bash
./release/bin/neige-app system setup --package ./release
```

Setup verifies the existing v2 package integrity contract, creates a fresh
private installation with isolated configuration/data/plugins, initializes
`state/installed.json`, and prints the foreground start command and browser URL.
No compiler, systemd unit, symlink assembly or admin HTTP bootstrap is needed.
Use `--root /absolute/new/directory` for an independent installation; existing
installations are refused. The command asks for a local owner password, or
accepts a private `--password-file` for automation.

The [configuration reference](neige-app-config.md) covers optional Claude
Planners and previews. The [deployment guide](deploy-and-upgrade.md) covers
systemd, upgrades and recovery; do not rerun fresh setup to upgrade.

## 3. Verify the installed candidate (maintainer)

After following the user guide, compare the public `/api/version.buildSha`
with `BUILD.json.sourceCommit`. Check owner login, provider readiness, a real
bounded investigation, and report persistence after browser refresh and service
restart. Verify `state/installed.json` under the setup root's data directory.
Run no real-model stack tests directly on the shared production host; use an
isolated Docker exercise with private state and fenced provider-only egress.

## 4. Alpha release checklist (maintainer)

- Candidate commit and reviewed diff are fixed; relevant CI has passed, with
  skipped checks distinguished from executed checks.
- The archive checksum verifies after transfer, every binary runs, and the
  target host matches the declared architecture/libc baseline.
- Fresh config requires login; root, `/next/`, its assets and a deep link work
  from the **installed package**, not Vite or the source tree.
- `/api/version.buildSha` matches BUILD.json; package integrity dry-run passes.
- Fresh setup bookkeeping, restart and persistence have been checked in an
  isolated installation. Record what real-model/upgrade checks were not run.
- Known product limitations (including outstanding desktop terminal fixes)
  appear in release notes. This packaging work does not claim to fix them.
- Only after those checks, create the intended tag and publish the artifact,
  checksum, BUILD.json and release notes. The build script does neither action.
