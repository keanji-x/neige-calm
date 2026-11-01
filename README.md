<p align="center">
  <img src="fe/web/src/ui/brand/neige-app.svg" width="104" alt="Neige Calm">
</p>

<h1 align="center">Neige Calm</h1>

<p align="center"><strong>Keep ongoing work coherent while agents do the work.</strong></p>

<p align="center">
  English · <a href="README.zh-CN.md">简体中文</a>
</p>

Neige Calm is a local-first, agent-native workspace for work that outlives a chat session. Organize enduring context into **Areas**, create a **Track** around an intent, let a planning agent coordinate workers, and keep the current outcome in a durable, inspectable **Report**.

> [!IMPORTANT]
> Neige Calm is an early-stage, source-first project under active development. There is no stable release yet, and APIs, UI flows, storage contracts, and terminology may still change. The current development path targets Linux.

## Why Neige Calm?

Most AI tools organize work around a folder, an editor, or a chat session. Neige Calm organizes it around the work itself.

It is not meant to replace your editor. Use an editor when you want to work directly; use Neige Calm when you want to hand off an outcome, supervise several agents or workspaces, and return later without reconstructing state from chat logs.

```text
Area — enduring context
└── Track — a focused or ongoing line of work
    ├── Conversation — intent, feedback, and decisions
    ├── Tasks — the executable plan
    ├── Workers — Codex, Claude, or terminal-backed execution
    ├── Workspace — managed or attached files
    └── Report — the durable current outcome
```

A Track can finish once, like fixing an issue, or remain useful across repeated cycles, like maintaining an investment thesis. Sessions and workers may come and go; the Track keeps its identity, history, evidence, and result.

## What is here today

- **Areas and Tracks** — separate long-lived context from individual streams of work.
- **Planning and execution** — choose Codex or a configured Claude Planner to plan tasks, dispatch workers, and react to results. Tracks are open or closed; tasks retain their own execution and verification states.
- **Durable Reports** — block documents with stable IDs and revisions, supporting prose, tasks, tables, candlestick charts, sandboxed app views, and links that open workspace files in the Track.
- **Track workspaces** — attach a Git repository to create a dedicated Track worktree, or let the kernel provision a managed workspace. Codex and Claude tasks run one at a time in the Track checkout.
- **Governed execution** — kernel-enforced role, scope, lifecycle, review, and gate boundaries around agent writes and side effects.
- **Desktop supervision** — task counts and status details, a Notification Center that opens the conversation or worker needing attention, and separate worker and verification directories on cards. Dismiss notifications you have handled; closing a Track hides it from the desktop rail unless it is unread or currently open.
- **Today** — a cross-Track view with waiting/running state and a daily progress report maintained through its own conversation.
- **Extensible tools** — add remote MCP servers or server-local plugin directories from Settings, then enable, configure, restart, or remove them. Plugins can contribute tools, templates, connectors, overlays, and sandboxed UI resources.
- **Track Recipes** — save working instructions and a Report format for the Planner; tasks are created for the actual request. Built-in templates remain read-only.
- **Report references and previews** — use `@`, `@#`, `@/`, and `@>` in Planner chat to reference the Area’s tags, Track reports, or report blocks. Configured HTTP previews embed live development servers with desktop and mobile viewports.
- **Recoverable execution** — persisted events, sessions, operations, and supervisor state are designed to survive retries and process replacement.

Local executable plugins run as trusted code with the service's OS identity;
browser UI sandboxing does not isolate native plugin code. See
[Plugin host security](docs/plugin-security.md) before installing plugins.

See [Using Neige Calm](docs/using-neige-calm.md) for the current UI paths and their limits, and the [recipe body format](docs/recipe-body-format.md) for authoring Planner working instructions and Report formats.

## Quick start

For a fresh Linux installation, use the [getting-started guide](docs/getting-started.md)
([中文](docs/getting-started.zh-CN.md)). A reviewed Alpha archive runs without
Rust, Node.js, Go, Docker or systemd:

```bash
# From the extracted Alpha archive:
./release/bin/neige-app system setup --package ./release
```

Setup checks package integrity, asks for a local owner password, and prints the
start command and browser URL. The guide takes you from provider readiness to
your first repository report, then a small change or a Development PR.
Archives are prepared with the [maintainer packaging runbook](docs/alpha-release.md).
Existing installations follow the [upgrade guide](docs/deploy-and-upgrade.md).

### Source preview

