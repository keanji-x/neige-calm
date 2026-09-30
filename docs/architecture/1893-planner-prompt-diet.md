# #1893 — Planner prompt diet

**Owner rules.**
1. Simple first, pain points only.
2. Prefer deleting a mechanism to adding one. A hypothetical case is one line in KNOWN GAPS.
3. Compatibility means the 4140 database only (`Q` = `sqlite3 -readonly ~/.local/share/neige-next/data/calm.db`).
   What is in it keeps working or is cleaned up, with the numbers and commands below.
4. This is a LARGE simplification, not a trim.
5. The main prompt does not inject everything. Situational detail is read on demand.

**Outcome.**
- `planner.md` goes from 47,039 B to about 8,300 B in S1, capped at 7,500.
- Four on-demand guides of 6 KB or less each are read with `neige cat guide/<topic>.md`.
- The Planner-visible tool surface (descriptions plus input schemas, which Codex loads up front) goes from 61,179 B to at most 29,000 B after S6.
- Four mechanisms are deleted, each with its tables: isolated file delivery, candidate repair, Codex semantic recovery, and the whole isolated-codex-v1 path.
- The git-delivery decision tool is deleted too.
- Byte ratchets keep all three numbers from growing back.

## 1. Facts

Verified at 69351f1cd by reading the code, or by `Q` on 2026-09-30.
- `T` is the per-card transcript table that `track_activity/sql.rs:20-30` reads.
- Its tool rows begin on 2026-09-16 (retention).
- Planner rows are joined through `cards.role='planner'`.

