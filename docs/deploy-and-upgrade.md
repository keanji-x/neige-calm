# Deploy & Upgrade Guide

Operator-facing recipe for installing `neige-app` and driving upgrades
through the `/upgrade/apply` admin endpoint. The design rationale lives in
[`upgrade-pipeline.md`](upgrade-pipeline.md); this doc is the "how do I
actually run it" reference.

The full v2 upgrade pipeline was delivered by #396 (PR1 #397, PR2 #398),
#399 (#403), and #404 (#405). If you're on `main` at or after `14c70f3`,
the recipes below work as written.

## 1. Layout

```
~/.local/share/neige-app/
├── releases/
│   ├── current-server  -> rel-XXXX/           (atomically swapped on apply)
│   ├── current-web     -> rel-XXXX/           (atomically swapped on apply)
│   ├── previous-server -> rel-YYYY/           (rollback target)
│   ├── previous-web    -> rel-YYYY/
│   ├── rel-XXXX/                              (release package)
│   │   ├── bin/{calm-server, calm-proc-supervisor, neige-codex-bridge,
│   │   │        neige-mcp-stdio-shim, neige, neige-app}
│   │   ├── web/dist/
│   │   └── manifest.json                      (schemaVersion=2 v2 manifest)
│   └── rel-YYYY/

~/.local/share/neige-calm/                     (data_dir; default for child.data_dir)
├── calm.db                                    (auto-created when child.db_url omitted)
├── mcp/kernel.sock
├── proc-supervisor.sock
├── backups/<release_id>/calm.db{,-wal,-shm}   (one per preserving apply)
└── state/
    ├── installed.json                         (what's installed now)
    ├── supervisor-identity.json               (live proc-supervisor's binary identity)
    └── release-history.jsonl                  (append-only audit log)

~/.config/neige-app/
├── config.toml
└── admin.token
```

`neige-app` listens on the **admin port** (`[admin] listen`, default
`127.0.0.1:4050`); `calm-server` (the kernel) listens on the **calm port**
(`[child] calm_listen`, default `127.0.0.1:4040`). Web UI lives on the
calm port under `/calm/`. The admin port is loopback-only for state
changes; never expose it to LAN.

## 2. First install

### 2.1 Build all binaries + web

```bash
cd /path/to/neige-calm
cargo build --release \
  -p neige-app -p calm-server -p calm-proc-supervisor \
  -p calm-codex-bridge -p neige-mcp-stdio-shim -p neige-cli
(cd web && npm ci && npm run build)
```

### 2.2 Build the first v2 release package

```bash
./target/release/neige-app system package \
  --release-dir ~/.local/share/neige-app/releases/rel-1 \
  --release-id rel-1 \
  --app-bin       target/release/neige-app \
  --web-dist      web/dist \
  --bin calm-server=target/release/calm-server \
  --bin calm-proc-supervisor=target/release/calm-proc-supervisor \
  --bin neige-codex-bridge=target/release/neige-codex-bridge \
  --bin neige-mcp-stdio-shim=target/release/neige-mcp-stdio-shim \
  --bin neige=target/release/neige
```

Inspect `releases/rel-1/manifest.json`:
- `schemaVersion: 2`
- `productMajor: 0` (override at package time with `NEIGE_PRODUCT_MAJOR=N`)
- `compatibility { ... }` (9 fields sourced from
  `calm-server --emit-kernel-compatibility-json` of the just-built binary)
- `units` map covering all 7 crates with `version` + `binarySha256` (or
  `treeSha256` for `web`) + `restartPolicy`. `calmServer.dbMigrationPolicy`
  defaults to `forwardOnly`; override at package time with
  `NEIGE_DB_MIGRATION_POLICY=none|additive|forwardOnly|destructive`.

### 2.3 Point the `current-*` symlinks at rel-1

```bash
cd ~/.local/share/neige-app/releases
ln -sfn rel-1 current-server
ln -sfn rel-1 current-web
```

### 2.4 Write the config + admin token

