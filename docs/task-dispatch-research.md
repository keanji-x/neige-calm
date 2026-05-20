# Task Dispatch Platform on neige-calm — Research

**Status:** research only. Decision input, not an implementation plan.
**Scope:** evaluate whether neige-calm can host a "user creates tasks → platform auto-spawns Claude Code / Codex agents → tasks dispatched via MCP" product.

This document inventories the existing pieces, lays out three plausible architectures with tradeoffs, recommends one, and sketches a one-week provable slice.

---

## 0. Non-negotiable constraints

These were stated by the product owner and must not be relaxed during design:

1. **TUI mode only, never `codex exec` / headless `claude-code` runs.** Agents are spawned as interactive TUIs over a PTY (the same path the existing Codex card already uses). The TUI must be visible in a neige card so the user can watch the agent work, intervene with keystrokes if needed, and read the live thinking. Headless `codex exec` and the `@anthropic-ai/claude-agent-sdk` are explicitly out of scope — both lose the live conversation view that makes this product *feel* like dispatching to a teammate rather than a black box.
2. **Dispatch flows over MCP, not stdin injection.** The platform mints tasks; the agent picks them up by calling an MCP tool (`neige.tasks.take`), reports progress via `neige.tasks.update_progress`, and signals completion via `neige.tasks.complete`. The initial PTY prompt only tells the agent "you have these tools available, start with `take`" — the actual task payload travels over MCP. This is what makes "re-assign", "queue", "many agents" work at all; pure stdin-push would couple task identity to the first PTY frame and lose any structured lifecycle.
3. **Working tree for this work lives in a git worktree at `/mnt/data2/kenji/neige-calm-task-dispatch`**, branch `feat/task-dispatch-platform`, based on `origin/main`. The product owner's `main` checkout stays untouched while this is in flight.

Everything below honors these constraints. Option 1 (Pull-only over MCP, no PTY) is presented for completeness but **fails constraint #1** — do not pick it.

---

## 1. What we already have

### 1.1 PTY session host (reusable, exactly as is)

`crates/calm-session/src/bin/daemon.rs` is a per-session supervisor with **two modes** baked into a single binary:

- **Terminal mode** — spawns a child under a PTY (via `portable-pty`), broadcasts raw bytes to every attached unix-socket client, keeps a ~1 MiB byte ring for replay-on-attach. The kernel side uses this to run `bash`, `codex`, anything that wants a TTY.
- **Chat mode** — spawns `node <runner-path>` (the `@anthropic-ai/claude-agent-sdk` runner — designed but not yet checked in under `runners/neige-chat-runner/`), forwards NDJSON `NeigeEvent` lines, accepts control frames on stdin (`user_message`, `stop`, `answer_question`).

The PTY mode already does everything we need for "spawn an interactive agent TUI in a sandbox visible to the UI." `routes::terminal::spawn_daemon_for` (in `crates/calm-server/src/routes/terminal.rs`, 245 lines) is the kernel-side spawn helper and gets reused by the codex card.

### 1.2 Codex card flow (closest existing analog to "dispatch one task to one agent")

`crates/calm-server/src/routes/codex.rs` (408 lines) does effectively this on every `POST /api/cards/:id/codex`:

1. `mkdir -p data_dir/codex-homes/<card_id>` (per-card `CODEX_HOME`, persists across reboots).
2. Seed-copy `~/.codex/` (auth.json, config.toml, history.jsonl) on first creation only.
3. Overwrite `hooks.json` to route every PreToolUse/PostToolUse/UserPromptSubmit/Stop/PermissionRequest/SessionStart hook through `neige-codex-bridge` (the bridge binary at `crates/calm-codex-bridge/src/main.rs`, 79 lines).
4. Create a `Terminal` row with `program = "codex"`, env carrying `CODEX_HOME`, `NEIGE_CARD_ID`, `NEIGE_CALM_BASE_URL`.
5. Spawn `calm-session-daemon` via the shared helper.
6. Patch `Card.payload.terminal_id` so the React `<CodexCard>` mounts xterm.

The bridge binary is dead-simple (~80 lines): read hook JSON from stdin, `POST /internal/codex/hook?card_id=...` with the body verbatim, exit 0 with `{}`. Failures are logged but never block codex.

The ingest route (`routes::codex::ingest_hook`) emits `Event::CodexHook { card_id, kind: "hook.codex.<snake>", payload }` on the bus.

### 1.3 Per-card FSM (already a mini-orchestrator — reusable)

`crates/calm-server/src/card_fsm.rs` (568 lines including tests) subscribes to the event bus and projects codex hooks onto a 6-state FSM per card: `Starting / Idle / Working / AwaitingInput / Errored / Done`. Severity order: `AwaitingInput > Errored > Working > Starting > Idle > Done`. Upgrades commit immediately; downgrades are held 750ms.

