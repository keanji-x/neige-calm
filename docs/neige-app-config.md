# neige-app Config

`neige-app` is system-only. The default config path is:

```text
~/.config/neige-app/config.toml
```

Every command that needs configuration accepts `--config <path>`. For
`system serve`, explicit CLI flags override the loaded config for the same
field. The generated systemd unit intentionally keeps `ExecStart` short:

```text
neige-app system serve --config ~/.config/neige-app/config.toml
```

## Create

```bash
neige-app system init-config --config ~/.config/neige-app/config.toml
```

`init-config` refuses to overwrite an existing file.
When `--config <path>` is passed explicitly to commands that load config, the
file must exist. Only the implicit default path falls back to in-memory starter
defaults.

## Example

```toml
[admin]
listen = "127.0.0.1:4050"
token_file = "~/.config/neige-app/admin.token"

[release]
root = "~/.local/share/neige-app/releases"
current_server = "~/.local/share/neige-app/releases/current-server"
current_web = "~/.local/share/neige-app/releases/current-web"
previous_server = "~/.local/share/neige-app/releases/previous-server"
previous_web = "~/.local/share/neige-app/releases/previous-web"
backups = "~/.local/share/neige-app/backups"

[child]
bin = "~/.local/share/neige-app/releases/current-server/bin/calm-server"
web_dist = "~/.local/share/neige-app/releases/current-web/web/dist"
fe_dist = "~/.local/share/neige-app/releases/current-web/web/dist/next"
calm_listen = "127.0.0.1:4040"
db_url = ""
data_dir = "~/.local/share/neige-calm"
mcp_stdio_shim_bin = "~/.local/share/neige-app/releases/current-server/bin/neige-mcp-stdio-shim"
auth_username = "owner"
auth_password = ""
auth_dev_autologin = false
cwd = ""
extra_args = []

[timing]
stop_grace_ms = 5000
restart_delay_ms = 1000

[systemd]
unit_path = "~/.config/systemd/user/neige-app.service"
unit_name = "neige-app"
bin = "~/.local/bin/neige-app"

[upgrade]
current_version_file = ""

[source]
url = ""
branch = "main"
# Optional: web-only, server-only, or bundle. Omit to infer from manifest.
mode = ""
checkout_dir = "~/.cache/neige-app/source"
build_args = ["make", "build", "fe-build"]
# v2 package compatibility is probed from the freshly built calm-server.
# DB migration policy defaults to forwardOnly; set NEIGE_DB_MIGRATION_POLICY
# in the service build environment to override it.
```

Empty strings mean "unset" for optional path/string fields.
The default child paths are split by component:

- server binaries use `release.current_server`
- web assets use `release.current_web`

`child.fe_dist` (or `system serve --calm-fe-dist`) explicitly selects the new
frontend directory. With `system package --fe-dist`, that directory is packaged
as `web/dist/next` within the same hashed Web unit. Omit `fe_dist` for a legacy-only
package. Existing configs that omit it remain legacy-only; new starter configs
include it for Alpha packages. When configured, the browser root redirects to
`/next/`, and `web_dist` continues to serve `/calm/`.

For a first install, use the [Alpha runbook](alpha-release.md) before running
`system install`: the package and executable symlinks must exist, and owner
credentials must be set in the private config file.

The legacy `release.current` and `release.previous` keys are still accepted.
They are retained as legacy fields only; when split keys are omitted,
`neige-app` uses the split defaults under `release.root`. Component activation
requires `current_server` and `current_web` to be different paths, and likewise
for `previous_server` and `previous_web`.

`calm_listen` may be set to `0.0.0.0:<port>` when the calm-server API and
built web bundle should be reachable from other hosts. Keep `admin.listen`
loopback-only unless the bearer-token admin API is protected by the local
machine or another access-control layer.

`auth_username` and `auth_password` are passed through to calm-server as
`CALM_AUTH_USERNAME` and `CALM_AUTH_PASSWORD`. Set `auth_dev_autologin=true`
only for local development; it disables the normal owner login flow.

## Install

```bash
mkdir -p ~/.config/neige-app
openssl rand -hex 32 > ~/.config/neige-app/admin.token
chmod 600 ~/.config/neige-app/admin.token

neige-app system install --config ~/.config/neige-app/config.toml
```

`install` creates the config if it is missing, writes the user systemd unit to
`systemd.unit_path`, creates `admin.token_file` as a random 32-byte hex token
with `0600` permissions on Unix if it is missing, and prints the
`systemctl --user` commands to run next. It does not call `sudo` and does not
start systemd automatically.

`install` refuses to overwrite an existing unit file unless `--force` is
passed. M1 also rejects systemd `ExecStart` paths containing whitespace or
control characters instead of trying to quote them.