```bash
mkdir -p ~/.config/neige-app
# Generate a strong token; keep this file 600
head -c 32 /dev/urandom | base64 | tr -d '/+=' > ~/.config/neige-app/admin.token
chmod 600 ~/.config/neige-app/admin.token

cat > ~/.config/neige-app/config.toml <<'TOML'
[admin]
listen     = "127.0.0.1:4050"
token_file = "~/.config/neige-app/admin.token"

[release]
root            = "~/.local/share/neige-app/releases"
current_server  = "~/.local/share/neige-app/releases/current-server"
current_web     = "~/.local/share/neige-app/releases/current-web"
previous_server = "~/.local/share/neige-app/releases/previous-server"
previous_web    = "~/.local/share/neige-app/releases/previous-web"
backups         = "~/.local/share/neige-calm/backups"

[child]
bin                  = "~/.local/share/neige-app/releases/current-server/bin/calm-server"
proc_supervisor_bin  = "~/.local/share/neige-app/releases/current-server/bin/calm-proc-supervisor"
web_dist             = "~/.local/share/neige-app/releases/current-web/web/dist"
calm_listen          = "127.0.0.1:4040"
data_dir             = "~/.local/share/neige-calm"
mcp_stdio_shim_bin   = "~/.local/share/neige-app/releases/current-server/bin/neige-mcp-stdio-shim"
# db_url omitted on purpose:
#   neige-app auto-defaults to sqlite://<data_dir>/calm.db?mode=rwc
#   when child.db_url is unset. Explicit "mock" stays in-memory (dev only).

[upgrade.source]
type = "git"
url  = "https://github.com/keanji-x/neige-calm.git"
ref  = "main"
TOML
```

### 2.5 Install + start the systemd user unit

`neige-app system install --config ~/.config/neige-app/config.toml`
writes `~/.config/systemd/user/neige-app.service`. Then:

```bash
systemctl --user daemon-reload
systemctl --user enable --now neige-app.service
systemctl --user status neige-app.service
```

Verify the surface:

```bash
TOKEN=$(cat ~/.config/neige-app/admin.token)
curl -s http://127.0.0.1:4050/health        # → {"ok":true,"service":"neige-app"}
curl -s http://127.0.0.1:4040/api/version   # 9-field VersionInfo
curl -s http://127.0.0.1:4050/status \
  -H "Authorization: Bearer $TOKEN"         # includes calmServer.identity + procSupervisor.identity
```

`installed.json` will not exist yet — the first upgrade will be classified
as `breaking { reason: noInstalledState }` and rejected unless you pass
`allowBreaking: true`. The bootstrap flow:

```bash
curl -X POST http://127.0.0.1:4050/upgrade/apply \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"source":{"url":"/abs/path/to/releases/rel-1"}, "allowBreaking": true}'
# verdict.kind=breaking, reason=noInstalledState, result=committed
# triggers exec-self (clean for the first install since rel-1 is already current)
```

After this, `state/installed.json` exists and subsequent applies will be
`preserving` or `noop` for compatible releases.

## 3. The upgrade trigger (one curl, many cases)

There is one endpoint:

```bash
curl -X POST http://127.0.0.1:4050/upgrade/apply \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '<UpgradeRequest>'
```

`<UpgradeRequest>` shape:

```json
{
  "source":         { ... },          // optional; merges into config [upgrade.source]
  "allowBreaking":  false,            // optional; required for breaking apply to commit
  "dryRun":         false             // optional; compute verdict only, zero writes
}
```

Common bodies:

```bash
# Use config's [upgrade.source] as-is
-d '{}'

# Override just the ref (commit/tag/branch); url + type from config
-d '{"source":{"ref":"479afae"}}'

# Full source override
-d '{"source":{"type":"git","url":"https://...","ref":"479afae"}}'

# Local pre-built package
-d '{"source":{"url":"/abs/path/to/release-dir"}}'

# Dry-run any source → verdict only, no disk writes (local source only;
# git sources must build to compute a verdict, so git + dry-run is rejected)
-d '{"source":{"url":"/abs/.../release-dir"}, "dryRun": true}'

# Breaking opt-in
-d '{"source":{"ref":"v1.0.0"}, "allowBreaking": true}'
```

