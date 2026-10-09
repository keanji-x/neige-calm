# #2493 — Worker conversation: explicit attempt↔session binding and TUI message delivery

Status: design, for L2 review. Issue: #2493 (requirements, 4140 data, TUI probe). Code
references are to `f1ca87776`. Builds on 773 (worker lifecycle), 2053 (worker reports), 2003 and
`docs/conventions/agent-commands.md` (command surface).

Two features, one foundation:

- **A, continuation.** After a worker's task ends (done or failed), the Planner gives the *same*
  worker (card, provider conversation, MCP token) the next round instead of a cold start.
- **B, message.** The Planner adds a line to a *running* worker's conversation.
- **S1** makes "which attempt does this worker session serve" an explicit kernel fact read through
  one resolver. **S2′** is one generic "deliver a message to an agent TUI" path. B = S2′ aimed at
  the current attempt's worker; A = an S1 continuation binding plus the new prompt sent by S2′.

Pain (issue table, 4140, 14 days): of 72 next-round pairs (same track and kind, `read_write`), 66
found the previous worker still alive (Codex thread loaded and idle, Claude at its prompt) and were
cold-started anyway. Owner decisions taken as fixed here: bind to the worker session; lease per
attempt; the Parked state refuses all input; the resolver returns a typed state (issue comment
"S1 评审意见", 2026-10-09).

## 1. Model

| Fact | Owner | Identity | Lifetime |
|---|---|---|---|
| Attempt | `tasks` row (calm-truth) | contract: a new key, a new `attempt_id` per round (unchanged) | frozen contract, CAS terminal state (773) |
| Worker session | `worker_sessions` row | conversation: thread / Claude session, MCP token, terminal | until track close, cancel/timeout reap, reaper, card delete |
| **Binding** (new) | calm-truth, `tasks.worker_session_id` + `tasks.worker_bind_seq` | attempt → the session that executes it | written once per attempt |

Today the binding is inferred three ways: the spawn op whose `idempotency_key` is the attempt and
whose target is the card (`calm-truth/src/db/sqlite/task.rs:270-294`), `tasks.worker_card_id`
stamped late by `COALESCE` (`task.rs:242-266`, `scheduler/mod.rs:1398-1402`), and card payload
`idempotency_key` (`track_vcs/runs.rs:85-95`, `scheduler/mod.rs:2061-2068`). The report path needs
an `owns_key` op proof for the window before the stamp (`decision_sink.rs:116-132`).

A session today outlives its attempt. The report transaction changes only the task row and the
lease (`decision_sink.rs:100-190`); the gate path never touches sessions. A session ends only on
track close (`terminal_sweeper.rs:68-74`, and only with no in-flight attempt), a cancel/timeout
marker sweep (`scheduler/mod.rs:95-136`, `:1650-1729` → `operation/driver.rs:233-264`), the reaper
on a dead process (`reaper/mod.rs:95-372`), or card deletion. Nobody decides "end after done".
So continuation needs **no keep-alive mechanism**.

### Decisions

| Question | Decision | Rejected alternative |
|---|---|---|
| Where | Two nullable columns on `tasks`: `worker_session_id TEXT REFERENCES worker_sessions(id) ON DELETE SET NULL`, `worker_bind_seq INTEGER CHECK (worker_bind_seq IS NULL OR worker_bind_seq >= 1)` | A table `worker_session_attempts`: the invariant needs `tasks.status`, so it would need a trigger. A pointer `worker_sessions.attempt_id`: loses attempt→session for history, which 773's replay rule and report idempotency need |
| Invariant | `CREATE UNIQUE INDEX tasks_one_working_attempt_per_session ON tasks(worker_session_id) WHERE worker_session_id IS NOT NULL AND status IN ('dispatched','running')`; `CREATE UNIQUE INDEX tasks_worker_bind_order ON tasks(worker_session_id, worker_bind_seq) WHERE worker_session_id IS NOT NULL` | Including `verifying`: `task_regate_tx` moves `failed → verifying` (`task_regate.rs:27-36`); excluding it keeps the index about the worker's acting states. Regate is refused while the checkout is in use or a later task delivered (`task_verify_adapter/regate.rs:169-186`) |
| Order | `worker_bind_seq` = 1 for a first spawn, last + 1 for a continuation; the unique index makes it strict | `created_at_ms` / `finished_at_ms` ordering: ties when two tasks are declared in one commit |
| Card column | `tasks.worker_card_id` stays as a read column, written only by the binding writer in the same `UPDATE`; the `COALESCE` stamps in `task_mark_running_tx` and the three report flips are deleted | Drop it: ~45 files read attempt→card, a meaning that stays correct |
| Single writer | `worker_binding::bind_attempt_tx` (calm-truth), called in the transaction that creates the session: the `prepare_tx` of `codex-worker` (`codex_adapter/mod.rs:777-897`), `claude-worker` (`claude_adapter/mod.rs:767-905`), `terminal-worker` (`terminal_adapter.rs:621-700`), the new `worker-continue` op (§5), and `claude-restart` carrying the binding (below) | Binding at `mark_running`: leaves today's unstamped window that needs the op proof |
| Claude restart | `claude-restart` ends the old session and starts a new row for the same card and Claude conversation (`--resume`, `claude_restart_adapter.rs:175-248`). In that transaction `carry_binding_tx(old, new)` moves the old session's latest binding to the new session. 0 `claude-restart` operations exist on 4140 | Leave it: the restarted card would resolve `Unbound` and accept input as a plain terminal |

