# Claude Planner backend (#1791) — evidence companion

Companion to [1791-claude-planner-backend.md](1791-claude-planner-backend.md). It holds the fact
table, the 4140 queries, the probe log and the review dispositions so the design stays readable.
Base `origin/main` = `c5abb3c3d`. Paths without a prefix are under `crates/calm-server/src/`.
Vocabulary: "transcript table/row" = the table created by migration 0031 (its name is spelled in two
halves in §E2 because `scripts/gate-1316-terminology-ratchet.sh` counts the retiring word in `docs/`).

## E1. Fact table (VERIFIED on `c5abb3c3d`)

### E1.1 Harness ↔ Codex coupling

| # | Fact | Location |
|---|---|---|
| H1 | `PlannerHarnessParams.daemon` / `Inner.daemon` are `Arc<SharedCodexAppServer>` | `harness/run_loop.rs:164`, `:178` |
| H2 | `PlannerHarness::run` subscribes `daemon.subscribe_notifications()` before spawning the loop | `harness/run_loop.rs:404` |
| H3 | Turn issuance only via `IssueTurnHandle::issue` → `daemon.turn_start(thread, items, selection, client_id)` | `harness/run_loop.rs:264-275` |
| H4 | One task: `select!` over commands, notifications, tick (`watchdog_tick` + `maybe_issue_turn`), shutdown; notifications arriving while `maybe_issue_turn` awaits are handled after it returns; `Lagged` is logged and skipped | `harness/run_loop.rs:1081-1186`, `:1167` |
| H5 | `on_notification`: drops frames of other threads; ignores `approval/*`; handles `ThreadStarted`, `ThreadStatusChanged` (systemError → `Wedged`), `TurnStarted`, `TurnCompleted`, `turn/aborted`, `Item`, `turn/plan/updated`, `thread/tokenUsage/updated` | `harness/run_loop.rs:1763-1776`, `:1780`, `:1793`, `:1808`, `:1852`, `:1942`, `:1982`, `:2102`, `:2140` |
| H6 | `TurnStarted` accepted in `Issuing{TurnStart}` only when `issued_turn_id == turn.id` | `harness/run_loop.rs:1816-1827` |
| H7 | `TurnCompleted`: interrupt-target arm (`:1860-1891`), systemError arm for the last turn while `Wedged(systemError)` (`:1893-1915`), else only from `TurnRunning` with a matching id (`:1916-1939`) | `harness/run_loop.rs:1852-1939` |
| H8 | A `userMessage` `item/completed` with `item.clientId` upgrades the drain projection row in place; a started echo of a projected message is skipped | `harness/run_loop.rs:2022-2080`; projection shape `:2315-2327` |
| H9 | `item/started`/`item/completed` persisted verbatim + `harness.item.added` event; `turn/plan/updated` persisted too but with no event and no reader | `harness/run_loop.rs:2195-2262`, `:2102-2135` |
| H10 | Turn outcome = one transcript row `method='turn/completed'`, params = Codex `Turn` minus items; idempotent per (session, card, thread, turn) | `harness/turn_outcome.rs:6-29`; `crates/calm-truth/src/db/sqlite/out_of_domain.rs:413-431` |
| H11 | `TokenUsage::from_params` reads `tokenUsage.last.totalTokens`, `.total.totalTokens`, `.modelContextWindow`; `total_tokens` is not shipped to the FE; `BASELINE_TOKENS = 12_000` (Codex-derived) | `harness/token_usage.rs:10`, `:33-55`; `fe/core/api/generated/openapi.json:9547` |
| H12 | Steer: `handle_steer` takes the entry, calls `turn_steer`; `CodexRefused` → `NotTaken`, else `Unanswered`; completion sweep `restore_steered_entries_codex_dropped` restores entries whose projection row was never upgraded | `harness/run_loop.rs:1273-1470`, `:1472-1562` |
| H13 | Interrupt: `Issuing{Interrupt}` + 30 s `interrupt_deadline` + `turn_interrupt` | `harness/run_loop.rs:3572-3653`; `harness/config.rs:27` |
| H14 | Watchdog: deadline → `Wedged("interrupt_timeout")`; 30 min → interrupt | `harness/run_loop.rs:3519-3570` |
| H15 | `shutdown_inner` closes ingress, signals shutdown, interrupts through the daemon, then aborts the loop (a buffered completion may never be consumed) | `harness/run_loop.rs:666-729` |
| H16 | Model resolution: `config_read` / `model_list` only for installation defaults, via `classify_codex_failure`; reader texts say "codex" | `harness/run_loop.rs:2404-2516`, `:2621-2653`, `:2430`, `:2547` |
| H17 | `semantic_recovery::registered == false` ⇒ exact-interface briefing (`calm.plan.recover`; the Planner keeps its own `idempotency_key`) | `harness/run_loop.rs:3060-3070`; `harness/recovery_briefing.rs:120-128` |
| H18 | Recovery fns take `Arc<SharedCodexAppServer>`, use `turn_thread_is_sealed` / `readiness_receiver` | `harness/mod.rs:117-254`, `:335`, `:408`, `:476`; uses `:156-160`, `:213`, `:421-426`, `:477` |
| H19 | `state_from_snapshot`: running phases restore as `Resumed` → `Idle` after 5 s | `harness/run_loop.rs:3925-3960`; `harness/config.rs:28` |
| H20 | Fixtures fake lives inside `SharedCodexAppServer`; ~70 `daemon:` fields in 31 files build `PlannerHarnessParams` | `shared_codex_appserver.rs:784-860`; `git grep -l PlannerHarnessParams` |
| H21 | Boot recovery runs only if the Codex daemon started; otherwise a deferred pass waits on Codex readiness | `lib.rs:615-640`; `state.rs:577-598` |
| H22 | Crash window: the issuing snapshot (client id, queue not yet drained) is persisted at `:3045`; after `turn_start` returns, `last_turn_id` is assigned in memory (`:3262`), persisted by `persist_issuance_outcome` (`:3267`), durable when that transaction commits (`:3876-3882`) | `harness/run_loop.rs:3025-3045`, `:3262-3267`, `:3876-3882` |
| H23 | Codex notification broadcast capacity 1024, shared by all threads | `shared_codex_appserver.rs:1042` |

