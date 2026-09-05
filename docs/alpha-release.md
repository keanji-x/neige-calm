# Linux Alpha release: build, install, verify

This runbook covers a **single-owner Linux Alpha** using the new desktop web UI.
It does not publish a GitHub release, create a tag, or upgrade an existing instance.
Use a clean candidate revision containing the fixes selected for the release.
The package release ID (for example `0.1.0-alpha.1`) is distinct from individual
crate versions; `/api/version.buildSha` identifies the compiled source.

## 1. Build the distribution (maintainer)

Prerequisites: Linux, Git, Bash, GNU tar/coreutils, Python 3, the pinned Rust
toolchain, and Node.js >=22.12 with npm. The build does not need Codex credentials.

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
`npm ci` and production builds for **both** frontends, and calls the real
`neige-app system package` command. `--target-dir /abs/path` selects a Cargo cache.
Keep output outside the source checkout so the final clean-tree check stays useful.

Outputs are `neige-calm-<version>-linux-<arch>.tar.gz`, its `.sha256`, and a
`.BUILD.json` recording source SHA, compiler, architecture, and build-host libc.
Inside the archive:

```text
neige-calm-0.1.0-alpha.1-linux-x86_64/
├── BUILD.json
├── INSTALL.md
├── docs/                             # installation and recovery runbooks
└── release/
    ├── manifest.json
    ├── bin/{neige-app,calm-server,calm-proc-supervisor,
    │        neige-codex-bridge,neige-mcp-stdio-shim,neige}
    └── web/dist/
        ├── index.html, assets/...     # legacy /calm/
        └── next/{index.html,assets/...} # new /next/
```

`web/dist/next` is included in the **same Web manifest unit and hashes** as the
legacy bundle: a next-only change changes the Web upgrade identity. Do not copy
extra files into `release/` after packaging; integrity verification rejects them.
The outer BUILD.json, INSTALL.md and docs/ are covered by the archive checksum.

This is a native build, not a universal Linux binary. Match CPU architecture and
check BUILD.json's build environment before distributing; release CI should build
on the oldest Linux/libc baseline the release intends to support. On the target,
`release/bin/neige-app --version` and `release/bin/calm-server --version` must run
without a missing loader/library error. Rust and Node are not runtime prerequisites.

## 2. Prepare a fresh installation (user)

Runtime prerequisites: compatible Linux/architecture, a desktop browser, Git,
Codex CLI with its companion binaries installed, and Codex authentication for the
same OS user who will run the service. Claude is optional. Systemd user services
are optional if using foreground mode below.

After downloading the archive and checksum to the same directory:

```bash
sha256sum -c neige-calm-0.1.0-alpha.1-linux-x86_64.tar.gz.sha256
tar -xzf neige-calm-0.1.0-alpha.1-linux-x86_64.tar.gz
cd neige-calm-0.1.0-alpha.1-linux-x86_64
./release/bin/neige-app --version
./release/bin/calm-server --version
codex login
```

The checksum must come from the same trusted release channel as the archive.
Replace `x86_64` with the actual artifact architecture if necessary. Do not extract
a new version over an existing release directory.

These next commands are for a **first install only**. If the config, service,
symlinks or data directory already belong to an installation, use the
[upgrade guide](deploy-and-upgrade.md), not this bootstrap sequence.

```bash
set -e
mkdir -p ~/.local/share/neige-app/releases ~/.local/bin
# Refuse an existing release rather than merging directories.
test ! -e ~/.local/share/neige-app/releases/0.1.0-alpha.1
cp -a release ~/.local/share/neige-app/releases/0.1.0-alpha.1
ln -s 0.1.0-alpha.1 ~/.local/share/neige-app/releases/current-server
ln -s 0.1.0-alpha.1 ~/.local/share/neige-app/releases/current-web
ln -s ~/.local/share/neige-app/releases/current-server/bin/neige-app ~/.local/bin/neige-app
umask 077
~/.local/bin/neige-app system init-config --config ~/.config/neige-app/config.toml
chmod 600 ~/.config/neige-app/config.toml
```

Edit `~/.config/neige-app/config.toml` before starting:

- Set `[child] auth_password` to your chosen password; default username is `owner`.
  Keep `auth_dev_autologin = false`. This credential is separate from Codex login
  and from the generated admin token. Keep the config private (`0600`).
- Keep `[child] web_dist` at `.../current-web/web/dist` and **fe_dist** at
  `.../current-web/web/dist/next`. The generated starter config includes both.
- Check `[child] calm_listen` (default `127.0.0.1:4040`) and `[admin] listen`
  (default `127.0.0.1:4050`) are free. Use separate ports **and data/config/release
  directories** for a second instance. Never point a rehearsal at production data.
- Keep the database unset (`db_url = ""`) to use SQLite in `[child] data_dir`.
  Never use `mock` for persistent work. A fresh data directory is created at boot.