The SQLite partial index is legal (deterministic `WHERE`, no subquery). `bind_attempt_tx` is one
guarded statement: `UPDATE tasks SET worker_session_id=?s, worker_card_id=?c, worker_bind_seq=?n
WHERE id=?a AND status='dispatched' AND worker_session_id IS NULL`, 0 rows → `Conflict`.

### Migration and 4140 backfill

One migration (numbered last at merge): the two `tasks` columns and indexes,
`workspace_leases.attempt_id TEXT` plus index `(attempt_id, state)` (§3), then:

```sql
UPDATE tasks SET
  worker_session_id = (SELECT ws.id FROM operations o JOIN worker_sessions ws ON ws.spawn_op_id = o.id
    WHERE o.idempotency_key = tasks.id AND o.kind IN ('codex-worker','claude-worker','terminal-worker','codex-isolated-worker')
      AND json_extract(o.payload_json,'$.actor.kind') = 'KernelDispatcher'),
  worker_bind_seq = 1
WHERE EXISTS (SELECT 1 FROM operations o JOIN worker_sessions ws ON ws.spawn_op_id = o.id
    WHERE o.idempotency_key = tasks.id AND o.kind IN ('codex-worker','claude-worker','terminal-worker','codex-isolated-worker')
      AND json_extract(o.payload_json,'$.actor.kind') = 'KernelDispatcher');
UPDATE tasks SET worker_card_id = (SELECT card_id FROM worker_sessions WHERE id = tasks.worker_session_id)
WHERE worker_card_id IS NULL AND worker_session_id IS NOT NULL;
UPDATE workspace_leases SET attempt_id = (SELECT t.id FROM operations o JOIN tasks t ON t.id = o.idempotency_key
  WHERE o.id = workspace_leases.lease_owner);
```

Run as `SELECT`s, read-only, on the 4140 database (`sqlite3 -readonly`, migration 160, 2026-10-10):

