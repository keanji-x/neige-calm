# Upgrade Pipeline

Status: PR 1 data model.

Scope: `neige-app` release manifests, installed-state accounting, preflight
verdicts, and spawn-time process identity.

This document describes the upgrade model that PR 1 introduces and that PR 2
will use when it implements the daemon-side apply endpoint.

It is intentionally explicit about what is present now and what is still
deferred.

## 1. Model

The upgrade pipeline is based on three ideas.

1. The product has a compatibility major named `productMajor`.
2. Each independently shipped crate or bundle is a release unit.
3. The upgrade result is a function of installed state and target manifest.

The release does not declare "this is preserving" or "this is breaking".
Preflight computes that verdict from facts.

The same target release can be preserving for one installation and breaking for
another.
That matters because a user can skip releases.

It also matters because the current running process identity may not match the
current disk symlink after a deferred supervisor upgrade.

## 2. Stability Tiers

The vocabulary follows `docs/upgrade-stability.md`.

Tier A surfaces are persisted state.

Tier B surfaces are cross-process or frontend/backend negotiation surfaces.

Tier C surfaces are internal.

Tier D surfaces are experimental or observable-only.

The upgrade manifest does not make Tier C surfaces stable.

It records only Tier A and Tier B compatibility boundaries that decide whether
an in-place upgrade can preserve running user work.

## 3. Product Major

`productMajor` is the broad compatibility boundary for the whole product.

Within one product major, releases are expected to preserve all Tier B wire
contracts listed in the manifest compatibility object.

Across product majors, preflight returns a breaking verdict.

Breaking upgrades require explicit opt-in in PR 2.

PR 1 only computes the verdict.

The value is not derived from `workspace.package.version`.

Crate semver and product major answer different questions.

Crate semver says whether one crate changed according to Rust/package
conventions.

Product major says whether the whole running installation can be upgraded
without intentionally breaking live boundaries.

## 4. Release Units

Manifest v2 promotes release units to a `BTreeMap<UnitName, ReleaseUnit>`.

The unit names are stable wire values.

Current units:

- `neigeApp`
- `calmServer`
- `calmProcSupervisor`
- `web`
- `neigeCodexBridge`
- `neigeMcpStdioShim`
- `neigeCli`

Each unit carries a crate or bundle version and a content hash.

Binary units use `binarySha256`.

The web unit uses `treeSha256`.

Some units are effective immediately after restart.

Some units are only effective on the next spawn.

Some units are intentionally deferred until a full reboot.

The unit's `restartPolicy` records that behavior.

## 5. Restart Policies

`restartViaAdminApi` is for `calmServer`.

In a preserving upgrade, PR 2 can switch the server symlink and call the local
admin `/restart` endpoint.

`deferUntilFullReboot` is for `calmProcSupervisor` and preserving `neigeApp`
updates.

The disk symlink can be switched, but the running process is left alone.

The new binary becomes active after a full service restart.

`refreshFrontend` is for `web`.

The web symlink can be switched and clients can be told to reload.

`nextSpawn` is for helper binaries such as `neigeCodexBridge` and
`neigeMcpStdioShim`.

The new binary is picked up when future work spawns the helper.

`execSelfForBreakingOnly` is reserved for `neigeApp` breaking upgrades.

PR 1 records the policy.

PR 2 implements the behavior.

## 6. Manifest v2 Schema

The v2 manifest is JSON with camelCase field names.

`schemaVersion` must be `2`.

`files` keeps the v1 shape.

The compatibility object is top-level, rather than duplicated under web and
server units.

Example:

```json
{
  "schemaVersion": 2,
  "releaseId": "2026.05.30-rc1",
  "productMajor": 0,
  "compatibility": {
    "terminalFrameVersion": 4,
    "terminalProtocolVersion": 4,
    "apiVersion": "1",
    "syncEventVersion": 1,
    "mcpProtocolVersion": "2024-11-05",
    "pluginMcpProtocolVersion": "2025-11-25",
    "webCompatVersion": 2,
    "minWebCompatVersion": 2,
    "supervisorControlVersion": 1
  },
  "units": {
    "calmServer": {
      "version": "0.5.4",
      "binarySha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "restartPolicy": "restartViaAdminApi",
      "dbMigrationPolicy": "forwardOnly"
    },
    "calmProcSupervisor": {
      "version": "0.5.1",
      "binarySha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "restartPolicy": "deferUntilFullReboot"
    },
    "web": {
      "version": "0.5.4",
      "treeSha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
      "restartPolicy": "refreshFrontend"
    }
  },
  "files": [
    {
      "path": "bin/calm-server",
      "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "bytes": 123456,
      "unit": "calmServer"
    }
  ]
}
```

### Compatibility Fields

`terminalFrameVersion` is `calm-session::FRAME_VERSION`.

It protects the outer terminal frame envelope.

`terminalProtocolVersion` is `calm-session::PROTOCOL_VERSION`.

It protects terminal application messages.

`apiVersion` is `calm_server::routes::version::API_VERSION`.

It protects REST contract compatibility.