| # | Claim | Where / how verified |
|---|---|---|
| F1 | `planner.md` is 47,039 B, embedded as-is and rendered with `{track_id}` and `{planner_wake_authors}` only | `wc -c`; `planner_card.rs:5`, `:47-52` |
| F2 | Assembly: `planner.md` + bound template input + the card's template context. Claude appends a 221 B fragment | `operation/planner_harness_start_adapter.rs:381-435`; `claude_planner/wiring.rs:46-54` |
| F3 | Section bytes: How you are driven 19,084 (L57 dispatch 2,607; L61 recovery 2,008; L48 1,549; L59-60 gates 2,128; L67 delivery 974); Terminal 6,578; Track Report 5,993; Reading outputs 5,611; Candidate delivery 4,153; Reacting 3,482; JSON delivery 1,421; Open/closed 470 | bytes per `##` heading and per line (python over the file) |
| F4 | 28 Planner-visible tools: 46,106 B of descriptions + 15,073 B of compact schemas = 61,179 B. Each description is exactly `prompts/tools/<name>.md` | `tests/goldens/mcp_tool_registry.json` (`visible_to_roles` ∋ `planner`); sha256 of each md = `description_sha256` |
| F5 | Codex receives every visible tool, plus the `Recover` dynamic tool on Planner threads. Claude defers tools behind `ToolSearch` | `shared_codex_appserver.rs:1287-1312`; `semantic_recovery/mod.rs:22`; `claude_planner/spawn.rs:18` |
| F6 | Planner calls since 09-16: report.commit 92, plan.list 90, report.read 74, terminal.input 59, task.verdict 48, terminal.control 30, review.round 27, source.capture 15. **0 calls**: task.dispatch, task.repair, task.delivery, plan.recover, user.notify, track.close, report.write_markdown | `Q "select c.role,json_extract(h.params,'$.item.tool') t,count(*) from T h join cards c on c.id=h.card_id where h.method='item/completed' and h.item_type in ('mcpToolCall','dynamicToolCall') group by 1,2"` |
| F7 | `calm.report.blocks.move` was already deleted by #1883. The commit `move` op has 0 uses (upsert 270, delete 8) | `git log -S'calm.report.blocks.move'` → 69351f1cd; `grep -o '"op":"[a-z_]*"'` over commit arguments in `T` |
| F8 | isolated-codex-v1 on 4140: 4 tasks, all Planner Dispatch, all 09-12, all `failed` because the worker had no network. They are on 3 tracks, all `done`, and 3 of them are current tasks | `Q "select key,status,date(created_at_ms/1000,'unixepoch') from tasks where context_json like '%isolated-codex-v1%'"` |
| F9 | Leftover operations: 9 `codex-isolated-worker`, 1 `task-file-publication`, 1 `candidate-verify`, all terminal (`succeeded`/`failed`). **All 11 carry an `idempotency_key`**, so `operations_keyed_rows_are_permanent` (`calm-truth/migrations/0099_isolated_parked_operation_receipts.sql:89`) aborts any DELETE. 4 are `worker_sessions.spawn_op_id` (codex/resumable/executor/exited, `handle_state_json` NULL) | `Q "select kind,phase,count(*),sum(idempotency_key is not null) from operations where kind in (...) group by 1,2"`; `Q "select count(*) from worker_sessions w join operations o on o.id=w.spawn_op_id where o.kind='codex-isolated-worker'"` → 4 |
| F10 | Leftover events: 6 `task.execution_settled` (last 09-12), 1 `task.file_publication_settled` and 1 `task.candidate_verification_settled` (09-08) | `Q "select kind,count(*),max(at) from events group by kind"` |
| F11 | Leftover rows elsewhere: recovery tables `planner_recovery_calls` 1, issuances 4, turns 4, threads 39; `planner_dispatch_receipts` 3; `task_attempt_allocations` 1 with origin `recovery` (09-12) and 106 `initial`. The 7 file-delivery/candidate tables and `task_git_delivery_abandonments` have **0 rows** | `Q "select count(*) from <table>"`; `Q "select json_extract(origin_json,'$.kind'),count(*) from task_attempt_allocations group by 1"` |
| F12 | Git delivery: 52 deliveries, all `candidate`, 0 failures, 0 retries | `Q "select settlement,failure_code,count(*) from task_git_deliveries group by 1,2"` |
| F13 | The operation driver never loads terminal rows. `claim_drive_batch` selects non-terminal phases only; the boot scan and parked sweep do the same. An unknown kind is an error only inside those paths (`driver.rs:386-391`) | `operation/repo_sqlite.rs:163-182`, `:231`; `operation/driver.rs:325-357`, `:767` |
| F14 | Event replay skips a row whose kind no longer deserializes: the `Err` arm logs one `tracing::error!` line and pushes nothing; the call still returns `Ok`. Prod readers: `ws/events.rs:255` (cold replay from an anchor, capped) and `replay.rs`. Precedent: the retired-event-kind skip test at `events_since_bound.rs:160` | `calm-truth/src/db/sqlite/events.rs:716-723`; `calm-truth/tests/events_since_bound.rs:160` |
| F15 | Ordinary codex/claude tasks never run isolated code (`isolated_codex/mod.rs:46-62`). They still pass through isolated or candidate helpers on every read or write; see the list in §3.2 | read |
| F16 | User recovery (REST `recover`, FE "Recover task") shares its core with the Planner path. For an ordinary worker it refuses with `new_task`; its only real target is an isolated attempt | `task_recovery/admission.rs:445-525`; `routes/task_recovery.rs:20`; `fe/web/src/features/report/task/recovery.tsx:41` |
| F17 | `planner.md:26-30` repeats `calm.terminal.*.md` clause for clause | side-by-side read |
| F18 | Planner terminal use (108 calls, 5 tracks). wait_for: text 37, change 31, elapsed 5, **signal 0**. Actions: submit 34, key 20, text 5, sequence 1, **replace 0, click 0**. allow_output_since_observation 6 (stale_observation fired 4 times); **allow_output_below_cursor 0**. `claim=true`: input 5, open 6. `release=true` on input 5. terminal.control: claim 15, release 15. **scroll_to_text 0, format=image 0, claude_permissions 0**. Tracks with a permissions policy: 0 | `grep -c` over the terminal arguments in `T`, split by tool; `Q "select count(*) from tracks where claude_permissions_policy is not null"` |
| F19 | terminal.input does fences → claim → write → release → readback in one request under one replay fingerprint | `terminal_interaction/operations.rs:43-55` |
| F20 | Situational text already exists where it is needed: the quiet-sync line (`harness/run_loop.rs:2808-2812`), failed-delivery wake text (`calm-types/src/observation.rs:370-401`), receipt texts (`prompts/result-receipt/*.md`) | read |
| F21 | `calm.user.notify` is the only ungated way to ask the user since #1876 | `planner.md:9`; `track_activity/sql.rs:20-30` |
| F22 | `calm.track.cat` intercepts `area/…` before `TrackFsView` (`mcp_server/tools/track_file.rs:119-121`, `:98-100` for ls). `neige cat` is the same tool for both providers: Codex runs it in its shell, Claude through Bash | `mcp_server/cli/commands.rs:408-420`; `claude_planner/spawn.rs:18` |
| F23 | Tests that pin prompt text: `TASK_BLOCK_PROTOCOL_GOLDEN` (`planner_card.rs:53`, `:359`); `planner_candidate_examples_use_the_native_execution_contract` (`:182`); the wake-set sentence (`:229`); provider kinds (`:649`); `read_route` (`tests/cases/planner_result_loop.rs:304`); the Dispatch sentence (`tests/cases/track_report_fork.rs:1818`); the full golden (`plugin_host/manifest.rs:1877`); the registry golden (`mcp_server/tools/mod.rs:118`) | read |
| F24 | Prose in Rust strings raises the prose ratchet, so new text goes in `prompts/**` files. The task block `usage` is stale ("slice 3b") | `scripts/gate-prose-ratchet.sh:1-6`; `mcp_server/tools/track_report_blocks/contracts.rs:357` |

