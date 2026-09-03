# Deploy & Upgrade Guide

Operator-facing recipe for installing `neige-app` and driving upgrades
through the `/upgrade/apply` admin endpoint.

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
├── backups/<release_id>/calm.db{,-wal,-shm}   (written by every apply that
│                                                changes calm-server, breaking
│                                                or preserving — see §8.1)
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
- `productMajor: 1` (the compiled-in default; override at package time with
  `NEIGE_PRODUCT_MAJOR=N`). It is deliberately a source constant, not an
  environment-only knob: a forgotten export would silently downgrade a
  breaking release to a `preserving` verdict.
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

[source]
url  = "https://github.com/keanji-x/neige-calm.git"
branch = "main"

[systemd]
bin = "~/.local/bin/neige-app"
TOML
```

### 2.5 Install + start the systemd user unit

First create the stable executable path named by `[systemd].bin`:

```bash
mkdir -p ~/.local/bin
ln -sfn ~/.local/share/neige-app/releases/current-server/bin/neige-app \
  ~/.local/bin/neige-app
```

`~/.local/bin/neige-app system install --config ~/.config/neige-app/config.toml`
writes `~/.config/systemd/user/neige-app.service`. It does **not** copy a
release, create the `current-*` symlinks, or start systemd; steps 2.2–2.4 and
the stable executable link above are prerequisites. Then:

`system install` bakes the caller's `$PATH` into the unit; use
`--path "$PATH"` if the install shell has the wrong PATH. Warnings like
`warning: <tool> not found on PATH` for `codex` / `claude` / `git` mean
matching tracks will be inert until PATH includes those tools.

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
  "source":         { ... },          // optional; merges into config [source]
  "allowBreaking":  false,            // optional; required for breaking apply to commit
  "dryRun":         false             // optional; validate local package + compute verdict, zero writes
}
```

Common bodies:

```bash
# Use config's [source] as-is
-d '{}'

# Override the configured branch (`ref` is accepted as an API alias)
-d '{"source":{"ref":"release/next"}}'

# Full source override
-d '{"source":{"type":"git","url":"https://...","ref":"release/next"}}'

# Local pre-built package
-d '{"source":{"url":"/abs/path/to/release-dir"}}'

# Dry-run a local package → verify every manifested byte and compute the
# verdict, with no disk writes. Git sources are rejected because they must build.
-d '{"source":{"url":"/abs/.../release-dir"}, "dryRun": true}'

# Breaking opt-in
-d '{"source":{"ref":"release/v1"}, "allowBreaking": true}'
```