`syncEventVersion` is `calm_server::event::SYNC_EVENT_VERSION`.

It protects persisted and replayed sync envelopes.

`mcpProtocolVersion` is the kernel-as-MCP-server protocol date.

It is sourced from `calm_server::mcp_server::transport::KERNEL_MCP_PROTOCOL_VERSION`.

`pluginMcpProtocolVersion` is the plugin-host protocol date.

It is sourced from `calm_server::plugin_host::mcp::KERNEL_PROTOCOL_VERSION`.

`webCompatVersion` is the target web bundle compatibility version.

`minWebCompatVersion` is the minimum web bundle compatibility the target server
accepts.

`supervisorControlVersion` is `calm_session::SUPERVISOR_CONTROL_VERSION`.

It protects the control messages between `calm-server` and
`calm-proc-supervisor`.

## 7. Compatibility Matrix

The PR 1 preflight matrix is deliberately conservative.

Different `productMajor` values are breaking.

Different terminal frame versions are breaking.

Different terminal protocol versions are breaking.

Different REST API versions are breaking.

Different sync event versions are breaking.

Different kernel-as-MCP-server protocol versions are breaking.

Different plugin-host MCP protocol versions are breaking.

Different supervisor control versions are breaking.

An increased `minWebCompatVersion` above the installed `webCompatVersion` is
breaking.

A changed `webCompatVersion` by itself is not automatically breaking.

It can be preserving when the target server still accepts the installed web
compatibility, or when the web unit is upgraded together with the server.

The matrix can become more permissive later if a compatibility layer is added.

Until then, exact matches are easier to reason about and safer for live
sessions.

## 8. DB Migration Policy

`dbMigrationPolicy` is meaningful on `calmServer`.

`none` means no DB backup is required by preflight.

`additive` means preflight can be preserving, but PR 2 should back up the DB
before activation.

`forwardOnly` also means preserving with backup required.

`destructive` means breaking.

PR 1 does not inspect individual SQL migration files.

The release manifest is the source of truth for the target server's declared
migration risk.

PR 2 still needs to implement backup and restore behavior.

## 9. Installed State

PR 1 introduces an installed-state file:

`<data_dir>/state/installed.json`

`data_dir` is the resolved calm data directory from `AppConfig`.

The file is written after activation commits symlink changes for a v2 manifest.

It is not written for v1 manifests.

It is read during v2 preflight.

If it is absent, v2 preflight returns `Breaking { reason:
NoInstalledState }`.

The legacy v1 preflight path remains mode-based and does not require this file.

Installed-state schema:

```json
{
  "schemaVersion": 1,
  "releaseId": "2026.05.30-rc1",
  "productMajor": 0,
  "compatibility": {
    "terminalFrameVersion": 4,
    "terminalProtocolVersion": 4,
    "apiVersion": "1",
    "syncEventVersion": 1,
    "mcpProtocolVersion": "2024-11-05",
    "pluginMcpProtocolVersion": "2025-11-25",
    "webCompatVersion": 2,
    "minWebCompatVersion": 2,
    "supervisorControlVersion": 1
  },
  "units": {
    "calmServer": {
      "version": "0.5.4",
      "binarySha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    },
    "web": {
      "version": "0.5.4",
      "treeSha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
    }
  },
  "installedAt": "2026-05-30T00:00:00Z"
}
```

Writes are atomic at the file level.

The writer serializes JSON to `installed.json.tmp.<pid>`.

It fsyncs the temp file.

It renames the temp file over `installed.json`.

It best-effort fsyncs the parent directory.

If the rename fails, the temp file is removed.

## 10. Installed State Lifecycle

Before the first v2 activation, the file may be missing.

That is expected.

A v1 package activation does not populate it.

The first v2 activation creates it.

Every later v2 activation replaces it with the target manifest's unit versions
and hashes.

Rollback in PR 1 still uses the existing symlink metadata.

Rollback does not rewrite installed state.

PR 2 must decide whether release history or rollback recovery updates installed
state after failed activation.

## 11. Verdict

`Verdict` is the main preflight result for v2 manifests.

Values:

```rust
enum Verdict {
    Noop,
    Preserving {
        units_changed: Vec<UnitName>,
        deferred: Vec<UnitName>,
        refresh_frontend: bool,
        requires_db_backup: bool,
    },
    Breaking {
        reason: BreakingReason,
        units_changed: Vec<UnitName>,
    },
}
```

`Noop` means the target units are identical to installed state.

`Preserving` means the target can be applied without intentionally terminating
live user work.

`Breaking` means the target crosses a product or wire boundary, or declares a
destructive migration.

PR 2 will require opt-in for breaking upgrades.

## 12. Verdict Algorithm

Pseudocode:

```text
compute_verdict(installed, target):
  changed = []

  for (name, target_unit) in target.units:
    installed_unit = installed.units[name]
    if installed_unit is missing:
      changed.push(name)
      continue
    if installed_unit.version != target_unit.version:
      changed.push(name)
      continue
    if installed_unit.binarySha256 != target_unit.binarySha256:
      changed.push(name)
      continue
    if installed_unit.treeSha256 != target_unit.treeSha256:
      changed.push(name)
      continue

  if target.productMajor != installed.productMajor:
    return Breaking(ProductMajorChanged, changed)

  if compatibility_breaks(installed.compatibility, target.compatibility):
    return Breaking(WireIncompatibility, changed)

  if any target unit has dbMigrationPolicy = destructive:
    return Breaking(DestructiveDbMigration, changed)

  if changed is empty:
    return Noop

  deferred = changed where restartPolicy = deferUntilFullReboot
  refresh = any changed where restartPolicy = refreshFrontend
  backup = calmServer in changed and dbMigrationPolicy in {additive, forwardOnly}

  return Preserving(changed, deferred, refresh, backup)
```

The change list is computed before breaking checks.

That keeps breaking responses useful to operators.

If a release is breaking and changes only the product major metadata, the list
can be empty.

## 13. V1 Compatibility

Manifest v1 remains supported.

The v1 schema still has fixed `units.app`, `units.web`, `units.calmServer`, and
`units.bundle` fields.

The v1 preflight path still returns `PreflightResult`.

It still uses `PreflightMode`.

Existing mode-based tests continue to cover that path.

The v1 path does not read `installed.json`.

The v1 path does not write `installed.json`.

The migration path is therefore explicit:

1. Existing installs can keep using v1 packages.
2. The first v2 package preflight is conservative if installed state is absent.
3. A controlled v2 activation writes installed state.
4. Later v2 preflights become state-function verdicts.

## 14. Supervisor Identity

PR 1 records spawn-time identity for supervised binaries.

`Supervisor::spawn_child` captures identity before `exec`.

Capture does four things:

1. Canonicalizes the configured binary path.
2. Streams the binary and computes SHA-256.
3. Runs `<bin> --version` with a short timeout.
4. Parses the crate semver from output such as `calm-server 0.1.0`.

The captured shape is:

```json
{
  "binaryPath": "/real/path/to/calm-proc-supervisor",
  "binarySha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "crateVersion": "0.1.0",
  "capturedAt": "2026-05-30T00:00:00Z"
}
```

If `--version` fails or cannot be parsed, `crateVersion` is `null`.

The spawn still proceeds.

If canonicalization or hashing fails, identity capture is skipped for that
spawn and the spawn still proceeds.

The failure is logged.

## 15. Supervisor Identity Persistence

The `calm-proc-supervisor` identity is persisted to:

`<data_dir>/state/supervisor-identity.json`

PR 2 needs this file when a new `neige-app` process inherits an already-running
supervisor process.

The `calm-server` identity is kept in memory only.

It can be recaptured on every server respawn.

The persisted supervisor identity is written after each supervisor spawn.

It uses the same atomic JSON writer as installed state.

No PR 1 code reads this file yet.

## 16. Status Shape

`/status` keeps its old top-level fields for compatibility.

Those fields mirror `calmServer`.

New structured fields expose both supervised processes:

```json
{
  "desiredRunning": true,
  "childState": "running",
  "childPid": 1234,
  "restartCount": 0,
  "lastExit": null,
  "calmListen": "127.0.0.1:4040",
  "calmServer": {
    "desiredRunning": true,
    "childState": "running",
    "childPid": 1234,
    "restartCount": 0,
    "lastExit": null,
    "identity": {
      "binaryPath": "/releases/current-server/bin/calm-server",
      "binarySha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "crateVersion": "0.1.0",
      "capturedAt": "2026-05-30T00:00:00Z"
    }
  },
  "procSupervisor": {
    "desiredRunning": true,
    "childState": "running",
    "childPid": 1200,
    "restartCount": 0,
    "lastExit": null,
    "identity": {
      "binaryPath": "/releases/current-server/bin/calm-proc-supervisor",
      "binarySha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "crateVersion": "0.1.0",
      "capturedAt": "2026-05-30T00:00:00Z"
    }
  }
}
```

The top-level fields should not be extended.

New callers should read the structured fields.

## 17. Version Endpoint

`GET /api/version` now includes all compatibility fields needed by manifest v2.

New keys:

- `webCompatVersion`
- `pluginMcpProtocolVersion`
- `supervisorControlVersion`

Existing `mcpProtocolVersion` now represents the kernel-as-MCP-server protocol.

`pluginMcpProtocolVersion` represents the plugin-host protocol.

The numeric values are not bumped by PR 1.

PR 1 only surfaces existing boundaries and adds the supervisor-control constant
at version `1`.

## 18. PR 2 TODO

PR 2 implements apply behavior:

- rename `/update/apply` to `/upgrade/apply`
- add release history
- implement preserving symlink swaps and calm-server restart
- implement frontend refresh notification
- implement breaking opt-in and exec-self
- implement healthcheck and rollback behavior
- implement DB backup and restore where promised
- decide how installed state is recovered after failed activation
- add E2E tests for live PTY preservation and breaking termination

PR 1 intentionally leaves these pieces untouched.