### E1.2 Start, stop, identity, deletion

| # | Fact | Location |
|---|---|---|
| S1 | `HarnessProfile::from_shape` returns `None` unless `kind == "codex"` | `harness/profile.rs:25-45` |
| S2 | Headless routes gate on `card_runs_headless_harness`; ratify requires `kind == "codex"` + Planner | `routes/cards.rs:88-90`, `:982` |
| S3 | Track create mints the Planner card `kind: "codex"` with `planner_harness_card_payload` | `routes/tracks.rs:1409-1439`, `:1864-1879` |
| S4 | `CreateTrackRequest` (`deny_unknown_fields`) carries `model` / `reasoning_effort` | `routes/tracks.rs:267-298` |
| S5 | Start adapter: instructions, card token mint, `approval_policy:"never"`, `sandbox_mode:"workspace-write"`, `thread_start_*`, `codex_thread_id`; snapshot phase set to `Idle` | `operation/planner_harness_start_adapter.rs:840-991`, `:955-956`, `:984`, `:1823-1860` |
| S6 | Start adapter passes `AgentProvider::Codex` | same file `:727`, `:1098`, `:1128` |
| S7 | Start preflights refuse when the Codex daemon is not running | same file `:420-422`, `:491-494` |
| S8 | Start compensation `interrupt_thread` → `daemon.interrupt_active_turn`; spawn builds `PlannerHarnessParams{daemon}` | same file `:1424-1430`, `:1275-1290` |
| S9 | Shutdown adapter (no live harness) interrupts through the daemon; interrupt adapter only calls `harness.interrupt` | `operation/planner_harness_shutdown_adapter.rs:101-135`; `operation/planner_harness_interrupt_adapter.rs:108-121` |
| S10 | `ensure_live_planner_harness` and planner recovery route refuse when Codex is not running | `routes/cards.rs:1232`, `:1289-1293`; `routes/planner_recovery.rs:58-61` |
| S11 | Model surfaces: GET `/api/models` calls `model_list` (`routes/models.rs:144`); create-time `catalog_advice` (`routes/tracks.rs:848-850`); PUT `/planner/model` `catalog_advice` (`routes/planner_model.rs:120`) | as listed |
| S12 | Persisted provider/mode come only from `derive_session_identity(&init.kind)`; `agent_provider` is not read; attribution binding updates only thread/session/turn ids; no `UPDATE … provider` exists | `crates/calm-truth/src/db/sqlite/session_mirror.rs:57-58`, `:420-440`; `session_row.rs:146-166` |
| S13 | `(Claude, Planner)` is unmappable → error; SharedPlanner-kind queries filter `provider = codex` | `crates/calm-truth/src/session_projection_row.rs:145-163`; `db/sqlite/session_projection.rs:160`, `:731-790`; `db/sqlite/read.rs:802` |
| S14 | Harness pushes and "shared card" tests key on the SharedPlanner kind | `dispatcher/mod.rs:1363`; `crates/calm-truth/src/session_projection_lookup.rs:253` |
| S15 | `track_activity` treats `mode=harness` rows as conversations before provider | `track_activity.rs:259-266` |
| S16 | Claude PTY paths key on `kind == "claude"`; `worker_flow` attaches only Codex/Claude card runtimes | `operation/claude_restart_adapter.rs:133`; `worker_flow/mod.rs:278`, `:519-529` |
| S17 | FE Planner identity = `kind === 'codex' && payload.planner_harness === true` (5 sites) | `fe/web/src/systems/cards/builtins/planner.ts:11-27`; `fe/web/src/app/router/public.tsx:1819`, `:1853`, `:1951`, `:2087` |
| S18 | Typed config `claude_bin` | `config.rs:91-92` |
| S19 | Server-owned payload keys: the list (`validation.rs:35`) and a separate `server_owned_value_is_sticky` match defaulting to false (`:44-58`); `card_update_tx` re-inserts only sticky values | `crates/calm-truth/src/validation.rs:35-66`; `db/sqlite/card.rs:158-170` |
| S20 | Create idempotency: `CreateRequestShape` + `create_request_digest` enumerate fields; optional fields enter the digest only when set ("preserve the exact pre-selection digest") | `routes/tracks/create.rs:100-120`, `:333-357` |
| S21 | Deletion seals are owned by the shared daemon and survive harness removal; `shutdown_track` seals every live Planner; card-grade quiesce seals + interrupts ("a destructive workspace move may only follow a confirmed quiesce"); track/area deletion plans hold `turn_daemon` and unseal on rollback | `harness/registry.rs:203-218`; `routes/cards.rs:128-156`; `routes/tracks.rs:128`, `:2677`, `:2870-2956`, `:2991-3009`, `:3160`; `routes/areas.rs:319`, `:398`, `:489-623`, `:652-663`, `:814` |
| S22 | Production unit: `KillMode=process` (only calm-server is signalled; children stay in the cgroup); SIGTERM runs axum graceful shutdown | `~/.config/systemd/user/neige-next.service.d/preserving.conf`; `main.rs:187-199`, `:253` |
| S23 | Stop precedents (production): marker-authenticated sweep + fail-closed wait (`operation/task_verify_adapter/target.rs:715-740` `stop_group`; `operation/gate_process.rs:350-372`); `proc_env_marker` → `Present`/`Foreign`/`Unreadable` (`proc_identity.rs:180-212`); `sigkill_verified_members` (`:247-252`); daemon spawn `process_group(0)` (`shared_codex_appserver.rs:2601`). Live scan (2026-09-24): same-uid processes with unreadable environ exist (zombie `sh` in the service cgroup, privsep `sshd`); review channel A adds setgid `ssh-agent` inside the service cgroup and headless Chromium rewriting its environ (marker gone) | as listed |
| S24 | Codex daemon env: `env_clear()` + `SPAWN_ENV_PASSTHROUGH` | `shared_codex_appserver.rs:55-90` |
| S25 | Codex trusts the workspace (`trust_level = "trusted"`) | `shared_codex_home.rs:303-313` |
| S26 | Attachments: png/jpeg/gif/webp, ≤ 8 MiB, bound to absolute paths; `attachments_supported` depends on workspace, not provider; issuance sends `localImage` items | `crates/calm-types/src/planner_attachment.rs:13-47`; `planner_attachments/mod.rs:30`; `planner_attachments/bind.rs:21-26`; `routes/cards.rs:1165-1172`; `harness/run_loop.rs:3178-3184` |
| S28 | Model UI: `ModelsResponse.source` (`routes/models.rs:92-101`), empty catalog + `source:"unavailable"` when unreachable (`:155-161`); FE disables the picker on it (`fe/web/src/features/chat/thread/model-pill.tsx:39`) with label "codex is not running" (`:109`) | as listed |
| S29 | Legacy create bindings (v0) are refused outright (`routes/tracks/create.rs:370-378`); the FE create body omits provider today (`fe/web/src/app/router/new-track-route.tsx:66-74`) | as listed |
| S30 | Optional-backend config precedent: `isolated_codex_config: Option<PathBuf>`, "missing keeps this backend unavailable" (`config.rs:33-36`; `state.rs:971`) | as listed |
| S31 | Seal set = thread-keyed `DashMap` on the daemon (`shared_codex_appserver.rs:672`); `DeletionThreadSeals` seals, retains, unseals on drop (`:733-771`) | as listed |
| S32 | Recovered-outcome precedent from `handle_state_json.last_turn_id` (`shared_codex_appserver/preserving_recovery.rs:120-160`); outcome put is SELECT-then-INSERT (`crates/calm-truth/src/db/sqlite/out_of_domain.rs:423-434`) | as listed |
| S33 | Reset: the start transaction supersedes the predecessor (`crates/calm-truth/src/db/sqlite/session_mirror.rs:318-322`), then the adapter shuts it down only if registered (`operation/planner_harness_start_adapter.rs:862-868`); the route runs start, then a shutdown op (`routes/cards.rs:1421-1427`); boot selects only active rows (`db/sqlite/session_projection.rs:771`) | as listed |
| S34 | Deletion order: card quiesce before `shutdown_track` (`routes/tracks.rs:2663-2695`); Codex re-checks the seal after `turn/start` (`shared_codex_appserver.rs:1386-1390`); MCP auth accepts only active sessions' token hashes (`mcp_server/handshake.rs:55-60`) | as listed |
| S35 | Instructions carry bound template input and template context (`operation/planner_harness_start_adapter.rs:313-316`, `template_context.rs:53-56`); `IsolatedCodexConfig` is `deny_unknown_fields` with a required `codex_binary` (`isolated_codex/config.rs:14-23`); versioned binaries 2.1.220/2.1.259/2.1.280 behind the `~/.local/bin/claude` symlink; `--version` takes 9 ms | as listed |
| S36 | Lifecycle facts: MCP listener spawned in `AppState::boot` (`state.rs:1091`) before `boot_harnesses` (`main.rs:43`); the shim reconnects with its cached `initialize` (`crates/neige-mcp-stdio-shim/src/pump.rs:243`); post-handshake calls check session activity only (`mcp_server/transport.rs:1479`); `spawn_recovered_harness` callers `harness/mod.rs:348`, `:429`, `:527`, `routes/cards.rs:1295`, `replay.rs:385`; the start adapter mints the token in `app_server_interact` (`:949`) but builds the harness in `spawn_side_effect` (`:1277`); the shutdown op supersedes first (`operation/planner_harness_shutdown_adapter.rs:82`); repoint fences all active runtimes (`routes/tracks.rs:2104-2118`), shuts down registered handles non-strictly (`:2125-2150`), Dirty branch `:2160`, recycle `:2239`; failure seam precedent `fail_workspace_repoint_shutdown_for_test` (`:1951-1976`); `sigkill_verified_members` verifies `start_time` (`proc_identity.rs:260-267`) | as listed |
| S37 | Credential storage: `persist_card_mcp_token_hash` writes only `card_mcp_tokens` (`mcp_server/wiring.rs:77-85`); `mint_and_persist_card_token` writes card and session rows in one transaction (`:103-111`); handshake reads `worker_sessions.mcp_token_hash` of active rows (`crates/calm-truth/src/db/sqlite/session_row.rs:94-113`); `session_mirror_card_mcp_token_tx` copies the card hash into an active row with a NULL hash (`session_mirror.rs:223-259`); thread reuse requires a card token row (`operation/planner_harness_start_adapter.rs:892`); track deletion deletes its session rows (`db/sqlite/track.rs:438-442`); deletion quiesce reads only the active runtime (`routes/cards.rs:131-136`); the repoint seam is one-shot (`routes/tracks.rs:1963-1970`) | as listed |
| S27 | Other Codex-only consumers: `liveness_feeder` subscribes the Codex stream (`dispatcher/mod.rs:798`); dev replay (`replay.rs:390`); TUI initial-prompt takeover query (`db/sqlite/read.rs:798-830`) | as listed |