Remote source checkout currently resolves `origin/<ref>`, so the override must
name a branch. Tags and arbitrary commit SHAs are not supported by this source
path yet; use a local pre-built package for an immutable release artifact.

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
  "releaseHistoryEntry":  { /* response metadata; dryRun does not append it to history */ }
}
```

## 4. Verdict cases — what apply actually does

| `verdict.kind` + flag             | Trigger                                                        | apply does                                                             | calm-server PID | proc-supervisor PID |
|-----------------------------------|----------------------------------------------------------------|------------------------------------------------------------------------|------------------|----------------------|
| `noop`                            | All unit hashes match installed                                | Short-circuits before staging; writes a noop history entry             | unchanged        | unchanged            |
| `preserving`                      | `productMajor` unchanged; only `calmServer` changed            | Backup DB → swap `current-server` symlink → `/restart` → derived healthcheck → success | **new PID** | unchanged            |
| `preserving` + `deferred`         | Only `calmProcSupervisor` (or other `deferUntilFullReboot` unit) changed | Swap symlink only; supervisor process keeps running old binary         | unchanged        | unchanged            |
| `preserving` + `refreshFrontend`  | Only `web` changed                                             | Swap `current-web` symlink + write sentinel file for frontend polling  | unchanged        | unchanged            |
| `preserving` + healthcheck fail   | Apply ran, the derived healthcheck timed out, or new calm-server exited | Auto-rollback: revert symlinks, restore DB backup, `/restart` old binary | **new PID on old binary** | unchanged            |
| `breaking` + `allowBreaking=false`| `productMajor` changed / wire incompat / destructive DB migration | `400 result=rejected`; no staging, symlink, or DB activation; rejection is appended to history | unchanged | unchanged |
| `breaking` + `allowBreaking=true` | Same                                                           | Swap all symlinks → `202 result=committed` → kill calm-server + proc-supervisor → exec self | **dies, new on respawn** | **dies, new on respawn** |

The healthcheck uses a **startup-progress** model: a process that hasn't
yet bound the port is treated as "starting" (keep polling); a process
that has **exited** triggers immediate rollback. The deadline is derived from
calm-server's shared-Codex app-server timing knobs as
`2 × start-timeout + stop-grace + 60s` (360s at defaults), capped at 30 days.
These are not `[timing].stop_grace_ms`; healthy boots return on the first
successful probe.

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
- **Dry-run**: `{"dryRun": true}` validates the local package's release id,
  manifested SHA-256/byte lengths, symlink-free payload, and absence of extra
  files before computing the verdict. Git sources require `dryRun: false`
  because the source needs to be built. No disk writes or staging occur.
- **Rejected / dry-run / noop never stage**: these short-circuit before
  the `staged/<release_id>/` directory is created. A second apply with the
  same `release_id` after a rejection just works, provided no stale staged
  directory was created by some earlier mutating attempt.
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

1. Build or download the target ref as a local package, then run `dryRun`
   against that package → confirm verdict is `preserving`,
   `requiresDbBackup` matches expectation, no breaking surprises.
   **Caveat for breaking verdicts, see §8.1**: `requiresDbBackup` is
   hardcoded `false` for `breaking`, which is not the same as "no backup
   is taken".
2. Read `/upgrade/history` to confirm the prior install is the rollback
   target you expect.
3. Confirm `/api/version` matches the release you think you're on.

### 8.1 Database backup and manual restore (read this before `allowBreaking`)

**The product does take a backup, and you cannot roll it back through the
API.** Both halves matter:

- `apply_breaking` calls `backup_db` whenever the `calmServer` unit changes,
  which every real upgrade does. `backup_db` stops the child, copies
  `calm.db` plus its `-wal` / `-shm` sidecars, and resumes — so the files
  under `<data_dir>/backups/<release_id>/` are consistent.
- `POST /upgrade/rollback` reverse-replays the most recent committed
  non-rollback **preserving** apply, and rejects anything else. After a
  breaking apply the backup is on disk with **no API that puts it back**.
- The breaking path also has no healthcheck window, so there is no
  automatic revert if the new binary is bad.
- `dryRun` reports `requiresDbBackup: false` for a breaking verdict
  regardless. Do not read that as "nothing was backed up".

**Before applying breaking:**

1. Confirm `<data_dir>/backups/` is writable and has room for a copy of
   `calm.db`.
2. Record the release id you are on:

   ```bash
   curl -s http://127.0.0.1:4050/upgrade/history \
     -H "Authorization: Bearer $TOKEN" | tail -1
   curl -s http://127.0.0.1:8080/api/version
   ```

3. Take your own backup. **Do not `cp` the three files while the service is
   running** — `calm.db`, `calm.db-wal` and `calm.db-shm` are not a snapshot
   of the same instant, and the copy can restore as a corrupt database.
   Use one of these instead:

   ```bash
   # (a) online backup — preferred, no downtime. Produces ONE consistent
   #     file; do not copy -wal / -shm alongside it.
   sqlite3 ~/.local/share/neige-calm/calm.db \
     ".backup '/var/backups/neige/calm-preupgrade.db'"

   # (b) stop first, then copy all three — what the product itself does
   systemctl --user stop neige-app
   cp ~/.local/share/neige-calm/calm.db{,-wal,-shm} /var/backups/neige/ 2>/dev/null || \
     cp ~/.local/share/neige-calm/calm.db /var/backups/neige/
   systemctl --user start neige-app
   ```

4. Confirm with the team, then pass `allowBreaking: true`; expect PTYs to
   die and both calm-server and proc-supervisor to change PID.

**Manual restore** (the only route back from a breaking apply; schema
migrations are forward-only, so an old binary cannot read the new DB):

```bash
# 1. stop the unit
systemctl --user stop neige-app

# 2. put the database back.
#    From an sqlite3 `.backup` single file — the stale sidecars MUST go,
#    or SQLite will replay them over the restored database:
cp /var/backups/neige/calm-preupgrade.db ~/.local/share/neige-calm/calm.db
rm -f ~/.local/share/neige-calm/calm.db-wal ~/.local/share/neige-calm/calm.db-shm
#    From the product's own backup dir, restore all three together instead:
# cp ~/.local/share/neige-calm/backups/<release_id>/calm.db{,-wal,-shm} \
#    ~/.local/share/neige-calm/

# 3. point the symlinks back at the previous release
cd ~/.local/share/neige-calm/releases   # or wherever releases/ lives
ln -sfn "$PWD/rel-N/bin"      ../current-server
ln -sfn "$PWD/rel-N/web/dist" ../current-web
ln -sfn "$PWD/rel-N"          ../current-app

