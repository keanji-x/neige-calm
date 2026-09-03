<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/neige-mark-dark.svg">
    <img src="fe/web/src/ui/brand/neige-mark.svg" width="104" alt="">
  </picture>
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
- **Planning and execution** — a root agent plans tasks, dispatches workers, reacts to results, and drives a typed lifecycle from draft through review.
- **Durable Reports** — block documents with stable IDs and revisions, supporting prose, tasks, tables, candlestick charts, and sandboxed app views.
- **Isolated workspaces** — attach an existing directory or let the kernel provision a managed workspace for a Track.
- **Governed execution** — kernel-enforced role, scope, lifecycle, review, and gate boundaries around agent writes and side effects.
- **Today** — a cross-Track view with waiting/running state and an AI-written daily progress document.
- **Extensible tools** — plugins can contribute tools, templates, connectors, overlays, and sandboxed UI resources.
- **Recoverable execution** — persisted events, sessions, operations, and supervisor state are designed to survive retries and process replacement.

Track Recipes—user-defined, reusable ways of structuring work—are currently under active development.

## Quick start

The simplest source-based preview runs directly on the host. It currently assumes:

- Linux
- Git, GNU Make, and Bash
- [rustup](https://rustup.rs/) (the repository pins Rust in `rust-toolchain.toml`)
- Node.js 22.12 or newer and npm
- An installed and authenticated OpenAI Codex CLI

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

The next-generation frontend is still being cut over. Its new-Track composer does not yet atomically deliver the first intent: after creating a Track, send that intent from the Track's planning conversation.

`make prod` is a foreground host-local run mode. For a supervised installation and upgrade procedure, see the [Deploy & Upgrade Guide](docs/deploy-and-upgrade.md).

### Containerized development

Docker Engine, Docker Compose v2, `curl`, and `ss` are optional requirements for the containerized development path. Before using it, copy and review the environment file:

```bash
cp .env.example .env
```

- Change `CALM_EXTRA_MOUNT=/mnt/data2` to a path that exists on your host, or use `/tmp` if no extra mount is needed.
- Set `CALM_CODEX_HOST_BIN` if Codex is not installed in the default system npm location.
- Change `CALM_AUTH_PASSWORD`; the checked-in development default is `dev`.
- Container outbound networking currently expects the host proxy/forwarder settings described in `.env.example`. If you do not use a host proxy, prefer the host-local path above.

Start the stack:

```bash
make dev
```

`make dev` builds and serves both frontends. Note the printed port, then open the new frontend at `http://localhost:<printed-port>/next/`; the bare root redirects there in Docker dev. The legacy frontend remains available at `http://localhost:<printed-port>/calm/` during the cutover.

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

Run the Rust gates used during normal development:

```bash
scripts/local-rust-gates.sh --quick
scripts/local-rust-gates.sh          # full local Rust gate; requires cargo-nextest
```

Run the next-generation frontend gates:

```bash
(cd fe && npm ci && npm run lint && npm run build && npm test)
```

Browser tests additionally require Playwright Chromium:

```bash
(cd fe && npx playwright install --with-deps chromium && npm run test:browser)
```

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

## Repository layout

```text
crates/    Rust kernel, persistence, execution, providers, CLI, and process supervision
fe/        Next-generation frontend and its framework-independent domain core
web/       Legacy frontend retained temporarily during the cutover
plugins/   Built-in plugin manifests and implementations
e2e/       Stack-level and real-agent end-to-end tests
docs/      Architecture, operations, design records, and executable-oracle documentation
docker/    Development-stack images and nginx configuration
scripts/   Local gates, diagnostics, generation, and release helpers
```

## Project direction

Neige Calm is converging on four user-facing ideas:

1. **Area** — where enduring context belongs.
2. **Track** — what stays coherent while work advances through one or more cycles.
3. **Report** — the current, inspectable outcome rather than a summary buried in chat.
4. **Recipe** — a reusable way to perform and deliver a kind of work.

Near-term work is focused on completing the new frontend cutover, making plugin configuration usable end to end, and turning Track Recipes into a user-facing workflow.

## Contributing

The project is evolving quickly. Before a large change, open an issue describing the user-visible outcome, the affected authority boundaries, and how the behavior will be verified. Keep pull requests scoped, preserve existing migrations and persisted contracts, and run the relevant local gates before submitting. See [CONTRIBUTING.md](CONTRIBUTING.md) for the pull request format, verification checklist, and squash-only merge policy.

## License

Unless otherwise noted, code in this repository is licensed under the [Apache License 2.0](LICENSE). Third-party dependencies and independently distributed plugins remain under their respective licenses.