Response (always the same shape):

```json
{
  "releaseId":            "rel-2",
  "verdict": {
    "kind":               "preserving",      // noop | preserving | breaking
    "unitsChanged":       ["calmServer"],
    "deferred":           [],
    "refreshFrontend":    false,
    "requiresDbBackup":   true,
    "reason":             null               // breaking reason when kind=breaking
  },
  "result":               "committed",       // committed | rolledBack | rejected | dryRun
  "unitsChanged":         ["calmServer"],
  "deferred":             [],
  "durationMs":           1718,
  "error":                null,
  "releaseHistoryEntry":  { /* full entry as written to release-history.jsonl */ }
}
```

## 4. Verdict cases — what apply actually does

| `verdict.kind` + flag             | Trigger                                                        | apply does                                                             | calm-server PID | proc-supervisor PID |
|-----------------------------------|----------------------------------------------------------------|------------------------------------------------------------------------|------------------|----------------------|
| `noop`                            | All unit hashes match installed                                | Short-circuits before staging; writes a noop history entry             | unchanged        | unchanged            |
| `preserving`                      | `productMajor` unchanged; only `calmServer` changed            | Backup DB → swap `current-server` symlink → `/restart` → 60s healthcheck → success | **new PID** | unchanged            |
| `preserving` + `deferred`         | Only `calmProcSupervisor` (or other `deferUntilFullReboot` unit) changed | Swap symlink only; supervisor process keeps running old binary         | unchanged        | unchanged            |
| `preserving` + `refreshFrontend`  | Only `web` changed                                             | Swap `current-web` symlink + write sentinel file for frontend polling  | unchanged        | unchanged            |
| `preserving` + healthcheck fail   | Apply ran, healthcheck timed out (60s) or new calm-server exited | Auto-rollback: revert symlinks, restore DB backup, `/restart` old binary | unchanged        | unchanged            |
| `breaking` + `allowBreaking=false`| `productMajor` changed / wire incompat / destructive DB migration | `400 result=rejected`; no disk writes                                   | unchanged        | unchanged            |
| `breaking` + `allowBreaking=true` | Same                                                           | Swap all symlinks → `202 result=committed` → kill calm-server + proc-supervisor → exec self | **dies, new on respawn** | **dies, new on respawn** |

The healthcheck uses a **startup-progress** model: a process that hasn't
yet bound the port is treated as "starting" (keep polling); a process
that has **exited** triggers immediate rollback. Slow DB migrations
under sqlx are tolerated up to the 60s ceiling.

## 5. History + rollback + full-reboot

### 5.1 `GET /upgrade/history`

```bash
curl -s "http://127.0.0.1:4050/upgrade/history?limit=10" \
  -H "Authorization: Bearer $TOKEN" | jq .
```

Tail of `<data_dir>/state/release-history.jsonl`. Each line is a
`ReleaseHistoryEntry` with `releaseId`, `kind` (`apply` / `rollback`),
`verdictKind`, `result`, `unitsChanged`, `deferred`, `durationMs`,
`error`, `source`, `dbBackup` path, and `symlinkChanges`.

### 5.2 `POST /upgrade/rollback`

```bash
curl -X POST http://127.0.0.1:4050/upgrade/rollback \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"to":"rel-1"}'
```

Reverse-replays the most recent committed non-rollback preserving apply:
revert symlinks + restore DB backup + `/restart`. Rejected with
`400 invalid_rollback_target` if `to` doesn't match the prior install,
`409 backup_missing` if the backup file was deleted manually. Chained
rollbacks (rolling back multiple releases) are tracked in #402; today
you can only undo the last preserving apply.

### 5.3 `POST /upgrade/full-reboot`

```bash
curl -X POST http://127.0.0.1:4050/upgrade/full-reboot \
  -H "Authorization: Bearer $TOKEN"
```