| Check | Result |
|---|---|
| tasks / with `worker_card_id` | 301 / 298 |
| bindings found | 282 rows = 282 attempts = 282 sessions: 278 via today's three spawn kinds (codex 201, claude 59, terminal 18) + 4 failed codex attempts via the retired `codex-isolated-worker` kind (no code left; without it they would resolve `NoSession`) |
| attempt with >1 session; session with >1 attempt; working attempts per session >1 | 0; 0; 0 |
| stamp ≠ session card | 0 |
| bound but `worker_card_id` NULL | 1 (`spawn-failed: operation drive failed …`) — filled by the second `UPDATE` |
| stamped, no session row | 17, all terminal; cards and sessions deleted → stay unbound (`NoSession`) |
| sessions per task-bound card | 1 for all 278 checked (owner's note: never a session switch) |
| leases resolving to an attempt | 273 / 273; >1 lease per attempt or card: 0 |
| non-terminal tasks, timeout markers, `claude-restart` ops | 0, 0, 0 |

No ambiguity: the backfill is exact. 50 worker spawn ops have no `tasks` row (pre-scheduler
workers); their sessions stay `Unbound`, as today's legacy report path expects
(`worker_report.rs:39-46`).

## 2. Resolver and reader rewrite

`calm-truth/src/db/sqlite/worker_binding.rs` owns the rule, in two forms tested to agree: a scalar
function and one SQL fragment for set-based readers.

```rust
pub enum WorkerBinding {
    Live { attempt_id: String, session_id: String, status: TaskStatus }, // dispatched | running
    Parked { last_attempt_id: String, session_id: String },              // verifying | done | failed | canceled
    Unbound { session_id: String },                                      // active session, no attempt ever bound
    NoSession,                                                           // session ended, deleted, or never bound
}
pub enum WorkerOf<'a> { Session(&'a str), Card(&'a str), Attempt(&'a str) }
pub async fn worker_binding_tx(conn, of: WorkerOf<'_>) -> Result<WorkerBinding>;
```

`Session(s)`: `s` not in an active state (`WorkerSessionState::is_active_authority`,
`calm-types/src/worker.rs:253-261`) → `NoSession`; else the max-`worker_bind_seq` attempt decides
Live/Parked; none → `Unbound`. `Card(c)` goes through the card's one active session
(`ws_one_active_per_card`, migration 0156:76). `Attempt(a)` resolves `a`'s session; the caller
compares `attempt_id`. **`Unbound` extends the owner's three states**: Planner-opened terminals
(`neige_terminal_open`) are worker-role sessions with no attempt, and the input path serves them;
without the variant that path would need a second question (an `Option` in disguise). See §10.

Every reader declares what it accepts. "Neutral" = identical result while each session has one
attempt (every 4140 row).

| # | Reader (f1ca87776) | Today | New call | Accepts | With a second attempt on the session |
|---|---|---|---|---|---|
| 1 | `decision_sink/worker_report.rs:14-73` `admit_worker_report_tx` | spawn-op keys ∪ `worker_card_id` of the card must be one key | `Session(identity.session_id)` | Live{a = reported} admits. A reported attempt bound to this session and terminal: same outcome → idempotent, else `Conflict` (773 CAS loser). Otherwise refuse naming the Live/Parked attempt | new attempt's reports admitted (today all refused) |
| 2 | `decision_sink.rs:116-132`, `task.rs:329-420` `TaskReporter::Card{owns_key}`, flips' `worker_card_id IS NULL AND owns_key` arm | op proof for the unstamped window | `TaskReporter::Session{session_id}`; flips guard `worker_session_id = ?` | Live | correct row only |
| 3 | `task.rs:242-266` `task_mark_running_tx` `COALESCE` stamp | late stamp | removed (bound in prepare) | — | — |
| 4 | `task.rs:270-327` `worker_op_targets_card_tx`, `WORKER_SPAWN_OPS_OF_CARD`, `card_is_worker_spawn_target_tx`, `worker_card_declared_head_tx`; callers `claude_restart_adapter.rs:152,196`, `scheduler/mod.rs:2145`, `worker_failure.rs:71` | creating spawn op | deleted; restart: `Card(c)` ≠ Unbound, head of Live/Parked attempt | Live, Parked | head of the latest attempt, not the first |
| 5 | `read.rs:296-308` `task_for_worker_card` (`LIMIT 2` → Conflict); callers `dispatcher/mod.rs:281-301`, `target.rs:193` | ambiguous on 2 rows | deleted; `Card(c)` | stop-hook push only for Live (as today's `dispatched|running`) | no Conflict |
| 6 | `operation/terminal_launch.rs:93-103` | card's worker op `LIMIT 2`; `EXISTS tasks.worker_card_id` | the terminal's session `spawn_op_id` (who launched it — a creation fact, not attempt ownership); task-owned = `Card(c)` ≠ Unbound | any | none: continuation never launches |
| 7 | `terminal_interaction/target.rs:109-238` | `spawn_op_id` → op key; fallback #5; Running-only `controllable` (`:218-222`); codex refusal (`:47-80`, `:216`) | `Session(terminal's session)` | writes: Live with `Running`; Parked → refusal §4; reads: Live, Parked | input reaches the new attempt; parked refuses |
| 8 | `worker_quiet.rs:120-160` | through #7 | through #7; quiet anchored at `max(last_output, running_started_at_ms)` (PR-3) | Live Running | no false wake right after a bind |
| 9 | `codex_adapter/mod.rs:1195,1246` `persisted_turn_id` skip | first-spawn crash re-drive | unchanged; the continuation op never calls `spawn_codex_worker_via_shared_daemon` | — | not on the path |
| 10 | `scheduler/mod.rs:95-136` marker by card | `UPDATE … WHERE card_id` | the fail tx resolves `Attempt(a)` = Live before its flip and marks that session `{task_id: a}`; no live session → release `a`'s lease | Live | cannot mark a newer attempt |
| 11 | `scheduler/mod.rs:1650-1729` sweep | kill card, release card's lease | kill, release the lease of `marker.task_id` | marker's attempt | per attempt |
| 12 | `scheduler/mod.rs:1752-1765` `worker_card_id_for_task`; `:1809-1825` `reconcile_running_terminal` | op fallback | binding of the attempt row | the attempt | neutral |
| 13 | `scheduler/mod.rs:2037-2068` `on_terminal_exit` | card payload `idempotency_key` | `Card(c)` Live | Live | — (terminal kind never continues) |
| 14 | `scheduler/running_worker.rs:175-196` `idle_candidate_thread` ("each execution gets a fresh card and thread") | latest codex session of the card | the attempt's `worker_session_id` row; comment rewritten; turn must complete after `running_started_at_ms` (PR-3) | the attempt | the predecessor's old turn cannot fail the successor |
| 15 | `reaper/mod.rs:375-470` `converge_dead_worker` | `spawn_op_id` → attempt; fallback release by card | `Session(s)`: Live → fail it, release its lease; Parked/Unbound → nothing to fail | Live | fails the new attempt, not the old |
| 16 | `workspace_lease/release.rs:54-72`, `:163-184` (`lease_attempt_tx`: owner op, else card's newest task); callers `decision_sink.rs:185`, `reaper:433,461`, `scheduler:125,1701`, `routes/cards.rs:634`, `plugin_host/callbacks.rs:536` | by card | `release_workspace_lease_for_attempt_tx(a)`; card delete releases `Card(c)` Live's lease | Live (the ending attempt) | a late old report cannot release the new lease |
| 17 | `workspace_lease/facts.rs:50-81`; callers `plan.rs:589-600`, `git_delivery_settled.rs:53-61`, `git_candidate/view.rs:308-311`, `task_gate_run/admission.rs:72`, `task_verify_adapter/target.rs:219-223` | newest lease of the card | lease of the attempt; last commit from the `worktree.committed` event whose `delivery_id` is the attempt's | the attempt | old attempt's view never shows the new lease |
| 18 | `task_gate_run/admission.rs:48-57` | `worker_card_id == card` and Running | `Session(identity)` Live{a = attempt_id} Running | Live | neutral |
| 19 | `track_activity.rs:145-159`, `:186-215`; `track_activity/sql.rs:144-170` | every current row raises its `worker_card_id` | only the session's max-seq attempt raises the card (SQL fragment) | Live or Parked.last | old `failed` no longer outranks new `working` |
| 20 | `track_vcs/delta.rs:344-394`, `runs.rs:85-95,373-395`, `track_fs_view/mod.rs:518` | card payload key; first run with the card | run ↔ card from bindings; card events go to Live or Parked.last | Live, Parked.last | events attributed to the latest run |
| 21 | `scheduler/worker_failure.rs:51-74` card delete | `worker_card_id` or op proof | `Card(c)` Live | Live | neutral |
| 22 | `terminal_sweeper.rs:68-74` closed-track sweep | no current task with `worker_card_id = card` in `dispatched|running|verifying` | same statuses over `worker_session_id = ws.id` | — | neutral |
| 23 | `plan/cancel_running.rs:110-114`, `task.rs:131-151` | CAS on `worker_card_id` | CAS on `worker_session_id` | the attempt | neutral |

Unchanged (attempt-keyed, already correct): checkout occupancy (`track_occupancy.rs`), liveness
deadline (`scheduler/worker_liveness.rs:36-52`: anchor `max(started, progress)` per attempt row),
capture and Claude settings per card, thread cache, every reader of a given attempt's
`worker_card_id` (`plan.rs`, `track_state.rs`, `task_recovery/view.rs`, `child_track_adapter`).

**Neutrality proof for PR-1.** (a) No existing assertion changes; fixtures that hand-insert a
running worker bind it through `bind_attempt_tx`, the production writer. (b)
`worker_binding_backfill_matches_spawn_op_inference`: the real migrator to N−1, seed spawn op /
session / task / lease rows for each kind × {dispatched, running, verifying, done, failed,
canceled} plus an op-less legacy session and a deleted-session row, migrate to N, and assert each
resolver answer against literal expectations derived from today's inference. (c) The 4140 SELECTs
above.

**Scan.** `scripts/gate-2493-worker-binding-inference.sh` (picked up by
`local-ratchet-gates.sh`): a whole-repo `git grep` for `spawn_op_id` reads, `worker_op_targets`,
`WORKER_SPAWN_OPS`, `operation_idempotency_key_by_id`, `task_for_worker_card`, `owns_key`, card
payload `idempotency_key` reads and `worker_card_id =` in a `WHERE`, with an allowlist that pins a
count per file and a reason (released migrations, the binding module, the writer in
`session_mirror.rs`, terminal launch's creation lookup).

## 3. Lease, timeout and cleanup per attempt

- **Lease key.** `workspace_leases.attempt_id` (backfilled 273/273). `acquire_workspace_lease_tx`
  (`workspace_lease/mod.rs:145-166`) takes the attempt; `lease_owner` stays the op. Every release
  and fact read is by attempt (#16, #17). `card_id` stays (path uniqueness and UI).
- **Worker timeout.** `running_started_at_ms` and `running_deadline_ms` are stamped by
  `mark_running` per attempt (`task.rs:242-266`), so the cap and idle window already start at the
  continuation's `running` stamp, not session start. Progress reads the card's capture cursor
  (`task_liveness.rs:47-72`); the anchor `max(started, progress)` hides the predecessor's progress.
- **Codex turn-ended check** (`running_worker.rs:104-127`) counts a turn only if it completed after
  the attempt's `running_started_at_ms`; otherwise the predecessor's long-finished turn fails a
  fresh continuation before its prompt lands. **Quiet detector** (#2507) likewise.
- **Cleanup marker.** Written only for the Live attempt being failed (#10), carries its id; the
  sweep releases that attempt's lease (#11). Continuation admission refuses a marked session, so a
  marker never meets a newer attempt.

## 4. Message delivery (S2′)

**Declaration.** The provider registration declares how its TUI takes input, typed and required on
`calm_exec::WorkerProvider` (`calm-exec/src/provider.rs:40`), implemented in `provider::worker`
and registered in `calm-server/src/provider_registry.rs:28-58`:

```rust
pub struct TuiInput { pub message: MessageDelivery, pub keys_while_bound: BoundKeys }
pub enum MessageDelivery { BracketedPasteSubmit, Unsupported }
pub enum BoundKeys { Accepted, Refused }
```

Codex `{BracketedPasteSubmit, Refused}`, Claude `{BracketedPasteSubmit, Accepted}`, terminal and
managed `{Unsupported, Accepted}`. `keys_while_bound` replaces the `card.kind == "codex"` check in
the generic layer (`target.rs:216`). Text transforms are documented, not coded: Codex trims leading
and trailing whitespace, Claude turns a tab into four spaces (probe c5/c7, k3).

**Envelope and encoding** (`terminal_interaction/actions.rs`, one function for A and B): one write,
`WriteShape::Verbatim`, of `ESC[200~` + header + `\n` + text + `ESC[201~` + `\r`. Headers are fixed
kernel text, so `/` and `!` never lead (probe c9, c10, k5):

- B: `[neige] Planner message for attempt <attempt_id>:`
- A: `[neige] Next round: attempt <attempt_id> of task <key>; attempt <previous_id> has ended.`

Text: non-empty; every `char::is_control` except `\n` and `\t` refused (C0 incl. ESC and CR, DEL,
C1), so no escape sequence and no early `ESC[201~`. Cap **8,000 bytes** for header + text (the
probe's 7,999-byte paste arrived byte-exact on both TUIs). Today's `submit` splits the CR into a
second write (`actions.rs:129-137`, `control_writer.rs:20-38`); a message never does.

**Preconditions,** checked in this order; all refusals write nothing:

1. Binding: Live with status `Running` (B; A checks Parked, §5). Parked/NoSession/Unbound refused.
2. Declaration: `message` ≠ `Unsupported`.
3. Terminal live and readable: renderer entry present and `observable()`, not exited (as
   `neige_terminal_show`'s `available`, `target.rs:268-279`; client attach
   `terminal_interaction/mod.rs:122-127`); input surface has DECSET 2004 bracketed paste on
   (`MODE_BRACKETPASTE`, a terminal mode the view already tracks in `InputSurface.modes`,
   `calm-terminal-view/src/lib.rs:59-67`).
4. No input-capturing dialog: Codex `worker_sessions.last_thread_status` is not `waitingOnApproval`
   or `waitingOnUserInput` (liveness feeder, `liveness_feeder.rs:26-41`). Claude: **no sound
   signal.** Hook payloads are "forgeable … never authority" (`terminal_hooks.rs:3`), no hook moves
   card state (`routes/claude_cards.rs:227`), and a dialog closed by Esc fires no hook; on 4140 the
   Claude worker hook stream holds 0 `permission_request` events and 0 `permission_prompt`
   notifications (55 `idle_prompt`). Decision: no Claude check; KNOWN GAP. Rejected: refusing while
   the last hook is `permission_request` (misses lost hooks, then blocks forever after Esc).
5. Control: as `input` decides today — claim if unowned, never from a human
   (`operations.rs:126-138`, `:294-305`).
6. Never sends Esc, Ctrl+C or Tab (the encoder has no such bytes).

**Surface: a new action of `neige_terminal_input`,** not a new tool. `input` is the vocabulary verb
for "send text or keys to a terminal" (agent-commands §3); `send` is mail only. The action reuses
target resolution, the anchored read (`observation_id`), `idempotency_key` replay, `claim`,
`release`, `allow_output_since_observation` and `read` readback unchanged, so every option means the
same on every action (owner rule 3). Schema delta (`mcp_server/tools/terminal.rs:49-58`): the first
`action` arm's `"type":{"enum":["text","submit"]}` becomes `["text","submit","message"]`; the cap
is enforced in the kernel, not by `maxLength`. Description (`prompts/tools/neige_terminal_input.md`)
gains one sentence: "message (agent workers: text pasted under a kernel header, then Enter; a
running worker gets it this turn or next; ≤8,000 bytes; no control characters but newline and
tab)." CLI: none — terminal tools are MCP-only (agent-commands §2); `neige terminal input` keeps
answering the "MCP only" usage error. PR-2 measures `planner_tool_surface_fits_its_byte_budget`
(cap 30,120 B) and trims wording, never the cap.

Refusals (`-32403` like every terminal runtime failure, §9 of agent-commands; text and
`data.refusal` agree):

| `data.refusal` | Text (after `neige_terminal_input: `) |
|---|---|
| `worker_parked` | `attempt <id> (task <key>) is <status>; its worker takes no input until a task continues it. Declare a task with "continues": "<key>", or a new task for a fresh worker.` |
| `worker_ended` | `the worker of attempt <id> has ended (<state>); nothing was sent. Declare a new task.` |
| `worker_starting` | `attempt <id> is dispatched; its worker is starting. Read again, then send.` |
| `message_unsupported` | `action "message" needs an agent worker (<providers that declare it>); terminal <id> runs <provider>. Use "submit".` |
| `worker_keys_refused` (was `codex_task_worker_input`) | `a <provider> task worker takes only action "message"; typed keys interrupt its turn without starting one.` |
| `worker_awaiting_input` | `the worker's thread is <status>; Enter would answer it. Read its screen; wait or cancel the task.` |
| `terminal_unreadable` | `the worker's terminal has no live readable view (after a server restart until it is reattached, #2499); nothing was sent.` |
| invalid text (`-32602`) | `message text: U+001B at byte 12; only printable characters, newline and tab` / `message is 9,213 bytes with its header; the limit is 8,000` |

#1787 changes from "codex task worker: no input, no claim" to "codex Live worker: `message` only";
the guide line "await codex task settlement before planning its successor"
(`prompts/guides/terminal.md:6`) becomes "send `message` by `attempt_id`".

**Feature B** = this action with `attempt_id`. Confirmation is the readback (`read: true`); a stuck
worker is caught by the #2507 quiet wake. No receipts.

## 5. Continuation (feature A)

**Declaration.** Task-block field `continues: "<key>"` — "this task runs in the worker of `<key>`'s
current attempt". Added like `start` (migration 0138): `TASK_FIELDS`
(`report_blocks/kinds.rs:160-183`), a validator next to `validate_task_start`
(`task_execution.rs:138-178`), the block JSON schema (`track_report_blocks/contracts.rs:377-382`
neighbour), projection (`task_projection.rs:874-878`, `:1643-1686`), column `tasks.continues TEXT
NULL`, `neige_task_ls` fields (`plan/list.rs:102`), CLI render. Rejected names: `depends_on`
(requires `done`; a gate-red predecessor is `failed`), `resume` (Claude's `--resume` meaning).

**Admission.**

| Stage | Rule | Where |
|---|---|---|
| Block (static) | kind `codex`/`claude`; access `read_write`; default `spawn` (no child track); `start: checkout`; no `head`/`base`; not its own key | validator |
| Plan (cross-task) | named key exists, same kind, `read_write` (so reviews never continue nor are continued); `continues` is a graph edge for `find_cycle` | `report_blocks/tasks.rs:550-571,725-732`, diagnostics `unknown_continuation`, `continuation_mismatch` |
| Ready | predecessor's current attempt is terminal (`done`, `failed`, `canceled`); else waits like a dependency | `checkout_admission` (`track_occupancy.rs:115-150`) |
| Bind (authoritative, `worker-continue` `prepare_tx`) | `Attempt(pred)` is Parked{last = pred} with pred `done` or `failed`; no cleanup marker; preconditions 2–5 of §4; rendered prompt + header ≤ 8,000 bytes | one transaction |

"One continuation per session" falls out: a second task continuing the same key waits on the
checkout (`InUse`) and then finds Parked{last ≠ pred} → refused.

**No in-progress turn** is not an admission rule. Signals the kernel has: Codex
`last_thread_status ∈ {active, waitingOnUserInput, waitingOnApproval}` (feeder; reaper uses the same
set, `reaper/mod.rs:171-176`); Claude only `UserPromptSubmit` / `Stop` hooks, unsound as above. The
probe shows a paste near or during a turn is delivered exactly once (c10, k7): Codex steers, Claude
queues. Only the dialog statuses refuse.

**Operation `worker-continue`** (one adapter for both providers; payload `{actor, track_id,
idempotency_key: attempt_id, continues}`, a pure function of the frozen row as
`build_worker_payload` requires, `scheduler/mod.rs:157-201`):

1. `prepare_tx` (one transaction): admission above; `bind_attempt_tx(seq = last + 1)`;
   `prepare_worker_lease_tx` + `acquire_workspace_lease_tx` with `attempt_id`; render the prompt
   with `render_task_worker_prompt_tx` for the **new** `attempt_id` and the same surface
   (MCP/CLI) as the provider's spawn; record `message_delivery: not_requested` in the output.
   Creates no card, session, thread, token or process.
2. `spawn_side_effect`: CAS `not_requested → requested` on the op row (the pattern of
   `terminal_launch.rs:113-121`), then one §4 write as a kernel terminal client (claim if unowned,
   write, release). A re-drive that finds `requested` does not write again: at-most-once.
3. Scheduler: `reconcile_spawn_result` → `mark_running` (new deadline). Compensation releases only
   the attempt's lease; it **never** deletes the reused card or session (unlike
   `cleanup_codex_worker`, `codex_adapter/mod.rs:1033-1040`).

It never calls `spawn_codex_worker_via_shared_daemon`, so the `persisted_turn_id` skip
(`codex_adapter/mod.rs:1195,1246`) is off the path. Any refusal fails the op; the scheduler's
`fail_spawn` (`scheduler/mod.rs:1459-1484`) records `spawn-failed: continuation refused: <reason>`
and `task.failed` wakes the Planner. Never a silent cold start. Reason texts: `the worker of <key>
(attempt <id>) has ended (<state>); declare the task without "continues"`, `… was canceled; its
worker was stopped`, `… already continued by <key2>; continue <key2>`, `… is being stopped
(cleanup pending)`, `the next-round prompt is <n> bytes; the paste limit is 8,000: shorten goal or
context, or drop "continues"`, plus §4's terminal, dialog and control reasons.

## 6. Races and crash windows

| Window | Kernel sees | Outcome |
|---|---|---|
| Crash after `prepare_tx` commit, before the write | op `tx_committed`/`spawn_started`, binding + lease, `not_requested` | recovery re-drives step 2; writes once |
| Crash after the write, before the phase moves | `requested` | re-drive does not resend; op succeeds; readback/quiet wake show the result |
| Write fails after the bind (terminal gone, dialog, control taken) | op error | compensation releases the lease; attempt `failed` (spawn-failed); session Parked{last = new}; Planner continues the new key or starts fresh |
| Worker reports the OLD attempt after the new prompt | Live{new}; old bound to this session, terminal | same outcome → idempotent no-op; else `Conflict` naming the Live attempt |
| Predecessor's late `neige_task_done` after the bind | same | as above; its lease release is by attempt and finds nothing held |
| Worker starts a turn between admission and the write | Codex `active` | text steers/queues, exactly once (c10, k7) |
| Planner B message while A is delivering | Live `dispatched` | B refused (`worker_starting`); A's write precedes `mark_running` |
| Two continuations of one key | second waits `InUse` | second refused at bind (Parked.last ≠ pred) |
| Timeout/cancel of the new attempt | Live{new} | marker for new; sweep kills session, releases new's lease |
| Reaper finds the session dead mid-attempt | Live{new} | fails new (CAS vs report, 773) |
| Kernel restart with reattached terminals (#2499) | no readable view | B refused `terminal_unreadable`; A refused at bind; both before any write |
| Claude restart while bound | `carry_binding_tx` in the restart transaction | the resolver follows the conversation |

## 7. Planner prompt and docs

`crates/calm-server/prompts/planner.md` Tasks, last bullet (line 30) gains two sentences, plus one
bullet:

> … Point the next review at the new key. For a mechanical round (red gate, fmt, golden, a review
> nit) add `continues: "<old key>"`: the same worker takes it. A design change, a long or
> timed-out run, and every review get a fresh worker.
> * To correct a running codex/claude worker without stopping it, send `neige_terminal_input`
> action `message` by its `attempt_id`.

Budget: `planner.md` is 6,581 bytes raw, ~6,883 rendered (`render_system_prompt`); +329 bytes →
~7,212 of `PLANNER_PROMPT_MAX_BYTES` 7,500 (`planner_card.rs:383`).
`tests/goldens/dev_planner_prompt.txt` regenerates (`TASK_BLOCK_PROTOCOL_GOLDEN`,
`planner_card.rs:62`, pins only the first bullet). `prompts/guides/terminal.md:6` as in §4.

`docs/using-neige-calm.md`: "Read a task's execution history" (line 116, "to try again, declare a
new task under a new key") adds: a codex/claude task may declare `continues` to run in the previous
round's worker; refused with a reason when that worker has ended. The checkout paragraph (line 90)
adds that a continuation takes the checkout like any writer.

## 8. PR split, tests, mutations

Tests drive production entry points (MCP tool calls through the kernel socket, the operation
runtime, real migrations) with fixture terminals/providers; no copied behaviour.

| PR | Scope | Tests | Mutation (single factor → predicted red) |
|---|---|---|---|
| PR-1 S1 (L2: authority, migration) | columns, indexes, migration + backfill, `worker_binding.rs`, readers #1–#23 except PR-3 anchors, lease `attempt_id`, scan gate | `worker_binding_backfill_matches_spawn_op_inference`; `first_spawn_binds_attempt_in_prepare_tx` (codex, claude, terminal; hook after prepare, before spawn); `one_working_attempt_per_session_is_enforced_by_sql`; `resolver_states_cover_live_parked_unbound_nosession` (scalar = SQL fragment); `report_for_another_attempt_names_the_bound_attempt`; `lease_release_is_keyed_by_attempt` (second binding via `bind_attempt_tx`); `claude_restart_carries_the_binding`; existing suites unchanged | M1 resolver counts `verifying` as Live → `resolver_states…`; M2 bind skipped in codex `prepare_tx` → `first_spawn_binds…(codex)` + existing fast-report tests; M3 release by card → `lease_release_is_keyed_by_attempt`; M4 drop the partial index → `one_working_attempt…`; M5 restore one `worker_op_targets_card_tx` call → scan gate |
| PR-2 S2′ + B (L2: authority) | `TuiInput` declaration, `message` action, envelope, preconditions, Parked refusal for every input action, refusal texts, prompt/guide/tool text | `message_is_one_bracketed_paste_write` (exact bytes, one `Verbatim` write); `message_text_refuses_controls`; `message_over_cap_is_refused`; `parked_worker_refuses_input_{done,verifying,failed}` (every action type); `input_after_continuation_binding_is_accepted`; `codex_live_worker_takes_message_refuses_keys`; `claude_live_worker_takes_message_and_keys`; `plain_terminal_refuses_message`; `codex_awaiting_approval_refuses_message`; `message_replay_writes_once`; `terminal_without_readable_view_refuses_message` | M6 input path treats Parked as Live → the three `parked_*`; M7 header omitted → `message_is_one…` + `message_never_leads_with_slash_or_bang`; M8 ESC allowed → `message_text_refuses_controls`; M9 `SplitTrailingCr` → `message_is_one…`; M10 Codex declares `BoundKeys::Accepted` → `codex_live_worker…` |
| PR-3 A + prompt (L2: authority, migration) | `continues` field + column, admission, `worker-continue` op, turn-ended/quiet anchors, prompt/docs | block/plan validation matrix; `continuation_binds_and_leases_before_delivery` (hook between commit and write: binding + lease present, 0 bytes written); `continuation_refused_when_worker_gone` (task failed with reason; card/session counts unchanged); `gate_red_then_continuation_reuses_card_and_session` (fake provider: new attempt's `neige_task_done` accepted, delivery committed, gate runs); `second_continuation_of_one_key_refused`; `continuation_redrive_does_not_resend`; `turn_ended_check_ignores_turns_before_the_attempt`; `quiet_wake_anchors_to_attempt_start`; planner prompt golden + budget | M11 write before the bind commit → `continuation_binds…`; M12 cold-start fallback when the session is gone → `continuation_refused…`; M13 drop the `requested` CAS → `continuation_redrive…`; M14 drop the turn anchor → `turn_ended_check…` |

Each PR runs `scripts/local-ratchet-gates.sh`, `scripts/local-contract-gates.sh`, the whole
`-p calm-server` run (new SQL readers and tools hit the source-scan suites), and quick Rust gates;
PR-3 regenerates OpenAPI/wire if the REST task row gains `continues`.

**4140 acceptance (issue).** After PR-1: every card→attempt read goes through the resolver, the
scan gate is green, `SELECT COUNT(*) FROM tasks WHERE worker_session_id IS NOT NULL` ≥ 282. After
PR-2: one message each to a running codex and claude worker arrives verbatim in the same or next
turn; a stuck worker wakes the Planner within ~60 s (#2507); input to a done worker is refused with
the `continues` hint. After PR-3: after a red gate, a `continues` task runs in the same
card/session/conversation, its `neige_task_done` is accepted, the kernel commits and the gate runs;
continuing an ended worker is refused with its reason.

## 9. Rejected alternatives

- **New card + `thread/fork`:** still one card per attempt; copies context instead of modelling
  one worker over several attempts; ignored developer instructions, a fork/`turn/start` race,
  config re-supply after restart, and a separate Claude `--fork-session` path.
- **Same-key recovery attempt (0097 generation > 1):** the schema forbids the shape, it has no
  writer, and candidate/delivery are keyed by attempt (#2405).
- **Reopening a done task:** it was delivered and gated; dependents may already run.
- **Per-site exceptions** for "one card, one attempt": a patch at each of 23 readers.
- **Task workers in the harness:** per-provider protocol delivery, Planner-only harness semantics
  (TrackGoal, report diff, `mcp_role`, catch-up), two transcript models.
- **Codex `turn/start` / `turn/steer` from the kernel:** precise and receipted, but a second,
  Codex-only path. What the TUI path gives up: delivery needs the viewer attached — the viewer is
  optional (`codex_adapter/mod.rs:1297,1351`) and unreadable after a restart (#2499) — and there is
  no turn id receipt.

## 10. Open questions for the owner

1. **`Unbound` as a fourth `WorkerBinding` state** (§2). Recommendation: accept; only input paths
   accept it, so Planner terminals keep today's behaviour.
2. **Terminal control held by a human blocks A and B.** UI viewers take control
   (`fe/web/src/systems/terminal/xterm-view.tsx:469-473`), and `input` never takes it from a human.
   Recommendation: keep — the continuation fails with the control reason, never a takeover.
3. **8,000-byte cap refuses 2 of 264 rendered worker prompts on 4140** (max 9,213 bytes).
   Recommendation: keep the probed cap; probe a larger paste only if refusals show up.

## 11. KNOWN GAPs

- Claude permission dialog: no sound signal; Enter would answer it. 0 occurrences on 4140.
- Residual composer text merges with the message (c3, k5); no safe clear key; the header makes the
  boundary visible.
- After a server restart, workers started before it take neither messages nor continuations until
  their terminal is reattached and readable (#2499).
- A Codex worker whose optional viewer failed or exited takes neither.
- No in-progress-turn check: a mid-turn paste steers (Codex) or queues (Claude).
- At-most-once on crash re-drive; outcome only by readback; no receipts.
- Untested: Codex `disable_paste_burst` and `tui.keymap.*`, Claude's Rewind selector, Codex Tab
  queue racing a kernel `turn/start`, other terminal sizes and `TERM`.
- Codex viewer starts a title-generation thread per turn (#2510).