## 2. Prompt shape (S1)

### 2.1 `planner.md` outline (≈8,300 B, cap 9,000)

| § | Section | ≈ B | Content |
|---|---|---|---|
| 0 | Identity | 250 | L1-3 |
| 1 | Turns | 900 | Turn-reactive; the wake kinds in one sentence; END YOUR TURN, never poll |
| 2 | State and the track | 1,000 | `neige state` is ground truth. Status via `calm.plan.list` detail=summary + key. Name once. Open/closed (L5-10). Ask with `calm.user.notify` or `calm.ratify.request` |
| 3 | Tasks | 2,700 | `TASK_BLOCK_PROTOCOL_GOLDEN` verbatim (965). Gates in 300 (re-runnable, never change tracked files; details → guide). Checkout in 300 (the track worktree, one task at a time, clean tree). Decisions in 550 (`ready:false` for a semantic dependency, verdicts only on producers, another round = cancel + new key). Candidate + publish in 350. Status in 250 |
| 4 | Track Report | 1,600 | The report carries its contract: maintain, never flatten. Blocks split at `#`/`##`. Prose budget from the contract, else 2000 字. Write in Chinese. Read with `calm.report.read`, one `calm.report.commit` per user intent (section ops); whole rewrite with `calm.report.write_markdown`. Do not restate kernel facts |
| 5 | Edits by others | 350 | Ground truth; never overwrite; re-read the section before writing |
| 6 | Reading outputs | 600 | `neige state/ls/cat`; "Read `runs/<attempt_id>.json`…" (kept for `read_route`); other reports are reference data; no `track_id` |
| 7 | Guides | 450 | One line per guide, below |
| | Headings | 250 | |

§7, verbatim shape:
> Read a guide before you need it, not every turn:
> - before driving an interactive terminal: `neige cat guide/terminal.md`
> - before writing a gate, or when one fails for environment reasons: `neige cat guide/gates.md`
> - before reacting to others' report edits, linking reports, tagging, or citing sources: `neige cat guide/report.md`
> - before reading worker results, gate logs, cards or other tracks' reports: `neige cat guide/outputs.md`

### 2.2 Guides: `crates/calm-server/prompts/guides/*.md`, served at `guide/<topic>.md`

| Guide | ≈ B | From |
|---|---|---|
| `terminal.md` | 3,000 | L26-30 as a procedure: resolve a Worker terminal by `attempt_id`; open with claim; start Claude; submit + wait; edit a draft; handle a stale observation; release. The switches' semantics stay only in `calm.terminal.*.md` |
| `gates.md` | 1,800 | L59-60: env allowlist, no neige/MCP, `gate.cwd`, `gate-target-mismatch`, `require_task_gates`/`no_gate_reason`, verification inputs |
| `report.md` | 3,000 | L109-123 condensed (the channel table), L69-70 links/outline/backlinks, L91 sources, L102 tags, L98 whole-document rewrite |
| `outputs.md` | 2,500 | L127-147: `runs/`, `plan/<key>/gate.log`, `cards/`, `area/reports/`, `@` mentions |

**Serving.** Add one branch in `track_file.rs` before `TrackFsView`, beside the `area/` branch (F22).
- A static table `GUIDES: &[(&str, &str)]` of `include_str!` entries is served for `guide/<name>.md` (cat) and `guide/` (ls).
- Every role that may call `calm.track.cat` can read it: the text is harmless.
- There is no new tool, no DB and no per-provider setup.

Why not skills: a skill is Claude-only and would need per-session install, while `neige cat` already reaches both providers.

**Where situational rules go.** Rules needed only on a failure or refusal go into that refusal or result text, not into a guide. Examples: `track-worktree-dirty`, `gate-target-mismatch`, a failed delivery (F20).

### 2.3 Fate of every current passage

| Current (line) | B | Fate |
|---|---|---|
| L12-14 JSON delivery, L16-22 candidate delivery | 5,574 | deleted with the mechanism (S2) |
| L24-30 terminal | 6,578 | one index line + `guide/terminal.md` |
| L36-42 wake list | 700 | 1 sentence; `execution_settled` gone (S4) |
| L48 briefing/preview reads | 1,549 | 200 in §2; the recovery text goes (S3) |
| L50 naming | 932 | 1 line; `calm.track.rename.md` |
| L51-56 status taxonomy | 2,967 | 250 in §3; one sentence per state in `calm.plan.list.md` |
| L57 Dispatch | 2,607 | deleted (S4) |
| L58 task block | 965 | verbatim |
| L59-60 gates | 2,128 | 300 + `guide/gates.md` |
| L61 recovery | 2,008 | deleted (S3); "another round = new key" stays |
| L62 shared attached workspace | 518 | deleted: legacy tracks without a worktree refuse tasks (`track-without-worktree`) |
| L63-66 | 1,761 | 850 |
| L67 delivery | 974 | 1 line; the wake text carries the rest (F20) |
| L69-70 links | 534 | `guide/report.md` |
| L74-107 report | 5,993 | 1,600 + `guide/report.md`; conflict and marker rules only in the tool descriptions |
| L109-123 reacting | 3,482 | 350 + `guide/report.md`; the quiet-sync rule is in the observation text |
| L127-147 views | 3,950 | 600 + `guide/outputs.md` |
| L149 receipts | 1,114 | deleted; `prompts/result-receipt/*.md` |

