# Claude Code as a second Planner backend (#1791) — design v1

> **Status (2026-09-24)**: design draft v1, not yet reviewed. Base `origin/main` = `c5abb3c3d`
> (worktree `design/1791-claude-planner-backend`). Every file:line below was read on that base;
> paths without a prefix are under `crates/calm-server/src/`. 4140 numbers come from the read-only
> live DB (`events.max(id)=58979`, last event 2026-09-24 10:17:43Z); queries are in §3.
> Probe evidence (claude 2.1.280, `claude-haiku-4-5`, scratchpad only, never the production
> server or MCP socket) is in Appendix A; each probe is cited as `[P-x]`.
>
> Vocabulary note: `scripts/gate-1316-terminology-ratchet.sh` counts retiring words in `docs/`.
> This document therefore says "transcript table" / "transcript row" for the provider-item table
> created by migration 0031, and spells its name in two halves inside the §3 shell snippet.

## 0. Owner rules (binding for every slice)

1. **Don't expand without limit — solve the pain point, simple first.** Ship the fewest mechanisms
   that give a working Claude Planner. Hypothetical cases become one-line KNOWN GAPS, not
   mechanism. When the slice table grows past the pain point, cut and record the cut (§9.4).
2. **Compatibility means only what is in the 4140 DB.** Numbers and the exact queries are in §3.
3. **Repo `AGENTS.md` applies**: required types over `Option`, typed config instead of ambient
   environment variables, explicit env allowlists for child processes at credential/isolation
   boundaries, no silent fallbacks, byte-frozen migrations, source files ≤ 800 lines. `fe/AGENTS.md`
   applies to the one FE slice (layering `app → features → systems → ui → core`, no barrels,
   gates need positive and negative fixtures).

## 1. Problem, goal, decisions

**Pain point.** The Planner (and the PlainChat / Assistant conversation profiles) can only run on the
Codex app-server. The owner wants a Planner that runs on Claude Code.

**Goal (v1).** A track can be created with a Claude Planner. That Planner receives the same queued
observations and user messages, calls the same `calm.*` MCP tools and `neige` CLI, can be interrupted,
survives a neige restart by resuming its Claude session, and its transcript renders in the existing
conversation UI with no FE renderer change.

**Non-goals.** No ACP adapter, no Node sidecar, no Agent SDK dependency; no provider-neutral item model
migration (the stored rows stay Codex-shaped); no Codex behaviour change beyond extracting the seam;
no worker-card changes; no Claude for PlainChat / Assistant in v1 (cut, §9.4).

**Key decisions (each argued below).**

| # | Decision | Where |
|---|---|---|
| D1 | Seam = `enum PlannerBackend { Codex(Arc<SharedCodexAppServer>), Claude(Arc<ClaudePlannerSession>) }`, nine members + two helpers; no trait object | §4.1 |
| D2 | The Claude backend emits **Codex-shaped `Notification` values** into its own broadcast; `on_notification`, persistence and the FE stay unchanged | §4.2 |
| D3 | Provider is a required, server-owned card payload key `planner_provider` (`"codex"`/`"claude"`), backfilled by one new migration; runtimes mirror it in `worker_sessions.provider` | §4.3 |
| D4 | **One `claude -p` process per turn** (stream-json in/out), `--session-id` on the first turn, `--resume` afterwards. Deviates from the issue's "resident per harness"; owner question Q1 | §5.1 |
| D5 | Permissions are **static CLI rules** (`--permission-prompts none` + `--allowedTools`), no `can_use_tool` responder | §5.5 |
| D6 | Dedicated `CLAUDE_CONFIG_DIR` under the neige data dir; the user runs Anthropic's own login in it; neige never reads credentials (owner question Q2) | §8 |
| D7 | `Recover` is not offered to Claude Planners: the thread is simply not registered, so the kernel's existing exact-interface briefing (`calm.plan.recover`) applies | §5.6 |
| D8 | Steer returns an explicit "not supported by this provider" answer in v1; the lifecycle-based design is recorded for a later slice | §5.8 |

## 2. Fact table (VERIFIED on `c5abb3c3d` unless marked)

### 2.1 Harness ↔ Codex coupling

| # | Fact | Location |
|---|---|---|
| H1 | `PlannerHarnessParams.daemon` and `Inner.daemon` are `Arc<SharedCodexAppServer>` | `harness/run_loop.rs:164`, `:178` |
| H2 | `PlannerHarness::run` subscribes `daemon.subscribe_notifications()` (a `broadcast::Receiver<Notification>`) before spawning the loop | `harness/run_loop.rs:404` |
| H3 | Turn issuance goes only through `IssueTurnHandle::issue` → `daemon.turn_start(thread, items, selection, client_id)` | `harness/run_loop.rs:264-275` |
| H4 | The loop is one task: `select!` over commands, notifications, a tick (`watchdog_tick` + `maybe_issue_turn`) and shutdown; notifications that arrive while `maybe_issue_turn` awaits are buffered and handled after it returns | `harness/run_loop.rs:1081-1186` |
| H5 | `on_notification` drops frames whose `thread_id()` differs from the harness thread; ignores `approval/*`; handles `ThreadStarted`, `ThreadStatusChanged` (systemError → `Wedged`), `TurnStarted`, `TurnCompleted`, `turn/aborted`, `Item` (only `item/started`/`item/completed`), `turn/plan/updated`, `thread/tokenUsage/updated` | `harness/run_loop.rs:1763-1776`, `:1780`, `:1793`, `:1808`, `:1852`, `:1942`, `:1982`, `:2102`, `:2140` |
| H6 | `TurnStarted` is accepted in `Issuing{TurnStart}` only when `issued_turn_id == turn.id` | `harness/run_loop.rs:1816-1827` |
| H7 | `TurnCompleted` while `Issuing{Interrupt}` for the target turn → `TurnCompleted` state + outcome row; otherwise only accepted from `TurnRunning` with a matching id | `harness/run_loop.rs:1852-1939` |
| H8 | A `userMessage` `item/completed` carrying `item.clientId` upgrades the projection row written at drain instead of inserting a second row; a started echo of a projected message is skipped | `harness/run_loop.rs:2022-2080`, projection shape `:2315-2327` |
| H9 | Only `item/started`/`item/completed` are persisted (verbatim `params`), then a `harness.item.added` event is logged | `harness/run_loop.rs:2195-2197`, `:2208-2233`, `:2235-2262` |
| H10 | Turn outcome = one transcript row, `method='turn/completed'`, `params` = the Codex `Turn` minus `items`; idempotent per (session, card, thread, turn) | `harness/turn_outcome.rs:6-29`; `crates/calm-truth/src/db/sqlite/out_of_domain.rs:413-431` |
| H11 | Token usage is read from `tokenUsage.last.totalTokens` / `.total.totalTokens` / `.modelContextWindow`, latest-wins in the snapshot | `harness/token_usage.rs:33-55`; `harness/run_loop.rs:2140-2172` |
| H12 | Steer: `handle_steer` takes the entry, calls `daemon.turn_steer`, maps `CalmError::CodexRefused` → `SteerRefused::NotTaken`, anything else → `Unanswered` | `harness/run_loop.rs:1273-1470`, call `:1339` |
| H13 | Interrupt: `issue_interrupt` → `issue_interrupt_for_turn` sets `Issuing{Interrupt}`, arms `interrupt_deadline` (30 s, `harness/config.rs:27`), calls `daemon.turn_interrupt` | `harness/run_loop.rs:3572-3653` |
| H14 | Watchdog: interrupt deadline elapsed → `Wedged("interrupt_timeout")`; `max_turn_duration` (30 min) → interrupt | `harness/run_loop.rs:3519-3570` |
| H15 | Shutdown: `shutdown_inner` calls `daemon.interrupt_active_turn` and `daemon.turn_interrupt(last)`; `shutdown_for_deletion` seals the thread | `harness/run_loop.rs:648-729` |
| H16 | Model resolution calls `daemon.config_read` (only when the card follows installation defaults) and `daemon.model_list` (only for a default effort); both go through `classify_codex_failure` | `harness/run_loop.rs:2404-2412`, `:2449-2516`, `:2621-2653` |
| H17 | Semantic recovery: `maybe_issue_turn` asks `semantic_recovery::registered(card, thread)`; `false` makes the briefing use the exact MCP interface (`calm.plan.recover`) | `harness/run_loop.rs:3060-3070`; `harness/recovery_briefing.rs:126-128`; `prompts/recovery-briefing/exact-interface.md` |
| H18 | Boot/deferred/aborted-deletion recovery fns take `Arc<SharedCodexAppServer>` and use `turn_thread_is_sealed` and `readiness_receiver` | `harness/mod.rs:117-254` (`spawn_recovered_harness`), `:335`, `:408`, `:476`; uses `:156-160`, `:213`, `:421-426`, `:477` |
| H19 | `state_from_snapshot`: a snapshot in `TurnRunning`/`IssuingInterrupt` restores as `Resumed`, which the watchdog turns into `Idle` after 5 s | `harness/run_loop.rs:3925-3960`; `harness/config.rs:28` |
| H20 | The fixtures fake lives inside `SharedCodexAppServer` (`fake: Option<…>`); ~70 `daemon:` fields in 31 files construct `PlannerHarnessParams` | `shared_codex_appserver.rs:784-860`, `:1328-1400`; `git grep -l PlannerHarnessParams` |
| H21 | Boot recovery runs only when the Codex daemon started; otherwise every harness waits for the deferred pass armed on Codex readiness | `lib.rs:615-640`; `state.rs:577-592` |

