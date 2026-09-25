# Claude Code as a second Planner backend (#1791) — design v3

> **Status (2026-09-24)**: v3, revised after review rounds 1 and 2 (both channels REVISE each round;
> every finding dispositioned in the [evidence companion §E4](1791-claude-planner-backend-evidence.md#e4-review-dispositions)).
> Round 2 mostly **deleted** mechanism. Base `origin/main` = `c5abb3c3d`. Paths without a prefix are
> under `crates/calm-server/src/`. Fact ids (`H*`, `S*`, `M*`), 4140 queries (`Q*`) and probes (`P-*`)
> live in the companion [1791-claude-planner-backend-evidence.md](1791-claude-planner-backend-evidence.md).
> Probes: claude 2.1.280, `claude-haiku-4-5`, scratch only, never the production server or MCP socket.
>
> Vocabulary: `scripts/gate-1316-terminology-ratchet.sh` counts retiring words in `docs/`, so this
> document says "transcript table / transcript row" for the provider-item table of migration 0031.

## 0. Owner rules (binding for every slice)

1. **Don't expand without limit — solve the pain point, simple first.** Hypothetical cases become
   one-line KNOWN GAPS; cuts are recorded (§9.5). Correctness defects are fixed with the fewest
   mechanisms, reusing existing precedents.
2. **Compatibility means only what is in the 4140 DB** (companion §E2).
3. **Repo `AGENTS.md` applies** (required types, typed config, env allowlists, no silent fallbacks,
   byte-frozen migrations, files ≤ 800 lines); `fe/AGENTS.md` for the FE slice.

## 1. Problem, goal, non-goals

**Pain point.** The Planner can only run on the Codex app-server; the owner wants a Planner on Claude Code.

**Goal (v1).** A track can be created with a Claude Planner that receives the same queued observations
and user messages (images included), calls the same `calm.*` MCP tools and `neige` CLI, can be
interrupted, survives a neige restart by resuming its Claude session, is fenced by card/track deletion
with a stop that confirms no marker-carrying process remains, and renders in the existing conversation UI with no FE renderer change.

**Non-goals.** ACP, a Node sidecar, the Agent SDK; a provider-neutral item model; Codex behaviour changes
beyond extracting the seam; worker cards; Claude for PlainChat / Assistant; steer for Claude Planners
(cut, §9.5). Model choice was cut here too and added afterwards by #1810 (§5.8).

### 1.1 Decision log

| # | Decision | Round | Where |
|---|---|---|---|
| D1 | Seam = `enum PlannerBackend { Codex(..), Claude(..) }` for the harness; code without a harness uses the Codex daemon as today plus one free function `claude_planner::stop(worker_session_id)` | v1; narrowed r2 (no process-wide registry) | §4 |
| D2 | The Claude backend emits **Codex-shaped `Notification`s**; `on_notification`, persistence, FE unchanged | v1 | §4.3 |
| D3 | Provider = existing `AgentProvider`, stored as a required, server-owned, sticky card key `planner_provider` (migration backfill); required at the Planner construction boundary; persisted to the session row from the mint input | v1; fixed r1; typed r2 | §4.4 |
| D4 | One `claude -p` process per turn; ≤ 1 per session; resident keepalive only if measured latency matters | orchestrator r1 | §5.1 |
| D5 | Permissions are static CLI rules (`--permission-prompts none` + `--allowedTools`) | v1 | §5.3 |
| D6 | Auth/isolation: dedicated `CLAUDE_CONFIG_DIR` (required `config_dir`) + the owner's own `/login` — **decided by the owner** | v1; r2 wording | §8 |
| D7 | `Recover` not offered; exact-interface briefing | v1 | §5.7 |
| D8 | **Steer cut.** The CLI dequeues a queued line immediately after `result` (P-F3, P-E), so the parent has no boundary to refuse it; v1 refusal stays and the entry runs as the next turn | **orchestrator r2** (was "scheduled" in r1) | §5.9 |
| D9 | Bash confinement: native sandbox with `failIfUnavailable` (**owner decided: sandbox**; the unconfined option is removed); enablement release-gated on §9.2 | orchestrator r1 | §5.3, §9.2 |
| D10 | Images: base64 image blocks (P-I) | orchestrator r1 | §5.6 |
| D11 | **Model choice cut**: Claude Planners run the CLI default; pickers show the existing `source:"unavailable"`. **Amended by #1810**: a fixed alias list (`opus`, `sonnet`, `haiku`) or the CLI default, passed as `--model`; no effort choice | **orchestrator r2**; #1810 | §5.8 |
| D12 | **Stop primitive**: one `/proc`-wide exact env-marker sweep (`NEIGE_CLAUDE_PLANNER=<worker_session_id>`), SIGTERM → bounded wait → verified SIGKILL → wait-empty (membership narrowed in r3, D16; users widened, D17). The r1 `(pid, pgid, start_time, boot_id)` record and group kill are deleted | **orchestrator r2** (A BLOCKING-1, B2-1) | §5.1 |
| D13 | **Crash recovery without a journal**: settlement = durable outcome → emit; boot = stop sweep, then idempotent `interrupted` outcome for `snapshot.last_turn_id`. The r1 journal and crash table are deleted | **orchestrator r2** (A MAJOR-1, B2-4/5) | §5.1 |
| D14 | Typed config: one optional `--claude-planner-config <file>`; absent ⇒ Claude Planner unavailable | **orchestrator r2** (A MAJOR-3) | §5.3 |
| D16 | Stop membership = `Present` only; `Unreadable` is not a member (no waiting on it); zombie and cgroup narrowing deleted; escapes are a KNOWN GAP | **orchestrator r3** (A BLOCKING-1, A MINOR-3) | §5.1 |
| D17 | `stop` runs on every terminal and retirement path (settlement, EOF, shutdown awaits it, reset/replay by old id, deletion, boot) + post-spawn seal re-check (settlement order refined by D25) | **orchestrator r3** (B3-1, B3-2, A MAJOR-2) | §5.1 |
| D18 | Config carries required `claude_binary` + `claude_version` (`deny_unknown_fields`); version verified before any input | **orchestrator r3** (B3-3, A MINOR-4) | §5.3 |
| D19 | Instructions via `--append-system-prompt-file` (private 0600 file), not argv | **orchestrator r3** (B3-4) | §5.2 |
| D20 | Boot rotates the MCP credential before cleanup, even if cleanup fails | **orchestrator r3** (B3-5) | §5.1 |
| D21 | Crash boundary = snapshot transaction commit (`:3882`), not the in-memory `:3262` | **orchestrator r3** (B3-7) | §5.1 |
| D22 | One lifecycle invariant: boot invalidates all Claude Planner credentials and sweeps all their markers **before the MCP listener starts**; the first turn mints the token and stops its own id (D31); every retirement stops by id regardless of registration (reset, shutdown op, repoint → Dirty on `Err`) | **orchestrator r4** (A-M1, A-M2, B4-1) | §5.1 |
| D23 | Fixtures-only `stop` failure seam; PR2b states `stop` returns `Ok` only after the marked child is gone | **orchestrator r4** (A-M3) | §5.1, §9.1 |
| D24 | Per-spawn guard for the instructions file; boot empties the directory | **orchestrator r4** (A-m4, B4-2) | §5.1 |
| D25 | Settlement = record → close stdin → wait ≤ 5 s → `stop` → emit; SIGTERM after `result` is harmless for `--resume` (P-L) | **orchestrator r4** (A-m5) | §5.1 |
| D26 | `--version` first token; SIGKILL phase re-scans; both phases verify by `start_time` | **orchestrator r4** (A-m6, A-m7) | §5.1 |
| D27 | Pre-destruction scoped set sweep over all Claude Planner ids of the card/track/area in any state; `Err` aborts before anything moves | **orchestrator r5** (A-M2 = B5-1) | §5.1 |
| D28 | Credential storage tables named; writer superseded by r11 (§5.1 token invariant); revocation text superseded by D33 | **orchestrator r5** (A-M1) | §5.1 |
| D29 | Seam only in `stop(id)` and scoped sweeps; timer path (and, per PR2b, shutdown and `Failed(check\|protocol)`) skips the stdin wait; instance-scoped marker; repoint's own 409 (its "inert loser" text is superseded by D31) | **orchestrator r5** (A-m3..m6) | §5.1 |
| D30 | Seam sticky until an explicit clear (its activation rollback and "inert" text are superseded by D31) | **orchestrator r6** (A MAJOR 1) | §5.1 |
| D31 | Activation phase deleted: `installed` flag + first-turn setup under the issuance lock (mint once, `stop(own id)`, spawn); `turn_start` never waits; supersedes D29's "inert loser" and D30's rollback and "inert" text | **orchestrator r7** (A MAJOR 1–2, cap rule) | §5.1 |
| D32 | Every sweep is revoke-then-sweep; `installed` set where `Slot::Live` is written (revocation scope superseded by D33) | **orchestrator r8** | §5.1 |
| D33 | **Token invariant** at the writers: for `(claude, planner)` rows only the first-turn mint writes the session hash; supersede nulls it; the card-hash mirror skips them; revocation = null session hashes (no card-row overwrite) | **orchestrator r9** (A MAJOR-1, 3rd sibling round) | §5.1 |
| D15 | Project config (`CLAUDE.md`/`AGENTS.md`, project settings) trusted like Codex trusts the workspace (S25); 4140 has no project settings (Q13) | r1 | §5.2 |

## 2. Evidence summary

Companion §E1–§E3. Shaping facts: the harness consumes one Codex-shaped stream and persists the turn id
before handling items (H4–H10, H22); session provider comes from the kind only (S12–S14); seals are a
thread-keyed set outliving harnesses (S21, S31); production restarts signal only calm-server (S22);
Claude's Bash runs each command in its own session and survives a `claude` SIGKILL (P-K); `-p` silently
ignores invalid settings and `system/init` does not report the sandbox.

## 3. 4140 compatibility (summary; queries in §E2)

| Fact | Number | Consequence |
|---|---|---|
| transcript rows | 4863, 13 cards/threads, all Codex types (Q1–Q2) | nothing to migrate |
| turn outcomes | completed 117, interrupted 5, failed 1 (Q3) | Claude reuses these three |
| `dynamicToolCall` rows, open projections | 0, 0 (Q5, Q6) | free type; no drain in flight |
| Planner cards | 24 (23 marker + 1 legacy shape) (Q7, Q11) | backfill `planner_provider:"codex"` on `role='planner'` |
| session rows | no `(claude, planner)` (Q8) | no legacy rows for the new identity |
| Planner operations | 122, all succeeded (Q9) | no replay sees a payload change |
| create idempotency | 38 fingerprinted (v1) + 7 legacy (v0) (Q12) | the 38 keep replaying (digest unchanged for Codex); the 7 keep failing closed (`routes/tracks/create.rs:374`) |
| workspaces | 20; 2 AGENTS.md-only; no project settings / `.mcp.json` (Q13) | AGENTS.md parity verified (P-J) |

## 4. The seam

### 4.1 Every Codex-coupled call on the Planner paths

| # | Call | Site | Decision |
|---|---|---|---|
| 1–6 | `subscribe_notifications`, `turn_start`, `turn_steer`, `turn_interrupt`, `interrupt_active_turn`, `active_turn_id_for_thread` | run_loop `:404`, `:271`, `:1340`, `:704`/`:3640`, `:691`, `:690`/`:3601` | `PlannerBackend` |
| 7 | seals (`seal_turn_thread_for_deletion`, `DeletionThreadSeals`, unseal on rollback) | run_loop `:658-679`; registry `:203-218`; deletion plans (S21) | **unchanged**: Claude thread ids are fresh UUIDs, so the same thread-keyed set works; the Claude session checks `turn_thread_is_sealed(thread)` before every spawn |
| 8 | `turn_thread_is_sealed` in recovery | harness/mod `:158`, `:424` | unchanged |
| 9 | `config_read`, `model_list` in resolution | run_loop `:2472`, `:2632` | Codex-only; the Claude arm reads the card at issue and resolves an alias or the CLI default without Codex (§5.8, #1810) |
| 10 | Codex readiness / deferred recovery | harness/mod `:213`, `:477`; `state.rs:577-598`; `lib.rs:615-640` (H21) | Codex rows only; Claude rows recovered in both boot arms |
| 11 | `is_running` preflights | start adapter `:420`, `:491` (S7); routes/cards `:1289`; planner_recovery `:58` | per provider: Claude ⇒ `claude_planner_config` present and `claude_binary --version` equals `claude_version` |
| 12 | `thread_start_*`, `remote_uri` | start adapter `:970-991` | Claude branch: UUID thread, no RPC |
| 13 | compensation `interrupt_thread` | start adapter `:1424-1430` | Claude row ⇒ `claude_planner::stop` |
| 14 | `PlannerHarnessParams{daemon}` | start adapter `:1277`; recovery; ~70 tests (H20) | `backend: PlannerBackend` (`From<Arc<SharedCodexAppServer>>`) |
| 15 | shutdown adapter, registry miss | `operation/planner_harness_shutdown_adapter.rs:109-135` | Claude row ⇒ `claude_planner::stop` |
| 16 | card teardown / deletion-grade quiesce | routes/cards `:92-127`, `:129-156` | Claude card ⇒ seal as today + scoped sweep over all its Claude Planner ids, any state (§5.1 item 4; propagates `Err`) |
| 17 | track / area deletion plans | S21 | unchanged fields; scoped sweep over all Claude Planner ids of the track/area, any state, before the destructive step (the track row delete removes the session rows, `crates/calm-truth/src/db/sqlite/track.rs:438-442`) |
| 18 | `resume_system_error_conversation` | planner_recovery | Codex-only (Claude never enters `Wedged(systemError)`) |
| 19–21 | GET `/api/models` (`routes/models.rs:144`), create advice (`routes/tracks.rs:848`), PUT advice (`routes/planner_model.rs:117`) | S11 | Claude (#1810): the alias catalog (`source:"built_in"`; `unavailable` without the flag) / an alias or 400 / an alias or 400 (§5.8) |
| 22 | reader texts naming codex | run_loop `:2430`, `:2547` | provider name from the backend (overlaps #1542) |
| 23 | `liveness_feeder`, dev replay, TUI takeover | S27 | Codex-only by nature |
| 24 | workspace repoint fence + recycle | `routes/tracks.rs:2104-2150`, `:2160`, `:2239` | scoped sweep over the track's Claude Planner ids before the pristine check (§5.1 item 4) |

PlainChat and Assistant stay Codex everywhere.

### 4.2 Types

```rust
// harness/backend.rs (new, ≤ 250 lines)
#[derive(Clone)]
pub enum PlannerBackend { Codex(Arc<SharedCodexAppServer>), Claude(Arc<ClaudePlannerSession>) }
impl PlannerBackend {
    pub fn subscribe_notifications(&self) -> broadcast::Receiver<Notification>;
    pub async fn turn_start(&self, thread: &str, items: Vec<InputItem>,
        selection: &TurnModelSelection, client_id: &str) -> Result<String>;
    pub fn supports_steer(&self) -> bool;                          // Claude: false
    pub async fn turn_steer(&self, thread: &str, turn: &str, items: Vec<InputItem>, client_id: &str)
        -> Result<String>;                                         // Claude: unreachable (pre-check)
    pub async fn turn_interrupt(&self, thread: &str, turn: &str) -> Result<()>;
    pub async fn interrupt_active_turn(&self, thread: &str) -> Result<()>;
    pub fn active_turn_id_for_thread(&self, thread: &str) -> Option<String>;
    pub fn provider(&self) -> AgentProvider;
    pub fn codex(&self) -> &Arc<SharedCodexAppServer>;            // seals + Codex-only calls
}
// claude_planner/stop.rs
pub async fn stop(instance: &MarkerInstance, worker_session_id: &str) -> Result<()>; // D12, §5.1
// `MarkerInstance` = the short hash of the canonical data_dir that prefixes every marker
```

`client_id: &str`: every harness issuance passes `Some` (run_loop `:3236`, `:1340`). The Claude session
holds the Codex daemon only for the thread-keyed seal set. PR1 introduces no provider type (it routes the
Codex arm only); `AgentProvider` already exists (`crates/calm-types/src/runtime.rs:27-32`).

### 4.3 Notification stream (D2)

A neutral event enum would rewrite `on_notification` (~400 lines, H5–H9) and the FE converters (M10).
Synthesis is honest: stored rows were always "the item the transcript renders" (Codex rows are filtered,
H9; projection rows are kernel-written in the same shape, H8), every row's provider is recoverable via
`worker_session_id`, and Claude keeps its raw record in its own session file. The Claude backend emits
only `TurnStarted`, `TurnCompleted`, `Item{item/started|item/completed}`, `Other{thread/tokenUsage/updated}`.
It writes two things itself: the durable turn outcome (`turn_outcome::record`, idempotent, H10) and
`agent_session_id` through the existing attribution bind (`crates/calm-truth/src/db/sqlite/session_mirror.rs:420-440`).

### 4.4 Provider identity (D3)

- **Card key.** `planner_provider` with `AgentProvider`'s serde names (`"codex"`, `"claude"`). New migration
  (next free number at merge) backfills `"codex"` for `role='planner'` (24 rows). Added to
  `SERVER_OWNED_CARD_PAYLOAD_KEYS` and to `server_owned_value_is_sticky` with `=> true` (the
  `template_context` precedent, S19). Tests: omission keeps it; a client payload carrying it is refused; a
  corrupt stored value survives and makes the card not-a-harness.
- **Construction boundary.** `HarnessProfile::from_card` keeps its serde-frozen enum; a new
  `PlannerBinding::from_card(card, role) -> Option<PlannerBinding { profile, provider: AgentProvider }>`
  reads the key for `CardRole::Planner` (missing/unknown ⇒ `None`, fail closed) and fixes `Codex` for
  PlainChat/Assistant. Start adapter and recovery take a `PlannerBinding`, never an `Option<AgentProvider>`.
  `card.kind` stays `"codex"` (S2, S16, S17; KNOWN GAP: legacy view name).
- **Session row.** The start adapter builds the runtime through `WorkerSessionInit::shared_planner(…,
  provider: AgentProvider)` (required parameter). The mirror maps `(SharedPlanner, provider)` to
  `(provider, Resumable, Planner)` instead of `derive_session_identity(kind)` (`session_mirror.rs:58`);
  `runtime_kind_from_session_identity` gains `(Claude, Planner) → SharedPlanner`; Planner-contract queries
  (`session_projection.rs:160`, `:735`) select `provider IN ('codex','claude')`; `read.rs:802` (TUI) stays
  Codex; the deferred-placeholder fns receive the explicit provider.
- **Boot.** Boot SQL arm `OR (ws.provider='claude' AND ws.contract='planner' AND c.role='planner')`
  (pinning test updated); Claude rows recovered in both arms of `recover_harnesses_after_daemon_boot`
  (H21); row provider must equal the card key, else skip + warn.
- **Create API.** `CreateTrackRequest.planner_provider: AgentProvider` (required) and
  `CreateRequestShape.planner_provider`; the digest adds the field only when `claude` (S20), so the 38
  fingerprinted bindings replay unchanged, the 7 legacy ones keep failing closed, and a Claude request
  never matches a Codex binding. **PR3 makes every existing caller send `"codex"`** (FE `new-track-route.tsx:66-74`
  body + `NewTrackBody`, `e2e/`, the Rust `/api/tracks` callers) with a regression test; fixtures inserting
  Planner cards directly get the key. A fork (`fork_report_from`) takes `planner_provider` from its own required request field; only Planner-opened child tracks inherit the parent Planner's key.
- **Start adapter (Claude branch).** UUID thread, no RPC, `phase = Idle`, `last_thread_id` (as `:982-985`).

## 5. The Claude backend

Module `claude_planner/` (files ≤ 800 lines): `protocol.rs`, `translate.rs` (pure), `session.rs`,
`stop.rs`, `config.rs`.

### 5.1 Process model, stop, lifecycle (D4, D12, D13, D22)

**One process per turn.** `turn_start` spawns `claude -p …` (§5.2), writes one `user` line, and the
process lives until the turn settles; then stdin is closed. First turn: `--session-id <thread>`; later:
`--resume <thread>` once `agent_session_id` is set (on the first `system/init`; the column's meaning for
Claude worker cards, `crates/calm-truth/src/session_projection_lookup.rs:58-69`). Per turn because resume
after kill works (P-F2), no idle reaper is needed (24 idle Planners on 4140), and flags are per spawn.
Readers, timers and the stop sweep remain. Per-turn cost: startup + resume parse + MCP handshake
(~0.5–1.5 s to `system/init`, P-B/P-E); whether `ToolSearch`-loaded schemas survive `--resume` is
UNVERIFIED (PR2b measures).

**Stop primitive `stop(worker_session_id)`** (precedent `operation/task_verify_adapter/target.rs:712-740`
`stop_group`, `operation/gate_process.rs:350-372`, `proc_identity.rs:180-264`). Every child carries the env
marker `NEIGE_CLAUDE_PLANNER=<instance>:<worker_session_id>`, `<instance>` = a short hash of the canonical
`data_dir`, so another calm instance on the host never matches this one's processes (inherited by Bash's
detached sessions, P-K). The same scan serves `stop(id)` (one id) and the **set sweep** (a set of ids,
one `/proc` pass with a set lookup). One `/proc` scan, `proc_env_marker`: **only `Present` is a member**; `Unreadable`/`Foreign` are never signalled nor
waited on (setgid `ssh-agent`, privsep `sshd` would otherwise fail every stop, S23). SIGTERM the members
(each verified by `start_time`) → ≤ 5 s → **re-scan** `/proc` and SIGKILL the members found (verified by
`start_time`, `proc_identity.rs:264`, so a recycled pid is rejected) → wait ≤ 10 s until a scan finds no
member, else `Err`. `stopped` means **no process carrying the exact marker remains**; descendants that
turn non-dumpable, rewrite or scrub their environ (headless Chromium), or leave the service cgroup escape
(KNOWN GAP). A fixtures-only `fail_claude_planner_stop_for_test(worker_session_id)` makes the next
`stop(id)` or scoped destructive sweep that covers that id return `Err` **without signalling anything**;
it is **sticky until an explicit fixtures-only `clear_claude_planner_stop_failure_for_test`** — a departure
from the one-shot precedent (`fail_workspace_repoint_shutdown_for_test`, `routes/tracks.rs:1951-1976`)
because one path stops the same id more than once (repoint shuts down fenced harnesses, `:2125-2150`,
before its sweep; reset shuts down the old handle, adapter `:862-867`, before its explicit `stop`); tests
arm, run the failing phase, clear, run the next phase. It is never consulted by the boot set sweep.

#### Claude process and credential lifecycle (the invariant)

1. **Boot, before the MCP listener starts** (the listener is spawned in `AppState::boot`, `state.rs:1091`,
   before harness recovery, `main.rs:43`; a surviving shim reconnects with its cached `initialize`,
   `crates/neige-mcp-stdio-shim/src/pump.rs:243`, and later calls check only session activity,
   `mcp_server/transport.rs:1479`; the step runs inside `AppState` construction, where the DB and
   `data_dir` are available): (a) **revoke**: null `worker_sessions.mcp_token_hash` of every
   `(claude, planner)` row in any state (the token invariant below keeps it null until a first-turn
   mint); (b) **one set sweep** over the markers of all those session ids — right after boot no Claude
   Planner process may legitimately be alive; (c) empty `<data_dir>/claude-planner/tmp/`.
2. **First-turn setup, not construction.** A Claude harness is built by the Claude arm of
   `spawn_recovered_harness` (callers `harness/mod.rs:348`, `:429`, `:527`, `routes/cards.rs:1295`,
   `replay.rs:385`) or of `spawn_side_effect` with an `installed` flag, true only after `install()`
   succeeds (set in `HarnessReservation::install`'s true arm, `harness/registry.rs:55-66`, the only
   production writer of `Slot::Live`, and in the test seam `HarnessRegistry::insert`, `:110-118`). `turn_start` never waits: not installed ⇒ `Err` (the existing retryable pre-`Ok` path:
   projection deleted, input re-buffered, retried after 2 s, run_loop `:3268-3300`); `shutting_down` ⇒
   `Err`. On the harness's first admitted turn, under the issuance lock it already holds (run_loop
   `:2958`): `mint_and_persist_claude_planner_token` (card row via `set_card_mcp_token` /
   `persist_card_mcp_token_hash`, session hash via `session_mcp_token_set_if_active_tx`, one transaction;
   plaintext kept only in the session, minted at most once per harness), then `stop(own id)`, then spawn. Because `shutdown_inner` sets `shutting_down`
   before taking the issuance lock (`:675`, `:685`) and a replacer builds only after `existing.shutdown()`
   returns (start adapter `:1271-1276`, `harness/mod.rs:201-205`), no mint can follow a shutdown or
   overwrite a replacer's hash. A setup `stop` `Err` returns `Err` from `turn_start`; the harness stays
   Live and retries on the next paced tick, so no dead Live handle blocks recovery. Nothing needs the
   token before the first turn (only the spawned process and its Bash use it), and the start adapter's
   first message is issued through `turn_start` like any other (H3). The Claude branch skips the Codex
   start-time mint (start adapter `:949`, `:1023`, `:1151`) and the thread-reuse token-row check (`:892`),
   which guards a token baked into a Codex thread config.
   **Token invariant.** For `(claude, planner)` rows the only writer of `worker_sessions.mcp_token_hash`
   is the first-turn mint, through a **separate guarded writer** `mint_and_persist_claude_planner_token`
   → `session_mcp_token_set_if_active_tx` (`… AND state IN ('starting','running','idle','turn_pending')`)
   in the same transaction as the card write (the carrier check, run_loop `:2964`, is a plain read, and
   reset commits its supersede before the old handle's shutdown, start adapter `:864-869`); a row count
   other than 1 ⇒ `Err` ⇒ `turn_start`'s retryable path. The shared `session_mcp_token_set_tx`,
   `mirror_session_mcp_token` and `mint_and_persist_card_token` stay unchanged, so Codex system-error
   recovery's deliberate mint on a `failed` row (`shared_codex_appserver/preserving_recovery.rs:56-72`)
   is unaffected; superseding such a row nulls its hash (both supersede statements,
   `session_mirror.rs:331-345` and `:561-576`); the card-hash mirror `session_mirror_card_mcp_token_tx`
   (`:223-259`) skips them. Restore and reactivation therefore always yield a NULL hash, which fails the
   handshake (active-row hash lookup, `crates/calm-truth/src/db/sqlite/session_row.rs:107-108`) until the
   next first-turn mint. The writer and activation list is companion S38. No production reader of
   `card_mcp_tokens` authenticates a Claude Planner (the handshake reads session rows only), so the card
   row needs no revocation.
   *PR4 amendment:* a spawn whose row hash was nulled under a live harness (a revocation under a live
   harness, e.g. one the aborted deletion's recovery could not replace) re-mints through the same guarded
   writer before spawning, after re-checking `shutting_down` and the seal;
   a session that was never installed (an install-race loser) signals nothing on shutdown, since its id
   may carry the winner's live turn.
3. **Every turn ends with `stop`** (settlement order below) and every retirement of a Claude id
   (supersede, shutdown, deletion) calls `stop(id)` **whether or not the id is registered** — the table
   below is only the caller list.
4. **Before every destructive step** — card, track or area deletion and workspace repoint (§4.1 rows 16,
   17, 24) — a **scoped revoke-then-sweep** over **all** Claude Planner session ids of that card, track or
   area in **any** state: null their session hashes (as 1(a)), then the set sweep of their markers. `Err`
   ⇒ abort before moving anything or deleting rows. After revocation the old token no longer completes a
   handshake, including for a session an aborted deletion reinstalls (`harness/mod.rs:429`), until its
   first turn mints; connections already established check only session activity
   (`mcp_server/transport.rs:1478-1497`) — that is inside the stop-failure/escape boundary (KNOWN GAP).
   Reset and the shutdown op need no revocation (supersede nulls the hash). This is what makes "logged,
   left to boot" safe for them.
   Id set: `session_list_by_track` (`crates/calm-truth/src/db/sqlite/session_repo_impl.rs:154`, no state
   filter) filtered to `(claude, planner)`; a card filters by `card_id`; an area iterates its tracks
   (`routes/areas.rs:401`).
   *PR4 amendment:* for track and area deletion the revoke-then-sweep runs after the threads are sealed
   and before the harness shutdowns (oracle row 15), so a stop that fails in a shutdown cannot abort the
   deletion before the revocation.
5. **Outcome.** Recovery records `interrupted` for `snapshot.last_turn_id` (idempotent SELECT-then-INSERT,
   `crates/calm-truth/src/db/sqlite/out_of_domain.rs:423-434`; precedent
   `shared_codex_appserver/preserving_recovery.rs:120-160`).
   *PR4 amendment:* only when the snapshot's phase says a turn may still have been running
   (`TurnRunning`, `IssuingInterrupt`, `Resumed`): a settled phase means the outcome was recorded, and a
   reset's transcript clear may since have removed it, which is no reason to invent one.

| Caller | Where | On `Err` |
|---|---|---|
| turn settlement / unexpected EOF | `session.rs` | logged; the turn is still emitted; the next spawn's `stop` fails closed |
| next spawn (serialization is the settlement wait; `stop` covers a crash-leftover) | `turn_start` | `turn_start` fails → retryable refusal |
| interrupt timer | `session.rs`; skips the stdin wait (the child already ignored the interrupt): invariant: interrupt ⇒ `TurnCompleted` by `settle_by` = interrupt + 10 s timer (armed before the interrupt line is written) + 12 s margin = 22 s < `interrupt_completion_budget` 30 s (`harness/config.rs:27`), leaving 8 s for the run loop around the call (`run_loop.rs:3639-3645`); every await after the stop is armed is capped by `settle_by` | cause already recorded; as settlement |
| harness shutdown | Claude arm of `shutdown_inner`'s interrupt (run_loop `:666-729`), records `Interrupted(shutdown)`, awaits `stop` | strict `shutdown_for_deletion` propagates; non-strict logs |
| reset (start adapter `:862-870`) and shutdown op (`operation/planner_harness_shutdown_adapter.rs:82`, `:109`) | `stop(old id)` for a Claude predecessor, registered or not; replay repeats it | logged; left to the next boot sweep or the next destructive step's scoped sweep (item 4); the old id's token no longer authenticates once the row is superseded (handshake accepts active rows only, `mcp_server/handshake.rs:55-60`) |
| workspace repoint | fence supersedes every active runtime (`routes/tracks.rs:2104-2118`); the scoped sweep over the track's Claude ids runs **before** the pristine check and recycle (`:2160`, `:2239`) | the Dirty branch's restart at the old path, with its own 409 message ("a previous Claude Planner process of this track could not be stopped; the workspace was not moved") |
| deletion quiesce, registry-miss paths | §4.1 rows 13, 15–17; deletion also runs the scoped sweep | propagates; nothing is moved and no row is deleted |

**Seal checks.** Before spawning and again after the spawn, before writing the user line (mirroring Codex,
`shared_codex_appserver.rs:1386-1390`); sealed ⇒ `stop` + `Err`. With the issuance lock that
`shutdown_inner` waits on (run_loop `:685`), a deletion that seals while a spawn is held ends stopped.

**Submission contract.** `turn_start` mints `turn_id`, checks the seal, runs `<claude_binary> --version`
and refuses unless its **first whitespace token** equals `claude_version` (it prints `2.1.280 (Claude
Code)`) — before any user input — writes the instructions file under a **per-spawn guard**, spawns,
re-checks the seal, writes the `user` line (bounded 5 s), and returns `Ok(turn_id)`. Every exit before
`Ok` ⇒ `stop` + the guard removes the file + `Err`; the existing path deletes the projection and
re-buffers (run_loop `:3268-3300`); no outcome, because no turn id was persisted. After `Ok` the guard
hands the file to settlement. **The turn id becomes durable** when the snapshot transaction started by
`persist_issuance_outcome` (`:3267`) commits (`:3876-3882`); before that commit the pre-drain snapshot
(`:3045`) still owns the batch (re-drain; possibly a second delivery, the at-least-once window Codex has
too, H22); after it a crash leaves `interrupted` and no re-drain.

**Settlement.** One `TurnSlot { cause: Option<TerminalCause> }`; paths about to interrupt or stop record
their cause first (§6.2). Order: `turn_outcome::record` → close stdin → wait ≤ 5 s for the direct child →
`stop` (only descendants remain in the normal case) → remove the instructions file → emit `TurnCompleted`.
Record first so a crash during the stop keeps the outcome; emit last so the next turn never overlaps this
turn's processes; closing stdin first avoids signalling a `claude` that is about to exit (a SIGTERM after
`result` exits 143 and `--resume` then sees the previous reply as complete, P-L). Undecodable stdout
(including bytes that are not UTF-8, or a read error) ⇒ cause `Failed("protocol")`. The stop timer,
shutdown and `Failed(check|protocol)` skip the stdin wait and go straight to `stop`. On EOF the direct
child's exit status is awaited (≤ 5 s) *before* recording, because the failure message carries it. A stop
(timer or shutdown) first reads the stdout lines already written, for at most 1 s and without answering
control requests (stdin closes next), so a result the CLI finished first still decides the turn (§6.2); a
control answer the CLI does not take is abandoned at the stop deadline. The stop timer is armed when the
interrupt is asked for, before its line is written. **Settlement deadline.** Once a stop is armed
(interrupt: now + 10 s; shutdown: now), `settle_by` = the stop deadline + 12 s, and every await after
that — control answers, the bind, the drain, the exit wait, the outcome write, `stop` (its SIGTERM grace
ends 1 s before its deadline so SIGKILL still goes out; each `/proc` scan is bounded too, a hung scan is
`Err` and its blocking thread is leaked), the final reap — gets the smaller of its own timeout and what is
left before `settle_by`. What misses it is logged and settlement moves on: an unrecorded outcome is left to
boot recovery (§5.1 item 5), a stop that did not confirm to the next spawn's `stop`, which fails closed.
A result read while stopping gets no exit wait. The first `system/init` of a `--session-id` spawn that names the thread
persists `agent_session_id` through the attribution bind, even when a later init check fails the turn (the
CLI has created the session); the bind passes the row's `active_turn_id` through unchanged, since the
harness snapshot owns it. A session opened for a row that has `agent_session_id` spawns `--resume`.

### 5.2 Spawn contract (no runtime directory)

```
<claude_binary> -p --input-format stream-json --output-format stream-json --verbose --replay-user-messages
  (--session-id <thread> | --resume <thread>)
  --setting-sources project --disable-slash-commands
  --tools Bash,Read,Edit,Write,ToolSearch,WebFetch,WebSearch
  --strict-mcp-config --mcp-config '<json>'  --settings '<json>'
  --permission-prompts none --allowedTools '<rules, §5.3>'
  --append-system-prompt-file <data_dir>/claude-planner/tmp/<worker_session_id>-<turn_id>.md
```

`<claude_binary>` is the configured versioned path (§5.3), never the auto-updated `~/.local/bin/claude` symlink.

- `--mcp-config`: `{"mcpServers":{"calm":{"type":"stdio","command":"<neige-mcp-stdio-shim>","args":[],
  "env":{"NEIGE_MCP_SOCKET":"${NEIGE_MCP_SOCKET}","NEIGE_MCP_TOKEN":"${NEIGE_MCP_TOKEN}"}}}}` — argv
  carries no secret; `${VAR}` expansion verified for a config *file* (P-S1), the JSON-string form is
  UNVERIFIED (PR2b could not check it without running the real CLI; it is a §9.2 release-gate item;
  fallback: one 0600 file per spawn, deleted on exit). `--settings` as a JSON
  string is verified (P-S1). No runtime directory, so nothing to clean up on reset or delete.
- Instructions are rendered once by the caller (the start adapter's renderer) when the session is built,
  and written at every spawn into a fresh 0600 file in the 0700
  directory `<data_dir>/claude-planner/tmp/`, removed by settlement (and by boot for leftovers). They are
  kept off argv because they include bound template input and template context
  (`operation/planner_harness_start_adapter.rs:314`, `template_context.rs:55`) and `/proc/<pid>/cmdline` is
  world-readable on this host (no `hidepid`). Non-secret JSON config stays inline. Claude reuses its
  recorded system prompt until compaction, then the current rendering applies. For Claude only, the
  rendering appends one sentence from a new `prompts/` fragment: start long-lived servers with
  `calm.terminal.open`, not Bash (they die with the turn and at restart).
- No `--permission-mode`, no `--include-partial-messages`, no `--effort`; `--model <alias>` only when the
  card has chosen one (D11 as amended by #1810, §5.8).
- `cwd` = track workspace; a moved workspace cannot resume (KNOWN GAP, #857 class). Project context:
  `CLAUDE.md`, else `AGENTS.md` (P-J).
- **Env** (`env_clear()` + allowlist): `SPAWN_ENV_PASSTHROUGH` (S24) minus `OPENAI_*`, `CODEX_*`, `RUST_*`,
  `LOG_FORMAT`; `HTTP(S)_PROXY` from the daemon's resolver (`shared_codex_appserver.rs:1862`); `PATH` =
  `kernel_led_path()`; `CLAUDE_CONFIG_DIR` = `config_dir`; `NEIGE_MCP_SOCKET`, `NEIGE_MCP_TOKEN`,
  `NEIGE_CLAUDE_PLANNER`, `DISABLE_AUTOUPDATER=1`. Never `NEIGE_MCP_DAEMON_TOKEN` (M4), `ANTHROPIC_*`,
  `CLAUDE_CODE_*`. The token is minted at the harness's first turn (§5.1 item 2, M3).
- **Init checks** (every spawn, second line of defence after the pre-spawn `--version` check):
  `claude_code_version == claude_version`; `session_id == thread`; `capabilities ⊇ {interrupt_receipt_v1}`;
  `calm` connected; `skills == []`; all plugins `@builtin`. Failure ⇒ cause `Failed(check)` + `stop`.

### 5.3 Confinement and typed config (D5, D9, D14)

**Escalation without confinement.** The Planner reads untrusted text (web results, plugin output, worker
results). Unconfined Bash as the neige user lets a prompt injection write `calm.db` directly (bypassing
the role gate and event log), alter other runtimes' state, read and exfiltrate Codex/Claude credentials,
push anywhere and kill neige processes. Codex Planners run `workspace-write` with network (S5,
`shared_codex_home.rs:302`): reads anywhere, writes confined.

**Typed config** `--claude-planner-config <file>` (optional; precedent `isolated_codex_config:
Option<PathBuf>`, `config.rs:33-36`, "missing keeps this backend unavailable"):

```rust
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
struct ClaudePlannerConfig {
    claude_binary: PathBuf,        // versioned binary, e.g. ~/.local/share/claude/versions/2.1.280
    claude_version: String,        // must equal `<claude_binary> --version` before any input is written
    config_dir: PathBuf,           // dedicated CLAUDE_CONFIG_DIR; the owner runs /login in it
}
```

`claude_binary` and `claude_version` are required: simplest, same as `codex_binary` in
`IsolatedCodexConfig` (`isolated_codex/config.rs:14-23`, also `deny_unknown_fields`), and the auto-updater
swaps the `~/.local/bin/claude` symlink (2.1.220, 2.1.259, 2.1.280 are installed side by side on this
host). `deny_unknown_fields` stops a mistyped field from being silently ignored.

Absent ⇒ create with `planner_provider:"claude"` answers 4xx naming the flag; `is_ready(Claude) = false`;
a recovered Claude harness refuses issuance with a retryable refusal and a reader message naming the flag.

- The sandbox is always on (owner decision): `--settings {"permissions":{"allow":["WebFetch(domain:*)"]},"sandbox":{"enabled":true,
  "failIfUnavailable":true,"allowUnsandboxedCommands":false,"network":{"allowAllUnixSockets":true}}}`
  and allow rules `Bash Read ToolSearch WebFetch WebSearch mcp__calm Edit(//<cwd>/**) Write(//<cwd>/**)`.
  Documented semantics: sandboxed Bash writes cwd + session temp and reads everywhere (as Codex); the
  bare `*` in `WebFetch(domain:*)` pre-allows every domain for sandboxed commands (sandboxing docs,
  "Network isolation", v2.1.186+), so the network is unrestricted like Codex's `network_access=true`
  (`shared_codex_home.rs:302`) while filesystem isolation stays on; `strictAllowlist` is not set. `socat`
  is still required (the sandbox proxy runs even when every domain is allowed). `socat` is found on the
  child's `PATH` (`kernel_led_path()`, which inherits the server's PATH); a user-local static binary in a
  directory on the service PATH suffices. `sandbox.socatPath` is not used: it is honoured only in managed
  settings (root-owned). `allowAllUnixSockets` is required on Linux so the Planner's Bash can run the
  `neige` CLI against `NEIGE_MCP_SOCKET` (`prompts/planner.md` uses `neige cat`/`neige state`); the
  per-path `allowUnixSockets` list is macOS-only (settings reference). `failIfUnavailable` is mandatory: without it the CLI only warns and runs everything unconfined (P-S2);
  with it the turn fails before any request (P-S1). Because `-p` silently drops invalid settings and
  `system/init` does not report the sandbox, the release-gate verification is recorded against
  `claude_version`, which the pinned binary and the pre-spawn check keep constant.

### 5.4 Protocol types (`protocol.rs`)

Discriminate on `type`, then on `subtype` / content kind, **before** reading variant fields; PR2a decodes
every line of every recorded fixture. Unknown discriminants ⇒ `Ignored{kind}`; `decode` does not log, the session (PR2b) logs `Ignored{kind}` at debug.

```rust
enum Record {
  SystemInit { session_id, claude_code_version, model: String, capabilities: Vec<String>,
               mcp_servers: Vec<McpStatus>, skills: Vec<String>, plugins: Vec<PluginRef> },
  UserReplay { uuid: Uuid, message: UserMessage },                      // isReplay: true
  UserText { uuid: Uuid, text: String },                                // e.g. "[Request interrupted…]" (P-F3)
  UserToolResults { uuid: Uuid, results: Vec<ToolResult>, tool_use_result: Value },
  Assistant { uuid: Uuid, blocks: Vec<AssistantBlock> },                // thinking | text | tool_use
  ResultSuccess { is_error: bool, result: String, usage: Usage,
                  model_usage: BTreeMap<String, ModelUsage>, terminal_reason: String },
  ResultError { subtype: ErrorSubtype, errors: Vec<String>, terminal_reason: Option<String>,
                usage: Usage, model_usage: BTreeMap<String, ModelUsage> },
  ControlResponseIn { response: ControlResponseBody },                  // nested envelope (P-F3)
  ControlRequestIn { request_id: String, request: Value },
  Ignored { kind: String },
}
// stdin: UserLine { type:"user", message:{role:"user", content:[text | image]}, parent_tool_use_id:null,
//        session_id, uuid }; ControlRequest{interrupt}; ControlResponseOut{Success{request_id, response}
//        | Error{request_id, error}}
```

Recorded shapes: success results carry `result` and no `errors` (P-B, P-I); error results carry
`errors` and no `result`; `terminal_reason` is `"aborted_streaming"` after an interrupt (P-F3) and absent
when the sandbox is unavailable (P-S1). Client ids (32 hex) go out as the dashed UUID of the same bits.
An inbound `can_use_tool` (not expected with `--permission-prompts none`, P-H/P-I) gets a `Success` deny;
any other inbound request an `Error` response.

### 5.5 MCP wiring and tool names

Shim, socket, card-bound token and role resolution are reused (M3–M5). Claude names tools
`mcp__calm__<name with [^A-Za-z0-9_-] → _>` (P-B); `translate.rs` restores the dotted name from the card's
visible tool list (M10); ambiguous or unknown ⇒ keep the Claude name + warn. `model_tool_key` already
accepts Claude spellings (M7).

### 5.6 Input, including images (D10)

Text + one `{type:"image", source:{type:"base64", media_type: AttachmentFormat::mime(), data}}` per bound
attachment (S26; P-I answered "Red."). The replay echoes the base64; the stored `userMessage` keeps text
plus `{type:"localImage", path}` placeholders only (rendering reads the projection's `input_segments`, H8).
Image-only messages are valid lines. KNOWN GAP: an image over the API's limit fails the turn.

### 5.7 Recover (D7)

Claude threads are never registered (M9) ⇒ the exact-interface briefing (`calm.plan.recover` with `key`,
`expected_attempt_id` and a Planner-kept `idempotency_key`, H17). KNOWN GAP: no bound `Recover`.

### 5.8 Model and effort (D11, amended by #1810)

*v1 (D11) cut the choice: a Claude Planner ran the CLI's default and every model surface refused. #1810
replaces that with the following.*

A Claude Planner chooses from one fixed alias list, `claude_planner/models.rs`: `opus`, `sonnet`, `haiku`.
The CLI resolves each alias to its current model, so the list does not go stale and needs no config key.
`null` is the CLI's default and passes no `--model`. There is no effort choice: the effort is always
`null`. The card keeps the selection in the same payload keys as Codex (`model`, `reasoning_effort`).

- **Turn.** The run loop's Claude arm reads the card at issue time, like Codex, but asks Codex nothing and
  needs no `*_ever_set` resolution (each turn is a fresh process). An alias spawns `claude -p …
  --model <alias>`. A payload the list cannot satisfy (unknown model, any effort, malformed keys) is
  refused at issue as "needs a choice": the message stays queued and the reader is told to pick a model.
  It is never sent under a guess. A PUT between turns applies from the next turn.
- **GET `/api/models`** for a Claude card (or `?provider=claude` before a card exists) answers the alias
  list with `source:"built_in"`, `default_source:"unknown"`, a `null` default, `fetched_at_ms: null`, and
  per entry `is_default: false`, no supported efforts and `default_reasoning_effort: null`. On a server
  without `--claude-planner-config` it answers `source:"unavailable"` with an empty catalog, and the FE
  hides the Claude group.
- **Create** with `claude` accepts a `model` from the list; any other model or any `reasoning_effort` ⇒ 400.
- **PUT `/planner/model`** on a Claude card accepts an alias or `null` with a `null` effort; anything else
  ⇒ 400, with nothing stored or adjusted.
- **FE.** On the new-track page one grouped picker replaces the provider pill: a Codex group (Default and
  the live catalog, with effort) and a Claude group (Default and the aliases, no effort). The pick decides
  `planner_provider` and `model`. Inside a track the picker lists only the track's provider group.
- **Compatibility.** An older bundle's schema rejects `source:"built_in"` and a `null`
  `default_reasoning_effort`, so #1810 bumps `REST_API_VERSION` 10 → 11 and `WEB_COMPAT_VERSION` 30 → 31.

### 5.9 Steer (D8)

`handle_steer` checks `supports_steer()` before taking the entry and answers `SteerRefused::NotTaken`
("this Planner's provider (Claude) cannot take messages into a running turn; it stays queued"); id and rev
unchanged; the entry runs as the next turn. KNOWN GAP: a future path via the `interrupt_cancel_queued_v1`
capability (listed in `system/init`, unprobed).

### 5.10 Token usage

On `ResultSuccess` or `ResultError` with a non-empty `usage.iterations`: emit `thread/tokenUsage/updated` `{threadId,
tokenUsage:{last:{totalTokens: input+cache_read+cache_creation+output of the last iteration},
total:{totalTokens: previous total + this turn}, modelContextWindow: modelUsage[init.model].contextWindow}}`;
otherwise emit nothing (P-D, P-F3), keeping the previous reading (H11). The total is seeded from the
snapshot. KNOWN GAP: the 12k baseline is Codex-derived (Claude's fixed prefix ~18k, P-B).

## 6. Mapping

### 6.1 Claude record → notification → stored item

Envelope `{threadId, turnId, item, startedAtMs | completedAtMs}`. Ids: `tool_use.id` for tools,
`<record uuid>:<block index>` for text/thinking.

| Claude record | Notification | Stored `item` |
|---|---|---|
| our line written | `TurnStarted{turn:{id}}` (handled after `turn_start` returns) | — |
| `UserReplay` (our uuid) | `item/completed` `userMessage` | `{id, clientId, content:[text, localImage…]}` → upgrades the projection (H8) |
| `UserReplay` of `<local-command-stdout>`, `UserText`, `system/*`, `stream_event`, `rate_limit_event`, `command_lifecycle`, `tool_use ToolSearch` | none | — |
| `thinking` / `text` block | started + completed `reasoning` / `agentMessage` | `{content:[],summary:[]}` / `{text}` |
| `tool_use mcp__calm__*` → its result | `mcpToolCall` started → completed | `server:"calm"`, dotted `tool`, `arguments`, `status`, `result` or `error`, `durationMs` |
| `tool_use Bash` → result | `commandExecution` | `command, cwd, status, aggregatedOutput`; `exitCode` 0 if not `is_error`, N from a leading `Exit code N` (P-E), else null |
| `tool_use Edit`/`Write` → result | `fileChange` | `changes:[{path, kind:{type: add or update}, diff}]` |
| `tool_use Read`/`WebFetch`/`WebSearch` → result | `dynamicToolCall` | `{tool, arguments, status}` (Q5: no legacy rows) |
| `ResultSuccess` / `ResultError` | usage (§5.10), durable outcome, `TurnCompleted` (§6.2) | outcome row (H10) |

### 6.2 Terminal cause × event → outcome (exactly one per turn)

A recorded cause wins over the event that follows it; otherwise the first event wins.

| Recorded cause | Event | status | `error.message` |
|---|---|---|---|
| none | `ResultSuccess`, `is_error:false` | completed | — |
| none | `ResultSuccess`, `is_error:true` (not logged in, P-D/P-G) | failed | `result` |
| none | `ResultError` (e.g. sandbox unavailable P-S1, SIGINT P-F1) | failed | `errors` joined |
| none | EOF + exit before a result (crash) | failed | `claude exited (<status>)` + last stderr line |
| `Interrupted(user\|watchdog\|shutdown)` | `ResultError`, EOF, or stop timer | interrupted | — |
| `Interrupted(…)` | `ResultSuccess`, `is_error:false` (finished first) | completed | — |
| `Interrupted(…)` | `ResultSuccess`, `is_error:true` | interrupted | — |
| `Failed(check\|protocol)` | any | failed | the cause |
| (boot) | `last_turn_id` without an outcome | interrupted | "neige restarted during this turn" |
| — | spawn / write fails before `Ok` | **no turn** (projection deleted, batch re-buffered) | — |

An interrupted turn emits `TurnCompleted` by `settle_by` (§5.1: interrupt + 22 s), inside the harness's 30 s budget; Claude never emits
`ThreadStatusChanged`, so `Wedged(systemError)` does not apply.
Tool items still open at settlement are completed as `failed` (`close_open`): Codex item statuses have no
interrupted variant, and the turn row carries `interrupted`.

## 7. Oracle trace

| seq | phase | actor | trigger | external effect | observable event | invariant | status |
|---|---|---|---|---|---|---|---|
| 1 | create | user | `POST /api/tracks {…, planner_provider:"claude"}` with the config flag set | card `kind:"codex"`, key `claude` | `card.added` (`routes/tracks.rs:1423`) | key required + sticky; Codex digests unchanged | NEW |
| 2 | start | kernel | `planner-harness-start` | row `claude/resumable/planner`; UUID thread; no token until the first turn; phase Idle | op phases (`operation/planner_harness_start_adapter.rs:840`) | provider from the construction boundary | NEW |
| 3 | message | user | `POST /api/cards/{id}/planner/input` (+ image) | entry persisted before 200 | `harness.user_message.enqueued` (`routes/cards.rs:921`) | durable before ack | existing |
| 4 | drain | run loop | tick | projection; seal checked; spawn with marker; one `user` line | `harness.item.added` (projection) | `Ok` only after the write | NEW spawn |
| 5 | turn begins | backend | `Ok(turn_id)` | snapshot commit with `last_turn_id` (`:3267`→`:3882`) | `TurnStarted` → `TurnRunning` | turn id durable before any item row | NEW producer |
| 6 | echo / init | CLI | `UserReplay`, `system/init` | projection upgraded; `agent_session_id` bound; checks | `harness.item.added` | `session_id == thread`; sandbox version pinned | NEW |
| 7 | tool call + result | model / CLI | `tool_use mcp__calm__…` | MCP via shim | `item/started`, `item/completed` (dotted) | no approval prompt (P-H) | NEW producer |
| 8 | steer | user | `POST …/planner/input/{entry}/steer` | none | 409 `NotTaken` | id/rev unchanged | NEW refusal |
| 9 | interrupt | user | `POST /api/cards/{id}/planner/interrupt` | cause recorded; `interrupt` line; stop timer | `IssuingInterrupt` | one outcome | NEW producer |
| 10 | settle | backend | `ResultError aborted_streaming` | outcome row, `stop` (detached Bash included), then emit | `TurnCompleted` | durable before emit | NEW |
| 11 | next turn | run loop | observation | previous child exited or `stop`; spawn `--resume` | normal turn | ≤ 1 process per session | NEW |
| 12 | restart mid-turn | ops | deploy restart (`KillMode=process`) | `claude` and its detached Bash keep running | none | — | existing ops |
| 13 | boot | kernel | `AppState::boot` before the MCP listener; then `recover_harnesses_after_daemon_boot` | all Claude Planner credentials invalidated; one marker sweep over all Claude Planner ids (incl. detached Bash, P-K); tmp dir emptied; per harness: `interrupted` outcome for `last_turn_id` (no-op if settled); token mint + `stop(own id)` at its first turn | outcome row | no marked process and no valid old token when the listener opens | NEW |
| 14 | resume | run loop | new input | `--resume` | normal turn | killed turn visible to the model (P-F2) | NEW |
| 15 | delete track | user | `DELETE /api/tracks/{id}` | seal (existing set); scoped revoke-then-sweep over all the track's Claude Planner ids (any state); harness shutdown awaits `stop`; a held spawn sees the seal after spawning and stops; rollback unseals | deletion events | destructive move only after a sweep finds no marked process (escapes: KNOWN GAP) | NEW call |

## 8. Auth and isolation (D6), compliance

| Option | Evidence | Verdict |
|---|---|---|
| A. `config_dir` set: dedicated `CLAUDE_CONFIG_DIR`, user runs `CLAUDE_CONFIG_DIR=<dir> claude` + `/login` | only built-ins; not logged in ⇒ "Not logged in · Please run /login" (P-D) | **chosen by the owner** |
| B. `--bare` + API key | hooks/plugins/OAuth skipped; loses CLAUDE.md discovery (P-G) | compliant alternative (not in v1 config) |
| C. `--safe-mode` | disables `--mcp-config` servers (P-C) | rejected |
| D. user's `~/.claude` | user skills/plugins not loaded (P-B, P-E) | not used (`config_dir` is required) |

Compliance: no login UI; neige never collects, stores, copies or relays OAuth credentials or session tokens;
the unmodified binary runs as a subprocess; neige never sends `initialize` (its response carries the
account email, P-A).

## 9. Delivery

### 9.1 Slices

| PR | Content | Acceptance | Must-red (mutation-verified) |
|---|---|---|---|
| **PR1 Seam** (~500) | `PlannerBackend` (Codex arm only), `PlannerHarnessParams.backend`, rows 1–6, 14 of §4.1; `client_id: &str`; invariant allowlist gains `harness/backend.rs` | targeted nextest of Planner harness, recovery, interrupt, deletion suites; `local-rust-gates.sh --quick` | a second `.turn_start(` in run_loop still red |
| **PR2a Translate** (~700) | `protocol.rs`, `translate.rs`; complete recorded fixtures (redacted) | every line of every fixture decodes | dotted name restored; per-block ids; base64 not stored; empty `iterations` ⇒ no usage frame; `UserText` is not a protocol error |
| **PR2b Session + stop** (~800) | `session.rs`, `stop.rs`, `config.rs`: spawn contract, env, `TurnSlot`, settlement, submission contract, instructions guard, failure seam | fake `claude` (bash) through the real session: exit / kill / linger / undecodable / stalled-write / immediate-exit paths | outcome durable before `TurnCompleted`; recorded interrupt + `ResultSuccess is_error:true` ⇒ `interrupted` (P-D fixture); `stop` returns `Ok` only after a marked `setsid sleep 300` child is gone (mutation: a pgid-scoped scan returns `Ok` while it lives); a fake that exits successfully while its marked `setsid` child survives ⇒ the child is gone before `TurnCompleted`; a readable unmarked process is never signalled, a recycled pid is rejected by `start_time`; the fake's `--version` prints the real `2.1.280 (Claude Code)` and a wrong version receives **no** user input; a private sentinel never appears in `/proc/<pid>/cmdline`; every pre-`Ok` exit (spawn failure, immediate exit, post-spawn seal) leaves no instructions file; immediate exit ⇒ `Err` and no outcome (holds when the CLI exits before the line write completes; the test writes a line larger than the pipe buffer; an exit after the write is a `failed` turn). Unique `worker_session_id` per test (host-wide sweep, parallel nextest) |
| **PR3 Provider identity** (~900, sweep-heavy) | key + migration + sticky; `PlannerBinding`; `WorkerSessionInit::shared_planner`; mirror + query sites; boot SQL arm; create field + digest; **every existing caller sends `"codex"`**; OpenAPI / `wire.ts` / `NewTrackBody`; fixture sweep; `claude` refused at create until PR4 | migration on a scratch `.backup` of the live DB (24 keys); FE lint/build/test | `(SharedPlanner, Claude)` mint persists `claude/resumable`; key omission keeps it; missing key ⇒ not a harness card; 38 v1 bindings replay, 7 v0 conflict, same key + other provider ⇒ conflict; an FE create without the field is a test failure |
| **PR4 Wiring** (~750) | start adapter branch; recovery by `PlannerBinding`; the §5.1 lifecycle (boot revoke-then-sweep before the listener, first-turn mint + stop, caller list incl. repoint); `--claude-planner-config`; model surfaces (§5.8); steer pre-check; provider-named reader texts; the Claude prompt fragment | fake-`claude` stack test via real routes: create → image message → MCP call → interrupt → restart → resume → next turn → track delete; without the flag: create 4xx, recovered harness refuses | provider persists through start, reset and boot; an aborted deletion: with no new input the old token is rejected, then a queued input mints and the turn succeeds; a Claude reset whose post-commit step fails — constructed with a one-shot start-adapter fixture that fails the Claude arm of `spawn_side_effect` before `install()` (precedents `fail_next_thread_start_for_test`, the `post_eligibility_hook` in `harness/mod.rs`; not a D29 seam), with no boot in between: at the fixture point (after the reset commit, before the injected failure) the old token is rejected; after compensation restores the old row the old token is still rejected; then an input via `/api/cards/{id}/planner/input` hits the registry miss, `routes/cards.rs:1295` respawns, the first turn mints and succeeds (predicted red, assertion-level within this test, each alone: dropping the supersede null reddens the post-compensation assertion — the restored row keeps the old hash; dropping the mirror skip reddens the fixture-point assertion — the mirror copies the card's old hash onto the active `starting` successor; mutation 2 also reddens the post-boot reset must-red below, since boot nulls session hashes but leaves `card_mcp_tokens`; the implementer enumerates the full-suite red set per mutation before running it; the fixture is an awaitable pause (oneshot or barrier) so the test can run a handshake between the reset commit and the injected `Err` — `post_eligibility_hook`, a sync `Fn` at `harness/mod.rs:456`, is a precedent for placement only); a first-turn mint on a row superseded after the carrier check returns `Err` and writes nothing; a reconnect with the old token from listener start fails; a superseded Claude row with a live marked process has none after boot; repoint of a Claude track with a live marked `setsid` child leaves no marked process by the recycle (and takes Dirty when `stop` is forced to fail via the seam); crash before the `:3882` commit ⇒ re-drain, after it ⇒ `interrupted` without re-drain, a settled outcome is preserved; reset-commit → crash → recover → delete leaves no predecessor process; after reset during a running turn no old-marker process is alive when the reset returns, and the reset/replay leaves no instructions file; delete while a spawn is held between seal check and spawn does not complete while a marked process lives; with the seam armed, delete aborts, the reinstalled harness is Live, its first turn returns `Err` until the seam clears, and no marked process is left after the seam clears and a turn runs; reset with a seam-failed `stop` then delete (no boot in between) leaves no old-id marked process before the move; a seam-failed repoint then a retried repoint leaves none before the recycle; after boot, a reset of a Claude card whose harness was not reinstalled never accepts the old token; an install-race loser with a queued input: `shutdown` returns within a bound and signals nothing; a seam-failed first-turn setup with a queued input: `turn_start` returns `Err` within a bound, the input stays queued, and it is issued once the seam is cleared (no dead handle); shutdown after install but before the first mint, then a replacer installs and mints, then the stale harness's tick: the replacer's token still authenticates and the card hash is unchanged (mutation: mint moved before the issuance lock, `:2958`, or its `:2959` `shutting_down` check); Codex daemon down at boot still recovers Claude |
| **PR5 FE selection** | provider choice in new-track / first-message flows; selection reset; neutral label | FE gates + browser test against the fake stack + real-browser preview | switching provider clears a retained model/effort |

PR1, PR2a, PR2b, PR3 are parallel; PR4 needs all; PR5 needs PR4. The pain point is solved at PR4 + PR5
once the release gate passes.

### 9.2 Release gate

The Claude Planner is unavailable until the owner writes `--claude-planner-config`, which requires, on the target
host with `socat` installed and the exact shipping flags, recorded against `claude_version`:
Bash write inside cwd succeeds, outside cwd and session temp fails; the `neige` CLI reaches
`NEIGE_MCP_SOCKET`; a sandboxed `curl` to an arbitrary host succeeds; a Bash write outside the workspace is denied; out-of-cwd `Edit`/`Write` denied; `calm` MCP
works; removing `socat` fails closed; SIGKILL of `claude` mid sandboxed `sleep 300` leaves no survivor
after `stop`; the shim sees `NEIGE_MCP_SOCKET`/`NEIGE_MCP_TOKEN` expanded from the JSON-string
`--mcp-config` (else switch to a 0600 config file per spawn); `ToolSearch`-loaded calm tool schemas are
still callable after `--resume` (else measure the per-turn reload). Plus live acceptance on the owner's box (never Codex E2E): report write, worker dispatch,
image message, interrupt, restart, resume, delete; `ps` shows no Planner `claude` between turns.

### 9.3 KNOWN GAPS (one line each)

- An MCP connection a surviving (escaped or unstoppable) process established before revocation stays
  authorized while its session row is active (established calls check activity only).

- `stop` only sees processes carrying the exact marker: descendants that turn non-dumpable, rewrite or scrub
  their environ (e.g. headless Chromium), or leave the service cgroup escape it; deletion is fenced only
  up to that.
- Sandboxed Bash can connect to every Unix socket the neige user can (Linux has no per-path list), e.g.
  `/var/run/docker.sock` when the user is in `docker` ⇒ an escape; same as a Codex `workspace-write`
  Planner with network.
- The auto-updater may delete an old versioned binary; `claude_binary` then fails the readiness check until
  the owner updates the config (and re-runs the release gate for `Sandbox`).

- No steer; future path via `interrupt_cancel_queued_v1`, unprobed.
- No effort choice; the model is one of the fixed aliases or the CLI default (#1810, §5.8).
- No bound `Recover`; exact `calm.plan.recover`.
- Every deploy restart loses in-flight Claude turns (Codex's daemon survives restarts).
- A crash between the line write and the `:3882` commit may deliver a batch twice (same window as Codex).
- Started tool lines of a lost turn stay "running" in the FE; the outcome line is correct.
- Long-lived Bash processes (e.g. a dev server for `calm.preview.register`) die at turn end, at restart,
  and may be unreachable under the sandbox's network namespace; the prompt fragment says to use
  `calm.terminal.open`.
- Token usage once per turn; Codex-derived baseline; rate-limit events dropped.
- No plan / `webSearch` / `imageView` / `contextCompaction` rows.
- Moved workspace cannot resume (#857 class).
- First turn dying before `system/init` leaves `agent_session_id` unset; a second `--session-id` may be
  refused (UNVERIFIED) ⇒ reset.
- Oversized image or instruction rendering fails the turn / spawn.
- After compaction, the current instruction rendering applies.
- `Lagged` may delay the harness state (the outcome is durable; the Codex channel has the same risk, H23).
- Planner card `kind: "codex"` is a legacy view name.
- `ToolSearch` schema reload per resume unmeasured.
- A tool_result line carrying several results loses the per-result structured payload (never recorded).
- Plugin calm tools keep the Claude spelling `mcp__calm__<sanitized>` in the transcript: the dotted name is
  restored only from the kernel's Planner tool descriptors (`claude_planner/config.rs:106-111`,
  `claude_planner/translate.rs:305-311` keeps an unknown name and warns).

### 9.4 Risks and conflicts

- **Undocumented protocol drift** (input lines and control envelopes are Agent-SDK internal): v1 writes
  only `user` lines and `interrupt`; fixtures come from the recorded version; the Planner runs a
  pinned versioned binary whose version is checked before any input (D18).
- **#1542**: PR4 changes the reader texts at `run_loop.rs:2430`/`:2547` that #1542 also touches.
- **#1727**: S1–S3 and S4 slices 1–4 are in the base; S4 slice 5 (`TaskDeclaration.base`,
  `operation/workspace_lease/base.rs:17-21`) is not — expect textual conflicts in `planner.md`, goldens and
  migration numbering. **#1785** slice 1 (#1790) is in the base.
- **#857**: same failure class for moved workspaces.

### 9.5 Cuts (recorded, not built)

Resident process per harness; approval responder; **steer** (r2); **provider-neutral prompt wording**
(r2, formerly PR7). The Claude model catalog and create/PUT validation were cut in r2 too, and were
later delivered by #1810 (§5.8). Also cut: the r1 journal, recorded process
identity, process-wide registry and provider-keyed seal API (r2); Claude for PlainChat/Assistant; SDK-served
`Recover`; streaming deltas, per-request usage, rate-limit surfacing, todo → plan; renaming the card kind;
a provider-neutral item model.

### 9.6 Owner decisions

- **Auth/isolation:** dedicated `CLAUDE_CONFIG_DIR` (`config_dir`, required); the owner runs `/login` in it.
- **Confinement:** sandbox always on (no unconfined mode); the release gate is installing `socat` and passing §9.2 under the pinned
  `claude_version`.
- **Network:** unrestricted (parity with Codex) via `WebFetch(domain:*)`.