## 3. Mechanism deletions

### 3.1 What goes, and what happens to 4140 rows

**Rule (B1).** No migration deletes rows from `operations` or `events`: keyed operations are permanent (F9), and leftover rows are inert history. A migration drops only a table whose code is gone. Children are dropped before parents; all FKs are child→parent with `ON DELETE CASCADE` or none.

| Slice | Mechanism | Tables dropped (4140 rows) | Rows left as inert history |
|---|---|---|---|
| S2 | (a)+(b) file delivery, candidate verify/review/repair, `calm.task.repair`, `verified-candidate`, crate `calm-task-artifacts` (only consumer) | `task_candidate_decision_bindings`, `task_candidate_decisions`, `task_candidate_input_bindings`, `task_candidate_verification_allocations`, `task_file_candidates`, `task_file_input_bindings`, `task_file_publications`, `task_candidate_repairs` (all 0) | 2 ops, 2 events (F9, F10) |
| S3 | (c) `Recover`, dynamic-tool offer (a reject-all server-request reply stays), briefing, `calm.plan.recover`, `recovery.guidance` | `planner_recovery_calls` (1), `…_turns` (4), `…_issuances` (4), `…_threads` (39) | — |
| S4 | (d) the whole isolated-codex-v1 path: `isolated_codex/`, `dedicated_codex/`, `calm.task.dispatch`, user start, isolated grants, `activity`, task artifacts, user and Planner-side recovery admission, REST `recover`, FE "Recover task" and independent-task form, `task.execution_settled` | `planner_dispatch_receipts` (3) | 9 ops, 4 worker sessions, 6 events, 4 tasks and 3 current tasks, 1 recovery allocation, 7 worker cards |
| S6 | (f) `calm.task.delivery`. A failed delivery fails a gated task (`delivery-failed`) and leaves an ungated one `done`. `git-delivery-failures.md` stays: every failed settlement uses it | `task_git_delivery_abandonments` (0) | — |

Sizes, from the inventory: S2 ≈11.7k lines (+5.3k crate), S3 ≈3.0k, S4 ≈17k src + 13.5k tests + ≈1.2k FE, S6 ≈0.9k + 1.5k tests.

**Why the leftover rows are safe** (each point is pinned by §3.3):
- Operations: the driver loads only non-terminal rows, and all 11 are terminal (F13).
- Worker sessions: the 4 rows are ordinary codex executor rows with no handle state.
- `TaskAttemptOrigin::Recovery` stays deserializable as history: `/attempts` reads it, 1 row.
- Events: replay skips them with one error line per row, only when a replay window spans ids ≤ 09-12 (8 rows, bounded, no `Err`) (F14).
- The 4 isolated tasks: S4's report-block validator gives a `neige_execution` context a `neige_execution_retired` diagnostic. The block is kept and never projected or scheduled, the same shape as the `gate_required` diagnostic.

### 3.2 Ordinary-path consumers and their removal (B3)

| Call site on an ordinary read or write | Removed in |
|---|---|
| `mcp_server/tools/plan.rs:684` `file_delivery::view_tx` per entry | S2 (field gone) |
| `scheduler/mod.rs:1098` `bind_claim_tx` on every claim; `:1648`, `:1653-1668` review backfill and sweeps | S2 |
| `decision_sink.rs:110` `validate_report_tx` on every report; `:265-292` `prepare_verdict_tx` on every verdict | S2 |
| `task_recovery/view.rs:173-190` file-delivery branches | S2 |
| `calm-truth/src/db/sqlite/track.rs:384-402` allocation guard on track/area delete | S2 |
| `calm-types/src/report_blocks/tasks.rs:903-950` producer/consumer diagnostics | S2 |
| `plan.rs:714-718` `recovery.guidance` | S3 |
| `plan.rs:674` `isolated_codex::activity::read_tx` per entry | S4 (field gone) |
| `plan.rs` `recovery` field and `task_recovery_view_with_refusal_tx`: keep `view.current` (attempt_id, generation, status, blocking_reason); drop the refusal | S4 |
| `task_recovery/view.rs:191`, `task_recovery.rs:55` isolated branches | S4 |
| `track_activity/sql.rs:9,71` `isolated_card_exists_sql` | S4 |
| `git_candidate/view.rs:284-291` `declares_isolated`: the 4 legacy tasks then read as `no_lease` | S4 |
| `scheduler/mod.rs:163,1168,1181-1215,1683,1901`; `operation/task_launch.rs:117-172`; `operation/mod.rs:110,121` (keep the generic startable guard, drop `check_recovery_attempt_tx`) | S4 |
| `mcp_server/transport.rs:474,612`, `transport/call.rs:20` `isolated_grants` | S4 |
| `shared_codex_appserver.rs:1730,3396`; `reaper/mod.rs:116`; `worker_flow/mod.rs:371`; `routes/task_artifacts.rs` | S4 |