# 4. start the unit and confirm the old release answers
systemctl --user start neige-app
curl -s http://127.0.0.1:8080/api/version
```

### 8.2 Plugin compatibility (#1209, #1268)

From #1209 onward, the ids a trusted plugin declares in its manifest
**must** be keys in the kernel's track-template roster — today
`issue-development`, `small-change`, `investigation`.

**#1268 renamed the array itself: `workflows[]` is now `templates[]`.**
The entries are unchanged (`{ "id": "<kernel template id>" }`); only the
containing key moved. This *is* a plugin-manifest schema break — #1209
deliberately avoided one, and #1268 took it only after confirming there are
no third-party plugins to break (the sole manifest declaring the array is
the in-repo `plugins/git-forge/manifest.json`, updated in the same commit).

A manifest still spelling the array `workflows` is **rejected at parse
time**, naming the new key:

```
manifest validation failed at `workflows`: renamed to `templates` in #1268;
rename the key (its entries are unchanged: { "id": "<kernel template id>" })
```

That refusal is deliberate. `Manifest` tolerates unknown top-level keys, so
the default behaviour would have been *silent*: the plugin would parse, declare
no binding at all, and the only symptom would be `issue-development` losing its
`input_schema` — every later `template_input` create failing with a 400 that
points nowhere near the cause.

**`manifest_version` moved 1 → 2, but only for manifests that declare a
binding.** The guard above protects you moving *forward*. Moving *backward* it
cannot help: a `templates[]` manifest handed to a pre-#1268 kernel parses
clean, because that kernel ignores unknown top-level keys — it binds nothing,
`issue-development` loses its `input_schema`, and there is no error and no log.
The one thing an old kernel refuses on its own is a `manifest_version` it does
not know, so any manifest declaring a non-empty `templates[]` must now say
`"manifest_version": 2`:

```
manifest validation failed at `manifest_version`: must be 2 to declare
`templates` (#1268 renamed the array; a v1 kernel would ignore it and
silently bind nothing), got 1
```

A manifest that declares **no** bindings keeps working at version 1 and needs
no edit. That scoping is intentional: such a file reads identically on both
kernels (the only thing they disagree about is the name of an array it does not
have), and forcing it to 2 would refuse every existing connector manifest for
no benefit — see the boot-path note below for why that would be quiet.

**Boot is lenient; install and reload are strict.** `POST /api/plugins`
(install) and `/reload` surface a manifest error to the caller. The boot-time
registry load does not: `registry::load_from_dir` logs
`manifest load failed — skipping plugin` at WARN and continues, so a plugin
whose manifest this release refuses simply **does not appear** after a restart
— no failed startup, no error in the UI. Run the scan below *before* you
upgrade rather than relying on noticing at boot, and if a plugin goes missing
after a restart, grep the server log for `manifest load failed`.

What #1209 changed, and #1268 did not, is the *acceptance* semantics:
declaring an id outside the roster does not let `POST /api/tracks` create a
track bound to it. That request returns

```
400  track create: `template_id` must reference a known track template; got `<id>`
```

Also note the request body itself changed in the same release: the two
track-create template fields are now spelled `template_id` and
`template_input`. `CreateTrackRequest` uses `deny_unknown_fields`, so any
out-of-repo script still sending the previous names is rejected by the JSON
extractor before the handler runs: `422 Unprocessable Entity`, plain-text body
`Failed to deserialize the JSON body into the target type: unknown field
\`workflow_id\``. Deliberately a hard failure rather than a silent partial
success. Browser bundles are covered by the `minWebCompatVersion`
floor and will show the refresh curtain instead of issuing such a request.

**Scan your installed plugins before upgrading.** Run this in the plugin
install root; any output names a plugin this release will break. It reports
all three failure modes: a manifest still carrying the retired `workflows` key
(refused at parse time), a manifest declaring `templates[]` while still at
`manifest_version: 1` (also refused), and a declared id outside the roster
(parses, but cannot be bound):

```sh
for m in <plugins_dir>/*/manifest.json; do
  [ -f "$m" ] || continue      # the plugin root may legitimately be absent or
                               # empty; the kernel treats that as an empty
                               # registry, and an unmatched glob would otherwise
                               # hand jq a literal path and error out
  jq -r --argjson roster '["issue-development","small-change","investigation"]' \
    'if has("workflows") then
       "\(input_filename): retired `workflows` key — rename it to `templates`"
     else empty end,
     if ((.templates // []) | length) > 0 and (.manifest_version != 2) then
       "\(input_filename): declares `templates` at manifest_version \(.manifest_version) — must be 2"
     else empty end,
     ((.templates // [])[].id | select(. as $i | $roster | index($i) | not)
      | "\(input_filename): \(.)")' "$m"
done
```

Zero output (and exit 0) means no installed plugin is affected.

## 9. What's NOT yet supported (open follow-ups)

- **Frontend auto-refresh** after `refreshFrontend` (#400): the sentinel
  file is bearer-gated, so browsers without the token don't see updates;
  manual reload required.
- **Multi-step rollback chains** (#402): only the last preserving apply
  is rollback-able today.
- **CLI wrappers** (#402): `neige-app system history`, `system rollback`,
  `system full-reboot` are not yet shipped; use the curl recipes above.
- **Dedicated healthcheck configuration `[upgrade.healthcheck]`** (#402):
  not yet supported. The current deadline is derived from calm-server's
  shared-Codex app-server start/stop knobs and a fixed 60s margin;
  `[timing].stop_grace_ms` does not participate.
- **PTY survival under real workload** (#401): proven only for the
  thread-based fake supervisor in CI; real-world supervisor PID
  survival has been validated via manual deploy testing (PRs #397,
  #398, #403, #405).
