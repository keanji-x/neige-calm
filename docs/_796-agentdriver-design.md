# #796 Design — Unify provider driving: make `claude` a first-class worker (Phase 1) + in-process multi-turn for spec via blocking Stop-hook (Phase 2)

Status: **CONVERGED — both review channels return no blockers (round 5). Phase-1 ratify-ready; Phase-2 has one explicitly-open, deferred design question.** Substrate beneath #760. HEAD `530b9357`.
Grounding: two independent code-exploration channels (codex `gpt-5.5` + subagent) + external Claude-Code capability verification (docs/CLI `2.1.170`) + a Phase-2 mechanism-design pass + **five rounds of dual-channel adversarial review to convergence** (resolutions logged in §9). File:line anchors verified on current `main`.

---

## 0. TL;DR

- **The asymmetry is real and the seam is pre-marked.** codex is a first-class *worker* (dispatchable via `TaskKind::Codex`, reports via MCP `calm.task.complete` through `neige-mcp-stdio-shim`). claude is second-class: hand-create-only (`POST /api/waves/{id}/claude-cards`), absent from `TaskKind` (the enum comment *explicitly reserves* `Claude` "for a later migration … together with the adapter"), and deliberately not MCP-wired.
- **Phase 1 (high-value, clean):** add `TaskKind::Claude` + a `ClaudeWorkerAdapter` that drives the **existing PTY `claude` TUI** (not `claude -p`) with MCP report wiring (via a **worker-specific** settings builder), so claude becomes a schedulable, MCP-reporting first-class worker. Plus a **per-agent-worker wall-clock liveness timeout** (codex+claude; terminal excluded) — closing the one *fatal* autonomous-readiness gap: today there is **no task-level wall-clock timeout**, so a hung-but-alive agent worker is undetectable. The liveness timeout lands **first, independently** and benefits codex immediately.
- **Phase 2 (residual gap, designed here, deferred):** spec/orchestrator multi-turn. codex injects turns via an RPC into a resident daemon (`turn_start`); claude has no daemon. Designed solution: a **blocking `Stop` hook that long-polls a kernel turn-readiness channel** → in-process multi-turn **without the Agent SDK and without PTY keystroke injection**. **This is a provider-aware harness refactor, not a sink swap** (§5); it has **one open design question — exactly-once-vs-observation-loss on turn delivery** (§5.3; leading direction = a **consumption oracle on claude's already-tailed session transcript** `worker_flow/claude_transcript.rs` — a feasible, mostly-built substrate that still needs a shape+ordering spike + a small purpose-built consumption ledger); and it rests on two claude behaviors that **must be de-risked by a live PTY spike** before commit.
- **`AgentDriver` trait: not now.** The repo already has *two* abstractions (`ProviderAdapter` for durable spawn, `WorkerProvider` for post-spawn liveness/resume) and `WorkerProvider`'s own docs say "fold later, don't abstract again now." Phase 1 is one more `impl` in the existing machinery.

---

## 1. The asymmetry today (verified anchors)

| | codex | claude |
|---|---|---|
| dispatchable by spec | ✅ `TaskKind::Codex` | ❌ not in `TaskKind` — manual `POST /claude-cards` only |
| prompt delivery | `turn/start` RPC into shared `codex app-server` | argv `-- '<prompt>'` to a PTY TUI (`claude_adapter.rs:202`, prompt push `:208`) |
| MCP report (`calm.task.complete`) | ✅ via `neige-mcp-stdio-shim` (`codex_adapter.rs:1156`) | ❌ deliberately omitted (`claude_cards.rs:1-7`); reports via `--settings` hook → `neige-codex-bridge` |
| in-process multi-turn | ✅ `turn_start` into resident daemon (`shared_codex_appserver.rs:556`) | ❌ no daemon; "continue conversation" = relaunch `--resume` (`claude_restart_adapter.rs:166`) |
| task-level wall-clock timeout | ❌ **none** (only gate deadlines + reaper death-detection exist) | ❌ **none** |

Verified facts:
- `TaskKind = {Codex, Terminal}` — `crates/calm-truth/src/model.rs:322`. Doc-comment (`:312-318`): claude "deliberately absent … so a later migration can add the variant together with the adapter." DB CHECK allows only `codex|terminal` (`migrations/0041_tasks.sql:6`); `calm.plan.upsert` rejects `kind="claude"` (`mcp_server/tools/plan.rs:213`; rejection asserted by `plan.rs:1310` + `tests/mcp_plan.rs:586`).
- `ProviderAdapter` trait (durable spawn) — `operation/mod.rs:561`. A *second* contract `WorkerProvider` (post-spawn liveness/resume) exists in `calm-exec/src/provider.rs:59`, whose docs say to fold `ProviderAdapter` in later, not re-abstract now (`provider.rs:66`).
- **The precise gap is a *task-level wall-clock* timeout.** Death-detection liveness *does* exist (reaper `reaper.rs:100`; claude provider liveness `calm-provider/src/provider/claude.rs:24`), and gates have parked deadlines (`task_verify_adapter.rs`). But the scheduler holds **no wall-clock judgment over a running worker**: `scheduler.rs:929-932` — the catch-all `Running => {}` arm (`:932`) leaves codex/claude workers alone ("no liveness judgment, R4"). A worker that is *alive but hung* (process up, never reports, reaper sees it as live) is invisible. That is the gap §4.4 closes.

## 2. Capability verification (Claude Code, installed `2.1.170` — confirmed from docs/CLI)

| Capability | Verdict | Mechanism |
|---|---|---|
| MCP client via stdio command transport | ✅ | `--mcp-config` accepts `{"type":"stdio","command":…,"args":…,"env":…}` → can spawn the same generic `neige-mcp-stdio-shim` (shim reads `NEIGE_MCP_SOCKET`/`NEIGE_MCP_TOKEN`, not codex-specific — `neige-mcp-stdio-shim/src/main.rs:60,71`) |
| call a tool **hidden** from `tools/list` | ⚠️ `calm.task.complete` is `visible_to_roles: &[]` (hidden, `emit.rs:117`) but the registry still **routes hidden `tools/call` by name** (`registry.rs:243`). codex calls it fine; **whether claude will call a tool it can't see in `tools/list` is UNVERIFIED** → Slice 4 smoke test must confirm; fallback (§4.3). |
| pre-allow a custom MCP tool headless | ⚠️ TUI already launches `--allow-dangerously-skip-permissions` (`claude_adapter.rs:204`); `--allowedTools` also available — but "no permission prompt" must be confirmed by the Slice-4 PTY smoke test, not assumed |
| system prompt | ✅ `--append-system-prompt` / `--system-prompt` |
| durable resume | ✅ `--session-id` persists JSONL; `--resume <id>` survives restart (`claude_restart_adapter.rs:166`) |
| blocking `Stop` hook injection | ✅ `{"decision":"block","reason":"…"}` continues in-process w/ full tool access; **timeout-discard + 8-block-cap** are the constraints (§5) |
| in-process multi-turn (bare CLI) | ❌ `--input-format stream-json` multi-turn undocumented (#24594) — **not** our path |

## 3. The turn-start mechanism — the theoretical spine

- **codex** has a long-lived shared `codex app-server` daemon; the thread is resident in memory. New turn = `turn_start(thread_id, items)` (`shared_codex_appserver.rs:556`) — an RPC into a process **purpose-built to accept injected turns**. Repeatable, interruptible (`turn_interrupt:592`).
- **claude** has no such daemon. First turn = argv `-- '<prompt>'` (auto-submitted, one TUI turn). "New turn in an existing conversation" today = **relaunch `claude --resume <session-id>`** (`claude_restart_adapter.rs:166`) — a fresh subprocess rehydrating from disk. **Every turn-injection is a process boundary.**

**Altitude split (why Phase 1 is clean, Phase 2 is the gap):**
- A **worker** needs exactly **one** turn (the brief) → does the whole task in one agent-loop → reports via MCP → exits. codex workers are *also* one-shot (`spawn_shared_worker_for_card` does one `turn_start`). So **Phase 1 never touches turn-injection** — symmetric, clean.
- A **spec/orchestrator** is re-prompted every round with new observations → needs repeated injection → codex's daemon shines, claude has no native analog → **Phase 2**.

---

## 4. Phase 1 — claude as a first-class worker (+ wall-clock liveness)

### 4.1 Driving mechanism — the existing PTY TUI, **not** `claude -p`
The claude worker is the **resident interactive `claude` TUI**, PTY-backed (observable), as claude cards are today. Rationale: (a) observability parity with codex/terminal cards; (b) `claude -p` would fork claude into two driving modes — the asymmetry we're killing; (c) the completion signal is the **worker calling `calm.task.complete` over MCP** (like codex) + a wall-clock backstop — not stdout parsing.

> ⚠️ The `Stop` hook is **not** a completion signal — it fires on *every* agent-loop stop = "awaiting input" (`card_fsm.rs:299` Stop entry → `AwaitingInput` at `:302`), not "task done." The **only** completion signal is the explicit `calm.task.complete` MCP call. This makes the wall-clock timeout (§4.4) **load-bearing for claude**: a worker that ends its turn without reporting sits `running` until the deadline fires.

### 4.2 Report path — symmetric MCP via a **worker-specific** settings builder
Give the claude **worker** TUI a single MCP-config mechanism: a **generated per-card `mcp.json` referenced by `--mcp-config`** (one mechanism — *not* also `mcpServers` embedded in settings; the round-2 review flagged the doc contradicting itself across three statements). The `mcp.json` declares one stdio server spawning the **same** `neige-mcp-stdio-shim`, with `card_mcp_env`'s `NEIGE_MCP_SOCKET`/`NEIGE_MCP_TOKEN` (`mcp_server/wiring.rs:9`) in its `env`, so the worker calls `calm.task.complete` → the existing guarded task-flip + lease-release sink (`decision_sink.rs:140,273`). **Reject** extending the `neige-codex-bridge` hook to carry completion (a *second completion producer* for one task row — the "don't patch one producer" failure class).

**⚠️ Token mint happens POST-COMMIT in `spawn_side_effect`, NOT `prepare_tx` (round-2 BLOCKER A1).** Operation `TxOutput` is **persisted** (`repo_sqlite.rs:271`), so a raw MCP token returned from `prepare_tx` would be written to disk — a leak. The codex worker avoids this by minting the token in spawn-side (post-commit) code: `mint_card_mcp_token` (defined `codex_adapter.rs:1209`, called from `spawn_side_effect` at `:899`) writes only the **hash** durably (`card_mcp_tokens` + the active `worker_sessions.mcp_token_hash` that handshake auth resolves, `handshake.rs:98`) and hands the raw token to the spawn env in-memory. The claude worker MUST mirror this: `prepare_tx` creates the card/lease/session **without** a token; `spawn_side_effect` mints + rotates the token, writes the hash to both stores, and injects the raw token into the `mcp.json` env. This is also why §4.3's `card_with_claude_worker_create_tx` takes no raw token.
> "Only the hash is persisted" is scoped to the **DB/operation rows**. The generated `mcp.json` itself **does** contain the raw `NEIGE_MCP_TOKEN` on disk by design — so it MUST be written `0600` in the per-card private dir (mirror codex's config write `shared_codex_home.rs:160`; today's claude settings use plain `std::fs::write` `claude_adapter.rs:438,449` because they hold no secret) and deleted by `compensate_step` (the settings-dir cleanup already removes the dir).

**Worker ≠ manual card — do not mutate the shared builder.** `build_claude_settings_json` (`claude_cards.rs:241`) is the **manual** card path, which deliberately has no MCP; `tests/claude_card_endpoint.rs:496,498` assert the manual card's settings contain **no `mcpServers`/`mcp_servers`** and mint **zero MCP tokens**. Therefore add a **new** `build_claude_worker_settings_json` (or a `mode`/`role` param defaulting to manual-no-MCP); the manual path stays byte-identical, the worker path emits the hook settings while the `mcp.json` (separate file) carries the server.
- `--allowedTools` must be the **narrow** allowlist `mcp__calm__task_complete` (or the exact name the Slice-4 spike confirms) — NOT "whatever the MCP server lists." Plugin-exposed tools are visible to the Worker role (`transport.rs:76`, `:610`); the worker must not be able to freely call them.

### 4.3 Slice plan
**Slice 1 — wall-clock liveness timeout (independent, lands first, agent-workers only).** See §4.4.

**Slice 2 — `TaskKind::Claude` plumbing** (**must NOT ship before Slice 3's adapter+target are registered** — a `plan.upsert{kind:claude}` would create a dispatchable row with no `claude-worker` adapter and wedge at dispatch; land 2+3 as one PR):
1. `model.rs:322` add `Claude`; drop the "deliberately absent" comment half.
2. DB migration widening `tasks.kind` CHECK to include `claude` (SQLite CHECK change = table rebuild; follow `0041`).
3. `plan.upsert` (`plan.rs:213`) — accept `claude`; **delete/flip** the rejection tests `plan.rs:1310` (`kind_claude_rejected_with_not_yet_supported`) + `tests/mcp_plan.rs:586`. **Gate policy** (`plan.rs:765` block, edit line `:776` `t.kind == Codex` → `matches!(t.kind, Codex|Claude)`).
4. `scheduler.rs:157` `build_worker_payload` + `task_kind_str` (`:200`) — add the `("claude-worker", payload)` arm (`cwd` rationale per codex `:165-172`; but see §4.5 lease anchoring).
5. **Ownership SQL** `worker_op_targets_card_tx` (`crates/calm-truth/src/db/sqlite.rs:1185`) — add `claude-worker` (else the MCP report won't map to the card).
6. Tests: explicit `claude` cases for plan parsing, gate policy, worker payload, and **terminal-exit-hook ignoring claude** (`scheduler.rs:1354`).

**Slice 3 — `ClaudeWorkerAdapter` + new worker card helper** (atomic with Slice 2):
- Adapter mirrors `CodexWorkerAdapter`: `kind()="claude-worker"`, phases `[Pending,TxCommitted,SpawnStarted,SpawnSucceeded,Succeeded]`, `app_server_interact` = NotApplicable.
- **New DB helper `card_with_claude_worker_create_tx`** — **NOT** `card_with_claude_create_tx` (which has *no* `spawn_op_id` param `sqlite.rs:2251`, mints no MCP token, inits runtime `spawn_op_id: None` `:2344`). The new helper must, mirroring codex worker creation (`sqlite.rs:2203`): **(a) set `spawn_op_id`** (reaper no-ops on NULL `spawn_op_id` `reaper.rs:528` → a killed-unreported worker never converges without it); **(b)** create the `worker_sessions` row ready to receive the post-commit token **hash** (the raw token is minted later in `spawn_side_effect`, §4.2 — the helper takes **no raw token**); **(c)** role `CardRole::Worker`.
- `prepare_tx`: acquire workspace lease (§4.5), render worker prompt, create the worker card via the new helper. **No token mint here** (§4.2 — TxOutput is persisted).
- `spawn_side_effect`: **mint + rotate the MCP token** (mirror `mint_card_mcp_token` `codex_adapter.rs:899/1209`; hash → `card_mcp_tokens` + `worker_sessions.mcp_token_hash`), write the **worker** settings + generate `mcp.json` with the raw token in its env (§4.2), build the TUI command line (argv prompt `:208` shape + `--mcp-config` + narrow `--allowedTools`), `spawn_terminal`; fast-exit preservation + `log_worker_card_added`; **recovery branch mirroring codex's already-exited check** (`codex_adapter.rs:861-886`) or a boot-sweep resubmit double-spawns.
- `compensate_step`: release lease + reap PTY + delete settings dir + `session_projection_complete_for_card(Failed)`.
- Register in **both** `state.rs:433` `build_operation_adapters` and `dispatcher.rs:247`.
- **Actor-identity check:** the Worker role maps to `ActorId::AiCodexSession(session_id)` (`registry.rs:119`) regardless of provider. Confirm this generic "AI worker session" tag is acceptable for a claude worker, or introduce a provider-neutral variant (attribution scope — verify, don't silently mislabel).

**Slice 4 — live PTY smoke test** (gates trust in §4.2): a real claude worker (a) calls `calm.task.complete` **by exact name despite it being hidden from `tools/list`** (`emit.rs:117`) with **no permission prompt**; on failure, fallback = make a Worker-visible report tool. Confirms the lease path is the cwd and the report maps to the card.

### 4.4 Wall-clock liveness timeout (Slice 1)
Per-**agent-worker** (codex+claude; **terminal excluded** — terminal workers reconcile by exit (`scheduler.rs:988`) and may legitimately run long; a wall-clock kill would change shell-task semantics. If a terminal timeout is ever wanted, it's an explicit per-task opt-in, not this slice). Persisted, crash-safe, swept by the existing reconcile tick.

**Storage (concrete):** new migration adding to the `tasks` table two nullable columns `dispatched_deadline_ms` and `running_deadline_ms` (+ a partial index on the active set). Written in existing txns:
- stamp `dispatched_deadline_ms = now + DISPATCH_TIMEOUT` at claim/dispatch;
- stamp `running_deadline_ms = now + RUN_TIMEOUT` inside `task_mark_running_tx` (`sqlite.rs:1133`, called from `mark_running` `scheduler.rs:729`).

Also update `Task` / `TASK_COLUMNS` / inserts / readers for the two new columns (codex review B-M3).

This covers **both** hang windows: the `running` window **and** the `dispatched`/spawn-start window — but the two are enforced differently (see below).

**`running`-deadline sweep — CAS first (codex review M1):** in the reconcile sweep, for an agent-worker task past its `running_deadline_ms`, **first** run the guarded `task_fail_from_worker_tx(reason="worker-timeout")` (`sqlite.rs:1446`, `WHERE status IN ('dispatched','running') AND <owner>`). **Only if it flips ≥1 row** do we then terminate the PTY/session, project failure, and **release the workspace lease**. If a real `calm.task.complete` won the race, the CAS matches 0 rows → no-op. (The naïve "kill then fail" ordering could complete-the-task-while-failing-the-session; CAS-first eliminates it.)

**`dispatched`-deadline must coordinate with the spawn OPERATION, not race it (round-2 MAJOR A3 / subagent).** A `dispatched` task is owned by an in-flight worker **operation** (`resume_dispatched`→`drive_spawn`, `scheduler.rs:957`) that may still be legitimately spawning; the reaper deliberately skips its `Starting` session (`reaper.rs:145`). If the sweep merely CAS-failed the task row, the op could then SUCCEED and leave an **orphaned live PTY + acquired lease** with no owning task (the sweep's lease-release can't reach a lease the op acquires afterward). Therefore the dispatched-deadline path MUST run **inside the same guarded sweep that holds the per-task `InflightGuard`** used by `resume_dispatched` (`scheduler.rs:957,958`), and on expiry drive the operation to a **compensated terminal phase** (`compensate_step` → release lease + reap PTY) rather than only CAS the row. (Note: there is **no** wall-clock bound on a spawn operation today — `Stuck` is error-driven, not time-driven, `driver.rs:226`; the operation `lease` is a claim lease, not a timeout, `operation/mod.rs:51`. So "set the deadline past the op's Stuck bound" is **not** an option — the InflightGuard-shared sweep, or adding an explicit operation cancel-to-compensation API, is the path.)

- Crash-safe: deadlines are persisted columns, re-read every sweep (survives restart). Reaper convergence still needs `spawn_op_id` (§4.3).
- **Independent of the claude work** — ships against codex alone, immediate value, claude inherits it free.

### 4.5 #760 workspace-lease dependency (sequencing)
Slice 3 MUST acquire a lease (#760 ①) and use the lease path as cwd, or `budget>1` disjointness breaks (`scheduler.rs:118`). **But** the lease helper still resolves paths **relative to the server process cwd** (TODO `workspace_lease.rs:39`) until repo-root anchoring lands. So Slice 3 either (a) depends on the finalized #760 cwd/lease anchoring, or (b) anchors the claude worker cwd to an absolute path before spawn. State the chosen dependency in the Slice-3 PR.

---

## 5. Phase 2 — in-process multi-turn for spec (designed, deferred)

The mechanism: a **blocking `Stop` hook that long-polls a kernel turn-readiness channel.** No Agent SDK, no PTY keystroke injection. Reintroduces a *pull* model that #293 deleted for codex (`/internal/codex/pending_events`), **claude-only**.

### 5.0 This is a provider-aware harness refactor, NOT a sink swap (codex review B3)
The spec harness is **codex-coupled at the type level**, not just at the turn-emission call. Making it drive claude requires a provider-aware boundary across **all** of:
- **turn sink** — codex `daemon.turn_start` (`run_loop.rs:1203`) → claude **park-for-pull**.
- **turn-event source** — the harness consumes codex `ThreadStarted/TurnStarted/TurnCompleted` notifications (`run_loop.rs:720`) to drive `Issuing→TurnCompleted`; `SpecHarnessParams`/`Inner` hold `Arc<SharedCodexAppServer>` (`run_loop.rs:44,58`). claude has none — completion arrives via the §5.3 hook-ack. The daemon field must become provider-enum/optional and the completion source swapped.
- **thread vs session identity** — the issue path gates on a codex `thread_id` (`PendingThreadStart`); claude has a `session_id`.
- **interrupt** — codex has `turn_interrupt` (`run_loop.rs:1495`); **claude has none** (the kernel can only withhold the next turn). Accept the capability gap.
- **routes + card creation** — spec-card routes reject non-codex kinds in **five** places: `routes/cards.rs:191,670,1075` **plus `/spec/interrupt` `:804` and `/spec/run` `:885`** (round-2 A4); lazy recovery is codex-daemon-coupled via `CodexShellState`/`shared_codex_appserver` (`cards.rs:950`); wave creation hard-codes spec `kind:"codex"` (`routes/waves.rs:476`).
- **recovery + session identity** — boot recovery takes a required `Arc<SharedCodexAppServer>` (`harness/mod.rs:189`); DB recovery requires `thread_id IS NOT NULL` for `SharedSpec` (`sqlite.rs:4865`); and `WorkerSessionKind::SharedSpec` **derives provider `Codex`** (`sqlite.rs:2482`) — a claude spec needs a provider-carrying spec-session kind.
- **spec MCP wiring** (codex review M4) — a spec **must** have MCP (plan/lifecycle tools), which worker/manual claude spawn omits. codex mints Spec/Worker MCP tokens (`sqlite.rs:2203`); claude creation does not (`sqlite.rs:2246`). Phase 2 needs a **distinct** claude-spec settings/MCP path with a `CardRole::Spec` token and the spec allowlist — not the worker MCP config.

**This is a provider-runtime interface, not a sink swap** — covering: issue, completion-source, recovery (lazy + boot), route behavior (5 guards), teardown, session identity (provider-carrying spec kind + thread/session), and *unsupported* interrupt semantics. The reused parts are real but bounded: observation debounce, snapshot persistence (`run_loop.rs:1160`), and the **since-last-turn diff** — keyed on the provider-agnostic `wave_vcs` head (`run_loop.rs:1365`), so the *diff content* is reusable unchanged.

### 5.1 Kernel channel
`POST /internal/claude/next_turn` (long-poll, sibling of `ingest_hook` `routes/claude.rs:26`). Request `{card_id, session_id, stop_hook_active, ack_turn_id}`. Responses: `{action:"turn", turn_id, prompt}` | `{action:"stop"}` | `{action:"keep_waiting"}` (heartbeat) | `{action:"stop",resume:true}` (idle-window). New kernel type `ClaudeTurnChannel{parked, notify, done, epoch}` reached via `HarnessRegistry`.

### 5.2 Timeout-discard mitigation — two-clock scheme
A timed-out long-poll **discards** the injection and claude stops. So the **server must always answer before claude's clock**: `SERVER_LONGPOLL(1500s) < T_bridge(1700s) < T_hook(1800s)`. On the server deadline, return `keep_waiting`; the spec-mode bridge **writes nothing to stdout and re-polls in the same process** (writing stdout would end the hook invocation). claude's turn clock never advances → no discard. The bridge writes stdout **exactly once** — the terminal `block`/allow-stop. **Derive the three constants from one base with fixed margins and assert the ordering** (clock-skew = silent failure). The "one blocked process survives N heartbeats" + "heartbeats don't consume the 8-block cap (cap counts emitted `decision:block` only)" claims are **empirical** — covered by the §5.6 spikes; the bridge loop is **new code** (today's bridge is one-shot, `calm-codex-bridge/src/main.rs:46`) and must be built + tested.

### 5.3 Hybrid idle + the turn-delivery problem (**OPEN**)
**Idle handling (settled):** idle > `IDLE_RELAUNCH`(30min) → return `{action:"stop",resume:true}` → claude exits cleanly (session persisted) → kernel relaunches via the `--resume` restart adapter when the next turn parks. In-process while turns flow; `--resume` for long-idle gaps.

**⚠️ OPEN Phase-2 design problem — neither pure policy is correct (rounds 2–4).** The "ack on next poll" protocol cannot distinguish "claude consumed the block then exited before the next poll" (turn done) from "process died before consuming the block" — the server sees `committed && !acked` in both. The two naïve policies each fail:
- **Redeliver** → double-runs non-idempotent side effects: the spec tools carry **no `turn_id`** (`plan.upsert` schema, `plan.rs:621`) and report writes **emit an event on every call, even content-equal** (`wave_report.rs:215`). A per-turn ledger does **not** fix it (a `consumed` flag set on ack still replays; set on first tool-write it suppresses the turn's *own* later writes — codex round-3).
- **No-redeliver** → **can LOSE observations, including human input.** The issued turn text is `prepend_diff_block(diff.block, joined_observation_text)` (`run_loop.rs:1192`); the VCS `diff.block` *is* re-covered by the next turn (`last_seen_head` advances only on observed completion `run_loop.rs:808`→`:1612`, separate from issue `:1365`), **but** the drained `pending_queue` observations are consumed on issue and not regenerated — and one of those is `UserMessage`, whose raw text is sent as prompt yet **deliberately omitted from the audit event** (`event.rs:459-462`, queued raw at `cards.rs:715`), so it is unreconstructable from `wave_vcs`. Worse, the harness only issues when the queue is non-empty (`run_loop.rs:1007`), so an un-advanced head does **not** guarantee eventual re-coverage. Losing a human message is **not** tolerable.

**Leading direction — a consumption oracle built on claude's already-tailed transcript (a mostly-built substrate; reviewed SOUND-WITH-CAVEATS).** The dilemma exists *only* because "did claude consume turn N?" is treated as unobservable. neige **already durably tails the claude session JSONL**: `ClaudeTranscriptFlowSource` (`worker_flow/claude_transcript.rs:94`) tails `~/.claude/projects/<cwd>/<session_id>.jsonl` from a **durable byte-offset cursor** in the `worker_flow_cursors` table (`migrations/0047`; survives a *kernel* crash, partial-final-line-safe) and appends normalized rows to a queryable `worker_flow_items` table. That is a strong substrate for a consumption oracle — but it is **not yet** one, and three gaps must be closed in Phase-2 design (**none blocks Phase 1**):
- **Correction:** these normalized rows are **out-of-domain read-model rows, NOT kernel `Event`s/gates** (`worker_flow_sink.rs` emits no Event). Recovery must scan `worker_flow_items`/raw JSONL — neither is a purpose-built "did turn N start?" ledger today.
- **Not yet a turn-consumption ledger.** `record_starts_turn` (`claude_normalizer.rs:231`) fires **only** for `type=="user"` records, and the item `turn` field is a **derived counter**, not the kernel's `turn_id`; a hook-feedback injection may land as a `system`/`attachment`/`Unknown` record (`:683`, raw content dropped) that never advances the counter. So the oracle must be a **purpose-built ledger keyed on a `turn_id` sentinel** embedded in the injected `reason` and recovered from the raw record/JSONL — *not* the derived counter.
- **The §5.6.3 spike must prove shape AND ordering, not mere presence.** If claude journals the `reason` when it *accepts* the block (before executing the continuation), recovery could read the sentinel and wrongly mark *consumed* → lose the drained observations (false-consumed). The spike must confirm the transcript append happens **only after real consumption**.
- **The MCP belt-and-suspenders is not independent as written.** `tools/call` carries card/session/wave/thread but **no `turn_id`** (`registry.rs:98`, `transport.rs:526`). For "kernel saw an effect ⇒ consumed" to be sound it needs a small **active-issued-turn ledger**: while turn N is the *sole outstanding* injected turn for the session, any session write OR transcript turn-start ⇒ N consumed.
- When the oracle says **not consumed → re-buffer that turn's drained observations** onto `pending_queue` (`run_loop.rs:1185/:1223`) so they fold into the next turn — no human-message loss; **consumed → never redeliver**.

**Net:** the existing transcript-tail subsystem makes a durable consumption oracle **feasible and mostly-built**, which is why this supersedes the blind re-buffer sketch — but *closing* the turn-delivery problem still needs (a) the §5.6.3 shape+ordering spike and (b) a small purpose-built consumption ledger keyed on the sentinel. **Still Phase-2; does not block Phase-1 ratification.** (Credit: this direction was contributed by the architect at ratify; the scoped review had only the weaker re-buffer sketch because it did not survey the orthogonal `worker_flow` transcript-ingest subsystem.)
- **Per-session fencing** (orthogonal, still needed regardless): `ClaudeTurnChannel.epoch` + a session lock so two *concurrent* Stop-hook processes cannot both serve the same parked turn.

### 5.4 Role split — worker untouched
`build_claude_settings_json(.., role)`: Worker/ReportCard keep the **byte-identical** fire-and-forget Stop hook (the signature change touches the two unit-test call sites `claude_cards.rs:262,284`, but worker *output* is unchanged); **Spec** replaces *only* the Stop group with `{type:command, command:<spec cmd>, timeout:1800}`. New `--mode spec` of `neige-codex-bridge` runs the §5.2 blocking loop. Spec env adds `CLAUDE_CODE_STOP_HOOK_BLOCK_CAP=1000` + spec `NEIGE_HOOK_URL`→`/internal/claude/next_turn`.

### 5.5 Crash safety
Kernel restart kills the long-poll conn → bridge treats conn-loss/`max-time` as **allow-stop** (claude exits cleanly, session on disk; never emits `block` on error → never hangs). Boot rebuilds the harness from snapshot (`:1160`) and re-engages live-less spec runtimes via the `--resume` restart adapter. A committed-but-unacked turn is governed by the **open §5.3 turn-delivery policy** (VCS-state is re-covered by the next diff regardless; the non-VCS/human-observation case is the unresolved part). Rides the existing reaper/restart — no new recovery actor.

### 5.6 Must-spike-before-commit (PTY-only, cannot be unit-tested)
1. **Feedback-semantics reliability (HIGH).** The injected `reason` is system-context "feedback," weaker than a true user turn. A spec must reliably *call* MCP plan/lifecycle tools every round. Spike: 10+ blocking injections each demanding a tool call — confirm tool calls fire, not just acknowledgements.
2. **8-block-cap reset (HIGH).** Does real work between blocks reset the consecutive-block counter or accumulate? `CLAUDE_CODE_STOP_HOOK_BLOCK_CAP` + `--resume` recycle mitigate either way, but the clean in-process window length depends on it.
3. **Transcript records the hook injection — SHAPE + ORDERING (MED — unlocks the §5.3 consumption oracle).** Confirm the blocking-Stop-hook `reason` injection (with its `turn_id` sentinel): (a) appears in the `<session_id>.jsonl` transcript at all; (b) in what *shape* (`type:"user"` vs `system`/`attachment`/hook-shaped — determines whether the sentinel survives normalization or must be read from raw JSONL); and (c) **ordering** — that the record is appended only *after real consumption*, not on block-accept (else recovery false-reads "consumed" and loses the turn's observations). Pair with the small active-issued-turn ledger (§5.3) so the kernel-effect fallback is independent.

Other risks: **no interrupt**; process churn under `--resume`; the harness refactor (§5.0) is real scope; spec MCP wiring is subject to the recurring MCP-socket delivery class.

---

## 6. Why not `AgentDriver` now
Two abstractions already exist (`ProviderAdapter` durable-spawn, `WorkerProvider` liveness/resume); `WorkerProvider` docs explicitly defer unification (`provider.rs:66`). A new super-trait would wrap *unlike* things (codex `turn_start` vs claude PTY argv) with one implementor and no second consumer. Add `ClaudeWorkerAdapter` to the existing machinery; revisit a trait only if a 4th provider lands or the spawn bodies converge.

## 7. Open questions for ratify
1. **Liveness timeout scope** — agent-workers only (codex+claude), terminal excluded (§4.4). Confirm, or do you want a per-task opt-in for terminal too?
2. **Phase-2 commit gate** — run the two PTY spikes (§5.6) *before* writing any Phase-2 code; commit Phase 2 only if both pass. (Recommend yes.)
3. **Sequencing vs #760** — Slice 1 (liveness) lands first regardless; claude-worker (Slice 2+3) depends on #760 lease anchoring (§4.5) — block on the finalized #760 cwd path or anchor absolute. Confirm preference.
4. **Phase 2 deferred** — ratify Phase 1 for impl now; Phase 2 stays designed-not-built (matches #760 Q3 "codex-as-spec for now").
5. **Actor identity** — accept `AiCodexSession` as the generic worker-session tag for claude (§4.3), or add a provider-neutral variant?

## 8. Sequenced deliverables
1. **Slice 1** wall-clock liveness timeout (codex+claude; terminal excluded; dispatched+running deadlines; CAS-first).
2. **Slice 2+3** (atomic) `TaskKind::Claude` plumbing + `ClaudeWorkerAdapter` + new worker card helper (sets `spawn_op_id`, seeds the session row; token-hash rotated post-commit in `spawn_side_effect`) + `0600` `mcp.json` + worker settings builder + ownership SQL. Depends on §4.5.
3. **Slice 4** live PTY smoke test (hidden-tool call-by-name, no-prompt, report→card mapping).
4. *(Phase 2, on ratify)* PTY spikes (incl. transcript-records-injection, §5.6.3) → **resolve the open turn-delivery question via the transcript consumption oracle (§5.3)** → provider-aware harness boundary (§5.0) + `next_turn` channel + `--mode spec` bridge loop + spec MCP wiring + route/wave widening + crash recovery.

---

## 9. Round-1 dual-channel review — resolutions
Channel A = subagent (verdict: "substantially sound, unusually well-anchored"); Channel B = codex `gpt-5.5` (4 blockers). All findings folded:

| # | Finding (severity) | Resolution |
|---|---|---|
| B-1 / A-MAJOR | §4.2 mcpServers folded into the **shared manual** builder → breaks manual no-MCP test/invariant | **FIXED §4.2** — new `build_claude_worker_settings_json`; manual path untouched; pick one MCP mechanism. |
| B-2 | `card_with_claude_create_tx` can't set `spawn_op_id`/MCP token → worker won't converge | **FIXED §4.3** — new `card_with_claude_worker_create_tx` sets `spawn_op_id` + seeds the `worker_sessions` row (token-hash rotated post-commit in `spawn_side_effect`, §4.2 — helper takes no raw token). |
| B-3 / A-MAJOR | Phase-2 "only the sink changes" false (daemon type-coupling, notifications, routes, wave kind:codex) | **FIXED §5.0** — reframed as a provider-aware harness refactor enumerating the full boundary. |
| B-4 | Phase-2 exactly-once unachievable (consumed-then-exit ≡ died-before-consume) | superseded by the round-4 A5 reframe → **open Phase-2 turn-delivery problem** (§5.3). |
| B-M1 | Liveness race: kill-then-fail can complete task while failing session | **FIXED §4.4** — **CAS-first**: guarded fail flips first; only on ≥1 row do we kill/reap/release lease. |
| B-M2 / A | Deadline at `mark_running` misses dispatched/spawn-start hangs | **FIXED §4.4** — `dispatched_deadline_ms` + `running_deadline_ms`, both windows. |
| B-M3 | Deadline storage unspecified/nonexistent | **FIXED §4.4** — migration + two nullable `tasks` columns + index + exact write txns. |
| B-M4 | Spec claude MCP wiring real + under-specified | **FIXED §5.0** — distinct claude-spec settings/MCP, `CardRole::Spec` token, spec allowlist. |
| B-M5 | `calm.task.complete` hidden from `tools/list` — can claude call it? | **FIXED §2/§4.3** — Slice-4 spike verifies call-by-name; fallback expose to Worker role. |
| B-M6 / A | Slice 2 not independently landable | **FIXED §4.3** — 2+3 atomic stated as mandatory; rejection tests flipped. |
| B-M7 | "Slice 1 provider-uniform incl terminal" wrong — terminal runs long | **FIXED §4.4** — scoped to agent-workers; terminal excluded (opt-in only). |
| B-M8 | keep_waiting/one-process/8-cap claims unsupported by current one-shot bridge | **FIXED §5.2** — bridge writes stdout once, silent re-poll; loop is new code to build+test; empirical via spikes. |
| B-M9 / A | #760 lease still relative-cwd (`workspace_lease.rs:39` TODO) | **FIXED §4.5** — explicit dependency: block on #760 anchoring or anchor absolute. |
| A-MINOR | lease leak on timeout path | **FIXED §4.4** — lease release in the CAS-success branch. |
| B-MINOR / A | anchor drift (`:202`→`:208`, `:299`→`:302`, `:929`→`:932`, `:765`→`:776`); "no liveness anywhere" too broad | **FIXED §1/§4** — anchors tightened; gap reworded to *task-level wall-clock* timeout. |
| A | Worker actor = `AiCodexSession` regardless of provider | **FLAGGED §4.3/§7.5** — verify acceptable or add provider-neutral variant. |

### Round-2 dual-channel re-review — resolutions
Channel A (subagent): **CONVERGED, no blockers** (4 round-1 blockers confirmed resolved against code) + 1 new MAJOR. Channel B (codex): **NOT CONVERGED (2 blockers)** — caught two round-1 patches as partly hand-waved. Codex's substance accepted; all folded:

| # | Finding (severity) | Resolution |
|---|---|---|
| A1/A2 | **Token can't be minted in `prepare_tx`** — operation `TxOutput` is persisted (`repo_sqlite.rs:271`) → raw-token leak; + doc self-contradicts on MCP-config mechanism | **FIXED §4.2/§4.3** — single mechanism (`mcp.json`+`--mcp-config`); token **minted post-commit in `spawn_side_effect`** (mirror `mint_card_mcp_token` `codex_adapter.rs:899/1209`), only the hash persisted; helper takes no raw token. |
| A3 | dispatched-deadline races the in-flight spawn op → orphaned live worker + lease | **FIXED §4.4** — enforce inside the shared `InflightGuard` path (`scheduler.rs:957`) + drive op to `compensate_step`; update `Task`/`TASK_COLUMNS`/readers. (The "deadline > op Stuck bound" alternative was removed — round-3 A3-alt below.) |
| A4 | Phase-2 boundary broader (`/spec/interrupt :804`, `/spec/run :885`, lazy/boot recovery, `SharedSpec` provider=Codex `:2482`, recovery needs `thread_id NOT NULL` `:4865`) | **FIXED §5.0** — expanded to a full **provider-runtime interface**; all sites enumerated. |
| A5 | at-least-once redelivery NOT harmless — no `turn_id` in tools (`plan.rs:621`), report writes non-idempotent (`wave_report.rs:215`) | round-2 ledger proposal was unsound → see round-3. |
| MINOR | anchor `:2333`→`:2344` (`spawn_op_id: None` init) | **FIXED** throughout. |

### Round-3 dual-channel re-review — resolutions
Channel A (subagent): **CONVERGED, no blockers** (all 4 round-2 blockers confirmed resolved). Channel B (codex): **NOT CONVERGED (1 blocker)** — A5 ledger unsound. Folded:

| # | Finding | Resolution |
|---|---|---|
| A5 (BLOCKER) | per-turn ledger can't distinguish consumed-then-exit from died-before-consume | ledger rejected (unsound). Round-3 tried a no-redeliver baseline → round-4 showed that *also* loses non-VCS/human observations. See round-4 row. |
| A3-alt (MAJOR) | "deadline > op Stuck bound" alternative not real — no spawn wall-clock bound; `Stuck` is error-driven (`driver.rs:226`), lease is a claim lease (`operation/mod.rs:51`) | **FIXED §4.4** — dropped the alternative; InflightGuard-shared sweep (or an explicit op cancel-to-compensation API) is the only path. |
| A5-contradiction (MAJOR) | TL;DR + tables still implied "idempotent turn_id + fencing sufficient" | **FIXED** — TL;DR + §9 rows reworded to the no-redeliver baseline. |
| secret-file (MINOR) | `mcp.json` carries raw `NEIGE_MCP_TOKEN` on disk | **FIXED §4.2** — `0600` private-dir write + `compensate_step` cleanup. |
| stale-wording (MINOR) | summaries said "helper … MCP token" after the post-commit move | **FIXED §8** — "seeds session row; token-hash rotated post-commit." |

### Round-4 dual-channel re-review — resolutions
Channel A (subagent): NOT CONVERGED (1) — `UserMessage`/queue-observation loss + leftover "at-least-once" wording. Channel B (codex): NOT CONVERGED (2) — same observation-loss (elevated: human-message loss + no guaranteed eventual coverage `run_loop.rs:1007`) + the wording/table leftovers. Both correct; folded:

| # | Finding | Resolution |
|---|---|---|
| A5 (final) | no-redeliver is NOT loss-free — drops the turn's drained non-VCS observations incl. human `UserMessage` (body omitted from the audit event `event.rs:459-462`, unreconstructable from `wave_vcs`); and issue requires a non-empty queue (`run_loop.rs:1007`) so eventual re-coverage isn't guaranteed | **REFRAMED §5.3 as an explicit OPEN Phase-2 design problem** (neither pure policy is correct). Direction recorded: **observation-level re-buffering** (re-queue the dropped turn's observations, not redeliver the turn) + per-observation identity. Does **not** block Phase-1 ratification. TL;DR/§5.5/§8 aligned to "open". |
| wording (BLOCKER, codex) | §5.3 heading / §5.5 / §8 still said "at-least-once / redelivered" | **FIXED** — all aligned to the open-problem framing. |
| table-scrub (MAJOR ×2, codex) | §9 round-2 row still had "OR deadline > Stuck bound"; round-1 row still said helper "+ token" | **FIXED** — both rows corrected. |

**Convergence note:** Phase-1 design reached no-blocker convergence on both channels by round 3. The only item that kept recurring is the Phase-2 spec turn-delivery exactly-once-vs-loss problem — now correctly **scoped as the single open Phase-2 design question** rather than force-closed. Phase 2 is deferred and spike-gated, so an honestly-open question there is the correct end-state; Phase 1 is ratify-ready.

### Ratify-stage addition — transcript consumption oracle (§5.3)
At ratify the architect pointed out that claude's actions are already tracked as a durable trace. A dedicated dual-channel review confirmed (both channels: **SOUND-WITH-CAVEATS, not a blocker**) that the existing `worker_flow/claude_transcript.rs` subsystem (durable JSONL tail + `worker_flow_cursors`/`worker_flow_items`) is a strong, mostly-built substrate for a turn-consumption oracle that would *close* the open §5.3 problem — folded as the **leading direction**, with the honest gaps the review surfaced: (1) the normalized rows are read-model, not kernel `Event`s; (2) `record_starts_turn` is `type=="user"`-only and the `turn` field is a derived counter → need a purpose-built ledger keyed on a `turn_id` sentinel; (3) the spike must prove transcript-append **ordering** (after real consumption, not block-accept) to avoid false-consumed loss; (4) the MCP fallback needs a small active-issued-turn ledger to be independent (`tools/call` carries no `turn_id`). Substrate exists; closing the problem needs the §5.6.3 spike + that small ledger. **Phase-1 unaffected.**

*Anchors:* `model.rs:312-325`; `operation/mod.rs:561`; `calm-exec/src/provider.rs:59,66`; `codex_adapter.rs:170-184,716-933,1156-1166`; `claude_adapter.rs:202-211,204,208`; `claude_cards.rs:1-7,241,262,284`; `claude_card_endpoint.rs:496,498`; `claude_restart_adapter.rs:166`; `scheduler.rs:157,200,729,926-932,952,988,1347,1354`; `mcp_server/wiring.rs:9`; `mcp_server/tools/plan.rs:213,672,765,776,1310`; `tests/mcp_plan.rs:586`; `db/sqlite.rs:1133,1185,1446,2203,2246,2251,2344,2482,2759,4865`; `repo_sqlite.rs:271`; `handshake.rs:98`; `codex_adapter.rs:899,1209`; `routes/cards.rs:191,670,804,885,950,1075`; `plan.rs:621`; `wave_report.rs:215`; `harness/mod.rs:189`; `operation/mod.rs:51`; `operation/driver.rs:226`; `shared_codex_home.rs:160`; `migrations/0041_tasks.sql:6,0053_*,0056_workspace_leases.sql`; `emit.rs:117`; `registry.rs:119,243`; `transport.rs:76,610`; `reaper.rs:100,145,528`; `calm-provider/src/provider/claude.rs:24`; `shared_codex_appserver.rs:556,592`; `harness/run_loop.rs:44,58,720,1160,1192,1203,1365,1495`; `routes/claude.rs:26`; `routes/cards.rs:191,670,1075`; `routes/waves.rs:476`; `card_fsm.rs:299,302`; `neige-mcp-stdio-shim/src/main.rs:60,71`; `calm-codex-bridge/src/main.rs:46,85`; `workspace_lease.rs:39`.