Kept on purpose:
- `calm.user.notify` (F21), with its description cut to 700 B.
- `calm.report.write_markdown` and the commit `move` op (owner).
- terminal.input `claim`/`release` (F18, F19).
- `allow_output_since_observation`: the stale-observation check fired 4 times and the flag cleared it 6 times.
- `wait_for=signal`: Claude hooks are live, with 3,938 `claude.hook` events.

### 3.3 The 4140-shaped fixture (B1, B4)

`crates/calm-server/tests/fixtures/legacy_4140_rows.sql` is added in S2 and applied after all migrations. It copies the shapes of F8–F11, anonymised:
- the 8 retired-kind events between two live events;
- the 11 keyed terminal ops and the 4 `worker_sessions` that point at them;
- the 4 isolated tasks and their allocations, including the 1 `recovery` origin;
- their report `task` blocks with `neige_execution`.

`tests/cases/legacy_4140_rows.rs::legacy_4140_rows_load` boots the app over it and asserts, with each expectation tightened per slice:
- `events_since(0, MAX)` is `Ok` and returns exactly the live neighbours;
- `drive()` and `recover_on_boot()` are `Ok`;
- the worker sessions and their cards load;
- `calm.plan.list` (summary and full) returns every current entry;
- `GET /attempts` for an isolated key is `Ok`;
- `calm.report.read` is `Ok`, with `taskDiagnostics` naming `neige_execution_retired` (from S4);
- track activity recomputes.

## 4. Terminal (Q3)

The index line plus `guide/terminal.md` replace L24-30. The switch semantics live only in the tool descriptions.

S5 deletes, with 0 Planner uses (F18):
- the `replace` and `click` actions;
- `allow_output_below_cursor`;
- `scroll_to_text`/`scroll_to_occurrence`;
- `format=image`;
- `claude_permissions` on open and the track policy. A migration drops the 0109 column; it has 0 non-null rows.

## 5. Report rules after #1877/#1883 (Q4)

`planner.md:93-98` still teaches block ops as a parallel path and re-explains -32001/-32602 and markers, which `calm.report.commit.md` and `calm.report.write_markdown.md` already state.

The new §4 says only:
- read the sections you change;
- make one `calm.report.commit` per user intent with section ops;
- a task block is an `upsert` op (the pinned paragraph);
- a whole rewrite is `calm.report.write_markdown` after a full read.

## 6. Tool-description cuts (Q5), bytes (description/schema)

| Tool | Now | S1 | Later |
|---|---|---|---|
| terminal.input | 4,626/2,126 | 1,800/1,700 | 1,500/1,300 (S5) |
| plan.list | 4,996/258 | 1,800 (one clause per state; guidance and `file_delivery` text go with S2/S3) | — |
| terminal.observe | 3,996/1,001 | 1,600 | 1,400/850 (S5) |
| terminal.open | 3,057/1,333 | 1,400 | 1,000/700 (S5) |
| task.dispatch | 2,856/1,540 | 1,500 | deleted (S4) |
| report.commit | 2,674/1,447 | 1,600/900 | — |
| report.read | 2,121/1,299 | 1,200/700 | — |
| source.capture | 2,736/579 | 1,500 (error list → refusal texts) | — |
| task.delivery | 2,727/345 | 1,200 | deleted (S6) |
| user.notify | 1,838/228 | 700 | — |
| plan.recover, task.repair | 1,249+345, 766+178 | as is | deleted (S3, S2) |
| 16 others | 16,858 | ≈12,200 | — |
| **Total** | **61,179** | **≈37,600** | **≈28,400 (24 tools)** |

## 7. Ratchets (Q6)