Schedules `exit(0)` from `neige-app` (after killing calm-server +
proc-supervisor cleanly). systemd's `Restart=always` brings it back
with whatever symlinks are committed — used after a `preserving +
deferred` apply to actually activate the new proc-supervisor binary.

## 6. Concurrent apply / dry-run / safety

- **Concurrent apply**: a second `/upgrade/apply` while one is in flight
  returns `409 apply_in_progress` immediately. Only one upgrade at a
  time; no queue.
- **Dry-run**: `{"dryRun": true}` computes the verdict from the local
  source's `manifest.json` (git sources require `dryRun: false` because
  the source needs to be built to compute a verdict). No disk writes.
- **Rejected / dry-run / noop never stage**: these short-circuit before
  the `staged/<release_id>/` directory is created (and clean it up if
  the source already existed). A second apply with the same `release_id`
  after a rejection just works.
- **Supervisor restart-rate limit**: if calm-server crashes more than
  10 times in 60 seconds, neige-app sets `desired_running=false` and
  stops respawning. Reset with `POST /restart`. Visible on `/status`.

## 7. Troubleshooting

**`/api/version` returns 503**: calm-server is in `starting` or `exited`
state. Look at `journalctl --user -u neige-app.service` for the kernel
boot tail. The boot loop will print a stable error if the DB or socket
cannot be opened.

**Apply returns `db_url is not configured as sqlite://path`**: you
explicitly set `child.db_url = "mock"` (in-memory). Remove that line
or change to `sqlite://...` — see config note in §2.4. Omitting the
field altogether is the recommended production path; neige-app
auto-fallbacks to `sqlite://<data_dir>/calm.db?mode=rwc` and logs the
choice on startup.

**`/upgrade/apply` succeeds but `/status` shows calm-server crashing**:
the new release's binary is broken. The auto-rollback path runs on
healthcheck failure; if it didn't fire (manual symlink swap?), revert
manually:

```bash
cd ~/.local/share/neige-app/releases
ln -sfn previous-server-target current-server   # check `ls -la previous-server`
ln -sfn previous-web-target    current-web
systemctl --user restart neige-app.service
```

**`/status` reports `state=running` but `childPid=null`**: the running
`proc-supervisor` was adopted on boot (`<data_dir>/proc-supervisor.sock`
already had a listener). #404's `SO_PEERCRED` fix populates the PID; if
you're on `main` ≥ `14c70f3` and still see `null`, file a bug.

**Sessions lost across breaking upgrade**: expected. Breaking applies
kill calm-server + proc-supervisor; on the new boot, calm-server's
`reconcile_supervisor_on_boot` marks any terminal whose proc is gone
with `exit_code = -1`. Use rollback or restore from a DB backup if the
breaking upgrade was a mistake.

## 8. Pre-flight checklist before applying to production

1. `dryRun` against the target ref → confirm verdict is `preserving`,
   `requiresDbBackup` matches expectation, no breaking surprises.
2. Read `/upgrade/history` to confirm the prior install is the rollback
   target you expect.
3. Confirm `/api/version` matches the release you think you're on.
4. If applying breaking: confirm with the team, then pass
   `allowBreaking: true`; expect PTYs to die.

## 9. What's NOT yet supported (open follow-ups)

- **Frontend auto-refresh** after `refreshFrontend` (#400): the sentinel
  file is bearer-gated, so browsers without the token don't see updates;
  manual reload required.
- **Multi-step rollback chains** (#402): only the last preserving apply
  is rollback-able today.
- **CLI wrappers** (#402): `neige-app system history`, `system rollback`,
  `system full-reboot` are not yet shipped; use the curl recipes above.
- **Healthcheck timeout `[upgrade.healthcheck]`** (#402): hardcoded 60s.
- **PTY survival under real workload** (#401): proven only for the
  thread-based fake supervisor in CI; real-world supervisor PID
  survival has been validated via manual deploy testing (PRs #397,
  #398, #403, #405).
