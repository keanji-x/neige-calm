# neige-app Release Manifest

M1 release packages are local directory packages. They are built by
`neige-app system package` and checked by `neige-app system preflight`.
`neige-app system upgrade` can stage these packages locally, optionally switch
the relevant release symlink with `--activate`, and roll back the last
activation. It still does not download remote packages or perform daemon-side
automatic apply.

This page documents the legacy schemaVersion 1 package shape. The
schemaVersion 2 model, per-crate units, installed-state file, and verdict
preflight algorithm are documented in `docs/upgrade-pipeline.md`.

## Shape

```json
{
  "schemaVersion": 1,
  "releaseId": "local-dev",
  "units": {
    "app": {
      "name": "neige-app",
      "version": "0.1.0"
    },
    "web": {
      "version": "local-web",
      "compatibility": {
        "apiVersion": "1",
        "syncEventVersion": 1,
        "mcpProtocolVersion": "2025-11-25",
        "webCompatVersion": 2,
        "minWebCompatVersion": 2
      }
    },
    "calmServer": {
      "version": "local-server",
      "compatibility": {
        "apiVersion": "1",
        "syncEventVersion": 1,
        "mcpProtocolVersion": "2025-11-25",
        "webCompatVersion": 2,
        "minWebCompatVersion": 2
      },
      "dbMigrationPolicy": "forwardOnly"
    },
    "bundle": {
      "binaries": [
        { "name": "calm-server", "path": "bin/calm-server" },
        { "name": "neige-codex-bridge", "path": "bin/neige-codex-bridge" },
        { "name": "neige-mcp-stdio-shim", "path": "bin/neige-mcp-stdio-shim" },
        { "name": "neige", "path": "bin/neige" }
      ]
    }
  },
  "files": [
    {
      "path": "bin/calm-server",
      "sha256": "...",
      "bytes": 123456,
      "unit": "calmServer"
    }
  ]
}
```

## Compatibility

Every `web` and `calmServer` unit carries the same compatibility object:

- `apiVersion`: REST contract version.
- `syncEventVersion`: sync event envelope version.
- `mcpProtocolVersion`: MCP protocol date advertised by the kernel.
- `webCompatVersion`: compatibility version of the web bundle.
- `minWebCompatVersion`: minimum web compatibility accepted by the server.

DB migration policy is one of:

- `none`
- `additive`
- `forwardOnly`
- `destructive`

Preflight denies `destructive`. It allows `additive` and `forwardOnly`, but
marks the result with `requiresDbBackup=true` because migration bookkeeping can
still block rollback to an older binary.

## Modes

- `web-only` checks target web against the current server.
- `server-only` checks target server against the current web and requires the
  backend sidecar bundle (`neige-codex-bridge`,
  `neige-mcp-stdio-shim`, and `neige`) alongside `calm-server`. If the current
  version JSON does not include `webCompatVersion`, M1 conservatively uses the
  current server's `minWebCompatVersion` as the current web compatibility.
- `bundle` checks target web and target server against each other inside the
  manifest.
- `app-only` only checks for an app unit named `neige-app`.

Missing required fields fail closed. The CLI still prints the standard
preflight JSON result with `allowed=false`.

Source-driven upgrades (`system upgrade` without `--package`) can set
`[source].mode = "web-only" | "server-only" | "bundle"` in `config.toml`, or
use `--mode` on the CLI. CLI mode wins. When neither is set, the source build
packages a full bundle and the mode is inferred from the manifest.

## Activation

Release symlinks are split by component:

- `current_server` / `previous_server`
- `current_web` / `previous_web`

The server and web symlink paths must stay distinct. Legacy `current` and
`previous` config keys do not replace the split defaults for component
activation.

`web-only` activation switches only the web pair and returns
`restartRequired=false`; refresh connected frontend clients after activation.
`server-only` activation switches only the server pair and returns
`restartRequired=true`. `bundle` activation switches both pairs and also
requires restart. `app-only` activation is not supported.

Activation writes `<release.root>/last-activation.json` with the mode and
symlink changes. `system rollback` uses that metadata to restore the symlink
pair or pairs touched by the last activation, then deletes the metadata so the
same activation cannot be rolled back twice.

## Safety Rules

`releaseId` is used as the final staged directory name, so it must match
`[A-Za-z0-9._-]+` and must not be `.` or `..`. The staging command rejects
path separators, symlinks, duplicate manifest file paths, unmanifested regular
files, and any manifest file whose hash or byte count does not match the package
contents.

Activation and rollback only accept release targets under
`<release.root>/staged/` that are real directories and contain a valid
`manifest.json`.