### E1.3 MCP, tools, prompts, FE

| # | Fact | Location |
|---|---|---|
| M1 | Codex home `[mcp_servers.calm]` = shim + daemon token | `shared_codex_home.rs:316-335`; `mcp_server/wiring.rs:61-69` |
| M2 | Per-thread Codex config: `NEIGE_MCP_SOCKET`, `NEIGE_MCP_TOKEN`, kernel-led `PATH`; three terminal tools pre-approved for Planners | `mcp_server/wiring.rs:11-57` |
| M3 | `mint_card_mcp_token_pair`, `mint_and_persist_card_token` | `mcp_server/wiring.rs:71`, `:104` |
| M4 | The shim prefers `NEIGE_MCP_DAEMON_TOKEN` over `NEIGE_MCP_TOKEN` | `crates/neige-mcp-stdio-shim/src/main.rs:60-65` |
| M5 | Card-bound connections may omit `_meta.threadId` | `mcp_server/registry.rs:53-61`; `mcp_server/transport.rs:277` |
| M6 | `calm.user.notify` writes nothing; the transcript `mcpToolCall` row (`arguments.text`) is the message | `mcp_server/tools/user_notify.rs:1-2`; `fe/core/domain/conversation.ts:764-798` |
| M7 | `model_tool_key` strips `mcp__<server>__` and folds to `[A-Za-z0-9_]` | `mcp_server/transport/worker_grants.rs:58-89` |
| M8 | Codex spellings in prompts | `prompts/planner.md:67`; `prompts/assistant/mechanics.md:14`; `prompts/tools/calm.task.dispatch.md:1`; `prompts/tools/calm.source.capture.md:1` |
| M9 | `Recover` = Codex dynamic tool registered at Planner `thread/start`, served via `item/tool/call`, provenance by Codex ids (migration 0101) | `shared_codex_appserver.rs:1276-1302`; `codex_appserver/server_requests.rs:235`; `semantic_recovery/mod.rs:22-26`, `:96-160` |
| M10 | FE reads `agentMessage.text`, `userMessage.content[].text` (or `input_segments`), `mcpToolCall.{tool,arguments,status,error}`, `commandExecution.{command,exitCode,status,aggregatedOutput,durationMs}`, `fileChange.changes.length`, outcome `{id,status,error.message,error.codexErrorInfo}`, envelope `completedAtMs`; `tools/list` answered at `mcp_server/transport.rs:338` | `fe/core/domain/conversation.ts:800-870`, `:979-1026`, `:1054-1091`, `:1116-1144`; `fe/core/keys/mcp-tools.ts:1-34` |
| M11 | Transcript methods: `item/started`, `item/completed`, `turn/completed` | `fe/core/domain/conversation.ts:1146-1152` |