### 2.2 Start, stop, identity

| # | Fact | Location |
|---|---|---|
| S1 | `HarnessProfile::from_shape` returns `None` unless `kind == "codex"` | `harness/profile.rs:25-45` |
| S2 | Headless routes gate on `card_runs_headless_harness` (= `from_card(..).is_some()`); ratify requires `kind == "codex"` + Planner role | `routes/cards.rs:88-90`, `:982` |
| S3 | Track create mints the Planner card with `kind: "codex"` and `planner_harness_card_payload` (`schemaVersion`, `codex_source`, `planner_harness: true`) | `routes/tracks.rs:1409-1439`, `:1864-1879` |
| S4 | `CreateTrackRequest` is `deny_unknown_fields`, carries `model` / `reasoning_effort` | `routes/tracks.rs:267-298` |
| S5 | Start adapter: developer instructions per profile, mints a card MCP token pair, `SharedThreadStartParams{approval_policy:"never", sandbox_mode:"workspace-write", config: McpShell}`, calls `thread_start_*`, records `codex_thread_id`, `appserver_sock` | `operation/planner_harness_start_adapter.rs:840-991`, `:1823-1860` |
| S6 | Start adapter mints the runtime with `AgentProvider::Codex` | `operation/planner_harness_start_adapter.rs:727`, `:1098`, `:1128` |
| S7 | Shutdown adapter with no live harness interrupts through the codex daemon directly | `operation/planner_harness_shutdown_adapter.rs:101-135` |
| S8 | Interrupt adapter only calls `harness.interrupt` (no codex coupling) | `operation/planner_harness_interrupt_adapter.rs:110-120` |
| S9 | `ensure_live_planner_harness` / planner recovery route refuse when `shared_codex_appserver.is_running()` is false | `routes/cards.rs:1232`, `:1289-1293`; `routes/planner_recovery.rs:58-61` |
| S10 | Model catalog route reads `shared_codex_appserver.model_list` | `routes/planner_model.rs:222` |
| S11 | `AgentProvider {Codex, Claude}`; `WorkerSessionKind` has a shared-Planner variant | `crates/calm-types/src/runtime.rs:14-32` |
| S12 | `worker_sessions.provider` CHECK allows `codex`/`claude`/`terminal` (migration 0045); `derive_session_identity(SharedPlanner)` → Codex | `crates/calm-truth/src/db/sqlite/session_row.rs:146-166` |
| S13 | `(Claude, Planner)` is **unmappable** today → error | `crates/calm-truth/src/session_projection_row.rs:145-163` |
| S14 | Boot recovery SQL selects `ws.provider = <codex>` (+ planner/plain_chat/assistant arms), pinned by `boot_recovery_sql_literals_track_the_minted_card_shape` | `crates/calm-truth/src/db/sqlite/session_projection.rs:731-790`; `operation/planner_harness_start_adapter.rs:2303` |
| S15 | `track_activity` treats any `mode=harness` row as a harness conversation before looking at `provider` | `track_activity.rs:259-266` |
| S16 | `claude_restart_adapter` and the Claude PTY routes key on `card.kind == "claude"`; `worker_flow` attaches only `CodexCard`/`ClaudeCard` runtimes | `operation/claude_restart_adapter.rs:133`; `worker_flow/mod.rs:278`, `:519-529` |
| S17 | FE identifies the Planner card by `kind === 'codex' && payload.planner_harness === true` (5 sites) | `fe/web/src/systems/cards/builtins/planner.ts:11-27`; `fe/web/src/app/router/public.tsx:1819`, `:1853`, `:1951`, `:2087` |
| S18 | Typed config already has `claude_bin` (`--claude-bin`) | `config.rs:91-92` |

### 2.3 MCP, tools, prompts, FE rendering

| # | Fact | Location |
|---|---|---|
| M1 | Codex home writes `[mcp_servers.calm]` = `neige-mcp-stdio-shim` + daemon token env | `shared_codex_home.rs:316-335`; `mcp_server/wiring.rs:61-69` |
| M2 | Per-thread Codex config: `shell_environment_policy.set` carries `NEIGE_MCP_SOCKET`, `NEIGE_MCP_TOKEN`, kernel-led `PATH`; Planner threads get `approval_mode:"approve"` for the three terminal tools | `mcp_server/wiring.rs:11-57` |
| M3 | Card token helpers: `mint_card_mcp_token_pair`, `mint_and_persist_card_token` | `mcp_server/wiring.rs:71`, `:104` |
| M4 | The shim prefers `NEIGE_MCP_DAEMON_TOKEN` over `NEIGE_MCP_TOKEN` | `crates/neige-mcp-stdio-shim/src/main.rs:60-65` |
| M5 | A card-bound MCP connection may omit `_meta.threadId` | `mcp_server/registry.rs:53-61`; `mcp_server/transport.rs:277` |
| M6 | `calm.user.notify` writes nothing; the transcript row of the `mcpToolCall` (with `arguments`) is the message | `mcp_server/tools/user_notify.rs:1-2`; FE `fe/core/domain/conversation.ts:764-798` |
| M7 | `model_tool_key` strips any `mcp__<server>__` qualifier and maps non-`[A-Za-z0-9_]` to `_` — Claude's hyphen-preserving names resolve too | `mcp_server/transport/worker_grants.rs:58-89` |
| M8 | Prompts name Codex spellings: `planner.md:67` ("Codex-sanitized spelling"), `assistant/mechanics.md:14`, `tools/calm.task.dispatch.md:1`, `tools/calm.source.capture.md:1` | `prompts/…` |
| M9 | `Recover` is a Codex dynamic tool: registered only for Planner threads at `thread/start`, served over server→client `item/tool/call`, provenance keyed by Codex thread/turn/call ids (migration 0101) | `shared_codex_appserver.rs:1276-1302`; `codex_appserver/server_requests.rs:235`; `semantic_recovery/mod.rs:22-26`, `:130-160` |
| M10 | FE converters read: `agentMessage.text`; `userMessage.content[].text`; `mcpToolCall.{tool,arguments,status,error}`; `commandExecution.{command,exitCode,status,aggregatedOutput,durationMs}`; `fileChange.changes.length`; `reasoning` (no text); outcome `{id,status,error.message,error.codexErrorInfo}`; envelope `completedAtMs` | `fe/core/domain/conversation.ts:800-870`, `:979-1026`, `:1054-1091`, `:1116-1144`; tool names `fe/core/keys/mcp-tools.ts:1-34` |
| M11 | Only `item/started`, `item/completed`, `turn/completed` are transcript methods | `fe/core/domain/conversation.ts:1146-1152` |

### 2.4 Source-invariant gates a slice will trip

| Gate | What it pins | Tripped by |
|---|---|---|
| `tests/cases/harness_turn_start_invariant.rs` | `.turn_start(` appears once in `run_loop.rs` and only in an allowlist of files | PR1 (new `harness/backend.rs` dispatches `.turn_start(`; add it to the allowlist with the reason) |
| `scripts/gate-1316-terminology-ratchet.sh` (CI `ci.yml:464`) | occurrence counts of retiring words in `crates/ fe/ docs/ e2e/`, both directions | any new Rust that names the transcript-row insert fn or the `…Item…Added` event; the Claude backend avoids this by emitting notifications only (D2) |
| `scripts/gate-prose-ratchet.sh` | no CJK runs, no ≥120-char string literal in `crates/**/*.rs` | error/refusal texts in the Claude backend; long texts go to `prompts/` |
| `planner_harness_start_adapter::tests::boot_recovery_sql_literals_track_the_minted_card_shape` (`:2303`) and `the_persisted_payload_field_names_are_frozen` (`:2140`) | boot SQL literals, payload field names | PR3/PR4 |
| goldens `tests/goldens/issue_development_planner_prompt.txt`, `assistant_prompt*.txt`, `mcp_tool_registry.json` | rendered prompts, tool registry | PR6 (prompt wording) |
| OpenAPI drift (`scripts/local-rust-gates.sh --quick` step 5) + `fe` `npm run gen:api` | `CreateTrackRequest` schema | PR3 |
| Migrations byte-frozen; `SYNC_EVENT_VERSION` lockstep gate | new migration only; no new event kind in v1 | PR3 |
| 800-line rule | `run_loop.rs` is already 4224 lines | every slice: new code goes in new files |