All caps are one-sided. A slice that shrinks a surface lowers its cap in the same PR, to the measured size rounded up to 500 B.
- **`planner_prompt_fits_its_byte_budget`** (`planner_card.rs` tests): `PLANNER_SYSTEM_PROMPT_TEMPLATE.len() <= 7_500`, anti-vacuity floor 3,000.
- **`every_guide_fits_its_byte_budget`** (same module): each `GUIDES` entry ≤ 6,144 B and ≥ 500 B; the sum ≤ 7,500.
- **`every_guide_named_in_planner_md_is_served`** (`tests/cases/`, through the real `calm.track.cat` MCP call as a Planner):
  - scan the rendered prompt for `neige cat guide/<name>.md` (≥ 4 hits, anti-vacuity);
  - each must return exactly its `prompts/guides/<name>.md` bytes;
  - `guide/` ls must list exactly the named set, so no orphan guides.
- **`planner_tool_surface_fits_its_byte_budget`** (`mcp_server/tools/mod.rs` tests, next to the registry golden):
  - sum `description.len()` + compact `input_schema` bytes over `descriptors_for_role(Planner)`;
  - each description ≤ 2,048 B;
  - cap S1 36,000; each later slice sets its measured size plus a small margin, ending at or below 29,000 after S6.
- The two goldens stay: they pin wording, and the caps pin size.

## 8. Slices (Q7)

Each slice is independently mergeable and green. Migration numbers are assigned last, at merge. Must-red entries read: test ← production line ← single mutation.

**S1**
- Content:
  - rewrite `planner.md` (§2.1);
  - add `prompts/guides/` + the `guide/` branch in `track_file.rs`;
  - cut descriptions (§6) and fix the stale `usage` (F24);
  - add the four ratchets;
  - delete `planner_candidate_examples_…`; update the fork sentence; regenerate both goldens.
- Must-red (each confirmed by running `-p calm-server --lib` plus `mcp_integration_suite`, `track_suite` and `mcp_core_suite`; the red set was exactly this):
  - `planner_prompt_fits_its_byte_budget` + `shipped_issue_development_rendered_prompt_matches_full_golden` ← `planner.md` ← re-append main's Terminal section.
  - `every_guide_named_in_planner_md_is_served` + `every_guide_fits_its_byte_budget` (its `GUIDES.len() >= 4` floor) ← `track_file.rs` `GUIDES` ← drop the `gates.md` row.
  - `every_guide_fits_its_byte_budget` ← `prompts/guides/terminal.md` ← append main's L30 twice.
  - `planner_tool_surface_fits_its_byte_budget` + `default_registry_matches_full_golden` ← `calm.terminal.input.md` ← restore main's text.
  - `planner_prompt_pins_callable_task_block_protocol` + `planner_prompt_contract_rejects_negative_context` + `shipped_git_forge_give_up_uses_the_track_close_tool` + `shipped_issue_development_rendered_prompt_matches_full_golden` ← `planner.md` §3 ← `ready: true` → `ready: false`.
- As built: `planner.md` 6,747 B; guides 1,751 + 2,011 + 1,650 + 2,033 = 7,445 B; tool surface 35,903 B. The terminal.input schema keeps its 2,126 B: every byte there is a live action shape or switch, so its cut waits for S5's deletions.

**S2**
- Content:
  - delete (a)+(b) and the S2 rows of §3.2;
  - migration drops 8 tables;
  - add the fixture and `legacy_4140_rows_load`;
  - FE schema and invalidation entries go.
- Must-red:
  - `legacy_4140_rows_load` ← `calm-truth/src/db/sqlite/events.rs:719-723` ← make the `Err(e)` arm `return Err(e.into())`.
  - new `file_delivery_tables_are_dropped` (calm-truth migration test) ← the migration ← delete its `DROP TABLE task_candidate_repairs`.
  - new `plan_list_ordinary_entry_has_exactly_the_kept_fields` ← `plan.rs` entry build ← re-add `entry["file_delivery"]=Value::Null`.
- As built:
  - Migration `0125_drop_file_delivery.sql` drops the 8 tables, children first. `file_delivery_tables_are_dropped` replaces the 0102 upgrade test in `calm-truth/tests/file_delivery_migration.rs`; it fills one linked row per table under enforced foreign keys, then drops.
  - Deleted with the mechanism: `file_delivery/`, `scheduler/file_delivery.rs`, `isolated_codex/{review_settled,repair_acceptance}.rs`, `track_report/repair.rs`, `calm.task.repair`, crate `calm-task-artifacts`, both settled event kinds, `IsolatedWorkspace::FileInput`, `DispatchArgs::VerifiedCandidate`, `RefusalSite::FileDeliveryInputUnhonoured`, `POST_EXECUTION_TASK_BOUND_ADAPTER_KINDS`, the track/area delete preflight, the replay reset rows, the non-Linux workspace-read stub, and the seven `docs/design-1501-*` documents of the mechanism.
  - `planner.md` stays 6,747 B: S1 had already removed the delivery text. The tool surface goes 35,903 → 33,996 B (27 tools), cap 34,000. Text removed: `calm.task.repair.md`; the verified-candidate workspace and `candidate_input` in `calm.task.dispatch.md` and its schema; the review-required acceptance sentence in `calm.task.verdict.md`; `file_delivery` in `calm.plan.list.md`.
  - Deviations. `calm.task.dispatch` keeps `workspace` required with the single value `empty`, so the 3 released receipts still deserialize. The fixture's isolated operations carry a minimal `tx_output_json` and its reports have no CRDT bytes (the payload-only legacy shape); the test seeds today's Planner sessions, which are not 4140 rows. Their recovery reads refuse with `track_not_ready`, because the tracks are closed, as on 4140.