### E1.4 Gates a slice will trip

| Gate | Pins | Tripped by |
|---|---|---|
| `tests/cases/harness_turn_start_invariant.rs` | `.turn_start(` once in `run_loop.rs`, else only allowlisted files | PR1 (`harness/backend.rs`); PR2b if `claude_planner/*` spells `.turn_start(` |
| `scripts/gate-1316-terminology-ratchet.sh` (CI `ci.yml:464`) | occurrence counts of the five retiring terms listed in the script's `TERMS` block (#1316: old area/track/Planner/runtime/transcript names) in `crates/ fe/ docs/ e2e/`, both directions | any new file using the house tracing field named runtime+id, the transcript insert fn, or the item-added event variant; the Claude backend emits notifications instead (D2) and logs `worker_session_id` |
| `scripts/gate-prose-ratchet.sh` | no CJK run, no ≥120-char literal in `crates/**/*.rs` | backend error texts; long texts go to `prompts/` |
| `boot_recovery_sql_literals_track_the_minted_card_shape` (`operation/planner_harness_start_adapter.rs:2303`), `the_persisted_payload_field_names_are_frozen` (`:2140`) | boot SQL literals, payload names | PR3 |
| goldens `tests/goldens/*prompt*.txt`, `mcp_tool_registry.json` | prompts, registry | none: shared prompt wording is cut; the Claude-only fragment is a new file |
| OpenAPI drift (`scripts/local-rust-gates.sh --quick` step 5), `fe` `npm run gen:api`, handwritten FE `NewTrackBody` | request schema | PR3 |
| migrations byte-frozen; `SYNC_EVENT_VERSION` lockstep | new migration only; no new event kind | PR3 |
| 800-line rule | `run_loop.rs` is 4224 lines | all: new code in new files |