Contributors can build and run directly on the host. This path assumes:

- Linux
- Git, GNU Make, and Bash
- [rustup](https://rustup.rs/) (the repository pins Rust in `rust-toolchain.toml`)
- Node.js 22.12 or newer and npm
- Go (the version in `tailnet/go.mod`; `make prod` also builds the Tailnet helper)
- An installed and authenticated OpenAI Codex CLI
- Optional: a configured and authenticated Claude Planner; see [Planner configuration](docs/neige-app-config.md#enable-a-claude-planner)
- Optional, for publishing Tracks as pull requests: an authenticated [GitHub CLI](https://cli.github.com/) (`gh`) and `jq` on the server's `PATH`. The development plugin's forge tools run `gh`, and its PR checks read also runs `jq`

Clone and prepare the environment:

```bash
git clone https://github.com/keanji-x/neige-calm.git
cd neige-calm

rustup toolchain install
codex login
```

Start Neige Calm in foreground host-local mode:

```bash
PROD_AUTH_PASSWORD=choose-a-local-password make prod
```

Open <http://localhost:4040/next/> and sign in as `owner` with the password you chose. This mode listens on loopback by default, stores state under `~/.local/share/neige-calm`, and links two helper binaries into `~/.local/bin`.

In Chrome, use the address-bar install icon to install **Neige Calm** and launch
it in a separate app window. Installation requires HTTPS or localhost; the app
still needs its server connection. See [Chrome installation](fe/README.md#install-in-chrome)
for menu instructions and troubleshooting.

The first message in the New Track composer is submitted with the create request; you do not need to send it again. For Alpha packaging and installation from a release archive, follow the [Linux Alpha runbook](docs/alpha-release.md).

`make prod` is a foreground host-local run mode. For a supervised installation and upgrade procedure, see the [Deploy & Upgrade Guide](docs/deploy-and-upgrade.md).

### macOS local builds (limited)

The Rust service binaries and maintained frontend can also be built on macOS:

```bash
rustup toolchain install
(cd fe && npm ci && npm run build)
CARGO_BUILD_JOBS=6 cargo build --locked --release \
  -p calm-server -p calm-codex-bridge -p neige-app \
  -p neige-mcp-stdio-shim -p calm-proc-supervisor -p neige-cli \
  --bin calm-server --bin neige-codex-bridge --bin neige-app \
  --bin neige-mcp-stdio-shim --bin calm-proc-supervisor --bin neige
```

This is a limited local-development path, not Linux feature parity. The Tailnet
helper and the `make prod` build path remain Linux-only. Configure a local
`neige-app` installation with Tailnet disabled and `child.fe_dist` pointing to
the absolute `fe/web/dist` path; the frontend is served at `/next/`.
New builds support native registered **Private ExternalDir + Pipe** execution
on macOS for `task-verify`, `task-gate-run`, forge commands and probes, and the
registered provider rewind checks. Claude turns and compaction now also use the
registered executor; their completion owner accepts the same foreground
settlement contract. This does not establish full native Claude feature parity.
`neige-app` provisions one private executor key and passes it to both the kernel
and supervisor. See [macOS execution](docs/macos-execution.md) for the supported
scope, permission inheritance, cleanup limits, and isolated verification steps.
Registered strong Worktree execution and registered PTY execution remain unsupported
on macOS; the existing legacy terminal path remains available.
Secure workspace file access requiring `openat2` and `/proc`-based process-group
enumeration and descendant cleanup are not ported. Unsupported file operations
fail closed; they never fall back to less restrictive filesystem access. This
build does not change Codex sandbox permissions or grant access to the user's
home directory.

Shared Codex daemon supervision on macOS reads native process start time through
`proc_pidinfo` and the boot UUID through `kern.bootsessionuuid`. A complete
persisted identity that verifies against the live process can enter
`try_takeover_live`: an eligible daemon with a successful handshake is adopted;
a verified daemon whose handshake fails is reaped before relaunch. Unreadable
or mismatched identity is not proof of ownership. Stale-socket listener reaping
remains a separate recovery path, observing exit through a kqueue `NOTE_EXIT`
watch.

Stops of a child this process spawned (a settings-triggered replacement or
failed-launch cleanup) observe exit through `proc_pidinfo` zombie state without
waiting the full stop grace. An owned, unreaped child pins its process group
for final cleanup. For an adopted daemon or stale-socket listener, once the
leader exits there is no per-member straggler sweep: macOS has no `/proc`
process-group enumeration. A member that ignores SIGTERM and outlives that
leader can remain running; if it inherited the listening socket, the next
daemon start can fail closed until that process and the socket are removed by
hand. Registered foreground execution uses a separate, continuously owned child
handle and observes its original process group. This weaker macOS fact does not
provide Linux's full descendant-drain proof. An already running installation does
not gain these changes until it starts the newly built binaries.

`cargo test -p calm-server --lib` as a whole is not expected to pass on macOS:
some Linux-only fixtures are not `cfg`-gated. Only filtered subsets were run
there.

### Containerized development

Docker Engine, Docker Compose v2, `curl`, and `ss` are optional requirements for the containerized development path. Before using it, copy and review the environment file:

```bash
cp .env.example .env
```

- Change `CALM_EXTRA_MOUNT=/mnt/data2` to a path that exists on your host, or use `/tmp` if no extra mount is needed.
- `make dev` resolves the active Codex installation from `PATH`. For a
  nonstandard package layout, set both `CALM_CODEX_HOST_BIN` and
  `CALM_CODEX_CODE_MODE_HOST_BIN` to executable files from the same release.
- Change `CALM_AUTH_PASSWORD`; the checked-in development default is `dev`.
- Container outbound networking currently expects the host proxy/forwarder settings described in `.env.example`. If you do not use a host proxy, prefer the host-local path above.

Start the stack:

```bash
make dev
```

`make dev` builds and serves the maintained frontend. Note the printed port, then open the new frontend at `http://localhost:<printed-port>/next/`; the bare root redirects there in Docker dev.

For frontend HMR instead of the built bundle, run the following in another terminal, replacing `<printed-port>`:

```bash
FE_API_PROXY_TARGET=http://127.0.0.1:<printed-port> make fe-dev
```

Then open <http://localhost:5180/next/>. You can check the public version endpoint without a login cookie:

```bash
curl -fsS http://localhost:<printed-port>/api/version
```

Useful commands:

```bash
make logs     # follow server and proxy logs
make stop     # stop the development stack
make help     # list all Make targets
```

## Security note

Neige Calm can run agent-generated commands and modify attached repositories. Treat it as a trusted, single-user environment—not as a hardened multi-tenant service—and inspect agent actions before granting access to sensitive code or secrets.

The Docker development port is published on **all host interfaces by default**. Its container also bind-mounts host paths and receives broad Linux capabilities so Codex can create its own sandbox. Do not expose the default Docker configuration to an untrusted network; use a firewall or an explicit loopback-only port binding and always replace the default credentials first.

## Development

Every change, including a docs-only one, must pass the CI lint job's text
gates before review:

```bash
scripts/local-ratchet-gates.sh
```

During normal Rust development, run only the tests that cover the affected
package and behavior (replace the placeholders with real selectors):

```bash
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked \
  -p <package> <test-name-filter> --test-threads 8
```

Add `--features calm-server/codex-e2e` only when the affected test requires it.
Narrow further with `--lib` or `--test <test-target>` when useful. Use
`scripts/local-rust-gates.sh --quick` when the change also needs the Rust compile,
lint, and OpenAPI preflight. The full `scripts/local-rust-gates.sh` is not a
routine development command: CI runs the broad workspace test suite, so a
nextest configuration change is verified there. Run the full local gate only
when explicitly requested or when changing it or `scripts/run-rust-nextest.sh`.

For next-generation frontend changes, run affected tests while iterating and
lint/build once before delivery (replace test paths with real selectors):

```bash
(cd fe && npm test -- <test-path>)
(cd fe && npm run lint && npm run build)
```

For browser behavior changes, run the affected browser tests:

```bash
(cd fe && npm run test:browser -- <browser-test-path>)
```

Run `(cd fe && npm ci)` when dependencies are missing or their manifest/lockfile
changed since installation. Install Chromium with
`(cd fe && npx playwright install --with-deps chromium)` only when its required
version or system dependencies are missing. Follow
[AGENTS.md](AGENTS.md#verification) for contract checks, broader-suite triggers
and reuse of successful verification.

The default end-to-end tier does not spend model tokens:

```bash
./e2e/run.sh
```

Tier 2 uses real Codex credentials and may incur model usage:

```bash
./e2e/run.sh --tier 2
```

Run that direct tier only on a dedicated host; there is no shared-host-safe entry
point for Tier 2 stack E2E. The shared production host can run the separate
isolated `codex_forge_e2e` suite:

```bash
make e2e-codex-isolated
```

That target does not replace Tier 2 stack coverage.

For a raw-only developer trace of Planner turns from a database or snapshot:

```bash
python3 -B scripts/track-trace.py --db /path/to/calm.db TRACK_ID
python3 -B scripts/track-trace.py --help
```

The trace reads raw `harness_items`; it is not a settled canonical conversation
view and does not reconstruct receipt content. After canonical ACP retained
recovery and before END settlement, including a crash in that window, a turn
can report `status=completed` while its receipt messages/actions have not yet
materialized as raw items. Empty `messages`/`actions` arrays do not certify a
complete empty history. END materializes the retained content through its
existing owner; re-export from the updated database afterward. An earlier
export or pre-END database snapshot retains that omission. Use the application's
canonical conversation view for the published history.

## Repository layout

```text
crates/    Rust kernel, persistence, execution, providers, CLI, and process supervision
fe/        Next-generation frontend and its framework-independent domain core
plugins/   Built-in plugin manifests and implementations
e2e/       Stack-level and real-agent end-to-end tests
docs/      Architecture, operations, design records, and executable-oracle documentation
docker/    Development-stack images and nginx configuration
scripts/   Local gates, diagnostics, generation, and release helpers
```

## OpenCode ACP

The managed OpenCode ACP profile supports only OpenCode 2.x, tested from 2.1.3.
Install it with `npm install -g @opencode/cli@2.1.3` and register the absolute
`opencode` executable. OpenCode 1.x and 3.x are refused independently of the
registration's minimum version. Sign in with the native CLI before use. Neige
owns `OPENCODE_CONFIG_CONTENT`; registrations must not set it. See the
[ACP profile guide](docs/architecture/2343-acp-planner.md) for permission precedence.


## Project direction

Neige Calm is converging on four user-facing ideas:

1. **Area** — where enduring context belongs.
2. **Track** — what stays coherent while work advances through one or more cycles.
3. **Report** — the current, inspectable outcome rather than a summary buried in chat.
4. **Recipe** — a reusable way to perform and deliver a kind of work.

The frontend is served at `/next/`. Plugin installation and Recipe editing are available today. Structured checkpoints remain [follow-up work](docs/architecture/long-task-reliability.md#delivery-scope).

## Documentation

- [Using Neige Calm](docs/using-neige-calm.md) — Recipes, task supervision, Report files, and plugin setup.
- [Linux Alpha runbook](docs/alpha-release.md) — build and install a release archive.
- [Deploy & Upgrade Guide](docs/deploy-and-upgrade.md) — supervised deployment, backups, and upgrades.
- [Documentation index](docs/README.md) — configuration, retention, architecture, and design records.

## Contributing

The project is evolving quickly. Before a large change, open an issue describing the user-visible outcome, the affected authority boundaries, and how the behavior will be verified. Keep pull requests scoped, preserve existing migrations and persisted contracts, and run the relevant local gates before submitting. See [CONTRIBUTING.md](CONTRIBUTING.md) for the pull request format, verification checklist, and squash-only merge policy.

## License

Unless otherwise noted, code in this repository is licensed under the [Apache License 2.0](LICENSE). Third-party dependencies and independently distributed plugins remain under their respective licenses.

### Admin history reconstruction

A human can create the enabled `opencode-history-rebuild` template and separately
grant Area Admin through the normal UI/API (#2783); binding never grants Admin
automatically or widens plugin access. The bound Planner can discover the same template/recipe catalog as the
REST picker and create an ordinary Track in its Area. See the
[OpenCode history builtin guide](docs/opencode-history.md) for configuration,
evidence capture, scope visibility and the unperformed isolated-instance acceptance.

```sh
neige catalog ls --kind template --limit 10
neige catalog cat --kind template --source-id small-change
neige track add --template-id small-change --title "Rebuild migration" --idempotency-key rebuild-1 --text "Current intent and self-contained selected evidence" --message "Rebuild selected migration"
```

Use exactly one of `--template-id` or `--recipe-id`; supply `--template-input` as
JSON only when the selected template declares it. Optional `--cwd /repo` requires
an existing same-Area folder claim or explicit `--attach-folder`; the Planner runs
in the resulting Track worktree. Cross-Area MCP creation is refused. Retry the
identical key and payload after a disconnect; a changed request conflicts.