**S3**
- Content: delete (c); migration drops the 4 recovery tables.
- Must-red:
  - new `codex_planner_thread_offers_no_dynamic_tools` ← `shared_codex_appserver.rs:1287` ← restore the `descriptor()` push.
  - new `planner_recovery_tables_are_dropped` ← migration ← drop one `DROP`.
  - `plan_list_ordinary_entry_has_exactly_the_kept_fields` ← `plan.rs` ← re-add `guidance`.

**S4**
- Content:
  - delete (d) and the S4 rows of §3.2;
  - drop `planner_dispatch_receipts`;
  - add the `neige_execution_retired` diagnostic;
  - regenerate OpenAPI, ts-rs and the worker prompt goldens (`prompts/worker/head-*.md:12` names `calm.task.dispatch`).
- Must-red:
  - `legacy_4140_rows_load` ← `operation/repo_sqlite.rs:173` ← add `'failed'` to the claimed phases (the retired kind then hits `unknown operation kind`).
  - `legacy_4140_rows_load` ← the `TaskAttemptOrigin` enum ← delete the `Recovery` variant.
  - new `neige_execution_context_is_not_projected` ← `report_blocks/tasks.rs` validator ← remove the diagnostic arm.
  - `plan_list_ordinary_entry_has_exactly_the_kept_fields` ← `plan.rs:674` ← re-add `activity`.
- Open items, resolved before S4 starts:
  - (a) Deleting recovery admission leaves a read-path hole. `GET /attempts` and `calm.plan.list` reach `admit_recovery_tx` for every failed task (`task_recovery/view.rs:169`, called from `plan.rs:650`), and the history view has a required `recovery` field (`task_recovery/view.rs:165`). S4 needs an explicit replacement read path and wire shape. 4140 has 1 `recovery` allocation and 4 failed isolated task rows, 3 of them current.
  - (b) "Task artifacts" means only the isolated attempt report REST route (`routes/isolated_tasks.rs:167`). The shared `task.completed.artifacts` field and the ordinary result views stay: 4140 has 23 current ordinary tasks whose `task.completed` carries non-empty artifacts (50 events, 0 isolated).

**S5**
- Content: terminal cuts (§4), with a migration dropping `tracks.claude_permissions_policy`.
- Must-red:
  - new `input_schema_lists_only_live_actions` (`tools/terminal/schema_tests.rs`) ← `tools/terminal.rs` action enum ← re-add `replace`.
  - new `input_keeps_claim_and_release_in_one_request` ← `terminal_interaction/operations.rs:43` ← skip the release step.
- As built:
  - Deleted with each switch, everything that existed only for it:
    - `replace`: `replace_plan.rs`;
    - `click`: `calm_terminal_view::click_bytes`;
    - `allow_output_below_cursor`: `ScreenDiff::only_below_cursor` and `Tolerance`. The stale `screen_diff` and the `allow_output_since_observation` drift stay;
    - `scroll_to_*`: `scroll_to.rs`, `TerminalView::find_text` and `Occurrence`;
    - `format=image`: the resvg `Rasterizer` (and the `resvg` dependency) and `ToolResult::png`;
    - `claude_permissions` and the Track policy: `terminal_permissions/`, `calm_types::claude_permissions`, the card stamp, the ceiling read and the PATCH field.
  - Migration 0124 drops the column.
  - `claude_permissions` and `claude_permissions_source` are no longer server-owned card keys. 4140 has 0 cards carrying them.
  - A PATCH that still sends `claude_permissions_policy` is ignored, like any extra `TrackPatch` key.
  - `track_updated.full.json` keeps the populated policy in `wire`, and `canonical` drops it: stored `track.updated` rows still replay.
  - The e2e UX collector drops its counters for the deleted switches.
  - Bytes: tool surface 35,903 → 33,980 B, cap 36,000 → 34,000.
    - terminal.input 1,494/1,647 (description/schema), observe 1,391/779, open 787/874.
    - The input and open schemas miss the §6 targets (1,300 and 700): every remaining property is live (claim/release stay).
  - `planner.md` is unchanged at 6,747 B. `guide/terminal.md` goes 2,033 → 1,989 B: step 8 fixes a draft with one `sequence`.
  - Must-red placement: `input_schema_lists_only_live_actions` takes over the action-arm checks of the flat-schema sweep. `input_keeps_claim_and_release_in_one_request` is in `tests/cases/terminal_input_control.rs`; it replaces the tail of the release test.
  - Mutation evidence, with `-p calm-server --profile ci --no-fail-fast` each time:
    - re-add the `replace` arm reds exactly `input_schema_lists_only_live_actions`, `default_registry_matches_full_golden` and `planner_tool_surface_fits_its_byte_budget` (34,197 B);
    - skip the release step reds exactly `input_keeps_claim_and_release_in_one_request`, `input_release_releases_after_the_write_and_reads_back_as_observer`, `input_release_after_a_takeover_reports_not_held` and `summary_on_written_receipts_follows_the_readback_not_the_lease`.