## 3. 4140 compatibility (the only compatibility that counts)

```sh
DB=~/.local/share/neige-next/data/calm.db
q(){ sqlite3 -readonly -header -column "$DB" "$@"; }
T='harness''_items'   # the migration-0031 transcript table; two halves only for the ratchet gate
q "SELECT method, item_type, COUNT(*) n FROM $T GROUP BY 1,2 ORDER BY n DESC;"                  # Q1
q "SELECT COUNT(*) total, COUNT(DISTINCT card_id) cards, COUNT(DISTINCT thread_id) threads FROM $T;" # Q2
q "SELECT json_extract(params,'\$.status') s, COUNT(*) FROM $T WHERE method='turn/completed' GROUP BY 1;" # Q3
q "SELECT json_extract(params,'\$.item.server') srv, COUNT(*) FROM $T WHERE method='item/completed' AND item_type='mcpToolCall' GROUP BY 1;" # Q4
q "SELECT COUNT(*) FROM $T WHERE item_type='dynamicToolCall';"                                  # Q5
q "SELECT COUNT(*) FROM $T WHERE json_extract(params,'\$._projection')=1;"                        # Q6
q "SELECT role, kind, COUNT(*) FROM cards GROUP BY 1,2 ORDER BY 3 DESC;"                         # Q7
q "SELECT provider, contract, state, COUNT(*) FROM worker_sessions GROUP BY 1,2,3;"              # Q8
q "SELECT kind, phase, COUNT(*) FROM operations WHERE kind LIKE 'planner-harness-%' GROUP BY 1,2;" # Q9
q "SELECT (SELECT COUNT(*) FROM planner_recovery_threads),(SELECT COUNT(*) FROM planner_recovery_turns),(SELECT COUNT(*) FROM planner_recovery_issuances),(SELECT COUNT(*) FROM planner_recovery_calls);" # Q10
q "SELECT json_extract(payload,'\$.planner_harness') ph, json_type(payload,'\$.harness') legacy, COUNT(*) FROM cards WHERE role='planner' GROUP BY 1,2;" # Q11
```

| Q | Result (2026-09-24) | Consequence |
|---|---|---|
| Q1 | `reasoning` 840 started / 838 completed; `commandExecution` 658/658; `agentMessage` 377/377; `mcpToolCall` 357/357; `userMessage` 123 completed; `turn/completed` 123; `fileChange` 35/35; `webSearch` 18/17; `subAgentActivity` 10/10; `imageView` 9/9; `collabAgentToolCall` 4/4; `contextCompaction` 2/2 | The FE already renders every type the Claude mapping produces (§6.1); nothing to migrate |
| Q2 | 4863 rows, 13 cards, 13 threads (2026-09-16 … 09-24) | small, all Codex |
| Q3 | `completed` 117, `interrupted` 5, `failed` 1 | the three statuses the FE draws; Claude outcomes reuse them |
| Q4 | server `calm` 351, `codex_apps` 4, `codex` 2 | the dotted `calm.*` names in `item.tool` are the rendering contract |
| Q5 | 0 | no stored `dynamicToolCall` rows; using that type for Claude-native tools breaks nothing |
| Q6 | 0 outstanding projection rows | no drain in flight |
| Q7 | planner/codex 24, assistant/codex 5, worker/codex 48, worker/claude 12, worker/terminal 12, reportcard 24 | 24 Planner cards need the D3 backfill; no Claude conversation card exists |
| Q8 | codex/planner idle 24, superseded 17; claude/executor exited 12; **no claude/planner row** | the new `(claude, planner)` mapping has no legacy rows |
| Q9 | start 85, shutdown 18, interrupt 19 — all `succeeded` | no operation replay can observe a payload change |
| Q10 | recovery threads 38, turns 4, issuances 4, calls 1 | Codex-only provenance; Claude never writes these tables |
| Q11 | 23 with `planner_harness=1`, 1 legacy `harness` object shape | backfill keys on `role='planner'`, not on the marker |

## 4. The seam

### 4.1 What the harness actually calls

Complete list of `SharedCodexAppServer` members reached from harness, start, stop and recovery code
(`git grep -n "daemon\.\|shared_codex_appserver\." -- harness operation/planner_harness_* routes/cards.rs routes/planner_*`):

| # | Member | Callers | Behind the seam? | Claude behaviour |
|---|---|---|---|---|
| 1 | `subscribe_notifications()` | run_loop `:404` | **yes** | per-session broadcast of synthesized notifications |
| 2 | `turn_start(thread, items, selection, client_id)` | run_loop `:271` | **yes** | spawn one process, write one `user` line (§5.1) |
| 3 | `turn_steer(…)` | run_loop `:1339` | **yes** | v1: refused before the entry leaves the queue (D8) |
| 4 | `turn_interrupt(thread, turn)` | run_loop `:704`, `:3640` | **yes** | `control_request{interrupt}`, then kill after budget |
| 5 | `interrupt_active_turn(thread)` | run_loop `:691` | **yes** | same, for whatever turn is live |
| 6 | `active_turn_id_for_thread(thread)` | run_loop `:690`, `:3601` | **yes** | the live process's turn id |
| 7 | `seal_turn_thread_for_deletion` / `DeletionThreadSeals` / `unseal…` | run_loop `:658-679`; routes/tracks, routes/areas | **yes** | a `sealed` flag that refuses `turn_start` and kills a live process |
| 8 | `turn_thread_is_sealed(thread)` | harness/mod `:158`, `:424` | **yes** | the same flag |
| 9 | `config_read` | run_loop `:2472` | Codex-only | never called for Claude (§5.7) |
| 10 | `model_list` | run_loop `:2632`, routes/planner_model `:222` | Codex-only | PR6 adds a Claude catalog |
| 11 | `readiness_receiver()` | harness/mod `:213`, `:477` | Codex-only | deferred recovery waits for the Codex daemon; Claude rows must not (H21, §4.3) |
| 12 | `is_running()` / `not_running_message()` | routes/cards `:1289`, planner_recovery `:58` | **yes** (`is_ready`) | `claude_bin` resolved at boot |
| 13 | `thread_start_for_card` / `thread_start_mint_for_card` / `remote_uri` | start adapter `:970-991` | branch in the adapter | no RPC: the thread id is a fresh UUID (the Claude session id) |
| 14 | `resume_system_error_conversation` | planner_recovery route | Codex-only | Claude never enters `Wedged(systemError)` (§6.2) |
| 15 | `interrupt_active_turn_for_card` | routes/cards `:117` (card teardown) | branch in the route | the process belongs to the harness: stop it through the harness registry |

So the seam is **nine members** (1–8, 12) plus `supports_steer` and the explicit `codex()` door. Everything
else stays a Codex-only call behind an explicit `match` on the backend.

**Why an enum, not a trait.** Two variants, both known at compile time; the Codex variant keeps its
fixtures fake (H20) untouched; an enum `match` makes each Codex-only call site visible instead of a
trait default that silently does nothing for Claude. `harness/backend.rs` (new, ≤ 300 lines):

```rust
#[derive(Clone)]
pub enum PlannerBackend {
    Codex(Arc<SharedCodexAppServer>),
    Claude(Arc<ClaudePlannerSession>),
}
impl PlannerBackend {
    pub fn subscribe_notifications(&self) -> broadcast::Receiver<Notification>;
    pub async fn turn_start(&self, thread: &str, items: Vec<InputItem>,
        selection: &TurnModelSelection, client_id: Option<&str>) -> Result<String>;
    pub async fn turn_steer(&self, …) -> Result<String>;          // Claude: unreachable, see D8
    pub fn supports_steer(&self) -> bool;
    pub async fn turn_interrupt(&self, thread: &str, turn: &str) -> Result<()>;
    pub async fn interrupt_active_turn(&self, thread: &str) -> Result<()>;
    pub fn active_turn_id_for_thread(&self, thread: &str) -> Option<String>;
    pub fn seal_for_deletion(&self, thread: &str);
    pub fn unseal_after_rollback(&self, thread: &str);
    pub fn is_sealed(&self, thread: &str) -> bool;
    pub fn is_ready(&self) -> std::result::Result<(), String>;
    pub fn codex(&self) -> Option<&Arc<SharedCodexAppServer>>;     // the explicit Codex-only door
}
```

`PlannerHarnessParams.daemon` becomes `backend: PlannerBackend`; `impl From<Arc<SharedCodexAppServer>>`
keeps the ~70 test sites a mechanical `daemon.into()` edit. Recovery fns take a
`PlannerBackendFactory { codex: Arc<SharedCodexAppServer>, claude: ClaudePlannerConfig }` and pick the
variant from the runtime row (§4.3).