## E2. 4140 queries and results (2026-09-24, `events.max(id)=58979`)

```sh
DB=~/.local/share/neige-next/data/calm.db
q(){ sqlite3 -readonly -header -column "$DB" "$@"; }
T='harness''_items'   # the migration-0031 transcript table
q "SELECT method, item_type, COUNT(*) n FROM $T GROUP BY 1,2 ORDER BY n DESC;"                     # Q1
q "SELECT COUNT(*) total, COUNT(DISTINCT card_id) cards, COUNT(DISTINCT thread_id) threads FROM $T;" # Q2
q "SELECT json_extract(params,'\$.status') s, COUNT(*) FROM $T WHERE method='turn/completed' GROUP BY 1;" # Q3
q "SELECT json_extract(params,'\$.item.server') srv, COUNT(*) FROM $T WHERE method='item/completed' AND item_type='mcpToolCall' GROUP BY 1;" # Q4
q "SELECT COUNT(*) FROM $T WHERE item_type='dynamicToolCall';"                                     # Q5
q "SELECT COUNT(*) FROM $T WHERE json_extract(params,'\$._projection')=1;"                           # Q6
q "SELECT role, kind, COUNT(*) FROM cards GROUP BY 1,2 ORDER BY 3 DESC;"                            # Q7
q "SELECT provider, contract, state, COUNT(*) FROM worker_sessions GROUP BY 1,2,3;"                 # Q8
q "SELECT kind, phase, COUNT(*) FROM operations WHERE kind LIKE 'planner-harness-%' GROUP BY 1,2;"   # Q9
q "SELECT (SELECT COUNT(*) FROM planner_recovery_threads),(SELECT COUNT(*) FROM planner_recovery_turns),(SELECT COUNT(*) FROM planner_recovery_issuances),(SELECT COUNT(*) FROM planner_recovery_calls);" # Q10
q "SELECT json_extract(payload,'\$.planner_harness') ph, json_type(payload,'\$.harness') legacy, COUNT(*) FROM cards WHERE role='planner' GROUP BY 1,2;" # Q11
q "SELECT request_fingerprint_version, COUNT(*) FROM track_create_idempotency GROUP BY 1;"           # Q12
q "SELECT DISTINCT workspace_path FROM tracks WHERE workspace_path<>'';"  # Q13, then test -f CLAUDE.md / AGENTS.md / .claude/settings.json / .mcp.json per path
```

