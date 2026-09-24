# Claude Code as a second Planner backend (#1791) — design v2

> **Status (2026-09-24)**: v2, revised after review round 1 (two channels, both REVISE; every finding
> dispositioned in the [evidence companion §E4](1791-claude-planner-backend-evidence.md#e4-review-round-1-dispositions)).
> Base `origin/main` = `c5abb3c3d`. Paths without a prefix are under `crates/calm-server/src/`.
> Fact ids (`H*`, `S*`, `M*`), the 4140 queries (`Q*`) and probes (`P-*`) live in the companion
> [1791-claude-planner-backend-evidence.md](1791-claude-planner-backend-evidence.md); every file:line
> there was read on the base above. Probes: claude 2.1.280, `claude-haiku-4-5`, scratch only, never the
> production server or MCP socket.
>
> Vocabulary: `scripts/gate-1316-terminology-ratchet.sh` counts retiring words in `docs/`, so this
> document says "transcript table / transcript row" for the provider-item table of migration 0031.

## 0. Owner rules (binding for every slice)

1. **Don't expand without limit — solve the pain point, simple first.** Fewest mechanisms that give a
   working Claude Planner; hypothetical cases become one-line KNOWN GAPS; cuts are recorded (§9.5).
   Correctness defects (provider persistence, deletion fencing, orphan processes, turn identity across
   a crash, spawn serialization) are fixed with the fewest mechanisms, reusing existing precedents.
2. **Compatibility means only what is in the 4140 DB** (numbers and queries: companion §E2).
3. **Repo `AGENTS.md` applies**: required types over `Option`, typed config over ambient env, explicit
   env allowlists for child processes, no silent fallbacks, byte-frozen migrations, files ≤ 800 lines.
   `fe/AGENTS.md` applies to the FE slice.

## 1. Problem, goal, non-goals

**Pain point.** The Planner can only run on the Codex app-server; the owner wants a Planner on Claude Code.

**Goal (v1).** A track can be created with a Claude Planner that receives the same queued observations
and user messages (images included), calls the same `calm.*` MCP tools and `neige` CLI, can be
interrupted, survives a neige restart by resuming its Claude session, is fenced by track/card deletion
like a Codex Planner, and renders in the existing conversation UI with no FE renderer change.

**Non-goals.** ACP, a Node sidecar, the Agent SDK; a provider-neutral item model (rows stay
Codex-shaped); Codex behaviour changes beyond extracting the seam; worker cards; Claude for the
PlainChat / Assistant profiles (they stay Codex).

### 1.1 Decision log

| # | Decision | Round | Where |
|---|---|---|---|
| D1 | Seam = `enum PlannerBackend { Codex(..), Claude(..) }` plus a process-wide `PlannerBackends` handle (Codex daemon + Claude runtime registry) for code that has no harness | v1, widened r1 | §4 |
| D2 | The Claude backend emits **Codex-shaped `Notification`s** into a per-session broadcast; `on_notification`, persistence and the FE stay unchanged | v1 | §4.3 |
| D3 | Provider = required, server-owned, sticky card payload key `planner_provider`, backfilled by one migration; the session row gets its provider from an explicit mint input (not from the kind) | v1, fixed r1 | §4.4 |
| D4 | **One `claude -p` process per turn** (`--session-id` first, `--resume` after); at most one process per session at any time; resident keepalive only as a later optimisation if measured latency matters | v1, **orchestrator r1 (D-Q1)** | §5.1 |
| D5 | Permissions are static CLI rules (`--permission-prompts none` + `--allowedTools`), no approval responder | v1 | §5.3 |
| D6 | Auth/isolation: dedicated `CLAUDE_CONFIG_DIR`, the user logs in with Anthropic's own flow; neige never reads or copies credentials | v1, owner Q1 | §8 |
| D7 | `Recover` not offered; unregistered thread ⇒ existing exact-interface briefing | v1 | §5.7 |
| D8 | Steer: refused in v1; **scheduled** as PR5 right after end-to-end wiring, reusing the projection-upgrade and `restore_steered_entries_codex_dropped` path | **orchestrator r1** | §5.9 |
| D9 | Bash confinement: Claude Code's native sandbox with `failIfUnavailable`; enabling the Claude Planner is **release-gated** on sandbox parity with Codex `workspace-write` or explicit owner acceptance | **orchestrator r1 (Q3)** | §5.3, §9.2 |
| D10 | Images: bound attachments become Claude base64 image blocks (P-I) | **orchestrator r1** | §5.6 |
| D11 | Model surfaces handled separately: GET `/api/models`, create-time advice, PUT validation | **orchestrator r1** | §5.8 |
| D12 | Durable per-turn journal in the runtime dir = turn identity before submission + orphan identity + settlement marker | r1 (B7/B8/A2) | §5.1 |
| D13 | Project config (`--setting-sources project`, CLAUDE.md or AGENTS.md) is trusted to the same degree Codex trusts the workspace (`trust_level = "trusted"`, S25); 4140 has no project `.claude/settings.json` or `.mcp.json` (Q13) | r1 (B12) | §5.2 |

## 2. Evidence summary

The complete fact table, gate table, queries and probe log are in the companion (§E1–§E3). The facts
that shape the design:

- The harness is one task consuming one `broadcast::Receiver<Notification>`; all state transitions and
  persistence are written against Codex frames (H4–H10).
- Provider identity of a runtime row is derived from its kind only; `(Claude, Planner)` is unmappable
  today (S12, S13). Harness pushes key on the SharedPlanner kind (S14), so the kind must stay.
- Deletion seals live on the shared daemon and outlive the harness (S21); production restarts signal
  only calm-server (`KillMode=process`, S22), so children survive a restart in the cgroup.
- The issuing snapshot is persisted before the provider call, `last_turn_id` only after it (H22).
- Model/effort are read on three separate surfaces (S11); attachments are sent as `localImage` items
  and advertised per workspace, not per provider (S26).

## 3. 4140 compatibility (summary; queries in §E2)

| Fact | Number | Consequence |
|---|---|---|
| transcript rows | 4863 (13 cards/threads), types all Codex (Q1–Q2) | nothing to migrate; Claude emits only types the FE already renders |
| turn outcomes | completed 117, interrupted 5, failed 1 (Q3) | Claude outcomes reuse these three |
| `dynamicToolCall` rows / open projections | 0 / 0 (Q5, Q6) | free type for Claude-native tools; no drain in flight |
| Planner cards | 24 codex (23 marker + 1 legacy shape) (Q7, Q11) | backfill `planner_provider:"codex"` on `role='planner'` |
| runtime rows | no `(claude, planner)` row (Q8) | the new identity has no legacy rows |
| Planner operations | 122, all succeeded (Q9) | no replay observes a payload change |
| create idempotency bindings | 45 (v1: 38, v0: 7) (Q12) | the create digest must stay byte-identical for Codex requests |
| workspaces | 20; 2 with only `AGENTS.md`; no `.claude/settings.json` / `.mcp.json` (Q13) | AGENTS.md parity matters (verified P-J); project hooks are hypothetical |

## 4. The seam

### 4.1 Every Codex-coupled call on the Planner paths

`git grep -n "daemon\.\|shared_codex_appserver\.\|turn_daemon\|DeletionThreadSeals\|catalog_advice\|model_list"` over
`harness/ operation/planner_harness_* routes/ state.rs lib.rs`:

| # | Call | Site | Decision |
|---|---|---|---|
| 1 | `subscribe_notifications` | run_loop `:404` (H2) | seam |
| 2 | `turn_start` | run_loop `:271` (H3) | seam |
| 3 | `turn_steer` | run_loop `:1339` (H12) | seam (`supports_steer` pre-check, §5.9) |
| 4 | `turn_interrupt` | run_loop `:704`, `:3640` | seam |
| 5 | `interrupt_active_turn` | run_loop `:691` | seam |
| 6 | `active_turn_id_for_thread` | run_loop `:690`, `:3601` | seam |
| 7 | `seal_turn_thread_for_deletion`, `DeletionThreadSeals` | run_loop `:658-679`; registry `shutdown_track` `:203-218` | `PlannerBackends` (§5.11) |
| 8 | `turn_thread_is_sealed` | harness/mod `:158`, `:424` | `PlannerBackends::is_sealed(provider, thread)` |
| 9 | `config_read`, `model_list` (resolution) | run_loop `:2472`, `:2632` | Codex-only; Claude arm never calls them (§5.8) |
| 10 | `readiness_receiver` / deferred recovery | harness/mod `:213`, `:477`; `state.rs:577-598`; `lib.rs:615-640` (H21) | Codex rows only; Claude rows recovered in both boot arms (§4.4) |
| 11 | `is_running` / `not_running_*` | start preflights `:420`, `:491` (S7); routes/cards `:1289`; planner_recovery `:58` (S10) | per provider: Codex checks the daemon, Claude checks `claude_bin` readiness |
| 12 | `thread_start_*`, `remote_uri` | start adapter `:970-991` | branch: Claude mints a UUID thread, no RPC |
| 13 | compensation `interrupt_thread` | start adapter `:1424-1430` (S8) | branch: Claude ⇒ `PlannerBackends::quiesce(provider, thread)` |
| 14 | `PlannerHarnessParams{daemon}` | start adapter `:1277`; harness/mod recovery; ~70 test sites (H20) | `backend: PlannerBackend` (`From<Arc<SharedCodexAppServer>>` keeps tests mechanical) |
| 15 | shutdown adapter registry-miss path | `operation/planner_harness_shutdown_adapter.rs:109-135` | branch: Claude ⇒ quiesce the runtime's process group |
| 16 | card teardown / deletion-grade quiesce | routes/cards `:100-122`, `:128-156` | `PlannerBackends` by the runtime row's provider |
| 17 | track / area deletion plans (`turn_daemon`, seal, unseal on rollback) | tracks `:128`, `:2677`, `:2870-2956`, `:2991-3009`, `:3160`; areas `:319`, `:398`, `:489-623`, `:652-663`, `:814` (S21) | field becomes `PlannerBackends`; seals carry `(provider, thread)` |
| 18 | `resume_system_error_conversation` | planner_recovery route | Codex-only (Claude never enters `Wedged(systemError)`, §6.2) |
| 19 | GET `/api/models` `model_list` | `routes/models.rs:144` | per provider (§5.8) |
| 20 | create-time `catalog_advice` | `routes/tracks.rs:848-850` | per requested provider (§5.8) |
| 21 | PUT `/planner/model` `catalog_advice` | `routes/planner_model.rs:120` | per card provider (§5.8) |
| 22 | reader texts naming codex | run_loop `:2430`, `:2547` (H16) | provider name from the backend (overlaps #1542, §9.4) |
| 23 | `liveness_feeder`, dev replay, TUI initial-prompt takeover | S27 | Codex-only: harness Planners take liveness from transcript rows; replay is dev; takeover is for TUI Planners |

PlainChat and Assistant stay Codex everywhere: their rows never carry `planner_provider`, the profile
gate (§4.4) only consults the key for `CardRole::Planner`.

### 4.2 Types

```rust
// harness/backend.rs (new, ≤ 300 lines)
#[derive(Clone)]
pub enum PlannerBackend { Codex(Arc<SharedCodexAppServer>), Claude(Arc<ClaudePlannerSession>) }
impl PlannerBackend {
    pub fn subscribe_notifications(&self) -> broadcast::Receiver<Notification>;
    pub async fn turn_start(&self, thread: &str, items: Vec<InputItem>,
        selection: &TurnModelSelection, client_id: &str) -> Result<String>;
    pub fn supports_steer(&self) -> bool;
    pub async fn turn_steer(&self, thread: &str, turn: &str, items: Vec<InputItem>, client_id: &str)
        -> Result<String>;
    pub async fn turn_interrupt(&self, thread: &str, turn: &str) -> Result<()>;
    pub async fn interrupt_active_turn(&self, thread: &str) -> Result<()>;
    pub fn active_turn_id_for_thread(&self, thread: &str) -> Option<String>;
    pub fn provider(&self) -> PlannerProvider;
    pub fn codex(&self) -> Option<&Arc<SharedCodexAppServer>>;   // the explicit Codex-only door
}
// state.rs: one per server, cloned into routes, recovery and deletion plans
#[derive(Clone)]
pub struct PlannerBackends { pub codex: Arc<SharedCodexAppServer>, pub claude: Arc<ClaudePlannerRuntimes> }
impl PlannerBackends {
    pub fn for_runtime(&self, provider: PlannerProvider, runtime: &WorkerSessionProjection)
        -> Result<PlannerBackend>;
    pub fn seal(&self, provider: PlannerProvider, thread: &str);
    pub fn unseal_after_rollback(&self, provider: PlannerProvider, thread: &str);
    pub fn is_sealed(&self, provider: PlannerProvider, thread: &str) -> bool;
    pub async fn quiesce(&self, provider: PlannerProvider, thread: &str) -> Result<()>; // confirmed stop
    pub fn is_ready(&self, provider: PlannerProvider) -> std::result::Result<(), String>;
}
```

`client_id` becomes `&str`: every harness issuance already passes `Some` (run_loop `:3208`, `:1339`).
Enum instead of trait: two variants known at compile time; the Codex fixtures fake stays untouched;
every Codex-only call site is a visible `match`, not a silent trait default.

### 4.3 Notification stream: synthesize Codex shape (D2)

(b) "neutral event enum" would rewrite `on_notification` (~400 lines of fenced transitions, H5–H9) and
the FE converters (M10) — an explicit non-goal. (a) is honest because the stored shape was always
"the item the transcript renders", not a raw provider log: Codex rows are already filtered (H9),
projection rows are kernel-written in the same shape (H8), every row's provider is recoverable through
`worker_session_id` → `worker_sessions.provider`, and Claude's raw record stays in its own session file.
The Claude backend emits only `TurnStarted`, `TurnCompleted`, `Item{item/started|item/completed}` and
`Other{thread/tokenUsage/updated}`. It writes exactly one kind of row itself: the durable turn outcome
(§5.1, via `turn_outcome::record`, idempotent with the loop's own write, H10).

### 4.4 Provider identity (D3)

- **Card key.** `planner_provider: "codex" | "claude"` → `enum PlannerProvider` (serde, deny unknown).
  New migration (next free number at merge; #1785's design names 0116) backfills
  `json_set(payload,'$.planner_provider','codex')` for `role='planner'` (24 rows, Q7/Q11). Added to
  `SERVER_OWNED_CARD_PAYLOAD_KEYS` **and** to `server_owned_value_is_sticky` with `=> true` (the
  `template_context` precedent: a corrupt stored value stays and fails the gate visibly) (S19).
  Tests: a replacement payload that omits the key keeps it; a client payload carrying it is refused;
  a corrupt stored value survives an update and makes the card not-a-harness.
- **Profile gate.** `HarnessProfile::from_shape` keeps `kind == "codex"`; for `CardRole::Planner` it also
  requires `PlannerProvider::from_payload` to parse (missing/unknown ⇒ `None`, fail closed).
  `card.kind` stays `"codex"`: `kind = "claude"` would make the card look like a Claude PTY worker to
  `claude_restart_adapter`, the Claude hook routes, `worker_flow` and the FE PTY builtin while the five
  FE Planner predicates and `ratify_card` stop recognising it (S2, S16, S17). KNOWN GAP: a legacy view name.
- **Session identity (fixes the v1 error).** Replace `derive_session_identity(kind)` at the mint site
  (`session_mirror.rs:58`) with `session_identity(kind, agent_provider) -> Result<(provider, mode,
  contract)>`: `(SharedPlanner, Some(p))` ⇒ `(p, Resumable, Planner)`; `(SharedPlanner, None)` ⇒ error;
  other kinds keep today's mapping and reject a contradicting provider. `runtime_kind_from_session_identity`
  gains `(Claude, Planner) → SharedPlanner` (S13). Query sites that derive the Planner provider
  (`session_projection.rs:160`, `:735`) select `provider IN ('codex','claude')` for the Planner contract;
  `read.rs:802` (TUI takeover) stays Codex. `session_refresh_deferred_placeholder_tx` /
  `session_prepare_deferred_planner_tx` pass the explicit provider. Must-red: minting
  `(SharedPlanner, Claude)` persists `provider='claude', mode='resumable'`; through start, reset and
  boot recovery.
- **Boot.** The boot SQL gains `OR (ws.provider='claude' AND ws.contract='planner' AND c.role='planner')`
  (pinning test updated). `recover_harnesses_after_daemon_boot` recovers Claude rows in both arms; the
  deferred Codex-readiness pass filters to Codex rows (H21). Recovery asserts row provider == card key;
  mismatch ⇒ skip + warn.
- **Create API.** `CreateTrackRequest.planner_provider: PlannerProvider` (required) and
  `CreateRequestShape.planner_provider`. The digest adds the field **only when `claude`** (the file's
  "preserve the exact pre-selection digest" precedent, S20), so the 45 bindings (Q12) replay unchanged and
  a Claude request never matches a Codex binding. Test: same key, different provider ⇒ conflict. Sweep:
  handwritten FE `NewTrackBody`, OpenAPI + `wire.ts` regeneration, `e2e/` (incl. `e2e/planner_claude_ux.py`),
  161 `/api/tracks` sites in 34 Rust files, and ~29 fixtures that insert a Planner card directly (after the
  gate change they would silently stop being harness cards — each gets the key). Fork inherits the source
  Planner's key; a child track opened by a Planner inherits its parent's.
- **Start adapter (Claude branch).** Fresh UUID thread id, no thread RPC, runtime dir, `AgentProvider::Claude`
  on the mint input, snapshot `phase = Idle` and `last_thread_id` as the Codex branch does at `:982-985`
  (`PendingThreadStart` only exits on `ThreadStarted`, which Claude never emits).

## 5. The Claude backend

New module `claude_planner/` (files ≤ 800 lines): `protocol.rs` (wire types), `translate.rs` (records →
notifications, pure), `session.rs` (one harness's turns), `runtimes.rs` (process-wide registry: seals,
journals, process identity), `config.rs` (typed config).

### 5.1 Process model (D4, D12)

**One process per turn.** `turn_start` spawns `claude -p …` (§5.2), writes one `user` line, and the
process lives until the turn settles; then stdin is closed. First turn of a runtime: `--session-id
<thread>`; later: `--resume <thread>` once `worker_sessions.agent_session_id` is set — set when the first
`system/init` of that session is observed (the column's meaning for Claude worker cards, resolved by
`resolve_claude_session_for_card`, `crates/calm-truth/src/session_projection_lookup.rs:58-69`).
Why per turn: resume after kill works (P-F2); a resident process per Planner needs an idle reaper
(24 idle Planners on 4140; channel A measured 250–440 MB RSS per long-lived `claude`); model/effort are
per-spawn flags, matching Codex per-`turn/start` selection. It does not remove supervision: readers,
timers, the journal and the boot sweep below remain. Costs per turn: startup + resume parse + MCP
handshake (~0.5–1.5 s to `system/init` in P-B/P-E) and whether `ToolSearch`-loaded schemas survive
`--resume` is UNVERIFIED (measure in PR2b; if not, set tool search off for `calm`).

**At most one process per session.** `LiveTurn` is cleared on child exit (`child.wait()`), not on
`result`. `turn_start` first awaits the previous child's exit for ≤ 5 s, then kills its group. Must-red:
a fake `claude` that lingers after `result` never overlaps the next spawn.

**Process group and identity** (precedent S23). Spawn with `process_group(0)`, `kill_on_drop(true)` and
the env marker `NEIGE_CLAUDE_PLANNER=<worker_session_id>`. Identity recorded:
`(pid, pgid, start_time, boot_id)` read via `proc_identity`. Stop = `signal_process_group(pgid, SIGTERM)`
→ ≤ 5 s → `sigkill_verified_group_members(pgid)` → wait until `group_members_with_env_marker` is empty.
This kills Bash grandchildren (builds, dev servers) too.

**Journal (D12).** `<runtime_dir>/turn.json` (0600, fsync + rename):
`{turn_id, client_id, pid, pgid, start_time, boot_id}`, written after spawn and **before** the `user`
line; removed only after the outcome is durable. It is the only new persistent state:

| Crash point | What recovery sees | Recovery action |
|---|---|---|
| before spawn | no journal; issuing snapshot with the batch still queued (H22) | existing re-drain under the same client id |
| after spawn, before journal | no journal; an idle orphan with no input | boot sweep by env marker kills it |
| after journal (line maybe written) | journal | kill the recorded group (identity-verified), record `interrupted` outcome for `turn_id` ("neige restarted before or during this turn"), remove journal; the queued batch is re-drained |
| after settlement | no journal | nothing |

The row "after journal" can deliver a batch twice (Claude may have accepted the line before the crash);
the same at-least-once window already exists for Codex between `turn/start` and the snapshot (H22).
KNOWN GAP, not a new mechanism.

**Settlement (B8).** Every live turn has one `TurnSlot { cause: Option<TerminalCause> }`. Any code path
that is about to interrupt or kill **first records its cause**; the terminal mapping (§6.2) uses the
recorded cause before the observed event. Settlement order: `turn_outcome::record` (durable, idempotent)
→ remove journal → emit `TurnCompleted`. So an outcome survives the loop being aborted by
`shutdown_inner` (H15) and a `Lagged` receiver. Stdout lines that fail to decode set cause
`Failed("protocol")` and stop the group; stdin writes have a 5 s bound (then the same).

**Boot sweep.** Before any Claude harness is installed: for every runtime dir with a journal, apply the
table above; then kill any remaining process carrying the env marker (production keeps children alive
across restarts, S22). Recovery also re-mints the card MCP token, so an orphan that escaped the sweep
cannot call `calm.*`. KNOWN GAP: unlike Codex (daemon survives via takeover), every deploy restart loses
in-flight Claude turns.

### 5.2 Spawn contract

```
<claude_bin> -p --input-format stream-json --output-format stream-json --verbose --replay-user-messages
  (--session-id <thread> | --resume <thread>)   [--model <m>] [--effort <e>]
  --setting-sources project --disable-slash-commands
  --tools Bash,Read,Edit,Write,ToolSearch,WebFetch,WebSearch
  --strict-mcp-config --mcp-config <runtime_dir>/mcp.json
  --permission-prompts none
  --allowedTools "Bash Read ToolSearch WebFetch WebSearch mcp__calm Edit(//<cwd>/**) Write(//<cwd>/**)"
  --settings <runtime_dir>/settings.json          # sandbox block, §5.3
  --append-system-prompt-file <runtime_dir>/instructions.md
```

- No `--permission-mode` (v1 passed the undocumented `default`; P-I/P-S ran without it). No
  `--include-partial-messages` (the FE never rendered deltas).
- `cwd` = track workspace path. Claude keys sessions by cwd; a moved workspace cannot resume (KNOWN GAP,
  #857 class).
- `<runtime_dir>` = `<data_dir>/claude-planner/runtimes/<worker_session_id>/`, 0700. `mcp.json`:
  `{"mcpServers":{"calm":{"type":"stdio","command":"<neige-mcp-stdio-shim>","args":[],
  "env":{"NEIGE_MCP_SOCKET":"${NEIGE_MCP_SOCKET}","NEIGE_MCP_TOKEN":"${NEIGE_MCP_TOKEN}"}}}}` — the
  token never touches disk or argv; `${VAR}` expansion verified (P-S1).
- `instructions.md` is re-rendered at **every** spawn with the same renderer the start adapter uses.
  Claude reuses its recorded system prompt on resume until the conversation is compacted (`claude --help`,
  `--system-prompt-snapshot`), so after compaction the current rendering applies — a small difference
  from Codex's "fixed at thread start".
- Project context: `CLAUDE.md`, or `AGENTS.md` when there is no `CLAUDE.md` (verified P-J) — parity
  with Codex for the 2 AGENTS.md workspaces (Q13). `--setting-sources project` also admits project
  settings/hooks; trusted like Codex trusts the workspace (D13); project settings cannot switch the
  sandbox filesystem layer off (sandbox docs).
- `--disable-slash-commands` ⇒ `skills: []`; `--tools` removes `Task`, `Cron*`, `ScheduleWakeup`,
  `RemoteTrigger`, `Workflow`, `EnterWorktree`, … (P-E).

**Environment** (`env_clear()` + allowlist): the Codex daemon's vetted `SPAWN_ENV_PASSTHROUGH` (S24)
minus `OPENAI_*`/`CODEX_*`/`RUST_*`/`LOG_FORMAT`, i.e. `HOME USER LOGNAME SHELL LANG LANGUAGE LC_ALL
LC_CTYPE TERM TZ TMPDIR TEMP TMP NO_PROXY no_proxy ALL_PROXY all_proxy SSL_CERT_FILE`; plus
`HTTP(S)_PROXY` from the same resolver as the daemon (`shared_codex_appserver.rs:1862`), `PATH` =
`kernel_led_path()` (so `neige` in Bash is the running kernel), `CLAUDE_CONFIG_DIR` (typed config
`--claude-planner-config-dir`), `NEIGE_MCP_SOCKET`, `NEIGE_MCP_TOKEN`, `NEIGE_CLAUDE_PLANNER`,
`DISABLE_AUTOUPDATER=1`. Never: `NEIGE_MCP_DAEMON_TOKEN` (the shim prefers it, M4), `ANTHROPIC_*`,
`CLAUDE_CODE_*`. The token is minted once per harness construction (M3) and held in memory.

**Init checks** (every spawn, from `system/init`): `session_id == thread` (no accidental fork);
`claude_code_version ≥ 2.1.280` (warn on another minor); `capabilities ⊇ {interrupt_receipt_v1}`;
`mcp_servers` has `calm` `connected`; `skills == []`; all `plugins[].source` end in `@builtin`. A failure
records cause `Failed(check)` then stops the group. Readiness (`is_ready`): `claude_bin` resolves.

### 5.3 Confinement (D5, D9) — and why it is a release gate

**Escalation without confinement.** The Planner reads untrusted text (WebFetch/WebSearch results,
plugin tool output, worker results). With unconfined Bash running as the neige user, a prompt injection
can write `calm.db` directly (bypassing the role gate and event log), modify other runtimes' state
dirs, read and exfiltrate Codex/Claude credentials, push to any remote, and kill neige processes. Codex
Planners run `workspace-write` (S5): reads anywhere, writes confined to the workspace, network on.
`Edit(//cwd/**)`/`Write(//cwd/**)` rules alone confine nothing while Bash is allowed.

**Mechanism.** One settings file, no code:
`{"sandbox":{"enabled":true,"failIfUnavailable":true,"allowUnsandboxedCommands":false,
"network":{"allowedDomains":[…owner list…],"allowUnixSockets":["<NEIGE_MCP_SOCKET>"]}}}`.
Documented semantics: sandboxed Bash may write the cwd and the session temp dir, reads everywhere
(same as Codex), network only to allowed domains; `autoAllowBashIfSandboxed` defaults on;
`allowUnsandboxedCommands:false` removes the escape hatch; MCP servers are not Bash and are unaffected.
Evidence on this host (`bwrap` present, `socat` absent):

- P-S1: with `failIfUnavailable:true` the turn **fails before any request** ("Sandbox required but
  unavailable … socat not installed") — fail-closed, surfaces as a `failed` outcome.
- P-S2: without it the CLI prints a stderr warning and **runs every command unsandboxed** (writes outside
  cwd and `/tmp`, Unix socket, HTTP 200). So `failIfUnavailable:true` is mandatory.

The path-scoped `Edit`/`Write` allow rules are kept **only together with** the sandbox (they confine the
file tools; the sandbox confines Bash). With the owner's "accept unconfined" choice (§9.2) the Edit/Write
rules are dropped and the plain rules `Bash Edit Write` are used, since partial rules give false comfort.

**Release gate (§9.2).** The Claude Planner is off unless typed config
`--claude-planner-confinement=sandbox|accept-unconfined` is set. `sandbox` requires the release-gate
probe list to pass on the host; `accept-unconfined` is the owner's explicit acceptance of the escalation
above. There is no default.

### 5.4 Protocol types (`protocol.rs`)

Pinned to recorded 2.1.280 fixtures; unknown `type`/`subtype` ⇒ `Ignored{kind}` (debug log), never a guess.

```rust
// stdin
struct UserLine { r#type: Const<"user">, message: UserMessage, parent_tool_use_id: Null,
                  session_id: String, uuid: Uuid }
struct UserMessage { role: Const<"user">, content: Vec<UserBlock> }        // text | image(base64)
struct ControlRequest { r#type: Const<"control_request">, request_id: String, request: ControlBody }
enum ControlBody { Interrupt }                                             // {"subtype":"interrupt"}
struct ControlResponseOut { r#type: Const<"control_response">, response: ControlResponseBody }
enum ControlResponseBody {                                                 // nested envelope (P-F3)
  Success { request_id: String, response: Value },   // can_use_tool ⇒ {"behavior":"deny","message":…}
  Error { request_id: String, error: String },       // any other inbound request subtype
}
// stdout
enum Record {
  SystemInit { session_id, claude_code_version, model: String, capabilities: Vec<String>,
               mcp_servers: Vec<McpStatus>, skills: Vec<String>, plugins: Vec<PluginRef> },
  UserReplay { uuid: Uuid, is_replay: True, message: UserMessage },
  User { uuid: Uuid, message: ToolResultBlocks, tool_use_result: Value },   // per-tool shape (P-E)
  Assistant { uuid: Uuid, message: AssistantBlocks },                       // thinking | text | tool_use
  Result { subtype: ResultSubtype, is_error: bool, result: Option<String>, errors: Vec<String>,
           usage: Usage, model_usage: BTreeMap<String, ModelUsage>, terminal_reason: Option<String> },
  ControlResponseIn { response: ControlResponseBody },
  ControlRequestIn { request_id: String, request: Value },
  Ignored { kind: String },
}
```

`Option` only where the wire omits the value (`result: null` on `error_during_execution`, P-F3;
`terminal_reason` absent there). `tool_use_result` stays `Value` because its shape differs per tool;
`translate.rs` reads typed views per tool name. Client ids (32 hex) are sent as the dashed UUID of the
same 128 bits and mapped back. With `--permission-prompts none` no `can_use_tool` is expected (P-H,
P-I); if one arrives it gets a `Success` deny, anything else an `Error` response.

### 5.5 MCP wiring and tool names

Shim, socket, card-bound token and role resolution are reused (M3–M5). Claude names tools
`mcp__calm__<name with [^A-Za-z0-9_-] → _>` (documented; P-B) and the server receives the original
name. `translate.rs` restores the dotted name from the card's visible tool list (the `tools/list` answer,
M10) via Claude's sanitizer; ambiguous or unknown ⇒ keep the Claude name + warn (visible as
"Called mcp__calm__…"). `model_tool_key` already accepts Claude spellings (M7).

### 5.6 Input, including images (D10)

The drained batch arrives as `InputItem`s: text + `localImage{path}` per bound attachment (S26). The
backend sends one `user` line whose content is `[{type:"text"}, {type:"image", source:{type:"base64",
media_type: AttachmentFormat::mime(), data}}…]` — verified (P-I: a 32×32 red PNG answered "Red."). All
four attachment formats are image types the API accepts. The replay echoes the base64 back; the
translator stores the `userMessage` item with text plus `{type:"localImage", path}` placeholders, never
the base64 (rendering reads the projection row's `input_segments`, H8). An image-only message is a valid
line (test). KNOWN GAP: an attachment above the API's per-image limit fails the turn with the API's
message.

### 5.7 Recover (D7)

Claude threads are never registered (`register` is only called on the Codex `thread/start` path, M9), so
`registered(card, thread)` is false and the briefing uses the exact interface (`calm.plan.recover` with
`key`, `expected_attempt_id` and a Planner-kept `idempotency_key`, H17). KNOWN GAP: no bound `Recover`.

### 5.8 Model and effort (D11)

| Surface | Codex (unchanged) | Claude |
|---|---|---|
| turn resolution (`resolve_model_selection`) | payload + `config_read`/`model_list` for defaults (H16) | card's explicit values only → `--model`/`--effort`; omitted ⇒ Claude default for that spawn; no Codex call |
| GET `/api/models` (`routes/models.rs:144`) | Codex catalog | `?provider=claude` (no card yet) or a Claude card ⇒ Claude catalog; PR4: empty catalog with `model_choice: "unavailable"`; PR7: from a zero-token `initialize` round trip (`models[{value, supportedEffortLevels}]`, P-A), cached per `claude_code_version` |
| create-time advice (`routes/tracks.rs:848`) | Codex advice | PR4: `claude` + any `model`/`reasoning_effort` ⇒ 400; PR7: validated against the Claude catalog |
| PUT `/planner/model` (`routes/planner_model.rs:120`) | Codex advice | PR4: 409 for Claude cards; PR7: validated against the Claude catalog (`low…max`) |

FE: switching the provider in the create dialog clears a retained model/effort choice (PR6 test).

### 5.9 Steer (D8, scheduled PR5)

v1 (PR4): `handle_steer` checks `supports_steer()` **before** taking the entry and answers
`SteerRefused::NotTaken{ "this Planner's provider (Claude) cannot take messages into a running turn;
it stays queued" }` — id and rev unchanged.

PR5 reuses the existing path: `turn_steer` writes the entry as a second `user` line (its own `uuid` =
the entry's client id) into the live process and returns the running turn id; the projection row is
written as today (`insert_projection_row`). Consumption = the `--replay-user-messages` echo, which
upgrades the projection (H8) — exactly Codex's "recorded at its next model request". Unconsumed at
`result` (text-only turn, P-E/P-F3) ⇒ the slot records cause `SteerUnconsumed` and stops the group so the
CLI cannot run it as an unsolicited turn; the completion sweep `restore_steered_entries_codex_dropped`
(H12) then restores the entry because its projection was never upgraded. The outcome of that turn is
its real `result` status (the stop happens after `result`).

### 5.10 Token usage

On a `result` with a non-empty `usage.iterations`: emit `Other{"thread/tokenUsage/updated", {threadId,
tokenUsage:{last:{totalTokens: input+cache_read+cache_creation+output of the last iteration},
total:{totalTokens: previous total + this turn's sum}, modelContextWindow:
modelUsage[init.model].contextWindow}}}`. Empty `iterations` or no entry for `init.model` (P-D, P-F3)
⇒ emit nothing; the snapshot keeps the previous reading (H11). The running total is seeded from the
snapshot's `token_usage.total_tokens` (passed at construction); it is not shipped to the FE (H11).
KNOWN GAP: `BASELINE_TOKENS` is Codex-derived (Claude's fixed prefix is ~18k in P-B), so the percentage
is slightly high.

### 5.11 Deletion, seals and quiesce

`ClaudePlannerRuntimes` (process-wide, in `PlannerBackends`) owns: the seal set `{thread}` (outlives any
harness, like the Codex daemon's), the journal/identity per runtime dir, and `quiesce(thread)`:
seal → record cause `Interrupted(shutdown)` on a live slot → stop the group (§5.1) → wait until no member
carries the marker (bounded; else `Err`, and deletion aborts as it does for a failed Codex interrupt)
→ settle the journal. `ClaudePlannerSession` consults the registry's seal set in `turn_start` and before
every spawn. Deletion plans carry `PlannerBackends` and seal `(provider, thread)`; rollback unseals the
same pair. Registry-miss paths (shutdown adapter `:109-135`, card teardown with no live harness,
compensation `:1424`) call `quiesce` by the row's provider, so an orphan without a harness is still stopped.

## 6. Mapping

### 6.1 Claude record → notification → stored item

Envelope: `{threadId, turnId, item, startedAtMs | completedAtMs}`. Item ids: `tool_use.id` for tools;
`<record uuid>:<block index>` for text/thinking (one record could carry two blocks).

| Claude record | Notification | Stored `item` |
|---|---|---|
| our `user` line written | `TurnStarted{turn:{id}}` (handled after `turn_start` returns, H4/H6) | — |
| `UserReplay` with our uuid | `item/completed` `userMessage` | `{id, clientId, type:"userMessage", content:[text, localImage…]}` → upgrades the projection (H8) |
| `UserReplay` `<local-command-stdout>…` | none | — |
| `thinking` block | `item/started` + `item/completed` `reasoning` | `{id, type:"reasoning", content:[], summary:[]}` |
| `text` block | `item/started` + `item/completed` `agentMessage` | `{id, type:"agentMessage", text}` |
| `tool_use mcp__calm__*` | `item/started` `mcpToolCall` | `{id, server:"calm", tool:<dotted>, arguments, status:"inProgress"}` |
| its `tool_result` | `item/completed` `mcpToolCall` | `status` "completed" or "failed", `result{content}` or `error{message}`, `durationMs` |
| `tool_use Bash` / its result | `commandExecution` started / completed | `command, cwd, status, aggregatedOutput`; `exitCode` = 0 if not `is_error`, N from a leading `Exit code N` (P-E), else null; `durationMs` |
| `tool_use Edit`/`Write` / result | `fileChange` started / completed | `changes:[{path, kind:{type: add or update}, diff}]`, `status` |
| `tool_use Read`/`WebFetch`/`WebSearch` / result | `dynamicToolCall` started / completed | `{tool:<Claude name>, arguments, status}` (Q5: no legacy rows) |
| `tool_use ToolSearch`; `[Request interrupted…]`; `system/*`; `stream_event`; `rate_limit_event`; `command_lifecycle` | none | — |
| `result` | usage (§5.10), then durable outcome + `TurnCompleted` (§6.2) | outcome row (H10) |

Not produced: `turn/plan/updated`, `webSearch`, `imageView`, `contextCompaction` (KNOWN GAPS).

### 6.2 Terminal cause × event → outcome (exactly one per turn)

Precedence: a **recorded cause** wins over the event that follows it; otherwise the first event wins.

| Recorded cause | Event | `status` | `error.message` |
|---|---|---|---|
| none | `result` success, `is_error:false` | completed | — |
| none | `result` success, `is_error:true` (e.g. not logged in, P-D/P-G) | failed | `result.result` |
| none | `result` `error_during_execution` (e.g. sandbox unavailable, P-S1) | failed | `errors` joined |
| none | `result` other `error_*` subtype | failed | subtype |
| none | EOF + exit before `result` (crash, OOM, SIGINT P-F1) | failed | `claude exited (<status>)` + last stderr line |
| `Interrupted(user\|watchdog\|shutdown)` | `result` `error_during_execution`, EOF, or stop timer | interrupted | — |
| `Interrupted(…)` | `result` success (race: finished first) | completed | — |
| `Failed(check \| protocol \| stdin)` | any | failed | the cause |
| `SteerUnconsumed` (PR5) | the `result` that preceded the stop | per that `result` | per that `result` |
| journal found at boot | — | interrupted | "neige restarted before or during this turn" |
| spawn / first write fails | — | **no turn**: `turn_start` returns `Err` ⇒ existing retryable refusal, re-buffer, 2 s pacing, reader told after 30 s | — |

An interrupted Claude turn settles well inside the harness's 30 s interrupt budget (stop timer ≤ 10 s),
so `Wedged(interrupt_timeout)` is only reachable if the kill itself hangs. Claude never emits
`ThreadStatusChanged`, so `Wedged(systemError)` and the systemError recovery route do not apply.

## 7. Oracle trace

`NEW` = introduced here. Locations per the companion; "existing" = unchanged carrier.

| seq | phase | actor | trigger | external effect | observable event | invariant | status |
|---|---|---|---|---|---|---|---|
| 1 | create | user | `POST /api/tracks {…, planner_provider:"claude"}` | card `kind:"codex"`, key `claude`; digest includes the key | `card.added` (`routes/tracks.rs:1423`) | key required, sticky; Codex digests unchanged | NEW |
| 2 | start | kernel | `planner-harness-start` | row `provider='claude', contract='planner', mode='resumable'`; UUID thread; token minted; phase Idle; no process | op phases (`operation/planner_harness_start_adapter.rs:840`) | provider from the mint input | NEW |
| 3 | message | user | `POST /api/cards/{id}/planner/input` (+ image) | entry persisted before 200 | `harness.user_message.enqueued` (`routes/cards.rs:921`) | durable before ack | existing |
| 4 | drain | run loop | tick | projection row; spawn in its own process group; journal; one `user` line (text + image block) | `harness.item.added` (projection) | journal before line | NEW spawn |
| 5 | turn begins | backend | line written | — | `TurnStarted` → `TurnRunning` | issued id == turn id | NEW producer |
| 6 | echo | CLI | `UserReplay` | projection upgraded; base64 not stored | `harness.item.added` | no duplicate user row | NEW producer |
| 7 | init | CLI | `system/init` | `agent_session_id := thread`; checks (§5.2) | none | `session_id == thread`; sandbox active or turn failed | NEW |
| 8 | tool call | model | `tool_use mcp__calm__calm_report_commit` | MCP via shim, card-bound token | `item/started`; kernel write events | no approval prompt (P-H) | NEW producer |
| 9 | tool result | CLI | `tool_result` | — | `item/completed`, dotted name | one completed per started | NEW producer |
| 10 | steer (PR4) | user | `POST …/planner/input/{entry}/steer` | none | 409 `NotTaken` | id/rev unchanged | NEW refusal |
| 11 | interrupt | user | `POST /api/cards/{id}/planner/interrupt` | cause recorded; `control_request{interrupt}` | `IssuingInterrupt` | one outcome | NEW producer |
| 12 | settle | backend | `result error_during_execution` | outcome row; journal removed; group exits | outcome `interrupted`, `TurnCompleted` | durable before emit | NEW |
| 13 | next turn | run loop | observation | previous child exited; spawn `--resume` | normal turn | ≤ 1 process per session | NEW |
| 14 | restart mid-turn | ops | deploy restart (`KillMode=process`) | children keep running | none | — | existing ops |
| 15 | boot | kernel | `recover_harnesses_after_daemon_boot` | journal ⇒ kill verified group, outcome `interrupted`, journal removed; marker sweep; token re-minted; harness rebuilt in both arms | outcome row | no orphan survives boot; independent of Codex | NEW |
| 16 | resume | run loop | re-drained batch | `--resume` | normal turn | killed turn visible as interrupted to the model (P-F2) | NEW |
| 17 | delete track | user | `DELETE /api/tracks/{id}` | seal `(claude, thread)`; quiesce confirms empty group; rollback unseals | deletion events | destructive move only after confirmed stop | NEW branch |

## 8. Auth and isolation (D6), compliance

| Option | Evidence | Verdict |
|---|---|---|
| A. dedicated `CLAUDE_CONFIG_DIR`, user runs `CLAUDE_CONFIG_DIR=<dir> claude` + `/login` | only built-in skills/plugins; MCP connected; not logged in ⇒ "Not logged in · Please run /login" turn failure (P-D) | **recommended** (owner Q1) |
| B. `--bare` + API key (`apiKeyHelper` in `--settings` or `ANTHROPIC_API_KEY`) | hooks/plugins/OAuth/keychain skipped; loses CLAUDE.md auto-discovery; no key ⇒ same failure (P-G) | compliant alternative |
| C. `--safe-mode` | disables `--mcp-config` servers (P-C) | rejected |
| D. user's `~/.claude` + flags | user skills/plugins not loaded (P-B, P-E) | works; Planner sessions land in the user's own session list |

`--setting-sources project` does not leak user skills: `deep-research` & co. are `builtin: true` (P-A).
Compliance: no login UI; neige never collects, stores, copies or relays OAuth credentials or session
tokens; the unmodified `claude` binary runs as a subprocess; API-key mode is always compliant. neige never
sends `initialize` in v1 (its response carries `account.email`, P-A); PR7's catalog type omits it.

## 9. Delivery

### 9.1 Slices (ordered; production lines are estimates)

| PR | Content | Acceptance | Must-red (mutation-verified) |
|---|---|---|---|
| **PR1 Seam** (~600) | `PlannerBackend` (Codex arm only), `PlannerBackends` in `state.rs`, rows 1–8, 11, 13–17 of §4.1 routed through them; `client_id: &str`; invariant allowlist gains `harness/backend.rs` | targeted nextest of `planner_harness_*`, `planner_preserving_recovery*`, `planner_card_interrupt`, deletion suites, `harness_turn_start_invariant`; `local-rust-gates.sh --quick` | a second `.turn_start(` in run_loop still red; deletion rollback unseals through `PlannerBackends` |
| **PR2a Translate** (~800) | `protocol.rs`, `translate.rs`; recorded NDJSON fixtures (redacted: no `account`) | pure tests over the probe recordings | dotted name restored; per-block ids; image base64 not stored; empty `iterations` ⇒ no usage frame |
| **PR2b Session lifecycle** (~900) | `session.rs`, `runtimes.rs`, `config.rs`: spawn contract, env allowlist, process group, journal, `TurnSlot` + causes, settlement, serialization, quiesce, seals | fake `claude` (bash) through the real `ClaudePlannerSession`: exit/kill/linger/decode-error/stdin-stall paths; child env equals the allowlist | exactly one outcome for `result`+EOF; recorded interrupt beats a following failure; lingering child never overlaps; quiesce fails while a marked member lives; no `NEIGE_MCP_DAEMON_TOKEN` |
| **PR3 Provider identity** (~900, sweep-heavy) | key + migration + sticky arm; profile gate; `session_identity(kind, provider)` + query sites; boot SQL arm + partition; `CreateTrackRequest`/digest; OpenAPI/`wire.ts`/`NewTrackBody`; fixture sweep. **`claude` is refused at create (400) until PR4** | migration on a scratch copy of the live DB (`.backup`): 24 keys; 45 bindings replay; FE `npm run lint && npm run build && npm test` | `(SharedPlanner, Claude)` mint persists `claude/resumable`; key omission keeps it; missing key ⇒ not a harness card; same idempotency key + other provider ⇒ conflict |
| **PR4 Wiring** (~800) | start adapter branch; `for_runtime`; boot sweep; model surfaces (§5.8 PR4 column); steer pre-check; provider-named reader texts; confinement config (§5.3); `claude` admitted at create only when confinement is configured | fake-`claude` stack test via real routes: create → image message → MCP call → interrupt → restart (journal) → resume → next turn → track delete | journal at boot ⇒ one `interrupted` outcome; Codex daemon down at boot still recovers Claude; delete aborts while a group member lives |
| **PR5 Steer** (~300) | §5.9 | fake `claude` echo/no-echo | unconsumed steer ⇒ entry restored at head, group stopped, no second turn |
| **PR6 FE** | provider choice in new-track / first-message flows; clears retained model/effort; picker handles `model_choice: "unavailable"` | FE gates + browser test against the fake stack + real-browser preview | every create path sends `planner_provider` |
| **PR7 Prompts + Claude catalog** | provider-neutral prompt wording (M8) + goldens; `initialize` catalog; create/PUT validation | goldens; catalog fixture | effort outside `supportedEffortLevels` ⇒ `needs_a_choice` |

PR1, PR2a, PR2b, PR3 can run in parallel worktrees; PR4 needs all four; PR5 and PR6 need PR4; PR7 last
(prompt goldens conflict most, §9.4). The pain point is solved at PR4 + PR6 once the release gate passes.

### 9.2 Release gate (enabling a Claude Planner for real use)

Either **(a) sandbox parity**, all verified on the target host with the exact shipping flags (`socat`
installed): Bash write inside cwd succeeds; outside cwd and outside the session temp dir fails; the
`neige` CLI reaches `NEIGE_MCP_SOCKET` (`allowUnixSockets`); `git fetch/push` and `gh` reach the owner's
allowed domains, others are refused; out-of-cwd `Edit`/`Write` are denied; the `calm` MCP shim works;
`failIfUnavailable` fails closed when `socat` is removed — or **(b)** the owner sets
`accept-unconfined`, having read §5.3's escalation list. Plus live acceptance on the owner's box (never
Codex E2E): report write, worker dispatch, image message, interrupt, restart, resume, delete; `ps` shows
no Planner `claude` between turns.

### 9.3 KNOWN GAPS (one line each)

- Without the release gate's (a), Bash is not confined (owner choice (b)).
- Steer refused until PR5.
- No bound `Recover`; exact `calm.plan.recover` interface.
- Every deploy restart loses in-flight Claude turns (Codex's daemon survives restarts).
- A crash after the journal is written may deliver a batch twice (same window as Codex, H22).
- Started tool lines of a lost turn stay "running" in the FE; the outcome line is correct.
- Token usage once per turn; Codex-derived baseline; rate-limit events dropped.
- No `turn/plan/updated`, `webSearch`, `imageView`, `contextCompaction` rows.
- Moved workspace cannot resume (#857 class); reset the Planner.
- A first turn that dies before `system/init` reaches neige leaves `agent_session_id` unset; a second
  `--session-id` spawn may be refused (UNVERIFIED) ⇒ reset.
- Oversized image ⇒ failed turn with the API's message.
- After compaction the current instruction rendering applies (differs from Codex).
- `Lagged` on the per-session channel can still delay the harness state (outcome is durable); the Codex
  channel carries the same risk at higher traffic (H23).
- A removed `claude_bin` after start is retried as transient (reader told after 30 s); start preflight
  refuses a missing binary.
- Planner card `kind: "codex"` is a legacy view name.
- `ToolSearch` schema reload per resume unmeasured (PR2b measures).

### 9.4 Risks and conflicts

- **Undocumented protocol drift**: input lines and control envelopes are Agent-SDK internal (MIT
  `claude-agent-sdk-python` `_internal/query.py`). Mitigations: v1 writes only `user` lines and
  `interrupt`; init version/capability checks; `DISABLE_AUTOUPDATER`; fixtures from the pinned version.
- **#1542**: the Claude arm never returns `CodexRefused`, but PR4 changes the reader texts at
  `run_loop.rs:2430`/`:2547` that #1542 also touches — land #1542 first or rebase PR4 on it.
- **#1255** (steer): implemented for Codex; PR5 reuses its completion sweep.
- **#1727** S1–S4 and **#1785** slice 1 (#1790) are already in the base; remaining work there touches
  dispatcher, scheduler, MCP tools and `prompts/planner.md` — textual conflicts expected in prompts +
  goldens (PR7 last) and in migration numbering (take the next free number at merge).
- **#857**: same failure class if a workspace moves.

### 9.5 Cuts (recorded, not built)

- Resident process per harness (later optimisation only if measured latency matters, D4).
- Interactive approvals / `can_use_tool` responder.
- Claude for PlainChat / Assistant.
- Claude `Recover` via an SDK-served MCP tool.
- Streaming deltas, per-request usage, rate-limit surfacing, todo → plan.
- Renaming the Planner card kind; a provider-neutral item model.

### 9.6 Open questions that are the owner's

- **Q1** Auth/isolation: dedicated `CLAUDE_CONFIG_DIR` with the owner's own `/login` (recommended),
  the normal `~/.claude` (zero setup), or an API key (separate billing).
- **Q2** Release gate: install `socat` and pursue sandbox parity (a), or accept unconfined Bash (b)?
- **Q3** If (a): the network allowlist for sandboxed Bash (`github.com`, package registries, …).

Resolved in round 1 by the orchestrator: process model (per turn), steer scheduling (PR5), confinement
as a release gate, images (base64 blocks), model surfaces (three).