### 4.2 Notification stream: synthesize Codex shape (D2)

Options: (a) keep `codex_appserver::Notification` and let the Claude backend synthesize Codex-shaped
frames; (b) a small neutral event enum consumed by `on_notification`.

(a) is chosen. `on_notification` is ~400 lines of fenced state transitions (H5–H9) whose tests and
invariants are expressed in Codex frames; (b) would rewrite it (explicit non-goal) and every FE
converter reads Codex item shapes (M10). (a) is *honest* because: the stored shape was always "the
item the transcript renders", not a raw provider log — Codex rows are already filtered (H9) and
projection rows are kernel-written in the same shape (H8); the provider of every row is recoverable
from its `worker_session_id` → `worker_sessions.provider`; and the raw Claude record stays in Claude's
own session JSONL under the dedicated config dir. A side effect that matters for the gates: the Claude
backend never writes transcript rows itself, so it adds no call to the persistence fn and no ratchet
movement.

The Claude backend owns one `broadcast::Sender<Notification>` per session. It only emits: `TurnStarted`,
`TurnCompleted`, `Item{"item/started"|"item/completed"}`, `Other{"thread/tokenUsage/updated"}`. It
never emits `ThreadStatusChanged`, `ThreadStarted`, `turn/aborted`, approvals or plans.

### 4.3 Choosing the backend per card (D3)

