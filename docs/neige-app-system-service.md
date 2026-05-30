# neige-app System Service

`neige-app` is the host-side shell for neige-calm. M1 supports one mode:
`system`, intended to be supervised by systemd while it supervises the current
`calm-server` release.

The split is deliberate:

- systemd owns `neige-app`.
- `neige-app` owns the `calm-server` child process.
- `calm-server` remains the business kernel and does not update itself.
- On Unix, `neige-app` starts `calm-server` in its own process group and
  sends restart/shutdown signals to that process group. That process group is
  the M1 lifecycle boundary for the supervised kernel and descendants.

## Run

```bash
mkdir -p ~/.config/neige-app
openssl rand -hex 32 > ~/.config/neige-app/admin.token
chmod 600 ~/.config/neige-app/admin.token

neige-app system init-config --config ~/.config/neige-app/config.toml

neige-app system serve \
  --config ~/.config/neige-app/config.toml
```

Admin API:

```bash
curl http://127.0.0.1:4050/health
curl http://127.0.0.1:4050/status
curl -H "Authorization: Bearer $(cat ~/.config/neige-app/admin.token)" \
  -X POST http://127.0.0.1:4050/restart
curl -H "Authorization: Bearer $(cat ~/.config/neige-app/admin.token)" \
  -X POST http://127.0.0.1:4050/update/apply
```

The admin listener defaults to loopback. Keep it loopback-only unless a later
deployment adds authentication or exposes it through a protected local socket.
Loopback is not authentication on multi-user machines: M1 requires a bearer
token for state-changing endpoints. `GET /health` and `GET /status` are public
read-only endpoints. `POST /restart` and `POST /update/apply` require
`Authorization: Bearer <token>` and are refused if no token is configured.
The token can be supplied as `NEIGE_APP_ADMIN_TOKEN` or through
`--admin-token-file` / `NEIGE_APP_ADMIN_TOKEN_FILE`.
`POST /restart` returns `202 Accepted`; restart is asynchronous, so callers
should poll `GET /status` or the kernel's `GET /api/version` before assuming the
new child is serving traffic.
`POST /update/apply` intentionally returns `501 Not Implemented` in M1; it is
reserved so scripts can settle on the endpoint name before release activation
exists.

## systemd

M1 generates a user-manager systemd unit by default. Install it under
`~/.config/systemd/user/`; do not install this generated unit as a system unit.

```bash
mkdir -p ~/.config/neige-app ~/.config/systemd/user
openssl rand -hex 32 > ~/.config/neige-app/admin.token
chmod 600 ~/.config/neige-app/admin.token

neige-app system install --config ~/.config/neige-app/config.toml

systemctl --user daemon-reload
systemctl --user enable --now neige-app.service
```

`system install` creates the config if missing, creates `admin.token_file` if
missing, and writes the user unit to the configured `systemd.unit_path`. It
refuses to overwrite an existing unit unless `--force` is passed. It does not
run `systemctl` for you.

## Update State Machine

The reserved admin path is `POST /update/apply`, currently a `501` placeholder.
`neige-app system preflight`, `neige-app system package`, and
`neige-app system upgrade` are local-only tools. `system upgrade` can build from
`[source]`, package, preflight, stage, and optionally switch symlinks with
`--activate`. Web-only activation hot-switches the configured web release
symlink; server and bundle activation still require a service restart before
the running backend changes.

The later implementation should follow this state machine:

```text
check
download/stage
verify
preflight
backup
activate
healthcheck
commit
rollback
```

Rules for the future implementation:

- Verify manifest signatures and file hashes before activation.
- Unpack into a staging directory; never overwrite the active release in place.
- Activate by atomically switching the relevant `current_*` symlink or
  symlinks to the staged release.
- Back up SQLite before booting a release that may run migrations.
- Healthcheck through the real served API, starting with `GET /api/version`.
- Roll back both the `current` symlink and the DB backup if healthcheck fails.

## Compatibility Preflight

Preflight takes the current install's version JSON and a target
`manifest.json`, then prints structured JSON:

```bash
curl -fsS http://127.0.0.1:4040/api/version > /tmp/current-version.json

neige-app system preflight \
  --mode bundle \
  --current-version /tmp/current-version.json \
  --manifest ./target/neige-release/manifest.json
```

Result shape:

```json
{
  "allowed": true,
  "mode": "bundle",
  "requiresDbBackup": true,
  "reason": "forward-only DB migration requires backup",
  "requiredAction": "backup-db-before-activate"
}
```

Rules:

- `web-only`: target web must have the same `apiVersion`,
  `syncEventVersion`, and `mcpProtocolVersion` as the current server, and its
  `webCompatVersion` must be at least the current server's
  `minWebCompatVersion`.
- `server-only`: target server must keep the same protocol versions as the
  current install, and the current web compatibility must meet the target
  server's `minWebCompatVersion`.
- `bundle`: target web and target server are checked against each other inside
  the manifest. This mode does not trust current web/server skew.
- `app-only`: only checks that the manifest contains an app unit named
  `neige-app`.
- `dbMigrationPolicy=destructive` is denied for automatic preflight.
  `additive` and `forwardOnly` are allowed but return
  `requiresDbBackup=true`, because migration bookkeeping can still block
  rollback to an older binary.