| Q | Result | Consequence |
|---|---|---|
| Q1 | `reasoning` 840/838, `commandExecution` 658/658, `agentMessage` 377/377, `mcpToolCall` 357/357, `userMessage` 123 completed, `turn/completed` 123, `fileChange` 35/35, `webSearch` 18/17, `subAgentActivity` 10/10, `imageView` 9/9, `collabAgentToolCall` 4/4, `contextCompaction` 2/2 | every type the Claude mapping emits already renders |
| Q2 | 4863 rows, 13 cards, 13 threads (09-16 … 09-24) | all Codex |
| Q3 | completed 117, interrupted 5, failed 1 | the three statuses the FE draws |
| Q4 | `calm` 351, `codex_apps` 4, `codex` 2 | dotted names are the rendering contract |
| Q5 | 0 | `dynamicToolCall` free for Claude-native tools |
| Q6 | 0 | no drain in flight |
| Q7 | planner/codex 24, assistant/codex 5, worker/codex 48, worker/claude 12, worker/terminal 12, reportcard 24 | 24 cards to backfill |
| Q8 | codex/planner idle 24, superseded 17; claude/executor exited 12; no claude/planner | new identity has no legacy rows |
| Q9 | start 85, shutdown 18, interrupt 19 — all succeeded | no op replay observes a payload change |
| Q10 | 38 / 4 / 4 / 1 | Codex-only provenance |
| Q11 | 23 marker, 1 legacy `harness` object | backfill keys on `role='planner'` |
| Q12 | version 1: 38, version 0: 7 | 38 replay contracts preserved; 7 keep failing closed (S29) |
| Q13 | 20 workspaces: 2 have only `AGENTS.md`, 18 have neither file; none has `.claude/settings.json` or `.mcp.json` | project executable config is hypothetical on 4140 |

## E3. Probe log (claude 2.1.280, `claude-haiku-4-5`)

Scratch, session-scoped (not preserved): `/tmp/claude-1000/-mnt-data2-kenji-neige-calm/852c3533-7ab9-4c83-ab0e-b2af4bcdbf0e/scratchpad/cc_probe2/`
(`h.py`, `fake_mcp*.py`, raw NDJSON per probe). Env limited to `HOME PATH USER LOGNAME LANG TERM` +
proxy vars; cwd `ws/` (a `CLAUDE.md` with marker `PELICAN-7`); no credential file read; never the
production server. Round 0: 15 runs; round 1: 4 runs; round 2: 2 runs; round 4: 2 runs. Common prefix: `claude -p --input-format
stream-json --output-format stream-json --verbose --include-partial-messages --replay-user-messages
--model claude-haiku-4-5 --session-id <uuid>`.