- New server-owned card payload key `planner_provider: "codex" | "claude"`, typed as
  `enum PlannerProvider { Codex, Claude }` (serde, deny unknown). Required on every `role='planner'`
  card: a new migration backfills `json_set(payload,'$.planner_provider','codex')` for the 24 Planner
  cards (Q7/Q11); track create always writes it. Missing or unknown ⇒ `from_shape` returns `None`
  (fail closed, same as today's non-codex kind), never "assume codex".
- Added to `SERVER_OWNED_CARD_PAYLOAD_KEYS` (`crates/calm-truth/src/validation.rs`) so a client cannot
  write it and `card_update_tx` keeps it sticky.
- `card.kind` stays `"codex"` for Claude Planners. Rejected alternative: `kind = "claude"`. It would make
  the card look like a Claude PTY worker to `claude_restart_adapter` (S16), the Claude hook/card routes,
  `worker_flow`, `track_fs_view`, and the FE Claude PTY builtin, while all five FE Planner predicates
  (S17) and `ratify_card` (S2) would stop recognising it — about fifteen security-relevant edits for no
  user-visible gain. For Planner cards `kind` is already a view marker (the Planner surface is selected
  by `planner_harness`, not by kind). KNOWN GAP: `kind: "codex"` on a Claude Planner is a legacy view
  name; renaming it is a separate cleanup.
- `profile.rs` change: `from_shape(kind, role, payload)` keeps `kind == "codex"`; for `CardRole::Planner`
  it additionally requires `PlannerProvider::from_payload(payload)` to parse. PlainChat/Assistant keep
  today's rule and are always Codex in v1. A second fn `HarnessProfile::provider(payload) ->
  Result<PlannerProvider>` feeds the factory.
- Runtime row mirrors the choice: start adapter mints `AgentProvider::Claude`, `worker_sessions.provider
  ='claude'`, `contract='planner'`, `mode='resumable'`. `runtime_kind_from_session_identity` gains
  `(Claude, Planner) → SharedPlanner` (S13); `derive_session_identity` is unchanged (it is provisional
  and overwritten at mint, S12). Boot recovery SQL gains one arm
  `OR (ws.provider='claude' AND ws.contract='planner' AND c.role='planner')` and its pinning test gains the
  literal. Recovery asserts `ws.provider` agrees with the card key; disagreement ⇒ skip + warn.
- Boot: `recover_harnesses_after_daemon_boot` (H21) recovers Claude rows in both arms; the deferred
  (Codex-readiness) pass and its `SkipIfClaimed` generation check apply to Codex rows only. Otherwise a
  Codex daemon outage at boot would silently park every Claude Planner.
- `CreateTrackRequest` gains **required** `planner_provider`. Every caller is swept: FE new-track and
  first-message flows, `e2e/`, Rust tests posting tracks, recipe/template instantiation, fork
  (inherits the source Planner's key), and child tracks opened by a Planner (inherit the parent's key).

## 5. The Claude backend

New module `claude_planner/` (each file ≤ 800 lines): `session.rs` (process lifecycle, turn slot),
`protocol.rs` (wire types), `translate.rs` (records → notifications), `config.rs`.

### 5.1 Process model (D4)

**One `claude -p` process per turn.** `turn_start` spawns the process, writes exactly one `user` line,
and the process lives until that turn's `result` (or kill); then stdin is closed and it exits
(clean exit verified `[P-B]`, `[P-E]`). Resume is by Claude's own session store:

- first turn of a runtime: `--session-id <thread_id>` (the harness thread id is a fresh UUID minted by
  the start adapter);
- later turns: `--resume <thread_id>` once `worker_sessions.agent_session_id` has been set. The backend
  sets `agent_session_id = thread_id` when it first observes `system/init` for that session (the CLI has
  accepted the conversation). This is the column's existing meaning for Claude worker cards (the
  resumable Claude conversation id: projection `session_id` ← `agent_session_id`,
  `crates/calm-truth/src/session_projection_row.rs:123`, read by `resolve_claude_session_for_card`,
  `crates/calm-truth/src/session_projection_lookup.rs:58-69`, used at
  `operation/claude_restart_adapter.rs:139`).

Why not resident per harness, as the issue proposed: (1) resume after kill is verified `[P-F2]`, so a
process between turns buys nothing; (2) a resident process per Planner costs one Node process per idle
Planner (24 Planner cards on 4140), which needs an idle reaper and a crash supervisor — two mechanisms
the per-turn model does not need; (3) model and effort become per-spawn flags, which matches Codex's
per-`turn/start` selection exactly (§5.7); (4) interrupt and (later) steer only need the process during
the turn, when it exists anyway. Cost: startup (~0.5–1.5 s to `system/init` in `[P-B]`/`[P-E]`) plus
resume load time per turn; the prompt cache is server-side (1 h ephemeral writes seen in
`message_start.usage`, `[P-B]`), so it survives respawns. Owner question Q1.

`ClaudePlannerSession` (one per harness) holds: `thread_id`, `cwd`, `sealed: AtomicBool`,
`notifications: broadcast::Sender`, `live: Mutex<Option<LiveTurn>>` where
`LiveTurn { turn_id, child: tokio::process::Child (kill_on_drop), stdin, interrupt_requested: bool,
started_at, client_id }`, and a reader task per process. `kill_on_drop` covers graceful shutdown. For a
neige crash the backend writes `<runtime_dir>/claude.pid` (pid + `/proc/<pid>/stat` start time) at spawn;
before any spawn for that runtime it kills a recorded process that is still alive **and** whose cmdline
names this session id (pid-reuse safe), the same "prove the old process gone before spawning" rule the
Codex daemon applies (`shared_codex_appserver.rs:498-501`). `PR_SET_PDEATHSIG` is not used: it fires when
the spawning *thread* exits, and tokio worker threads come and go. Without the check, an orphan would
finish its turn against the same session while a resumed process starts another.

**Neige restart mid-turn.** The child dies with neige. On boot the runtime restores as `Resumed` → `Idle`
(H19). Recovery records one interrupted outcome for the snapshot's `last_turn_id` when the snapshot
phase was `TurnRunning`/`IssuingTurn`/`IssuingInterrupt` (via `turn_outcome::record`, whose stated
purpose already includes history recovery, `harness/turn_outcome.rs:1`), with
`error.message = "neige restarted while this turn was running"`. The next turn resumes the session;
Claude shows the killed turn as interrupted `[P-F2]`.

### 5.2 Spawn contract

```
<claude_bin> -p --input-format stream-json --output-format stream-json --verbose
  --replay-user-messages
  (--session-id <thread> | --resume <thread>)
  [--model <m>] [--effort <e>]
  --setting-sources project --disable-slash-commands
  --tools Bash,Read,Edit,Write,ToolSearch,WebFetch,WebSearch
  --strict-mcp-config --mcp-config <runtime_dir>/mcp.json
  --permission-mode default --permission-prompts none
  --allowedTools "Bash Read ToolSearch WebFetch WebSearch mcp__calm Edit(//<cwd>/**) Write(//<cwd>/**)"
  --append-system-prompt-file <runtime_dir>/instructions.md
```

(`--append-system-prompt-file` and `--permission-prompts` are named in `claude --help` of 2.1.280;
the former only inside the `--bare` description.)

- `cwd` = the track workspace path (same value `thread/start` receives today, S5). Claude keys its
  session store by cwd, so a moved workspace cannot resume; that surfaces as a failed turn with Claude's
  own "No conversation found" text (KNOWN GAP, same class as #857).
- `<runtime_dir>` = `<data_dir>/claude-planner/runtimes/<worker_session_id>/`, mode 0700; `mcp.json`
  and `instructions.md` are 0600 and removed when the runtime shuts down. The token never appears on
  argv (argv is world-readable via `/proc`).
- `instructions.md` is the same developer-instruction text the start adapter renders for Codex. Claude
  records the system prompt at the first request and reuses it on resume (`--system-prompt-snapshot on`
  is the default, `claude --help`), which is the same "fixed at thread start" semantics Codex has.
- `mcp.json`: `{"mcpServers":{"calm":{"type":"stdio","command":"<neige-mcp-stdio-shim>","args":[],
  "env":{"NEIGE_MCP_SOCKET":"…","NEIGE_MCP_TOKEN":"<raw card token>"}}}}`. Verified that config `env`
  reaches the server and that the server receives the original dotted tool name `[P-B]`.
- `--setting-sources project` keeps project `CLAUDE.md` and project settings (verified: marker read
  `[P-B]`), matching Codex reading the workspace `AGENTS.md`. `--disable-slash-commands` empties the
  built-in skill list (`skills: []`, `[P-E]`). `--tools` removes Claude-native scheduling/agent tools
  (`Task`, `Cron*`, `ScheduleWakeup`, `RemoteTrigger`, `Workflow`, `EnterWorktree`, …) that would bypass
  the kernel `[P-E]`.

**Environment (explicit allowlist, built with `Command::env_clear()`):**

| Var | Value | Why |
|---|---|---|
| `HOME`, `USER`, `LOGNAME`, `LANG` | from the server process | CLI basics |
| `PATH` | `kernel_bin_path::kernel_led_path()` (`kernel_bin_path.rs:42`) | `neige` CLI in Bash resolves to the running kernel (same as M2) |
| `HTTP(S)_PROXY`, `NO_PROXY` (both cases) | `SharedCodexAppServer::resolved_proxy_env_pairs` source (`shared_codex_appserver.rs:1862`) | same proxy policy as the Codex daemon |
| `CLAUDE_CONFIG_DIR` | typed config `--claude-planner-config-dir` (default `<data_dir>/claude-planner/config`) | isolation (§8) |
| `NEIGE_MCP_SOCKET`, `NEIGE_MCP_TOKEN` | per runtime | `neige` CLI inside Bash (same as M2) |
| `DISABLE_AUTOUPDATER` | `1` | the planner process never rewrites the binary it was checked against |

Never passed: `NEIGE_MCP_DAEMON_TOKEN` (the shim would prefer it, M4), `ANTHROPIC_*`,
`CLAUDE_CODE_*`, anything else from the server env. The token is minted once per harness construction
(`mint_and_persist_card_token`, M3) and kept in memory, exactly as the Codex path keeps it in the thread
config.

**Readiness / version.** `is_ready` = `claude_bin` resolves to an executable (checked at boot and on
`ensure_live_planner_harness`). Each spawn reads `system/init` and requires: `claude_code_version` ≥
`2.1.280` (warn on a different minor), `capabilities ⊇ {interrupt_receipt_v1}`,
`mcp_servers` contains `calm` with `status:"connected"`, `skills == []`, every `plugins[].source` ends
in `@builtin`. A failed check interrupts the turn and completes it as `failed` with the reason (§6.2).
No `initialize` control request is sent in v1 (not needed; one fewer undocumented message).

### 5.3 Protocol types (`protocol.rs`)

Only what v1 reads or writes; unknown `type`/`subtype` values deserialize into an explicit
`Ignored { kind: String }` arm and are logged at debug — never an error, never guessed.

```rust
// stdin
struct UserLine { r#type: Const<"user">, message: UserContent, parent_tool_use_id: Null,
                  session_id: String, uuid: Uuid }                       // uuid required
struct InterruptLine { r#type: Const<"control_request">, request_id: String,
                       request: Const<{"subtype":"interrupt"}> }
// stdout
enum Record {
  SystemInit { session_id: String, claude_code_version: String, capabilities: Vec<String>,
               mcp_servers: Vec<McpStatus>, skills: Vec<String>, plugins: Vec<PluginRef> },
  UserReplay { uuid: Uuid, is_replay: True, message: UserContent },       // our line echoed
  User { uuid: Uuid, message: UserBlocks },                              // tool_result blocks
  Assistant { uuid: Uuid, message: AssistantBlocks },                    // thinking|text|tool_use
  Result { subtype: ResultSubtype, is_error: bool, result: Option<String>,
           usage: Usage, model_usage: BTreeMap<String, ModelUsage>, terminal_reason: String },
  ControlResponse { request_id: String, subtype: String },
  ControlRequest { request_id: String, subtype: String },                // answered with an error
  Ignored { kind: String },                                              // stream_event, rate_limit_event, system/status, …
}
```

`Option` appears only where the wire genuinely omits the value (`result` is `null` on
`error_during_execution`, `[P-F3]`). The client-id mapping: Codex-style ids are 32 hex chars
(transcript sample, §3 Q1 rows); the `uuid` field is written as the dashed UUID of the same 128 bits
and mapped back when echoing (UNVERIFIED that the CLI rejects undashed ids; the bijection makes the
question moot).

### 5.4 MCP wiring and tool names

The shim, socket, card-bound token and role resolution are reused unchanged (M3–M5). Claude exposes
`calm.report.write` as `mcp__calm__calm_report_write` and `plugin.dev-neige-market_market.quote` as
`mcp__calm__plugin_dev-neige-market_market_quote`: every char outside `[A-Za-z0-9_-]` becomes `_`
(documented, verified `[P-B]`). The server receives the original name `[P-B]`.

For the transcript, `translate.rs` restores the dotted name: at each spawn it takes the card's visible
tool names (the list `tools/list` answers for this connection, `mcp_server/transport.rs:338`), builds
`claude_sanitized(name) → name`, and maps `tool_use.name` minus `mcp__calm__`. Two registry names that
sanitize alike (only possible if Claude itself cannot tell them apart) or an unknown name ⇒ the row keeps
the Claude name and a warn is logged; the FE then shows "Called mcp__calm__…" (visible, not silent).
`model_tool_key` (M7) already accepts Claude spellings wherever the Planner echoes a tool name back.

### 5.5 Approvals (D5)

Codex Planners run `approval_policy = "never"`, `workspace-write` sandbox with network, and the three
terminal tools pre-approved (S5, M2). The Claude equivalent without any responder:
`--permission-prompts none` (anything that would prompt is denied automatically) plus the allow rules in
§5.2. Verified with `--allowedTools "Bash Read Edit Write ToolSearch mcp__calm"`: MCP call, Bash write and
`git push` all ran with **no `control_request`** `[P-H]`; a bare `Write` rule also allowed a write
*outside* the cwd `[P-H]`, hence the path-scoped `Edit(//<cwd>/**)`/`Write(//<cwd>/**)` rules (the
`//` absolute-path rule syntax is documented; the denial of an out-of-cwd write under it is UNVERIFIED —
live acceptance). A denied call ends up as a failed `tool_result` and `result.permission_denials` (issue probe
`probe2.py` T2, not re-run here), which the transcript shows as a failed activity line.

Any `control_request` the CLI still sends (e.g. `can_use_tool`) is answered
`{"behavior":"deny","message":"neige planner: prompts are disabled"}` and logged — the turn never hangs.

KNOWN GAP (owner question Q3): Bash is not OS-sandboxed. Codex's planner shell is confined to the
workspace; Claude's `Bash` rule is not. Claude Code's own sandbox setting was not probed.

### 5.6 Recover (D7)

`semantic_recovery::register` is only called from the Codex `thread/start` path (M9); a Claude runtime
never registers, so `registered(card, thread)` is `false` (H17) and every recovery briefing carries the
exact-interface decision text naming `calm.plan.recover`. No new mechanism. KNOWN GAP: Claude Planners
do not get the bound `Recover(key, reason)` shortcut; they use `calm.plan.recover` with explicit
`expected_attempt_id`/`idempotency_key`, which the briefing already supplies.

### 5.7 Model and effort

- Per-turn, per-spawn: `TurnModelSelection{model, effort}` → `--model`, `--effort` when `Some`; omitted
  means Claude's own default for that spawn. Because every spawn is fresh there is no sticky value to
  undo, so `resolve_model_selection` takes a Claude arm that uses only the card's explicit values and
  never calls `config_read`/`model_list` (H16). Invalid effort for Claude (Claude accepts
  `low|medium|high|xhigh|max`, `[P-A]` `models[].supportedEffortLevels`) ⇒ `needs_a_choice` refusal with
  a reader text, same path as today.
- Catalog: `routes/planner_model.rs` answers Claude cards with `409 "model choice for Claude Planners is
  not available yet"` until PR6; PR6 fills it from a zero-token `initialize` round trip
  (`control_request{subtype:"initialize"}` returns `models[{value, supportedEffortLevels}]`, `[P-A]`).

### 5.8 Steer (D8)

`handle_steer` checks `backend.supports_steer()` **before** taking the entry: for Claude it answers
`SteerRefused::NotTaken { message: "this Planner's provider (Claude) does not take messages into a
running turn; it stays queued for the next turn" }` with the entry untouched (id and rev unchanged).

Recorded design for a later slice (cut from v1, §9.4): write the queued entry as a second `user` line
with its own `uuid` into the live process; `command_lifecycle{command_uuid, state}` reports `queued →
started → completed` `[P-E]`. `started` before the running turn's `result` ⇒ taken into that turn (one
`result`, verified during a tool batch and while an approval was pending `[P-E]`); otherwise it would
become a new turn — so the steer must be cancelled or the running turn interrupted with
`still_queued` handling (`[P-F3]`: an interrupt does **not** drop queued lines; they run next). This is
the complexity the cut avoids.