- Missing manifest fields fail closed because the JSON cannot be parsed as the
  release schema.

## Local Release Package

`neige-app system package` builds a directory package. It never downloads from
the network and refuses to overwrite a non-empty package directory. M1 uses the
host `sha256sum` command to hash copied files.

```bash
neige-app system package \
  --release-dir ./target/neige-release \
  --release-id local-dev \
  --web-dist ./web/dist \
  --web-version local-web \
  --calm-server-version local-server \
  --api-version 1 \
  --sync-event-version 1 \
  --mcp-protocol-version 2025-11-25 \
  --web-compat-version 2 \
  --min-web-compat-version 2 \
  --db-migration-policy forwardOnly \
  --bin calm-server=./target/release/calm-server \
  --bin neige-codex-bridge=./target/release/neige-codex-bridge \
  --bin neige-mcp-stdio-shim=./target/release/neige-mcp-stdio-shim \
  --bin neige=./target/release/neige
```

The output directory contains:

```text
manifest.json
bin/*
web/dist/*
```

Each copied file is listed in `manifest.json` with `path`, `sha256`, `bytes`,
and the owning unit. Passing `--app-bin ./target/release/neige-app
--app-version <version>` adds the app unit and copies `bin/neige-app`.

If `--out <dir>` is supplied, the package is written to
`<dir>/<basename --release-dir>`; otherwise `--release-dir` is the final package
directory.

See `docs/neige-app-release.md` for the manifest schema.

## Staged Upgrade

Default source-driven flow:

```bash
neige-app system upgrade --config ~/.config/neige-app/config.toml
```

Activate after staging:

```bash
neige-app system upgrade --config ~/.config/neige-app/config.toml --activate
```

Advanced package entry:

```bash
neige-app system upgrade \
  --config ~/.config/neige-app/config.toml \
  --package ./target/neige-release
```

The command auto-detects the mode from `manifest.json`; `--mode` is only an
override. It performs local package preflight, verifies every file hash and byte
length listed in `manifest.json`, rejects symlinks and unmanifested regular
files, and copies only `manifest.json` plus manifest-listed files to the
configured release root's `staged/` directory. It refuses unsafe `releaseId`
values and non-empty stage targets.

For git sources, `neige-app` writes a `.neige-app-source.json` marker after
clone and refuses to fetch/reset an existing checkout directory unless that
marker and `origin` URL match the config. Source-driven packaging also requires
explicit compatibility and DB migration policy fields in `[source]`.

With `--activate`, it backs up SQLite when required, updates the relevant
`previous_*` symlink, and atomically switches the matching `current_*` symlink
to the staged release. `web-only` activation switches `current_web` in place and
returns `restartRequired=false`; refresh frontend clients to pick up the new
assets. `server-only` and `bundle` activation return `restartRequired=true` and
do not directly control a running `system serve` process; use the printed admin
API or `systemctl --user restart neige-app.service` next step.
SQLite backup uses the `sqlite3` CLI online backup command; activation fails if
`sqlite3` is unavailable or the backup command fails.

Rollback is available as a symlink-only operation:

```bash
neige-app system rollback --config ~/.config/neige-app/config.toml
```

DB restore is not implemented.

## Admin Apply Endpoints

`system serve` exposes the PR 2 apply state machine at
`POST /upgrade/apply`. The endpoint accepts a v2 package source, stages it,
computes the manifest/installed-state verdict, and returns only after commit,
rollback, rejection, or dry run.

Breaking upgrades return `202` after `installed.json` and release history are
written. The process then waits briefly for the response to flush and execs the
new `bin/neige-app` through the current server-release symlink. Under systemd
this keeps the same service PID and lets the manager treat the process as
continuing.

Before execing, `neige-app` terminates the supervised `calm-server` process
tree. If the breaking release also changes `calmProcSupervisor`, it terminates
that process tree too. Each tree receives SIGTERM, then SIGKILL after the
configured stop grace if it is still alive.

On boot, `neige-app` checks the configured proc-supervisor socket. If an
existing supervisor answers, the process adopts it instead of spawning a new
one and reloads the persisted supervisor identity from
`<data_dir>/state/supervisor-identity.json`.

`neige-app` also checks `<data_dir>/mcp/kernel.sock` before spawning
`calm-server`. If another process still owns that socket, the process is
treated as an orphaned server from a previous service image and is killed before
the new server starts.

`POST /upgrade/full-reboot` returns `202` and exits shortly afterward so
systemd can restart the service. Use it after a preserving upgrade deferred a
`neigeApp` or `calmProcSupervisor` binary change.

If `[child] data_dir` is configured and `[child] db_url` is omitted or empty,
`neige-app` defaults `CALM_DB_URL` to
`sqlite://<data_dir>/calm.db?mode=rwc`. An explicit `db_url = "mock"` remains
mock mode for development; set an explicit `sqlite://...` URL to use another
database path.

`GET /upgrade/history` tails `<data_dir>/state/release-history.jsonl`.
`POST /upgrade/rollback` is intentionally narrow in PR 2: it only reverses the
last committed preserving apply to the immediately previous release id.