On state change it writes a kernel-owned `Overlay { plugin_id="kernel", entity_kind="card", kind="status", payload:{state} }`, then recomputes the owning wave's union (most-severe-of-its-cards) and writes a wave-level overlay `{ state, counts }`. The UI subscribes to `overlay.set` on `card:<id>` and `wave:<id>` and renders the dots.

For task dispatch this is **exactly the orchestrator signal we'd want** — once a card transitions from Working → AwaitingInput → Done, we know the task is finished. No new FSM is needed for the basic "is this agent busy" question.

### 1.4 MCP plugin host (kernel-as-client today; would need inversion for MCP-server-mode)

`crates/calm-server/src/plugin_host/` is the kernel's side of the plugin protocol. ~3,300 LOC across `mod.rs` (801) + `mcp.rs` (1156) + `callbacks.rs` (1201) + manifest/registry/process/perms/etc.

Topology today, per `mcp.rs` doc comment line 8:

> The kernel = MCP client, plugin = MCP server, but the same socket carries plugin-initiated `neige.*` requests that the kernel routes.

Concretely:
- Kernel spawns the plugin binary (`process::PluginProcess::spawn`), takes its stdin/stdout pair, runs `McpClient::connect_with_auth` over them — line-delimited JSON-RPC 2.0, `initialize` handshake with `expected_echo` auth, capability negotiation (`experimental.dev.neige/kernel-callbacks`).
- `McpClient` is bidirectional in framing but **client-shaped** in API: it has `.call(method, params)` (kernel→plugin), `.notify`, plus `take_inbound_requests`/`take_inbound_notifications` channels that drain plugin-originated traffic. Slice C's router (`spawn_neige_router`, `plugin_host/mod.rs:725`) reads those channels and dispatches `neige.overlay.set` / `neige.card.create` / `neige.event.subscribe` / `neige.kv.*` via `callbacks::dispatch`.

**Key observation:** the protocol code in `mcp.rs` is already symmetric in framing (it handles inbound requests, responses, and notifications equally). What's missing for "kernel-as-server" is just:

1. A **listener** that accepts a new transport per agent connection (today: each plugin gets its own stdio pair owned by `PluginProcess`; for an agent we'd want either a unix socket the agent connects to, or — if we pass stdin/stdout via `child.spawn()` — the inverse stdio role).
2. A `dispatch_for_agent(method, params)` entry point that exposes `tasks/list`, `tasks/take`, `tasks/complete`, etc. (mirror of `callbacks::dispatch`).
3. An auth model — current plugin auth uses per-process `NEIGE_PLUGIN_TOKEN` mint + `initialize._meta["dev.neige/auth"].expected_echo`. The same scheme works for agents (mint per spawn, inject via env or config).

**Estimated scope:** new module `crates/calm-server/src/agent_host/` (~600–900 LOC), reusing `plugin_host::mcp::{spawn_reader, spawn_writer, RpcError, RequestId, ...}` directly. The big lift is choosing the transport (stdio vs unix socket) — see §3.

### 1.5 Card / Wave / Cove model + SQLite

`crates/calm-server/src/model.rs` (247 lines). Cove (folder) → Wave (board) → Card (kind-discriminated work unit). `Card.kind` is `"terminal"`, `"codex"`, `"plugin:<id>:<view>"`, or `"ui://<plugin>/<view>"`. `Card.payload` is opaque JSON (`validation.rs` only constrains kernel-owned kinds; `ui://*` and `plugin:*` accept anything).

`crates/calm-server/src/db/sqlite.rs` (932 lines) implements `Repo`. Migrations at `crates/calm-server/migrations/000{1,2,3}_*.sql`:
- 0001: coves, waves, cards, overlays, terminals, plugins.
- 0002: plugin_tokens, plugin_kv.
- 0003: settings.

A `tasks` table would slot in cleanly as `0004_tasks.sql` with FKs to `cards(id)` (the spawned agent card) and self-referential (parent task → subtask), plus a status column. **OR** — and this is the lighter option — model tasks as `Card.kind = "task"` with payload `{ description, assignee, status, agent_card_id }`. The kernel already has all the WS / overlay / list machinery for cards, so adding a `task` kind to `validation.rs` and (optionally) a `routes/tasks.rs` view layer that filters `cards WHERE kind='task'` is probably ~150 LOC.

### 1.6 Event bus + WS bridge (works as is)

`crates/calm-server/src/event.rs` (198 lines) defines the typed `Event` enum (ts-rs exports the TS shape to `web/src/api/generated-events.ts`). `topics()` projects each event onto a set of topic strings the WS layer filters on (`card:<id>`, `wave:<id>`, `plugin:<id>`, `*`). The `ws::events` handler accepts a subscription filter from the client and forwards matches. Adding `task:<id>` or `agent:<id>` topics is one match arm.

### 1.7 Codex config injection lever

The codex card's `spawn_codex_for` writes a fresh `hooks.json` inside the per-card `CODEX_HOME` on every spawn (line 162-163). The seed-copy step also brings in `config.toml`. **This is the lever for injecting "use neige MCP server at unix:///foo/sock":** codex reads `~/.codex/config.toml` for `mcp_servers.*` entries. We can write that file inside the per-card CODEX_HOME alongside hooks.json. Same trick applies to Claude Code's `~/.claude/settings.json` `mcpServers` block.

### 1.8 What's plumbed but unused for this product

- **Chat mode in the session daemon** wraps the Anthropic agent SDK in a Node runner. This is the "Claude Code via SDK, headless" path — exists in skeleton, runner not checked in.
- **`@anthropic-ai/claude-agent-sdk`** — the user explicitly wants a TUI, so SDK is a **mention** only. Flag: it's the only existing route to drive Claude programmatically without launching the TUI. If someday "user just wants the answer, not the TUI" becomes a requirement, the rails are already laid.

---

## 2. The hard questions

### 2.1 MCP server inversion — how much code?

The existing `plugin_host::mcp::McpClient` is misnamed-but-correct: the framing layer (reader/writer tasks, request/response correlation, inbound channels) is direction-neutral. To act as a **server**:

- Reuse `spawn_reader` and `spawn_writer` verbatim (they take generic `AsyncRead`/`AsyncWrite`).
- Skip the `initialize` outbound call — instead, **respond to** an inbound `initialize` request (decide capabilities, echo back `_meta["dev.neige/auth"].echoed_token`).
- Bind a `UnixListener` (or accept stdio from a spawned `claude` / `codex` that we passed `--mcp-server <sock>` to) and run one `McpServer` per accepted connection.
- New dispatcher: `agent_host::dispatch(method, params)` handling `tools/list`, `tools/call` for the task-management tool set.

**Estimated scope:** ~600 LOC of net-new code in a new `agent_host/` module, plus a small refactor of `plugin_host::mcp` to extract the bidirectional framing into a shared `mcp_framing` module both `McpClient` and the new `McpServer` consume. That refactor is the bigger risk because plugin_host is already shipped and tested — a sloppy extraction breaks plugins.

### 2.2 Push vs pull dispatch — what does the agent actually do?

**Pull (MCP server, agent client):** kernel exposes `tasks/list`, `tasks/take(task_id)`, `tasks/complete(task_id, result)`, `tasks/report_progress(task_id, msg)`. Agent connects, lists open tasks, takes one, runs it. Resource subscription (`notifications/resources/updated` on `tasks://open`) wakes the agent when new tasks arrive. Matches the user's stated MCP requirement.
- Wire surface: ~5 tools + 1 resource.
- Failure mode if MCP disconnects mid-task: agent has no way to report result; on reconnect the task is still marked taken-but-not-complete. We need a heartbeat or "stale-take reclaimer" timer.
- Re-assignment: kernel can flip `task.assigned_card_id`, the new agent's next `tasks/list` shows it. Hard to interrupt the previous agent gracefully — see §2.4.

**Push (PTY stdin):** kernel spawns codex with an initial prompt fed via stdin (the existing codex card already supports this conceptually — `NewCodexBody.initial_prompt` is in the API and currently ignored). Observe via the existing hook bridge. Roughly what `routes/codex.rs` does today plus an unused `initial_prompt` field.
- Wire surface: zero new MCP wire.
- Failure mode: the agent has no structured way to say "I'm done." We'd infer from `hook.codex.stop` + no further events for N seconds. Brittle.
- Doesn't satisfy the user's stated MCP requirement.

**Hybrid (recommended below):** spawn via PTY exactly as the codex card does today (so the TUI is visible in the UI, FSM works, hooks fire). Additionally inject an `~/.codex/config.toml` `mcp_servers.neige = { transport = "stdio", command = ... }` (or `transport = "unix-socket", path = ...`) entry that points the codex / claude TUI at the kernel's MCP server. The agent has a tool palette including `neige.tasks.take`, `neige.tasks.complete`, `neige.tasks.update_progress`. The user's initial prompt to codex says "you have an MCP tool called neige.tasks — call `take` then work on it." Disconnect failure mode: PTY stays alive, the kernel's stale-task reaper handles abandonment. Re-assignment: send a stop frame to the PTY (existing infra) and spawn a new card for the same task id.

### 2.3 Agent pool / lifecycle

**Spawn-per-task (recommended).** Matches the current Codex card flow exactly. Card creation = task assignment = agent spawn. Card death = task done or abandoned. Zero new lifecycle code; reuses the per-card `CODEX_HOME` persistence that already handles "agent crashed mid-task, kernel auto-revives the daemon, codex picks up its session state." No idle-agent token-burn concerns.

**Always-on pool of N idle agents.** Skip. Adds idle-detection, agent-reuse routing, conversation-context-hygiene (do we clear context between tasks?), and burns model tokens on idle health-check loops. None of that is needed for a v1; defer until proven by usage.

### 2.4 Re-assignment semantics

- **Hard** (kill agent A, spawn fresh agent B with same task spec): trivial — kernel sends `ClientMsg::Kill` to the existing PTY (the daemon already handles SIGHUP→SIGKILL with pgid awareness, see `daemon.rs:855`), then creates a new card. A's in-progress thinking is lost. **This is what we should ship first.**
- **Soft** (signal A to drop, hand task to B with A's transcript as context): codex and Claude Code TUIs don't expose a clean "graceful interrupt-and-export-context" — there's the `--resume` flag for codex, but that resumes A's session, doesn't hand it to B. Building this honestly means either parsing the session JSONL on disk (fragile) or running agents through the SDK chat-mode runner instead of TUI. **Defer.**

### 2.5 Task model storage

Two viable shapes, low cost either way:

**A. New `tasks` table + new routes:**
```sql
CREATE TABLE tasks (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    status TEXT NOT NULL,            -- 'open' | 'assigned' | 'in_progress' | 'awaiting_input' | 'done' | 'failed'
    assigned_card_id TEXT REFERENCES cards(id) ON DELETE SET NULL,
    parent_task_id TEXT REFERENCES tasks(id),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
```
Migration: `0004_tasks.sql`. Repo additions: ~15 methods (`task_create`, `task_get`, `tasks_list`, `task_assign`, `task_complete`, ...). REST routes: `routes/tasks.rs` ~250 LOC. Estimate: half a day.

**B. Reuse `Card.kind = "task"` with payload:**
```jsonc
// Card.payload for kind="task"
{ "description": "...", "status": "open", "assignee_card_id": "<codex-card-uuid>" }
```
The card itself is the task. "Assignment" = create a child codex card and stamp `assignee_card_id` on the task card. Status transitions = overlay writes. Zero new tables. Zero new REST handlers (card CRUD already works). One new entry in `validation.rs::validate_card_payload` for the `"task"` kind. Estimate: a couple hours.

**Trade-off:** A is cleaner long-term (tasks are first-class, can have lifecycle distinct from any card). B is faster to ship and lets the existing UI render tasks as cards-on-a-wave **for free**. Given the user said "Status is observable from the neige UI (task list, per-agent dashboard, live TUI cards)", **B gets us the live cards immediately**; we add A later if tasks grow features that don't fit into a card (e.g. tags, search across coves, history).

### 2.6 Reusable today vs net-new

| Component | Status | Notes |
|---|---|---|
| PTY supervisor (`calm-session-daemon`) | Reuse as-is | Already drives codex card. |
| Codex spawn helper (`spawn_codex_for`) | Reuse with one addition | Add `mcp_servers.neige = ...` to the per-card `config.toml`. |
| Codex hook bridge + ingest route | Reuse as-is | FSM already consumes this. |
| Card FSM | Reuse as-is | The state vocabulary already maps to task lifecycle. |
| WS event bus + topics | Reuse + 1 new topic | Add `task:<id>` topic in `event::topics`. |
| MCP framing (`plugin_host::mcp`) | Refactor + extend | Pull bidirectional pieces into a shared module, build `McpServer` on top. **Biggest engineering risk.** |
| Card model + SQLite repo | Reuse with one validator | Add `"task"` to `validate_card_payload`. |
| Plugin auth (`PluginToken`) | Borrow pattern | Same scheme works for per-agent tokens. |
| Settings page (proxy etc.) | Reuse | Agent spawns inherit proxy settings already. |
| Claude Code CLI plumbing | **Net-new** | We have Codex card today; Claude Code is a fresh `routes/claude.rs` + `web/src/cards/builtins/claude.tsx`, both close mirrors of the codex equivalents. |

---

## 3. Architecture options

### Option 1 — "PTY-only, no MCP for dispatch"

**Philosophy:** the agent is launched per task with the prompt fed to stdin via the existing PTY pipe. The kernel observes via the codex hook bridge (already there) and the existing card FSM. No MCP server in the kernel; MCP isn't on the dispatch path at all. Dispatch is just: card-create → agent reads prompt → agent runs → `hook.codex.stop` → FSM marks Done → UI updates.

**What changes:**
- `crates/calm-server/src/routes/tasks.rs` — new ~200 LOC: create task, list tasks, assign task (which spawns a codex card with the task description as initial prompt, then writes back the task's `assigned_card_id`).
- `crates/calm-server/src/routes/codex.rs` — wire up the existing-but-ignored `initial_prompt` field by writing it to PTY stdin after the daemon is ready (~30 LOC).
- `crates/calm-server/src/validation.rs` — add `"task"` validator (~20 LOC).
- `crates/calm-server/src/event.rs` — new topic `task:<id>` and a `TaskUpdated` variant.
- `web/src/cards/builtins/task.tsx` — new card showing task description + status + link to the assigned agent card.
- `web/src/pages/TasksPage.tsx` — task list + dispatch UI.

**What's reusable:** PTY daemon, codex spawn helper, hook bridge, FSM, card/wave model, event bus, plugin auth (not needed here).

**Effort estimate:** **1–3 engineer-weeks.** Mostly UI work; backend changes are small.

**Biggest risk:** does not satisfy the user's explicit MCP requirement. Dispatch becomes "shove prompt into stdin" which has no structured way for the agent to signal partial progress, ask the platform for related context, or hand work back. Re-assignment requires killing the PTY and spawning fresh — no way for the agent to checkpoint.

---

### Option 2 — "Kernel-as-MCP-server, agents pull tasks"

**Philosophy:** the kernel runs an MCP server exposing `tasks/list`, `tasks/take`, `tasks/complete`, `tasks/update_progress`, plus a `tasks://open` resource agents can subscribe to. Each agent process (codex or Claude Code) is spawned by the kernel, but the task assignment happens **inside the agent's tool loop** — the agent on startup calls `tasks/list`, picks the highest-priority open one, calls `tasks/take`, works on it, calls `tasks/complete`.

**What changes:**
- `crates/calm-server/src/agent_host/` — new module, ~700 LOC:
  - `mod.rs`: per-agent state, listener accept loop.
  - `mcp_server.rs`: bidirectional MCP framing as a server (refactor of `plugin_host::mcp` to share `spawn_reader`/`spawn_writer`).
  - `dispatch.rs`: tools/list, tools/call → tasks-table writes + bus emissions.
  - `auth.rs`: per-agent token mint, mirrors `plugin_host::auth`.
- `crates/calm-server/migrations/0004_tasks.sql` — new tasks table.
- `crates/calm-server/src/db/sqlite.rs` — repo additions (~15 methods).
- `crates/calm-server/src/routes/tasks.rs` — REST CRUD (~250 LOC).
- `crates/calm-server/src/routes/codex.rs` (and new `routes/claude.rs`) — write `mcp_servers.neige = { transport = "unix-socket", path = "<per-agent.sock>" }` into the per-agent CODEX_HOME's config.toml; mint and inject the per-agent token via env.
- `web/src/pages/TasksPage.tsx` + new task card.
- `plugins/`: optional — nothing required.

**What's reusable:** PTY daemon, codex spawn helper (extended), card/wave model, event bus, FSM (still useful for the agent-card status), `plugin_host::mcp` framing primitives (with refactor).

**Effort estimate:** **3–6 engineer-weeks.** The MCP server refactor is real work; the tasks table + UI are small; the per-agent socket lifecycle (one socket per agent, cleaned up when the agent exits) needs care.

**Biggest risk:** **the MCP server refactor lands in `plugin_host::mcp`, which is shipped, tested, and currently the only thing standing between us and a broken plugin ecosystem.** A botched extraction breaks plugins. Also: there's no spec-canonical way for an MCP host to advertise "subscribe to `tasks://open` and you'll get notified" that codex/Claude Code TUIs honor today; we'd need to verify both clients actually honor `notifications/resources/updated` for plain pull. If they don't, the agent has to long-poll `tasks/list` (waste).

---

### Option 3 — "Hybrid: PTY for visibility, MCP for control" **[Recommended]**

**Philosophy:** spawn the agent through the existing PTY path (so the TUI is visible in the card, the hook bridge keeps firing, the FSM keeps working). **Additionally**, inject a `mcp_servers.neige` entry into the agent's per-spawn config (`config.toml` for codex, `settings.json` for Claude Code) pointing at a kernel-hosted MCP server (one unix socket per agent, lifetime-bound to the agent card). The user's initial prompt to the agent says "you have an MCP tool called `neige.tasks` — call `neige.tasks.take` to get the spec, then work on it, then call `neige.tasks.complete`."

**What changes:**
- `crates/calm-server/src/agent_host/mod.rs` — new module, ~500 LOC. Just the MCP server side; no listener (the kernel pre-creates the socket before spawning the agent and binds it on spawn).
- `crates/calm-server/src/agent_host/mcp_server.rs` — bidirectional framing, ~200 LOC. **Does not refactor plugin_host::mcp this round** — copy-paste the framing helpers, factor later when there's a second consumer. (Cheaper risk profile than the Option 2 refactor.)
- `crates/calm-server/src/agent_host/dispatch.rs` — handles `tools/list`, `tools/call` for `neige.tasks.take`, `neige.tasks.complete`, `neige.tasks.update_progress`. ~250 LOC.
- `crates/calm-server/src/routes/tasks.rs` — REST surface for the UI (create task, assign task → spawns agent card with `config.toml` written + socket bound). ~200 LOC.
- `crates/calm-server/src/routes/codex.rs` — small addition (~40 LOC): after creating the CODEX_HOME, write a config.toml entry for `mcp_servers.neige` pointing at `data_dir/agent-sockets/<card_id>.sock`. The kernel binds the socket **before** the daemon spawn so the agent's first MCP call doesn't race.
- `crates/calm-server/src/validation.rs` — add `"task"` validator (~20 LOC). Tasks live as `Card.kind = "task"` per §2.5 option B.
- `crates/calm-server/src/event.rs` — `Event::TaskUpdated` + `task:<id>` topic; alternatively reuse `OverlaySet` for status, which means no new event variant. Choose the latter for v1 — it's free.
- `web/src/cards/builtins/task.tsx` — new card: shows description, status, assigned-agent-card link.
- `web/src/pages/TasksPage.tsx` — new page: task list, create-task form, dispatch button.

**What's reusable:** everything in the "Reuse" column of §2.6 plus the PTY-side codex flow.

**Effort estimate:** **3–6 engineer-weeks**, but the *minimum-viable slice* (see §5) lands in **1 week**.

**Biggest risk:** the MCP server we build is bespoke — both codex and Claude Code support the `mcp_servers` config block, but the exact transport options vary (stdio with subprocess vs unix socket vs HTTP). We **must** verify which transports both TUIs actually support before sinking 3 weeks of work into a unix-socket implementation neither client honors. See §6 unknown #1.

---

## 4. Recommendation: Option 3, hybrid

Three reasons:
1. **Satisfies the user's stated MCP requirement** literally — task dispatch goes through MCP.
2. **Inherits the codex card's TUI visibility for free** — no new UI architecture needed for the per-agent live view; the existing `<CodexCard>` and FSM dot just work.
3. **Cheaper than Option 2** because we deliberately don't refactor `plugin_host::mcp` on the first pass. Copy-paste the framing, ship, refactor later when an actual third consumer appears.

The cost is some duplication between `plugin_host` and `agent_host` for one or two release cycles. That's a known tax we can pay down deliberately.

---

## 5. Smallest provable slice (1 week)

Goal: one open task in a list → user clicks "Dispatch to Codex" → codex spawns in a card → codex's MCP tool palette includes `neige.tasks.take` and `neige.tasks.complete` → user watches codex `take` the task, work on it, `complete` it → task row flips to Done.

Brutal scope cuts:
- **Only codex.** No Claude Code support in week 1.
- **No re-assignment.** Hard-kill only; user re-dispatches manually if they don't like the result.
- **No task list polish.** A bare REST endpoint + a 50-line React page listing tasks.
- **Tasks as `Card.kind = "task"`**, no new SQL migration.
- **MCP server is bespoke and copy-pastes plugin_host::mcp framing.** No refactor.
- **Status is observed via codex hooks → existing FSM** for the agent card; tasks' own status is updated by the agent calling `neige.tasks.complete` (sets an overlay or patches the task-card payload).
- **One unix socket per agent**, path = `data_dir/agent-sockets/<card_id>.sock`, bound before spawn, removed on agent exit (best-effort cleanup; reuse the daemon-cleanup pattern from `routes::terminal::spawn_daemon_for`).
- **Per-agent auth = the codex CODEX_HOME directory itself + filesystem perms.** Skip token auth in week 1; treat localhost socket access as good enough for v1. (Add token auth in week 2 mirroring `plugin_host::auth`.)

Concrete deliverables:
1. `validation.rs` accepts `"task"` kind with `{ description, status, assignee_card_id? }`.
2. `routes/tasks.rs` with `POST /api/tasks` (creates a `task`-kind card on a "Tasks" wave), `POST /api/tasks/:id/dispatch` (spawns a codex card on a separate "Agents" wave, writes `config.toml` with `mcp_servers.neige` pointing at the per-agent socket, stamps `assignee_card_id` on the task card).
3. `agent_host/mod.rs` + `agent_host/mcp_server.rs` (~500 LOC) accepting one connection per socket, dispatching `tools/list` and `tools/call` for `neige.tasks.take` (returns the task description for the agent's assigned task) and `neige.tasks.complete` (patches the task card's payload status).
4. `web/src/pages/TasksPage.tsx` with create-task form + list + per-row "Dispatch" button.
5. End-to-end smoke test: create task, dispatch, watch codex card's TUI, type prompt in codex like "call the neige.tasks.take tool", verify the task body is delivered, type "now mark it complete", verify task status flips.

What week 1 demonstrates:
- Tasks live as first-class entities in the UI.
- Agents are spawned per task, visible as PTY cards.
- Dispatch is genuinely on the MCP wire (not stdin hacks).
- The card FSM gives us per-agent status for free.
- The task status round-trips: agent → MCP `tools/call` → kernel writes back → WS event → UI updates.

Out of scope for week 1: Claude Code, re-assignment, agent pools, task DAGs, retries, cost tracking, multi-tenant.

---

## 6. Decisions (was open in v0)

### 6.1 MCP transport — **HTTP, no per-agent bridge needed**

Confirmed against the published config references for both clients:

| Client | stdio (subprocess) | Streamable HTTP | SSE | unix socket |
|---|---|---|---|---|
| **codex CLI** | ✓ (`command` + `args`) | ✓ (`url` + `http_headers` + `bearer_token_env_var`) | — | ✗ |
| **Claude Code** | ✓ (`command` + `args`) | ✓ (`type = "streamable-http"` / `"http"`) | ✓ (deprecated) | ✗ |

Neither honors unix socket. Both honor `stdio` and `streamable-http`. **Decision: HTTP.** The kernel already runs axum on `127.0.0.1:4040`; we add `/api/mcp` mounting a streamable-HTTP MCP server endpoint. No per-agent subprocess bridge needed.

Auth: per-task **bearer token** minted on dispatch, written into the agent's spawn env as `NEIGE_TASK_TOKEN`. Agent's per-spawn `config.toml` references it via `bearer_token_env_var = "NEIGE_TASK_TOKEN"`. The kernel's MCP route maps token → task id at request time; every `neige.tasks.*` tool call is implicitly scoped to that task. Token TTL = agent process lifetime; revoked on `card.delete` or task `complete`.

Codex per-spawn config injection: codex reads `~/.codex/config.toml` by default but honors `CODEX_HOME` to relocate. The kernel already sets a per-card `CODEX_HOME` to write `hooks.json` (per `routes::codex` spawn flow); we write `config.toml` to the same directory with one `[mcp_servers.neige]` block.

Claude Code per-spawn config: same pattern via `~/.claude.json` under a per-card `HOME` override, or via `claude mcp add-json` at boot. Week-2 work; not in MVP.

### 6.2 Task semantics — **flat (description + assignee)** for MVP

Product owner confirmed: flat tasks. No DAGs, no deadlines, no acceptance criteria, no subtasks. Future iteration can add metadata; ship the dispatch loop first.

This unlocks Option B for storage (`Card.kind = "task"`, payload carries `{ title, description, assignee?, status }`). No new SQL migration. The wave grouping becomes the de-facto "project" for tasks.

---

## 7. Notes for the user / follow-ups (not blockers)

- **Cost tracking** — not addressed. If this product runs unattended agents, token spend is a real concern; defer to a follow-up doc.
- **Multi-tenant security** — not addressed. Today everything assumes one user; if this becomes a hosted product, per-user agent-socket auth + isolation needs revisit.
- **Claude Code parity** — week 2 work. The `routes/codex.rs` → `routes/claude.rs` mirror should be ~150 LOC, plus a new `web/src/cards/builtins/claude.tsx` ~ another 150. The PTY layer is identical; the bridge equivalent depends on Claude Code's hook surface.
- **The dormant chat-mode runner** in `calm-session/src/bin/daemon.rs` is a parallel path: SDK-driven, headless. If "we want the answer, not the TUI" ever becomes a requirement, that's the rails. The TUI-first design here doesn't preclude it.
- **`docs/m3-mcp-apps-migration.md` §1.4** describes inverting card creation so the **kernel** calls the plugin via `tools/call` to produce cards. The task-dispatch design here is the same inversion applied to agents: the **agent** calls the **kernel** via `tools/call` to take/complete tasks. The mental model carries over cleanly.

---

## 8. Summary table

| Question | Answer |
|---|---|
| Existing PTY infra reusable for spawning agents? | Yes — `calm-session-daemon` and `spawn_daemon_for` are exactly what we need. |
| Existing FSM reusable for tracking agent status? | Yes — 6-state FSM already wired to codex hooks. |
| Can the kernel act as MCP server? | Not today; needs ~500–700 LOC of new code that can copy framing primitives from `plugin_host::mcp` without immediately refactoring. |
| Does codex's per-card CODEX_HOME let us inject MCP config? | Yes — the kernel already writes `hooks.json` there; writing `config.toml` is the same trick. |
| Best task storage shape for v1? | `Card.kind = "task"` — zero new tables, free UI. |
| Best agent lifecycle for v1? | Spawn-per-task, same as codex card today. |
| Best re-assignment for v1? | Hard kill + respawn. Soft handoff is hard and not needed yet. |
| Recommended architecture? | Option 3 (Hybrid: PTY for visibility + MCP for control). |
| Smallest provable slice? | 1 week, codex-only, no re-assignment, tasks-as-cards. |
| Top blocking unknown? | Resolved — both clients support streamable-HTTP; we use that with per-task bearer tokens. |

---

## 9. Implementation plan (post-research)

Locked design after §6 decisions. Five logical commits, ~1 engineer-week. Each ships independently and leaves `main` working.

### Commit 1 — kernel: per-task bearer tokens + MCP HTTP endpoint skeleton

- `crates/calm-server/migrations/000X_agent_tokens.sql` — `agent_tokens(token PRIMARY KEY, task_id, card_id, created_at, revoked_at)`.
- `crates/calm-server/src/db/sqlite.rs` — `agent_token_mint(task_id, card_id) -> String`, `agent_token_resolve(token) -> Option<(task_id, card_id)>`, `agent_token_revoke_for_card(card_id)`.
- `crates/calm-server/src/routes/mcp.rs` (new, ~250 LOC) — `POST /api/mcp` that speaks streamable-HTTP MCP. Reads `Authorization: Bearer <token>`, resolves to a task scope, replies to standard MCP `initialize` / `tools/list` and a single stub tool that returns "not implemented yet". The wire bring-up; the tool surface arrives in commit 2.
- Wires into the existing axum router; no other changes.

### Commit 2 — `neige.tasks.*` tools + payload schema

- `crates/calm-server/src/validation.rs` — add `"task"` to `validate_card_payload`. Schema: `{ title: str, description: str, assignee?: str, status: "pending"|"taken"|"working"|"complete"|"failed" }`.
- `crates/calm-server/src/agent_host/mod.rs` (new) + `agent_host/tools.rs` (~300 LOC) — implements `neige.tasks.take`, `neige.tasks.update_progress`, `neige.tasks.complete`. Each one resolves the task id from the bearer-token scope, mutates the card payload through `write_with_event` (per the new sync engine), emits Card / Overlay events.
- `routes/mcp.rs` dispatches `tools/call` into `agent_host::dispatch`.

### Commit 3 — `routes/tasks.rs` + dispatch spawn flow

- `crates/calm-server/src/routes/tasks.rs` (new, ~200 LOC) — `POST /api/tasks { wave_id, title, description, assignee? }` creates a `Card.kind = "task"` with `status = "pending"`. `POST /api/tasks/:id/dispatch` mints a bearer token, spawns a Codex card bound to the task via the existing codex spawn path with two additions:
  - Writes `<CODEX_HOME>/config.toml` containing `[mcp_servers.neige] url = "http://127.0.0.1:4040/api/mcp", bearer_token_env_var = "NEIGE_TASK_TOKEN", enabled_tools = ["neige.tasks.take", "neige.tasks.update_progress", "neige.tasks.complete"]`.
  - Sets `NEIGE_TASK_TOKEN` in the agent process env.
  - Initial prompt to codex: "You have an MCP server `neige` with `tasks.take`. Call `tasks.take` to get the spec, work on it, then call `tasks.complete`."
- Task card carries `payload.agent_card_id` linking task → spawned codex card.

### Commit 4 — web: tasks page + task card

- `web/src/api/tasks.ts` — list / create / dispatch hooks.
- `web/src/pages/TasksPage.tsx` (new) — table view of tasks (title, status, assignee, agent card link). Filters by status. Single "Dispatch" button per pending task.
- `web/src/cards/builtins/task.tsx` (new) — compact task display when one is dropped into a wave. Shows status, links to the spawned agent card.
- Registry entry + `+ Add → Task` flow.

### Commit 5 — end-to-end smoke + docs

- `crates/calm-server/tests/task_dispatch.rs` — bring up server, mint token, simulate an MCP `tools/call neige.tasks.take` → verify it returns the task body; `tools/call neige.tasks.complete` → verify the card status flips and events fire.
- `docs/task-dispatch-research.md` → mark §6 unknowns resolved (this commit).
- `plugins/todo/README.md`-style README at `docs/task-dispatch-user-guide.md` explaining the dispatch loop end-to-end with one screenshot of the codex card calling `neige.tasks.take`.

### Out of MVP (queued for v0.2)

- Claude Code parity (separate spawn path, ~1 day).
- Re-assignment (kill + respawn the codex card with new initial prompt; no context handoff). ~½ day.
- Worker pool / "always-on idle agent" mode.
- Cost tracking.
- Per-task tool allowlist UI (today: hardcoded).