### 5.9 Token usage

On each `result`, the translator emits `Other{"thread/tokenUsage/updated", params}` with
`tokenUsage.last.totalTokens = input + cache_read + cache_creation + output` of the last entry of
`result.usage.iterations`, `tokenUsage.total.totalTokens` = running sum over the session's results seen
by this backend instance, and `modelContextWindow = result.modelUsage[<model>].contextWindow`
(200000 for haiku, `[P-B]`). Parsed by the existing `TokenUsage::from_params` (H11). Usage updates once
per turn, not per request (KNOWN GAP).

## 6. Mapping

### 6.1 Claude record → notification → stored row

Envelope for every item: `{threadId: <thread>, turnId: <turn>, item: {…}, startedAtMs | completedAtMs}`
(`completedAtMs` is read by the FE, M10). Item ids: `tool_use.id` for tools, the record `uuid` for
text/thinking.

| Claude record (verified in) | Notification | Stored `item` (fields the FE reads) |
|---|---|---|
| our `user` line written | `TurnStarted{turn:{id}}` (emitted right after the write; handled after `turn_start` returns, H4/H6) | — |
| `user` with `isReplay:true`, `uuid` = ours `[P-B]` | `item/completed` `userMessage` | `{id: client_id, clientId: client_id, type:"userMessage", content:[{type:"text",text}]}` → upgrades the drain projection (H8) |
| `user` replay whose text is `<local-command-stdout>…` `[P-B]` | none | — |
| `assistant` `thinking` block | `item/started` + `item/completed` `reasoning` | `{id, type:"reasoning", content:[], summary:[]}` (no text, like Codex rows, §3 Q1 sample) |
| `assistant` `text` block | `item/started` + `item/completed` `agentMessage` | `{id, type:"agentMessage", text}` |
| `assistant` `tool_use` `mcp__calm__*` | `item/started` `mcpToolCall` | `{id, type:"mcpToolCall", server:"calm", tool:<dotted>, arguments:<input>, status:"inProgress"}` |
| `user` `tool_result` for it | `item/completed` `mcpToolCall` | + `status:"completed"` or `"failed"`, `result:{content}` or `error:{message}`, `durationMs` |
| `assistant` `tool_use` `Bash` | `item/started` `commandExecution` | `{id, type:"commandExecution", command, cwd, status:"inProgress"}` |
| `user` `tool_result` for it (`tool_use_result.{stdout,stderr}` or error text `"Exit code 3\n…"`, `[P-E]`) | `item/completed` `commandExecution` | `status`, `aggregatedOutput`, `exitCode: 0` when `is_error:false`, `null` when failed (the exit code is only in prose; not parsed), `durationMs` |
| `tool_use` `Edit`/`Write` + result (`structuredPatch`, `type:"create"`, `[P-E]`) | `item/started`/`item/completed` `fileChange` | `{id, type:"fileChange", changes:[{path, kind:{type:<"add" or "update">}, diff}], status}` |
| `tool_use` `Read`/`WebFetch`/`WebSearch` + result | `item/started`/`item/completed` `dynamicToolCall` | `{id, type:"dynamicToolCall", tool:<Claude name>, arguments, status}` (FE: "Called tool"; Q5: no legacy rows) |
| `tool_use` `ToolSearch` | none | MCP schema loading is plumbing (`[P-B]`, `[P-H]`) |
| `user` text `[Request interrupted by user…]` `[P-F1]`, `[P-F3]` | none | the outcome row says it |
| `system/init` | none (checks §5.2; sets `agent_session_id` once) | — |
| `result` | `Other{thread/tokenUsage/updated}`, then `TurnCompleted` (§6.2) | outcome row (H10) |
| `stream_event`, `rate_limit_event`, `system/status`, `system/thinking_tokens`, `command_lifecycle`, `system/vcs_state_changed` | none | — (`--include-partial-messages` is not passed; the FE never rendered deltas) |

Not produced: `turn/plan/updated` (Claude's todo tools are excluded by `--tools`; nothing reads plans
today, `harness/run_loop.rs:2100-2101`), `webSearch`, `imageView`, `contextCompaction` (KNOWN GAPS).

### 6.2 Turn outcome: producer × state matrix

Every live turn has one `TurnSlot` that emits **exactly one** `TurnCompleted`; the first terminal
producer wins, later ones are dropped and logged. `status` values are the three the FE draws (M10/Q3).

| Producer | Condition | `turn.status` | `turn.error.message` |
|---|---|---|---|
| `result` | `subtype=success`, `is_error=false` | `completed` | — |
| `result` | `subtype=success`, `is_error=true` (e.g. "Not logged in · Please run /login", `terminal_reason:"api_error"`, `[P-D]`, `[P-G]`) | `failed` | `result.result` |
| `result` | `subtype=error_during_execution` and `interrupt_requested` for this turn `[P-F3]` | `interrupted` | — |
| `result` | `subtype=error_during_execution`, no interrupt requested | `failed` | `terminal_reason` + `errors` |
| `result` | any other `error_*` subtype (`error_max_turns`, `error_max_budget_usd`, …; not configured by us) | `failed` | subtype |
| init check | §5.2 check fails | `failed` | the failed check (after an interrupt + kill) |
| process exit | stdout EOF before `result`, no interrupt requested (crash, OOM, SIGINT `[P-F1]`) | `failed` | `"claude exited (<status>)"` + last stderr line |
| process exit | stdout EOF before `result`, interrupt requested or harness shutting down | `interrupted` | — |
| kill timer | interrupt sent, no `result` within 10 s (< the harness 30 s budget, H13/H14) → SIGKILL | `interrupted` | — |
| recovery | neige restarted with the snapshot in a running phase (§5.1) | `interrupted` | "neige restarted while this turn was running" |
| spawn/write failure | before any line was accepted | **no turn**: `turn_start` returns `Err` → existing retryable refusal, batch re-buffered, paced 2 s, reader told after 30 s (`harness/run_loop.rs:2523-2562`) | — |