**S6**
- Content: delete (f); drop `task_git_delivery_abandonments`.
- Must-red:
  - new `failed_delivery_fails_the_gated_task` (`tests/cases/git_delivery.rs`) ← `dispatcher/git_delivery_settled.rs` ← leave the task `verifying`.
  - `planner_tool_surface_fits_its_byte_budget` ← re-register `task_delivery`.
- As built:
  - The flip lives in the settlement transaction, `scheduler/git_delivery.rs::settle_tx`: `dispatcher/git_delivery_settled.rs` only maps the event to an observation. A failed settlement of a gated `verifying` row sets `failed/delivery-failed` (`task_fail_delivery_tx`, the old abandon UPDATE renamed) and appends one kernel `task.failed` after the settlement event. The gated `task.failed` rule already suppresses it, so the settlement stays the one wake. The must-red mutation is therefore in `settle_tx`.
  - Migration `0126_drop_task_git_delivery_abandonments.sql` drops the one table (0 rows on 4140).
  - Deleted with the tool: `git_candidate/{action,abandonment}.rs`, the retry-row insert and request-key reader, `DeliveryState::Abandoned`, `NoCandidateReason::DeliveryAbandoned`, `Observation::TaskGitDeliverySettled.delivery_id`, `SchedulerPokes::poke`, the wake text's `Decide:` clause, and `failure.retry_allowed` on the read surface.
  - Kept: the `task_git_deliveries` columns `retry_allowed`, `ordinal`, `predecessor_delivery_id`, `request_idempotency_key` and `reason`. The 0113 CHECK and trigger name them, so dropping them needs a table rebuild. `retry_allowed` also stays on the persisted `task.git_delivery_settled` event (`deny_unknown_fields`). The settlement still writes it; nothing reads it.
  - A `verifying` row beside a failed delivery can no longer exist, so the gate's `NoCandidateReason::DeliveryFailed` is kept only to keep the mapping total. The `gate_binding` tests that built that state now use a delivery held in its hook, and the case that pinned `DeliveryFailed` is gone.
  - Tool surface 33,980 B (after S5) → 32,564 B, cap 34,000 → 33,000. The ≤ 29,000 target needs S2–S4 too. `planner.md` is unchanged (6,747 B): it never named the tool.

Dependencies: S2 and S3 are independent. S4 needs both. S5 and S6 are independent of S2–S4.

## 9. Decided (owner)

- Delete isolated-codex-v1 entirely (S4), including the user "Start independent task" and "Recover task".
- Delete `calm.task.delivery` (S6).
- Make the terminal cuts of §4 (S5), keeping terminal.input `claim`/`release`.
- Keep `calm.report.write_markdown` and the commit `move` op.
- Guides are read on demand (§2.2).

## 10. Review findings adjudicated

- B1 accepted. Verified: the trigger is at `0099:89`; all 11 ops are keyed; 4 are `spawn_op_id`. The fix is to leave the rows and prove the loaders (§3.1, §3.3).
- B2 accepted. F18 is corrected to input claim 5 / release 5, plus open claim 6; the atomic path is F19.
- B3 accepted. Every ordinary-path call site in §3.2 has a slice, pinned by `plan_list_ordinary_entry_has_exactly_the_kept_fields`.
- B4 accepted. See §3.3 and the S2/S4 must-reds.

## 11. KNOWN GAPS

- The template JSON (3–10 KB), the Claude fragment and plugin tools visible to the Planner are not budgeted.
- A Planner may skip a guide it needed. Its index line names the moment to read it, and the tool descriptions still carry the contracts.
- Between S1 and S2–S4, the deletion-bound tools stay visible.
- `T` holds tool calls only from 09-16; older use comes from tables and events.
- Bytes are a proxy for tokens; CJK costs about 3 B per character.
- A replay spanning ids up to 09-12 logs 8 skip lines.
- `task_replacements` (1 row, unread since #1866) stays.