| Id | Flags beyond the prefix | Observed |
|---|---|---|
| P-A (5 runs, `initialize` only) | `--strict-mcp-config --mcp-config mcp.json --permission-prompt-tool stdio` + (a) `--setting-sources project` (b) `--safe-mode` (c) (a)+`--disable-slash-commands` (d) (a)+empty `CLAUDE_CONFIG_DIR` (e) `--bare` | commands 53/53/**0**/46/49, all `builtin: true` (incl. `deep-research`), none from `~/.claude/skills`; `models[].supportedEffortLevels` `low…max`; `account` has `email` when logged in, `{apiProvider, tokenSource}` otherwise |
| P-B | (a) + `--permission-mode default`, user line with `uuid` | replay echoes our `uuid` (`isReplay`); `command_lifecycle` queued/started/completed; CLAUDE.md read; `mcp__calm__calm_report_write`, `mcp__calm__plugin_dev-neige-market_market_quote`; `ToolSearch select:` first; `can_use_tool` for MCP calls; `result.usage.iterations[]`, `modelUsage.<m>.contextWindow=200000`; `get_context_usage` works; `set_model` → success + a `<local-command-stdout>` replay |
| P-C | `--safe-mode` + MCP config | `mcp_servers: []`, user plugins listed, CLAUDE.md not read |
| P-D | (a) + empty `CLAUDE_CONFIG_DIR` | MCP connected; "Not logged in · Please run /login"; `result{subtype:"success", is_error:true, terminal_reason:"api_error", usage.iterations:[], modelUsage:{}}`; exit 1 |
| P-E | (a) + `--disable-slash-commands --tools Bash,Read,Edit,Write,ToolSearch,WebFetch,WebSearch` | `skills: []`; Write `tool_use_result{type:"create"}`, Edit `structuredPatch`; failing Bash → `is_error`, text `"Exit code 3\none"`; a steer written while `can_use_tool` was pending: `queued`, then `started` after the tool batch, one `result` |
| P-F1 | as P-E, SIGINT during `sleep 20` | rejected `tool_result` + interrupt text, **no `result`**, exit 0 |
| P-F2 | as P-E, SIGKILL during `sleep 20`, then `--resume` | nothing emitted before input; context kept; killed turn reported interrupted |
| P-F3 | as P-E, second line queued, then `interrupt` | `control_response{response:{subtype:"success", request_id, response:{still_queued:[…]}}}`; `result{error_during_execution, result:null, iterations:[], modelUsage:{}}`; lifecycle `cancelled`; queued line then ran as its own turn |
| P-G | `--bare`, no key | `account{tokenSource:"none"}`; same failure as P-D |
| P-H | (a) + `--tools Bash,Read,Edit,Write,ToolSearch --permission-mode default --permission-prompts none --allowedTools "Bash Read Edit Write ToolSearch mcp__calm"` | no `control_request`; MCP call, `touch` in cwd, Write **outside** cwd, `git push` all executed |
| P-S1 (round 1) | P-H flags without `--permission-mode`, `--settings '{"sandbox":{"enabled":true,"failIfUnavailable":true,"allowUnsandboxedCommands":false}}'`, `--mcp-config` whose env is `{"NEIGE_MCP_TOKEN":"${PROBE_TOKEN}"}` | turn fails before any request: `result{error_during_execution, errors:["Sandbox required but unavailable…"]}`, stderr "socat not installed"; exit 1; nothing written; the MCP child saw `NEIGE_MCP_TOKEN='tok-123'` (**`${VAR}` expansion works**) |
| P-S2 (round 1) | same, without `failIfUnavailable` | stderr "⚠ Sandbox disabled: … socat not installed"; **every command ran unsandboxed**: write in cwd, outside cwd, `/tmp`, a Unix-socket connect, `curl` HTTP 200 |
| P-I (round 1) | `--tools Read --permission-prompts none`, user line content `[text, {type:"image", source:{type:"base64", media_type:"image/png", data}}]` | model answered "Red." (32×32 red PNG); the replay echoes the base64 block; a `Read` attempt was auto-denied: "requires approval, and this session has no approval surface" (**denial path verified**) |
| P-K (round 2, 2 runs) | `--tools Bash --permission-prompts none --allowedTools Bash`, child env `NEIGE_CLAUDE_PLANNER=probe-r2-marker`, unsandboxed; SIGKILL `claude` 8 s into `sleep 300; echo done` (run 1 killed at 3 s, before the shell started: inconclusive) | before the kill: `claude` pgid 45904 / sid 45431; its Bash `zsh -c … sleep 300` in **its own session** (pgid = sid = 64676); after the kill `zsh` (re-parented to the user subreaper) and `sleep 300` **survived, both carrying the marker**; removed by the probe script |
| P-L (round 4, 2 runs) | P-I flags; one turn, `result`, then SIGTERM while `claude` waits on stdin; then `--resume` | exit 143; resumed turn answered "KESTREL. No, my previous reply was complete." — a signal after `result` does not mark the turn interrupted |
| P-J (round 1) | as P-I, cwd with only `AGENTS.md` (marker `HERON-3`) | answered `HERON-3`: **AGENTS.md is read when there is no CLAUDE.md** |