- `[systemd] bin` should be `~/.local/bin/neige-app`. Put Codex and Git on the PATH
  used for installation; the unit captures that PATH.

Installing plugins grants execution under this user's account. Only this trusted
user should be able to write plugin sources and the plugin installation directory.
The Alpha does not provide an untrusted multi-user/plugin isolation boundary.

## 3. Start the installed package

Install the user unit and create the admin token, without starting anything yet:

```bash
~/.local/bin/neige-app system install --config ~/.config/neige-app/config.toml
```

Run this preparation for **both** launch modes: it creates the required admin
token and writes a unit file, but does not require systemd to be running. Then
choose one of the following; do not run both on the same ports/data directory.

With a systemd user manager:

```bash
systemctl --user daemon-reload
systemctl --user enable --now neige-app.service
systemctl --user status neige-app.service
journalctl --user -u neige-app.service -n 50 --no-pager
```

`system install` refuses to overwrite an existing unit; do not reflexively add
`--force` on a first install. Missing `claude` warnings are harmless if only using
Codex; missing `codex` or `git` must be addressed for their workflows.

Without systemd, after the same `system install` preparation above:

```bash
~/.local/bin/neige-app system serve --config ~/.config/neige-app/config.toml
```

Keep that process running. The default browser URL is <http://127.0.0.1:4040/>;
with `fe_dist` configured it redirects to `/next/`. Log in with the configured
owner credentials. `/calm/` remains the legacy fallback. The admin port serves
operator APIs, not the product UI; keep it loopback-only.

### Network setup before the first agent task

Systemd does not inherit the proxy variables exported in your interactive shell;
`system install` captures PATH, not HTTP_PROXY/HTTPS_PROXY. If this host needs a
proxy to reach the model provider, sign in and configure **Settings → Network →
HTTP proxy / HTTPS proxy** before creating your first Track. Use the proxy URL
appropriate to your machine, leave each field to save, then start a new Track or
conversation. Existing running cards retain their launch configuration; changing
a field does not repair an already waiting request. Direct-network installations
can leave these fields empty.

A successful web login only verifies local application access, not provider
connectivity. If a first task remains Working without a reply, inspect the agent
status/logs and these settings before retrying the same instruction repeatedly.
The service's isolated Codex log is under
`<child.data_dir>/logs/shared-codex-appserver/stderr.log`.

## 4. Verify identity and initialize upgrade bookkeeping

```bash
curl -fsS http://127.0.0.1:4040/api/version
curl -fsS http://127.0.0.1:4050/health
```

Compare `buildSha` with BUILD.json's `sourceCommit`. A null SHA is not a verified
release identity. Confirm the browser loads `/next/` assets and deep links, login
succeeds, a new Track can be created, and its report survives browser refresh.
Use a small non-destructive task for real Codex checks; do not run real Tier 2
stack E2E on the shared production host.

The first symlink bootstrap has no `state/installed.json`. Register the **same
local package** through the existing admin apply path before future upgrades:

```bash
TOKEN=$(cat ~/.config/neige-app/admin.token)
curl --fail-with-body -X POST http://127.0.0.1:4050/upgrade/apply \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  --data '{"source":{"url":"'"$HOME"'/.local/share/neige-app/releases/0.1.0-alpha.1"},"dryRun":true}'
# Inspect the dry-run: on a first install, noInstalledState is expected.
# This first registration restarts the host; do it before creating real work.
curl --fail-with-body -X POST http://127.0.0.1:4050/upgrade/apply \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  --data '{"source":{"url":"'"$HOME"'/.local/share/neige-app/releases/0.1.0-alpha.1"},"allowBreaking":true}'
unset TOKEN
```

Recheck health and buildSha after the restart; verify `state/installed.json`
under the configured data directory and open the report after one service restart.
This bootstrap opt-in is **not** a blanket permission for later breaking upgrades.
For those, follow [database backup and restore](deploy-and-upgrade.md#81-database-backup-and-manual-restore-read-this-before-allowbreaking).

## 5. Alpha release checklist (maintainer)

- Candidate commit and reviewed diff are fixed; relevant CI has passed, with
  skipped checks distinguished from executed checks.
- The archive checksum verifies after transfer, every binary runs, and the
  target host matches the declared architecture/libc baseline.
- Fresh config requires login; root, `/next/`, its assets and a deep link work
  from the **installed package**, not Vite or the source tree.
- `/api/version.buildSha` matches BUILD.json; package integrity dry-run passes.
- Bootstrap bookkeeping, restart and persistence have been checked in an
  isolated installation. Record what real-model/upgrade checks were not run.
- Known product limitations (including outstanding desktop terminal fixes)
  appear in release notes. This packaging work does not claim to fix them.
- Only after those checks, create the intended tag and publish the artifact,
  checksum, BUILD.json and release notes. The build script does neither action.