Consequences for harness states: an interrupted Claude turn reaches `on_notification` as a
`TurnCompleted` for the interrupt target (H7), so `Issuing{Interrupt}` settles well inside the 30 s
watchdog and `Wedged("interrupt_timeout")` becomes unreachable for Claude except if the kill itself
hangs. Claude never produces `ThreadStatusChanged`, so `Wedged(systemError)` and the systemError
recovery route (S9, member 14) do not apply. `CodexRefused` is never produced by the Claude backend, so
`classify_codex_failure` sees only retryable errors from it (see #1542, §9.3).

## 7. Oracle trace

`NEW` = introduced by this design. File:line refers to the carrier on `c5abb3c3d`.

| seq | phase | actor | trigger | external effect | observable event (kind, where) | invariant | status |
|---|---|---|---|---|---|---|---|
| 1 | create | user | `POST /api/tracks {…, planner_provider:"claude"}` | Planner card `kind:"codex"`, payload `planner_provider:"claude"` | `card.added` via create tx (`routes/tracks.rs:1423`) | key required, server-owned | NEW field |
| 2 | start | kernel | `planner-harness-start` op | runtime row `provider='claude', contract='planner'`, fresh UUID thread id, token minted, no process yet | op phases (`operation/planner_harness_start_adapter.rs:840`) | `(claude,planner)` maps to the shared-Planner kind | NEW branch |
| 3 | first message | user | `POST /api/cards/{id}/planner/input` | queue entry persisted before 200 | `harness.user_message.enqueued` (`routes/cards.rs:921`) | durable before ack (`harness/run_loop.rs:482-489`) | existing |
| 4 | drain | run loop | tick | projection row written, then `turn_start` → spawn `claude --session-id <thread>` + one `user` line | `harness.item.added` for the projection (`harness/run_loop.rs:2271-2290`) | projection before provider call | existing + NEW spawn |
| 5 | turn begins | backend | line written | — | `TurnStarted` → phase `TurnRunning` (H6) | issued id == turn id | NEW producer |
| 6 | echo | CLI | `isReplay` user record | projection upgraded in place | `harness.item.added` (`harness/run_loop.rs:2022-2046`) | no duplicate user row | NEW producer |
| 7 | init | CLI | `system/init` | `agent_session_id := thread` | none | checks §5.2 | NEW |
| 8 | tool call | model | `tool_use mcp__calm__calm_report_commit` | MCP call via shim, card-bound identity | `item/started` row; kernel's own write events | allowed by `mcp__calm` rule, no prompt `[P-H]` | NEW producer |
| 9 | tool result | CLI | `tool_result` | — | `item/completed` row, dotted tool name | exactly one completed per started | NEW producer |
| 10 | steer | user | `POST …/planner/input/{entry}/steer` | none; entry stays queued | `409` with `NotTaken` message | entry id/rev unchanged | NEW refusal |
| 11 | interrupt | user | `POST /api/cards/{id}/planner/interrupt` | `control_request{interrupt}` on stdin | phase `IssuingInterrupt` (H13) | one outcome per turn | existing path + NEW producer |
| 12 | interrupted | CLI | `result error_during_execution` | process exits after stdin close | outcome row `interrupted` (H7, H10) | settles < 10 s, never `Wedged` | NEW producer |
| 13 | next turn | run loop | new observation | spawn `claude --resume <thread>` | `TurnStarted` … `completed` | context retained `[P-F2]` | NEW |
| 14 | server restart mid-turn | ops | neige stops | graceful: `kill_on_drop`; crash: the orphan finishes or is killed by the pidfile check before the next spawn (§5.1) | none | never two processes on one session | NEW |
| 15 | boot | kernel | `recover_harnesses_after_daemon_boot` (`lib.rs:620`) | harness rebuilt from the Claude arm of the boot SQL, independent of Codex daemon state; interrupted outcome for the lost turn | outcome row `interrupted` (§5.1) | lost turn is never shown as running forever at turn level | NEW |
| 16 | resume | run loop | queued entry | `claude --resume <thread>` | normal turn | Claude reports the killed turn as interrupted `[P-F2]` | NEW |

## 8. Auth & isolation (D6), compliance

Evidence:

| Option | Isolation evidence | Auth | Verdict |
|---|---|---|---|
| A. dedicated `CLAUDE_CONFIG_DIR` + the user's own login inside it | empty dir: no user plugins, only built-in skills (removed by `--disable-slash-commands`), MCP from `--mcp-config` connected `[P-D]` | user runs `CLAUDE_CONFIG_DIR=<dir> claude` then `/login` themselves; not logged in ⇒ every turn fails with the CLI's own "Not logged in · Please run /login" `[P-D]` | **recommended** |
| B. `--bare` + API key (`apiKeyHelper` via `--settings`, or `ANTHROPIC_API_KEY`) | hooks, plugins, CLAUDE.md auto-discovery, OAuth, keychain skipped (`claude --help`); no key ⇒ same "Not logged in" `[P-G]` | API billing; neige must be given a key helper command (typed config) | compliant; loses project CLAUDE.md; available later as a config variant |
| C. `--safe-mode` with `~/.claude` | **disables `--mcp-config` servers** (`mcp_servers: []`) and still lists user plugins `[P-C]` | normal | rejected: no `calm` tools |
| D. user's `~/.claude` + `--setting-sources project` + `--disable-slash-commands` | user skills (`~/.claude/skills/*`) and plugins not loaded; only built-ins `[P-B]`, `[P-E]` | the user's existing login | works, but Planner sessions land in the user's own session list and state, and isolation rests on flag semantics alone |

The issue's observation that `--setting-sources project` still lists user commands was a misreading:
`deep-research`, `design`, … are `builtin: true` in the initialize response `[P-A]`; no entry from
`~/.claude/skills` appeared in any variant.

Recommendation A: separate session store (Planner sessions never appear in the user's `claude --resume`
list and are not affected by the user's own housekeeping), no dependency on the user's personal
settings, and the same startup checks (§5.2: `skills == []`, `@builtin` plugins only, `calm` connected)
turn any future leak into a failed turn instead of a silent behaviour change. Setup is one documented
command. The directory is created 0700 by neige; neige never reads, copies, or relays anything in it.

Compliance: neige ships no login UI, never collects, stores, copies or relays OAuth credentials or
session tokens, and runs the unmodified `claude` binary as a subprocess (the documented integration for
non-Python/TS hosts). Option B (API key) is always compliant. The `initialize` response carries the
account email (`account.email`, `[P-A]`); neige neither requests `initialize` in v1 nor would it
deserialize that field (PR6 reads `models` only).

## 9. Delivery

### 9.1 Slices PR1–PR6 (ordered; each ≲ 1k production lines)

| Slice | Content | Acceptance | Must-red tests (mutation-verified) |
|---|---|---|---|
| **PR1 Seam (pure refactor)** | `harness/backend.rs` enum (nine members + `supports_steer` + `codex()`); `PlannerHarnessParams.backend`; recovery fns take `PlannerBackendFactory`; start/shutdown adapter and card routes call through the enum; `harness_turn_start_invariant` allowlist gains `harness/backend.rs` | Codex behaviour identical: targeted nextest of `planner_harness_*`, `planner_preserving_recovery*`, `planner_card_interrupt`, `harness_turn_start_invariant`; `local-rust-gates.sh --quick` | invariant still red for a second `.turn_start(` in run_loop; a test that a `Claude` variant stub's notifications reach `on_notification` (proves the loop subscribes via the enum) |
| **PR2 Claude session core** | `claude_planner/{protocol,translate,session,config}.rs`; spawn contract §5.2; env allowlist; per-turn process; `TurnSlot`; mapping §6.1; outcomes §6.2; tokenUsage; tool-name restore; control_request deny; typed config `--claude-planner-config-dir` | unit tests feed recorded NDJSON fixtures (the Appendix probes, redacted) through `translate`; a fake `claude` script (bash) driven through the real `ClaudePlannerSession` for spawn/kill/exit paths; env of the child asserted to be exactly the allowlist | one-outcome-per-turn (duplicate `result` + EOF ⇒ one `TurnCompleted`); interrupt ⇒ `interrupted` not `failed`; `is_error:true` success ⇒ `failed`; env contains no `NEIGE_MCP_DAEMON_TOKEN`; dotted tool name restored |
| **PR3 Provider identity (data + API)** | `planner_provider` key + backfill migration (next free number at merge; #1785's design claims 0116); server-owned key; `profile.rs` gate; `CreateTrackRequest.planner_provider` (required) + OpenAPI + `gen:api` + caller sweep (FE create calls pass `"codex"` until PR5); fork/child-track inheritance; `(claude,planner)` projection mapping; boot SQL arm + its pinning test | migration applied to a copy of the live DB (`sqlite3 … .backup` into scratch, never the live file): 24 Planner cards carry `codex`; `local-rust-gates.sh --quick` (OpenAPI drift) | missing `planner_provider` ⇒ not a harness card; unknown value ⇒ not a harness card; `(claude,planner)` row maps to the shared-Planner kind with `agent_provider=claude` |
| **PR4 Claude Planner wired end to end** | start adapter Claude branch (fresh UUID thread, no thread RPC, `AgentProvider::Claude`, runtime dir); recovery picks the variant from the row and asserts it matches the card; lost-turn outcome on boot; `resolve_model_selection` Claude arm; model catalog 409 for Claude; steer pre-check; card-teardown route branch; boot recovery partition (H21) | fake-`claude` stack test through the real routes: create Claude track → message → MCP call → interrupt → server restart → resume → next turn; transcript rows match §6.1 | card/row provider mismatch ⇒ skipped; steer leaves entry id/rev unchanged; restart mid-turn ⇒ exactly one `interrupted` outcome; a live orphan from the pidfile is killed before the next spawn; Codex daemon failing at boot still recovers the Claude Planner |
| **PR5 FE: choose the provider** | provider choice in the new-track and first-message flows (`fe/web/src/features/area/new-track/`); Planner model picker hides on 409 | `cd fe && npm run lint && npm run build && npm test`; browser test creating a Claude track against the fake stack; real-browser preview | FE sends `planner_provider` on every create path (test fails if omitted) |
| **PR6 Prompt wording + Claude model catalog** | provider-neutral wording at `planner.md:67`, `assistant/mechanics.md:14`, `tools/calm.task.dispatch.md`, `tools/calm.source.capture.md`; goldens regenerated; catalog from `initialize.models`; effort validation | goldens; catalog test with a recorded `initialize` fixture (email field absent from the Rust type) | Claude effort outside `supportedEffortLevels` ⇒ `needs_a_choice` |
| **Live acceptance (no slice)** | on a dedicated host / owner's box, never via Codex E2E: one real Claude Planner track doing a report write + a worker dispatch + interrupt + neige restart + resume | transcript renders; outcomes correct; `ps` shows no Planner `claude` between turns | — |

PR1, PR2 and PR3 are independent and can proceed in parallel worktrees; PR4 needs all three; PR5 needs
PR3's API; PR6 is last because prompt goldens are the most conflict-prone files (§9.3). The pain point is
solved after PR4 + PR5 (PR6 is polish and may be cut).

### 9.2 KNOWN GAPS (one line each)

- Bash in a Claude Planner is not OS-sandboxed (Codex's is workspace-write) — Q3.
- No steer for Claude Planners in v1 (§5.8).
- No bound `Recover` tool; the exact `calm.plan.recover` interface is used (§5.6).
- A turn in flight during a neige restart is lost and recorded as interrupted; its started tool lines
  stay "running" in the FE (the outcome line is correct).
- Token usage updates once per turn; rate-limit events are dropped.
- No `turn/plan/updated`, `webSearch`, `imageView`, `contextCompaction` rows from Claude.
- A moved workspace cannot resume the Claude session (#857 class); reset the Planner to start fresh.
- Per-turn startup + resume load latency (unmeasured on long sessions; measured in live acceptance).
- The Planner card keeps `kind: "codex"` as a legacy view name.
- The out-of-cwd denial of `Edit(//cwd/**)` rules is UNVERIFIED until the live acceptance run.
- A first-turn process that dies after Claude created the session but before `system/init` reached neige
  leaves `agent_session_id` unset; the next `--session-id` spawn may be refused (UNVERIFIED) and the
  Planner must be reset.

### 9.3 Risks and conflicts

- **Undocumented protocol drift.** Input `user` lines and `control_request` shapes are Agent-SDK
  internal (reference: MIT `anthropics/claude-agent-sdk-python` `_internal/query.py`). Mitigation: v1
  uses only `user` + `interrupt`; the `system/init` version/capability check; `DISABLE_AUTOUPDATER` in
  the child; recorded NDJSON fixtures from the pinned version in the test suite.
- **#1542** (codex classifier chokepoint): PR1 adds no call site in `maybe_issue_turn`; whichever of
  #1542's two fixes lands first, PR1 rebases trivially. The Claude backend never returns `CodexRefused`.
- **#1255** (steer): already implemented for Codex; D8 only adds a pre-check.
- **#1727** (S1 wake predicates, S2 `report.read`, S3 recovery guidance) and **#1785**
  (`calm.task.replace`, settlement wake reasons, `HarnessObservation` fields in the snapshot): they touch
  `dispatcher/`, `harness/snapshot.rs`, `observation.rs`, MCP tools and `prompts/planner.md`; this
  design touches `run_loop.rs` only at the backend call sites and the model-resolution arm. Textual
  conflicts expected in `planner.md` + goldens (hence PR6 last) and in migration numbering (take the
  next free number at merge, per the merge rules).
- **#857**: same failure class for Planner sessions if the workspace moves.

### 9.4 Cuts (recorded, not built)

- Resident process per harness (the issue's proposal) — replaced by per-turn processes (D4, Q1).
- `can_use_tool` responder / interactive approvals — static rules suffice (D5).
- Steer via `command_lifecycle` (§5.8).
- Claude for PlainChat and Assistant profiles.
- Claude `Recover` via an SDK-served MCP tool (`mcp_message` control channel).
- Streaming deltas, per-request token usage, rate-limit surfacing, todo → plan mapping.
- Renaming the Planner card `kind`; a provider-neutral item model.

### 9.5 Open questions that are the owner's

- **Q1** Per-turn process (recommended, §5.1) versus the issue's resident-per-harness process.
- **Q2** Auth/isolation: dedicated `CLAUDE_CONFIG_DIR` with the owner's own `/login` (recommended) versus
  the owner's normal `~/.claude` (option D, zero setup) versus API key (option B, separate billing).
- **Q3** Is an unsandboxed Planner `Bash` acceptable for v1, or must sandbox parity with Codex land first?

## Appendix A — probes (claude 2.1.280, model `claude-haiku-4-5`)

Scripts and raw NDJSON: `/tmp/claude-1000/-mnt-data2-kenji-neige-calm/852c3533-7ab9-4c83-ab0e-b2af4bcdbf0e/scratchpad/cc_probe2/`
(session-scoped scratch; the issue's own probes are in the sibling `cc_probe_ref/`; neither is preserved
beyond this session). `h.py` is the harness, `fake_mcp.py` exposes `calm.report.write` and `plugin.dev-neige-market_market.quote`.
Every child was started with `env` limited to `HOME PATH USER LOGNAME LANG TERM` + proxy variables, cwd
`ws/` containing a `CLAUDE.md` with marker `PELICAN-7`. No credential file was read. 15 runs. Common
prefix: `claude -p --input-format stream-json --output-format stream-json --verbose
--include-partial-messages --replay-user-messages --model claude-haiku-4-5 --session-id <uuid>
--strict-mcp-config --mcp-config mcp.json`.

| Id | Flags beyond the prefix | Observed |
|---|---|---|
| P-A (5 runs, `initialize` only, 0 tokens) | `--permission-prompt-tool stdio` + (a) `--setting-sources project`, (b) `--safe-mode`, (c) (a)+`--disable-slash-commands`, (d) (a) with `CLAUDE_CONFIG_DIR=emptycfg`, (e) `--bare` | response keys `account, agents, commands, models, …`; commands 53 / 53 / **0** / 46 / 49, all names built-in (`deep-research` is `builtin: true`), none from `~/.claude/skills`; `models[].supportedEffortLevels = low…max` (none for haiku); `account` has `email, subscriptionType` when logged in, `{apiProvider, tokenSource}` for (d)/(e) |
| P-B | (a) + `--permission-mode default`, one turn, user line with `uuid` | replay echoes our `uuid` with `isReplay:true`; `command_lifecycle` queued/started/completed keyed by our uuid; `CLAUDE.md` marker read; tools listed as `mcp__calm__calm_report_write`, `mcp__calm__plugin_dev-neige-market_market_quote`; model used `ToolSearch select:` first; `can_use_tool` for both MCP calls; `result.usage.iterations[]`, `modelUsage.claude-haiku-4-5.contextWindow=200000`; `get_context_usage` → `{totalTokens, maxTokens, percentage, categories…}`; `set_model` → success + a `<local-command-stdout>` replayed user record; built-in skills visible to the model |
| P-C | `--safe-mode` | `mcp_servers: []` despite `--mcp-config`; user plugins listed; `CLAUDE.md` not read |
| P-D | (a) with empty `CLAUDE_CONFIG_DIR` | MCP connected, only `telemetry@builtin` plugin; assistant record `"Not logged in · Please run /login"` with `error`; `result {subtype:"success", is_error:true, terminal_reason:"api_error"}`; exit 1 on stdin close |
| P-E | (a) + `--disable-slash-commands --tools Bash,Read,Edit,Write,ToolSearch,WebFetch,WebSearch` | `skills: []`, tools exactly the 7 + 2 MCP; Write `tool_use_result {type:"create"}`, Edit `structuredPatch`; `echo one; exit 3` → `is_error:true`, text `"Exit code 3\none"`; steer line written while `can_use_tool` was pending: lifecycle `queued`, then `started` after the tool batch, model obeyed, **one** `result` for both lines |
| P-F1 | as P-E, SIGINT during `sleep 20` | synthetic rejected `tool_result` + `[Request interrupted by user for tool use]`, **no `result`**, process exited 0 |
| P-F2 | as P-E, SIGKILL during `sleep 20`, then `--resume <sid>` | resumed process emitted nothing for 8 s before input; answered "ZEBRA … the sleep command was interrupted" (context kept, killed turn seen as interrupted) |
| P-F3 | as P-E, second line queued, then `control_request{interrupt}` | `control_response {still_queued:[<queued uuid>]}`; interrupted assistant record carries `aborted`; `result {subtype:"error_during_execution", is_error:true, result:null}`; lifecycle `cancelled` for the running line; the queued line then ran as its own turn with its own `result` |
| P-G | `--bare` (no API key) | `account {tokenSource:"none", apiProvider:"firstParty"}`; same "Not logged in" failure as P-D |
| P-H | (a) + `--tools Bash,Read,Edit,Write,ToolSearch --permission-mode default --permission-prompts none --allowedTools "Bash Read Edit Write ToolSearch mcp__calm"` (no `--permission-prompt-tool`) | no `control_request` at all; MCP call, `touch` in cwd, Write **outside** cwd and `git push` all executed |