## Enabling the new frontend on an existing source installation

Old config files keep their saved `build_args = ["make", "build"]` and omit
`child.fe_dist`. They remain explicitly legacy-only: source packaging does not
require or pick up a stray `fe/web/dist` tree. To opt into both frontends, update
**both** settings before a source build:

```toml
[child]
fe_dist = "~/.local/share/neige-app/releases/current-web/web/dist/next"
[source]
build_args = ["make", "build", "fe-build"]
```

For an existing running instance, opt in during planned downtime **after**
installing a package that includes the next frontend. `neige-app` reads its admin
configuration at host startup: after editing these settings, restart the
`neige-app` service (or stop/start the foreground host) before using its admin
source-upgrade endpoint. `POST /restart` restarts only calm-server and does not
reload this configuration. CLI source builds read the file on each invocation,
but the running host still needs a restart to serve the newly enabled frontend.

Custom build commands must also rebuild `fe/web/dist` when `fe_dist` is enabled;
a directory left from a prior build is not evidence of a current frontend. For
release artifacts use the Alpha build script, which unconditionally performs
both production builds and records their source revision.

## Staged Upgrade

`system upgrade` can run without `--package`. In that mode it reads `[source]`,
prepares the source, runs `build_args` without a shell, creates a local bundle
package from the standard Makefile outputs, infers the upgrade mode, and stages
the result.

```bash
neige-app system upgrade --config ~/.config/neige-app/config.toml
```

`source.url` may be a local path or a git URL. A local path is used directly.
A non-local URL is cloned/fetched into `source.checkout_dir`, checked out to
`source.branch`, and reset to `origin/<branch>`. `neige-app` writes a
`.neige-app-source.json` marker after clone and refuses to fetch/reset an
existing checkout directory unless the marker and `origin` URL match the
current config. The default build command is:

```text
["make", "build", "fe-build"]
```

No arbitrary shell command is executed. Package compatibility is probed from
the freshly built `calm-server`, and the DB migration policy defaults to
`forwardOnly` unless `NEIGE_DB_MIGRATION_POLICY` overrides it for the build.
The legacy compatibility fields still accepted under `[source]` are not the
authority for a v2 package.

Advanced users can still provide an already-built package:

```bash
neige-app system upgrade \
  --config ~/.config/neige-app/config.toml \
  --package ./target/neige-release
```

Mode defaults to auto-detection:

- app unit only -> `app-only`
- web unit only -> `web-only`
- calmServer unit + backend bundle -> `server-only`
- web + calmServer + bundle -> `bundle`

When `system upgrade` runs without `--package`, `[source].mode` controls which
source-built units are packaged. `--mode` overrides `[source].mode`; when both
are omitted, source builds a full `bundle` package and mode is inferred from
the resulting manifest.

`system upgrade --activate` verifies a local package and copies only
`manifest.json` plus the files listed in `manifest.json` to:

```text
<release.root>/staged/<releaseId>
```

It rejects unsafe `releaseId` values, symlink payloads, duplicate manifest
paths, hash or byte mismatches, unmanifested regular files, and non-empty stage
targets. With `--activate`, the staged release target must live under
`<release.root>/staged/`, be a real directory, and contain a valid
`manifest.json`.

With `--activate`, `neige-app` also:

- backs up the SQLite DB when preflight returns `requiresDbBackup=true`
- for `web-only`, switches only `current_web`/`previous_web`
- for `server-only`, switches only `current_server`/`previous_server`
- for `bundle`, switches both server and web symlink pairs
- writes `<release.root>/last-activation.json` so rollback can undo the last
  activation by mode

It does not directly control a running `system serve` process. `web-only`
activation does not require a calm-server restart; refresh connected frontend
clients. `server-only` and `bundle` activation require restarting with the
admin API or systemd.
SQLite backups use the `sqlite3` CLI online backup command; activation fails if
`sqlite3` is unavailable or the backup command fails.

For `web-only` and `server-only`, set `upgrade.current_version_file` to a JSON
file captured from the current kernel:

```bash
curl -fsS http://127.0.0.1:4040/api/version > ~/.local/share/neige-app/current-version.json
```

Then:

```bash
neige-app system upgrade \
  --config ~/.config/neige-app/config.toml \
  --activate
```

Rollback is symlink-only in this version:

```bash
neige-app system rollback --config ~/.config/neige-app/config.toml
```

It reads `<release.root>/last-activation.json` when present and restores the
server and/or web symlink pair touched by the last activation, then deletes the
metadata so the same activation cannot be rolled back twice. It does not restore
a DB backup.