Not probed (host lacks `socat`, no install rights): sandbox confinement under the shipping flags;
network allowlist behaviour; Unix-socket access for the `neige` CLI from inside the sandbox; denial of an
out-of-cwd `Edit(//<cwd>/**)` write. These are release-gate checks (main doc §9.2).

## E4. Review dispositions

### E4.1 Round 1

Channel A = subagent review, channel B = codex review, both on `c2ebf619d`. The per-row round-1 table is in commit `92fadd1f8` (this file, §E4); it is condensed here.

| Id | Finding (short) | Disposition |
|---|---|---|
| A1–A15, B1–B14 | all accepted or merged (B1→A1, B3→A4, B6→A5, B13→A13); the round-1 fixes that round 2 replaced are: A2/B2 process identity + registry (→ D12), B7 journal (→ D13), B4 three model surfaces (→ D11 cut), A6 path rules (kept only with the sandbox), A13 steer PR (→ D8 cut). The rest stand as written in the main doc: A1/B5/A5 provider identity (§4.4), A3 serialization, A4 inventory (§4.1), A7 env, A8 flags, A9/B9 usage, A10 mapping, A11 gaps, A12 gate rows, A14 AGENTS.md (P-J), A15 re-rendering, B8 settlement, B10 images (P-I), B11 wire types, B12 release gate, B14 claim corrections (#1727 S1–S3 + S4 slices 1–4, #1785 slice 1 in base) |

### E4.2 Round 2

All 25 items (A-B1, A-M1..M4, A-m1..m10, B2-1..B2-10) accepted or merged (B2-7→A-m2); the per-row
table is in commit `9c2d0983e` (this file). Round 2 deleted the journal, recorded process identity,
registry, provider-keyed seals, runtime dir, steer PR and model-catalog PR (D8, D11–D14); later rounds
narrowed D12 (D16) and replaced the per-path stop reasoning with the lifecycle invariant (D22).

### E4.3 Round 3

All 12 items (A-B1, A-M2, A-m3..m5, B3-1..B3-7) accepted; the per-row table is in commit `7964b01cb`
(this file). Resulting decisions: D16 (marker-present membership), D17 (stop on every path), D18
(pinned binary and version), D19 (instructions file), D20 (credential rotation), D21 (commit boundary).

### E4.4 Round 4

All 9 items (A-M1..M3, A-m4..m7, B4-1, B4-2→A-m4) accepted; per-row table in commit `637578f71` (this
file). Resulting decisions: D22 (lifecycle invariant), D23 (failure seam), D24 (instructions guard),
D25 (settlement order, P-L), D26 (stop mechanics).

### E4.5 Round 5

All 7 items (A-M1, A-M2 = B5-1, A-m3..m6, repoint 409) accepted, verified against S37; per-row table in
commit `38b84b780` (this file). Resulting decisions: D27 (scoped any-state sweep), D28 (credential
tables), D29 (inert loser, seam scope, timer budget, instance marker, repoint message).

### E4.6 Round 6

B6-1 = A-m2 (dead `Slot::Live` after failed activation), A-M1 (one-shot seam consumed by shutdown before
the sweep) and A-m3 ("inert" undefined) accepted; per-row table in commit `927702bc1` (this file). D30's
activation parts were replaced in round 7 (D31); the sticky seam and the id-set source remain.

### E4.7 Round 7

| Id | Finding (short) | Disposition |
|---|---|---|
| A-M1 | "`turn_start` waits for activation" deadlocks `shutdown` (issuance lock held in the tick arm, run_loop `:2958`, `:1172-1181`; shutdown waits for it, `:685`) | ACCEPTED, verified → D31: `turn_start` never waits; not installed ⇒ retryable `Err` |
| A-M2 | late mint after shutdown overwrites the replacer's hash (`wiring.rs:103-111`; replacer after `existing.shutdown()`) | ACCEPTED, verified → D31: mint only inside `turn_start` under the issuance lock after the `shutting_down` check; must-red 3 |
| Fallback check | Pending refusal safe? token needed earlier? adapter first turn through `turn_start`? | yes (existing re-buffer path); no (only the spawned process uses it; the Claude branch skips the Codex reuse token-row check, adapter `:892`); yes (H3, invariant test) → preferred fix adopted |

