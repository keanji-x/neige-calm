# #2493 — Worker conversation: explicit attempt↔session binding and TUI message delivery

Status: design, L2 review round 2. Issue: #2493 (requirements, 4140 data, TUI probe). Code
references are to `f1ca87776`. Builds on 773 (worker lifecycle), 2053 (worker reports), 2003 and
`docs/conventions/agent-commands.md` (command surface).

- **A, continuation.** After a worker's task ends (done or failed), the Planner gives the *same*
  worker (card, provider conversation, MCP token) the next round instead of a cold start.
- **B, message.** The Planner adds a line to a *running* worker's conversation.
- **S1** makes "which attempt does this worker session serve" an explicit kernel fact with one
  owning module. **S2′** is one generic "deliver a message to an agent TUI" path. B = S2′ aimed at
  the current attempt's worker; A = a continuation binding plus the new prompt sent by S2′.

Pain (issue table, 4140, 14 days): of 72 next-round pairs (same track and kind, `read_write`), 66
found the previous worker alive and were cold-started anyway. Owner decisions taken as fixed: bind
to the worker session; lease per attempt; Parked refuses agent input; the resolver returns a typed
state (issue comment "S1 评审意见", 2026-10-09).

## 1. Model

| Fact | Owner | Meaning |
|---|---|---|
| Attempt | `tasks` row | contract: a new key, a new `attempt_id` per round (unchanged); CAS terminal state (773) |
| Worker session | `worker_sessions` row | conversation: thread / Claude session, MCP token, terminal |
| **Binding** (new) | `tasks.worker_session_id` | history: which session executed this attempt; read whether or not the session still lives |
| **Authority** (derived) | `WorkerBinding` | may this session act for an attempt *now*: binding ∧ session liveness |

Today the binding is inferred three ways: the spawn op whose `idempotency_key` is the attempt and
whose target is the card (`calm-truth/src/db/sqlite/task.rs:270-294`), `tasks.worker_card_id`
stamped late by `COALESCE` (`task.rs:242-266`, `scheduler/mod.rs:1398-1402`), and the card
payload's `idempotency_key` (`track_vcs/runs.rs:85-95`, `scheduler/mod.rs:2061-2068`). Reports need
an `owns_key` op proof for the window before the stamp (`decision_sink.rs:116-132`).

A session outlives its attempt today. The report transaction changes only the task row and the
lease (`decision_sink.rs:100-190`); the gate path never touches sessions. A session ends only on
track close (`terminal_sweeper.rs:68-74`, only with no in-flight attempt), a cancel/timeout marker
sweep (`scheduler/mod.rs:95-136`, `:1650-1729` → `operation/driver.rs:233-264`), the reaper on a
dead process (`reaper/mod.rs:95-372`), or card deletion. So continuation needs no keep-alive.

### Decisions

| Question | Decision | Rejected alternative |
|---|---|---|
| Where | `tasks.worker_session_id TEXT REFERENCES worker_sessions(id) ON DELETE SET NULL` (PR-1); `tasks.worker_bind_seq INTEGER CHECK (worker_bind_seq >= 1)` (PR-3, when the second writer arrives) | A table: the invariant needs `tasks.status`, so a trigger. A pointer `worker_sessions.attempt_id`: loses attempt→session history, which activity, run views and report idempotency read |
| Invariant | PR-1: `UNIQUE INDEX … ON tasks(worker_session_id) WHERE worker_session_id IS NOT NULL` (one attempt per session ever, as today). PR-3 replaces it with `UNIQUE (worker_session_id) WHERE … AND status IN ('dispatched','running')` plus `UNIQUE (worker_session_id, worker_bind_seq)` | Including `verifying`: `task_regate_tx` moves `failed → verifying` (`task_regate.rs:27-36`); regate is refused while the checkout is in use or a later task delivered (`task_verify_adapter/regate.rs:169-186`) |
| Order (PR-3) | `worker_bind_seq` 1 for a first spawn, last + 1 for a continuation | `created_at_ms`: ties when two tasks are declared in one commit |
| Card column | `tasks.worker_card_id` stays, written only by the binding writer in the same `UPDATE`; the `COALESCE` stamps in `task_mark_running_tx` and the three report flips go. Child-track flips keep writing `worker_card_id = NULL` (`scheduler/mod.rs:290,319,347`): harmless, a child-track row never binds | Drop it: ~45 files read attempt→card, which stays correct |
| Writer | `worker_binding::bind_attempt_tx` (calm-truth), in the transaction that creates or reuses the session: `prepare_tx` of `codex-worker` (`codex_adapter/mod.rs:777-897`), `claude-worker` (`claude_adapter/mod.rs:767-905`), `terminal-worker` (`terminal_adapter.rs:621-700`); PR-3's `worker-continue`. A unique-index violation maps to `Conflict` | Binding at `mark_running`: keeps the unstamped window and the op proof |
| Claude restart | `claude-restart` ends the old session and starts a new row resuming the same conversation (`--resume`, `claude_restart_adapter.rs:175-248`). In that transaction `carry_binding_tx(old, new)` moves the binding: the only write that moves one. 0 such ops on 4140 | Leave it: the restarted task worker resolves `Unbound` and takes Planner input with no lease |

`bind_attempt_tx` is one guarded statement: `UPDATE tasks SET worker_session_id=?s,
worker_card_id=?c WHERE id=?a AND status='dispatched' AND worker_session_id IS NULL`; 0 rows or a
unique violation → `Conflict`.

### Migration and 4140 backfill

PR-1 migration (numbered last at merge): the column and index, `workspace_leases.attempt_id TEXT`
plus index `(attempt_id, state)` (§3), the `worker_session_binding` view (§2), then:

```sql
UPDATE tasks SET worker_session_id = (SELECT ws.id FROM operations o JOIN worker_sessions ws ON ws.spawn_op_id = o.id
    WHERE o.idempotency_key = tasks.id
      AND o.kind IN ('codex-worker','claude-worker','terminal-worker','codex-isolated-worker')
      AND json_extract(o.payload_json,'$.actor.kind') = 'KernelDispatcher')
WHERE worker_session_id IS NULL;
UPDATE tasks SET worker_card_id = (SELECT card_id FROM worker_sessions WHERE id = tasks.worker_session_id)
WHERE worker_card_id IS NULL AND worker_session_id IS NOT NULL;
UPDATE workspace_leases SET attempt_id = (SELECT t.id FROM operations o JOIN tasks t ON t.id = o.idempotency_key
  WHERE o.id = workspace_leases.lease_owner);
```

Run as `SELECT`s, read-only, on 4140 (`sqlite3 -readonly`, migration 160, 2026-10-10):

| Check | Result |
|---|---|
| tasks / with `worker_card_id` | 301 / 298 |
| bindings | 282 = 282 attempts = 282 sessions: 278 via today's spawn kinds (codex 201, claude 59, terminal 18) + 4 failed codex attempts via the retired `codex-isolated-worker` kind |
| attempt with >1 session; session with >1 attempt; stamp ≠ session card | 0; 0; 0 |
| bound, `worker_card_id` NULL | 1 (`spawn-failed: operation drive failed …`), filled by the second `UPDATE` |
| stamped, session row gone | 17, all terminal; cards deleted too → no binding |
| current tasks whose card survives but session ended | 280 (done 233, failed 37, canceled 10; failed: 32 `exited`, 5 `failed`) — history readers must still see them |
| leases resolving to an attempt | 273 / 273 |
| non-terminal tasks, timeout markers, `claude-restart` ops | 0, 0, 0 |

No ambiguity. 50 worker spawn ops have no `tasks` row (pre-scheduler workers); their sessions stay
unbound, as today's legacy report path expects (`worker_report.rs:39-46`).

## 2. One owner, two questions

`calm-truth/src/db/sqlite/worker_binding.rs` owns both questions. The rule is one SQL view,
`worker_session_binding(session_id, card_id, session_active, attempt_id, attempt_status)`: per
session, its bound attempt (PR-1: the only one; PR-3: max `worker_bind_seq`) and whether the
session is active (`WorkerSessionState::is_active_authority`, `calm-types/src/worker.rs:253-261`).
History readers select from the view ignoring `session_active`. Authority is a `SELECT … WHERE`
over the same view, typed:

```rust
pub enum WorkerBinding {
    Live { attempt_id: String, session_id: String, status: TaskStatus }, // active; dispatched | running
    Parked { last_attempt_id: String, session_id: String },              // active; verifying | done | failed | canceled
    Unbound { session_id: String },                                      // active; never bound
    NoSession,                                                           // ended, deleted, or unknown
}
pub enum WorkerOf<'a> { Session(&'a str), Card(&'a str), Attempt(&'a str) }
pub async fn worker_binding_tx(conn, of: WorkerOf<'_>) -> Result<WorkerBinding>;
pub async fn may_write_tx(conn, session: &str, writer: Writer<'_>) -> Result<bool>;
pub enum Writer<'a> { Planner, Continuation { attempt_id: &'a str } }
```

`Card(c)` uses the card's one active session (`ws_one_active_per_card`, migration 0156:76).
`Unbound` extends the owner's three states: Planner-opened terminals are worker-role sessions with
no attempt, served by the input path (§10). `may_write_tx` is the single write predicate (§4):
`Planner` → Live with `Running` (today's Claude rule, `target.rs:218-222`, moved here);
`Continuation{a}` → Live{a} (dispatched) and no cleanup marker on the session.

Each reader asks one question. H = history (the binding fact, any session state); A = authority.
"Neutral" = identical result while each session has one attempt, ended sessions included.

| # | Reader | Today | New | Q / accepts | With a second attempt on the session |
|---|---|---|---|---|---|
| 1 | `decision_sink/worker_report.rs:14-73` | spawn-op keys ∪ `worker_card_id` of the card = one key | `Session(identity)`; identity is active by construction (`handshake.rs:66-73`) | A: Live{a = reported} admits; a terminal attempt bound to this session → same outcome idempotent, else `Conflict` (773); else refuse naming the bound attempt | new attempt's reports admitted |
| 2 | `decision_sink.rs:116-132`; `task.rs:329-420` flips' `worker_card_id IS NULL AND owns_key` arm | op proof | `TaskReporter::Session`; flips guard `worker_session_id = ?` | A: Live | correct row only |
| 3 | `task.rs:242-266` `COALESCE` stamp | late stamp | removed | — | — |
| 4 | `task.rs:270-327` op-proof helpers; callers `claude_restart_adapter.rs:152,196`, `scheduler/mod.rs:2145`, `worker_failure.rs:71` | creating spawn op | deleted; restart reads the view for the card's last session (it is exited at restart) | H | head of the latest attempt |
| 5 | `read.rs:296-308` `task_for_worker_card`; callers `dispatcher/mod.rs:281-301`, `target.rs:193` | `LIMIT 2` → Conflict | deleted; stop-hook push iff the card's bound attempt is `dispatched|running` | H | no Conflict |
| 6 | `terminal_launch.rs:93-103`, `:220-231` | card's worker op `LIMIT 2`; `EXISTS tasks.worker_card_id` | the terminal's session `spawn_op_id` (who launched the PTY: a creation fact); task-owned = the view has a row | H | none: continuation never launches |
| 7 | `terminal_interaction/target.rs:109-238` | `spawn_op_id` → op key; Running-only control (`:218-222`); codex refusal (`:47-80`, `:216`) | reads: view (H); writes: `may_write_tx(Planner)` | reads H; writes A: Live Running | input reaches the new attempt; Parked refused |
| 8 | `worker_quiet.rs:120-160` | through #7 | through #7; quiet from `max(last_output, running_started_at_ms)` (PR-3) | A: Live Running | no wake right after a bind |
| 9 | `codex_adapter/mod.rs:1195,1246` `persisted_turn_id` | first-spawn re-drive | unchanged; `worker-continue` never calls it | — | off the path |
| 10 | `scheduler/mod.rs:95-136` marker by card | `UPDATE … WHERE card_id` | the fail tx marks the session bound to the attempt it fails; none active → release that attempt's lease | A: Live (read before the flip) | cannot mark a newer attempt |
| 11 | `scheduler/mod.rs:1650-1729` sweep | kill card, release card's lease | kill; release the lease of `marker.task_id` | marker's attempt | per attempt |
| 12 | `scheduler/mod.rs:1752-1765`, `:1809-1825` | op fallback | the attempt row's binding | H | neutral |
| 13 | `scheduler/mod.rs:2037-2068` `on_terminal_exit` | card payload key | view for the card + status check | H | — (terminal kind never continues) |
| 14 | `scheduler/running_worker.rs:175-211` idle check ("each execution gets a fresh card and thread") | latest codex session of the card | the attempt's session row; comment rewritten; turn must complete after `running_started_at_ms` (PR-3) | H | old turn cannot fail the successor |
| 15 | `reaper/mod.rs:375-470` | `spawn_op_id` → attempt; release by card | `Session(s)` (active while converging): Live → fail it, release its lease | A: Live | fails the new attempt |
| 16 | `workspace_lease/release.rs:54-72`, `:163-184`; callers `decision_sink.rs:185`, `reaper:433,461`, `scheduler:125,1701`, `routes/cards.rs:634`, `plugin_host/callbacks.rs:536` | card's newest active lease; `lease_attempt_tx` via owner op | lease by `attempt_id`; card delete → the card's bound `dispatched|running` attempt | the ending attempt | late old report cannot release the new lease |
| 17 | `workspace_lease/facts.rs:50-81`; callers `plan.rs:589-600`, `git_delivery_settled.rs:53-61`, `git_candidate/view.rs:308-311`, `task_gate_run/admission.rs:72`, `task_verify_adapter/target.rs:219-223` | card's newest lease | the attempt's lease; last commit from `worktree.committed` whose `delivery_id` is the attempt's | the attempt | old views never show the new lease |
| 18 | `task_verify_adapter/mod.rs:373-399` gate cwd | card's newest lease; terminal cwd from `terminal-worker` op `tx_output` by `idempotency_key` | the attempt's lease; terminal cwd from the bound session's terminal row | the attempt | regate of an old attempt uses its own lease |
| 19 | `task_gate_run/admission.rs:48-57` | `worker_card_id == card` and Running | `Session(identity)` Live{a = attempt_id} Running | A | neutral |
| 20 | `mcp_server/transport.rs:1039-1046` + `read.rs:970-985` `workspace_lease_for_card` (Worker forge cwd) | card's newest `held` lease | `Session(identity)` Live → its lease | A: Live | the running attempt's lease |
| 21 | `track_activity.rs:145-159`, `:186-215`; `track_activity/sql.rs:144-170` | every current row raises its `worker_card_id` | the view's attempt per session raises the card | H (ended sessions included) | old `failed` no longer outranks new `working` |
| 22 | `track_vcs/delta.rs:344-394`, `runs.rs:85-95,373-395`, `track_fs_view/mod.rs:518`, `:1303-1312` | card payload key; first run with the card; run Markdown from card payload | run ↔ card from bindings; attempt facts per §5 | H | each run shows its own goal and prompt |
| 23 | `scheduler/worker_failure.rs:51-74` card delete | `worker_card_id` or op proof | the card's bound attempt if `dispatched|running` | H | neutral |
| 24 | `terminal_sweeper.rs:68-74` | no current task with `worker_card_id = card` in `dispatched|running|verifying` | same statuses through the view | H | neutral |
| 25 | `plan/cancel_running.rs:110-114`, `task.rs:131-151` | CAS on `worker_card_id` | CAS on `worker_session_id` | the attempt | neutral |

Unchanged: checkout occupancy (`track_occupancy.rs`), the liveness deadline
(`scheduler/worker_liveness.rs:36-52`, anchored per attempt row), capture and Claude settings per
card, every reader of a given attempt's `worker_card_id` (`plan.rs`, `track_state.rs`,
`task_recovery/view.rs`, `child_track_adapter`). Non-readers checked: `session_projection_active_for_card`
and `terminal_get_by_card` callers in `routes/` (Planner cards, UI) and `shared_codex_appserver.rs`
ask "the card's session/terminal", not "its attempt".

**Sweep method.** `git grep` over `crates/` (tests excluded) for `worker_card_id`,
`FROM workspace_leases`, `spawn_op_id`, the worker op kind literals in SQL,
`find_by_kind_and_idempotency`, card payload `idempotency_key` reads,
`session_projection_active_for_card` and `terminal_get_by_card`; each hit read at its call site.
Rows 6 (`:220-231`), 18 and 20 were added by round 1.

**Neutrality proof for PR-1.** (a) No existing assertion changes; fixtures that hand-insert a
running worker bind through `bind_attempt_tx`. (b) `worker_binding_backfill_matches_spawn_op_inference`:
the real migrator to N−1; seed spawn op / session / task / lease per kind × attempt status
{dispatched, running, verifying, done, failed, canceled} × session state {running, exited, failed}
with surviving cards, an op-less legacy session and a deleted-session row; migrate; assert the view,
`worker_binding_tx` and the activity fold against literals from today's inference. (c) The 4140
SELECTs above.

**Scan.** `scripts/gate-2493-worker-binding-inference.sh` (run by `local-ratchet-gates.sh`): `git
grep` for `spawn_op_id` reads, `worker_op_targets`, `WORKER_SPAWN_OPS`,
`operation_idempotency_key_by_id`, `task_for_worker_card`, `owns_key`, card payload
`idempotency_key` reads, `worker_card_id =` in a `WHERE`, and `workspace_leases` filtered by
`card_id`. Exemptions, each with its reason and no count pin: released migrations; `worker_binding.rs`;
the session writer (`session_mirror.rs`); terminal launch's creation lookup (row 6); the first-round
run view (§5).

## 3. Lease, timeout and cleanup per attempt

- **Lease key.** `workspace_leases.attempt_id` (backfilled 273/273). `acquire_workspace_lease_tx`
  (`workspace_lease/mod.rs:145-166`) takes the attempt; `lease_owner` stays the op; `card_id`
  stays (path uniqueness, UI). Every release and fact read is by attempt (#16-#18, #20).
- **Worker timeout.** `running_started_at_ms` / `running_deadline_ms` are stamped by `mark_running`
  per attempt (`task.rs:242-266`), so cap and idle window start at the continuation's `running`
  stamp. Progress reads the card's capture cursor (`task_liveness.rs:47-72`); the anchor
  `max(started, progress)` hides the predecessor's progress.
- **Codex turn-ended check** (`running_worker.rs:104-127`) counts a turn only if it completed after
  `running_started_at_ms`; otherwise the predecessor's old turn fails a fresh continuation before
  its prompt lands. The quiet detector (#2507) likewise.
- **Cleanup marker.** Written only for the Live attempt being failed (#10), carries its id; the
  sweep releases that attempt's lease (#11). A marker revokes `may_write_tx(Continuation)` and
  continuation admission refuses a marked session.

## 4. Message delivery (S2′)

**Declaration.** Typed and required on `calm_exec::WorkerProvider` (`calm-exec/src/provider.rs:40`),
implemented in `provider::worker`, registered in `calm-server/src/provider_registry.rs:28-58`:

```rust
pub struct TuiInput { pub message: MessageDelivery, pub keys_while_bound: BoundKeys }
pub enum MessageDelivery { BracketedPasteSubmit, Unsupported }
pub enum BoundKeys { Accepted, Refused }
```

Codex `{BracketedPasteSubmit, Refused}`, Claude `{BracketedPasteSubmit, Accepted}`, terminal and
managed `{Unsupported, Accepted}`. `keys_while_bound` replaces `card.kind == "codex"` in the generic
layer (`target.rs:216`). Text transforms are documented only: Codex trims surrounding whitespace,
Claude turns a tab into four spaces (probe c5/c7, k3).

**Envelope** (`terminal_interaction/actions.rs`, one encoder for A and B): one `WriteShape::Verbatim`
write of `ESC[200~` + header + `\n` + text + `ESC[201~` + `\r`. Fixed kernel headers, so `/` and `!`
never lead (probe c9, c10, k5); each names only the attempt the text is for:

- B: `[neige] Planner message for attempt <attempt_id>:`
- A: `[neige] Next round: attempt <attempt_id> of task <key>.`

Text: non-empty; every `char::is_control` but `\n` and `\t` refused (C0 incl. ESC and CR, DEL, C1),
so no escape sequence and no early `ESC[201~`. Cap **8,000 bytes** for header + text (the probe's
7,999-byte paste arrived byte-exact on both TUIs). `submit` splits its CR into a second write
(`actions.rs:129-137`, `control_writer.rs:20-38`); a message never does.

**Preconditions,** in order; a refusal writes nothing. Live renderer and control facts are not DB
facts: they are pre-checked before a bind and re-checked at the write.

1. Write authority: `may_write_tx` (§2). It is the `control` predicate of the client's
   `ClientInputScope::Bound` (`terminal_interaction/mod.rs:151-176`), which the pump evaluates at
   the claim fence (`client_pump.rs:316-336`) and `WriteAuthority::admit` re-evaluates at every
   physical write (`input_authority.rs:84-93`). Parked/NoSession refused; Unbound falls through to
   today's plain-terminal rules.
2. Declaration: `message` ≠ `Unsupported`.
3. Terminal live and readable: renderer entry present, `observable()`, not exited (as
   `neige_terminal_show`'s `available`, `target.rs:268-279`; `terminal_interaction/mod.rs:122-127`),
   and DECSET 2004 bracketed paste on (`MODE_BRACKETPASTE` in `InputSurface.modes`,
   `calm-terminal-view/src/lib.rs:59-67`).
4. No input-capturing dialog: `worker_sessions.last_thread_status` ∉ {`waitingOnApproval`,
   `waitingOnUserInput`}, read for every provider as the reaper reads it (`reaper/mod.rs:171-176`;
   fed for Codex by `liveness_feeder.rs:26-41`, NULL for Claude). Claude has **no sound signal**:
   hook payloads are "forgeable … never authority" (`terminal_hooks.rs:3`), no hook moves card
   state (`routes/claude_cards.rs:227`), Esc closes a dialog with no hook; the 4140 Claude worker
   hook stream holds 0 `permission_request` and 0 `permission_prompt` events. KNOWN GAP.
5. Control: claim if unowned, never from a human (`operations.rs:126-138`, `:294-305`); see §10.
6. Never Esc, Ctrl+C or Tab (the encoder has no such bytes).

Parked refusal covers every *agent* write path (`neige_terminal_input`, `worker-continue`). A human
typing in the browser connects as `ClientInputScope::InteractiveUser` (`ws/terminal.rs:194-200`,
`input_authority.rs:29-35`, `InteractiveUser` → `true`) and keeps that authority: the hazard is an agent writing to a finished
worker without a lease; a human at the keyboard is explicit authority (§10, confirm).

**Surface: a new action of `neige_terminal_input`.** `input` is the vocabulary verb for "send text
or keys to a terminal" (agent-commands §3); `send` is mail only. The action reuses target
resolution, the anchored read (`observation_id`), `idempotency_key` replay, `claim`, `release`,
`allow_output_since_observation` and `read` unchanged, so every option means the same on every
action. Schema (`mcp_server/tools/terminal.rs:49-58`): `"type":{"enum":["text","submit"]}` →
`["text","submit","message"]`; the cap is the kernel's. Description
(`prompts/tools/neige_terminal_input.md`, 1,331 B of `DESCRIPTION_MAX_BYTES` 2,048,
`mcp_server/tools/mod.rs:193`) gains: "message (agent workers: text pasted under a kernel header,
then Enter; a running worker gets it this turn or next; ≤8,000 bytes; no control characters but
newline and tab)." PR-2 measures the Planner surface against `SURFACE_MAX_BYTES` 30,884
(`mcp_server/tools/mod.rs:192`) and trims wording, never the cap. CLI: none (terminal tools are
MCP-only, agent-commands §2).

Refusals (`-32403` like every terminal runtime failure, agent-commands §9; text and
`data.refusal` agree; prefix `neige_terminal_input: `):

| `data.refusal` | Text |
|---|---|
| `worker_parked` | `attempt <id> (task <key>) is <status>; its worker takes no input until a task continues it. Declare a task with "continues": "<key>", or a new task for a fresh worker.` |
| `worker_ended` | `the worker of attempt <id> has ended (<state>); nothing was sent. Declare a new task.` |
| `worker_starting` | `attempt <id> is dispatched; its worker is starting. Read again, then send.` |
| `message_unsupported` | `action "message" needs an agent worker (<providers declaring it>); terminal <id> runs <provider>. Use "submit".` |
| `worker_keys_refused` (was `codex_task_worker_input`) | `a <provider> task worker takes only action "message"; typed keys interrupt its turn without starting one.` |
| `worker_awaiting_input` | `the worker's thread is <status>; Enter would answer it. Read its screen; wait or cancel the task.` |
| `terminal_unreadable` | `the worker's terminal has no live readable view (after a server restart until reattached, #2499), or bracketed paste is off; nothing was sent.` |
| `-32602` text | `message text: U+001B at byte 12; only printable characters, newline and tab` / `message is 9,213 bytes with its header; the limit is 8,000` |

#1787 becomes "codex Live worker: `message` only"; `prompts/guides/terminal.md:6` ("await codex
task settlement before planning its successor") becomes "send `message` by `attempt_id`".
**Feature B** = this action with `attempt_id`; confirmation is the readback, a stuck worker is
caught by the #2507 quiet wake. No receipts.

## 5. Continuation (feature A)

**Declaration.** Task-block field `continues: "<key>"` ("run in the worker of `<key>`'s current
attempt"), added like `start` (migration 0138): `TASK_FIELDS` (`report_blocks/kinds.rs:160-183`), a
validator beside `validate_task_start` (`task_execution.rs:138-178`), the block schema
(`track_report_blocks/contracts.rs:377-382`), projection (`task_projection.rs:874-878`,
`:1643-1686`), column `tasks.continues TEXT NULL`, `neige_task_ls` (`plan/list.rs:102`), CLI
render. The allowed kinds are the closed task-kind set {codex, claude} in calm-types; a calm-server
test pins it equal to the task kinds whose provider declares `message ≠ Unsupported`. Rejected:
`depends_on` (requires `done`; a gate-red predecessor is `failed`), `resume` (Claude's meaning).

| Stage | Rule | Where |
|---|---|---|
| Block | kind in the set above; `read_write`; default `spawn` (no child track); `start: checkout`; no `head`/`base`; not its own key | validator |
| Plan | named key exists, same kind, `read_write` (reviews neither continue nor are continued); `continues` is a graph edge for `find_cycle` | `report_blocks/tasks.rs:550-571,725-732`; diagnostics `unknown_continuation`, `continuation_mismatch` |
| Ready | the predecessor's current attempt is terminal; else waits like a dependency | `checkout_admission` (`track_occupancy.rs:115-150`) |
| Bind (`worker-continue` `prepare_tx`) | `Attempt(pred)` is Parked{last = pred}, pred `done` or `failed`; no cleanup marker; preconditions 2–5 pre-checked; prompt + header ≤ 8,000 bytes | one transaction |

A second task continuing the same key waits on the checkout (`InUse`), then finds
Parked{last ≠ pred} and is refused. **No in-progress turn** is not a rule: the kernel's signals are
`last_thread_status ∈ {active, waitingOnUserInput, waitingOnApproval}` (Codex) and unsound Claude
hooks, and the probe delivered a paste near or during a turn exactly once (c10, k7): Codex steers,
Claude queues. Only the dialog statuses refuse.

**Operation `worker-continue`** (one adapter, both providers; payload `{actor, track_id,
idempotency_key: attempt_id, continues}`, a pure function of the frozen row as
`build_worker_payload` requires, `scheduler/mod.rs:157-201`; listed in `TASK_BOUND_ADAPTER_KINDS`,
`operation/mod.rs:77-84`):

1. `prepare_tx`, one transaction: `refuse_if_context_stale` (`operation/mod.rs:100-118`);
   admission; `bind_attempt_tx(seq = last + 1)`; `prepare_worker_lease_tx` +
   `acquire_workspace_lease_tx(attempt_id)`; render the prompt with `render_task_worker_prompt_tx`
   for the new `attempt_id` and the provider's surface; record it and `message_delivery:
   not_requested` in the op output. Creates no card, session, thread, token or process.
2. `spawn_side_effect`: `admit_task_side_effect` (`operation/mod.rs:121-132`); CAS
   `not_requested → requested` on the op row (pattern of `terminal_launch.rs:113-121`); then
   `TerminalInteraction::deliver_for_operation(op_id, attempt_id, text)`, the kernel terminal-client
   entry point: it attaches a `Client` (`client.rs:124`) keyed by the op id (Planner clients are
   keyed by identity, `target.rs:40-45`) with scope `Bound{control: may_write_tx(Continuation{a})}`,
   claims if unowned, writes once, releases, detaches. A re-drive that finds `requested` does not
   write: at-most-once.
3. Scheduler: `reconcile_spawn_result` → `mark_running`. Compensation releases only the attempt's
   lease; it never deletes the reused card or session (unlike `cleanup_codex_worker`,
   `codex_adapter/mod.rs:1033-1040`). A cancel or cleanup marker revokes the write predicate.

It never calls `spawn_codex_worker_via_shared_daemon`, so the `persisted_turn_id` skip is off the
path. Any refusal fails the op; `fail_spawn` (`scheduler/mod.rs:1459-1484`) records
`spawn-failed: continuation refused: <reason>` and `task.failed` wakes the Planner — never a cold
start. Reasons: `the worker of <key> (attempt <id>) has ended (<state>); declare the task without
"continues"`; `… was canceled; its worker was stopped`; `… already continued by <key2>; continue
<key2>`; `… is being stopped (cleanup pending)`; `the next-round prompt is <n> bytes; the paste
limit is 8,000: shorten goal or context, or drop "continues"`; §4's terminal, dialog, control.

**Attempt-owned facts.** The card payload's `goal`, `context`, `acceptance_criteria`, `prompt`,
`idempotency_key` and the card title are first-round facts (`codex_adapter/mod.rs:847-856`,
`claude_adapter/mod.rs:852-861`); a continuation never mutates them. Run views
(`track_fs_view/mod.rs:1303-1312`, `track_vcs/runs.rs`) read goal/context/acceptance from the
attempt's `tasks` row and the prompt from its op output (the first spawn op or `worker-continue`),
the card payload only for the session's first attempt.

**Worker contract.** `prompts/worker/head-mcp.md` and `head-cli.md` ("You were spawned to execute
one job", line 3) gain: "A later round may arrive in this conversation as a `[neige] Next round`
message: it is a new job with its own `attempt_id`; report it once with that id." Worker prompt
goldens regenerate.

## 6. Races and crash windows

| Window | Kernel sees | Outcome |
|---|---|---|
| Crash after `prepare_tx`, before the CAS | binding, lease, `not_requested` | re-drive writes once |
| Crash after the CAS, before the write | `requested`, nothing written | zero delivery (at-most-once): attempt Running with no prompt; the #2507 quiet wake or the idle window catches it; the Planner can `message` it. No receipts |
| Crash after the write, before the phase moves | `requested` | no resend; readback/quiet wake show the result |
| Write fails after the bind (terminal gone, dialog, control taken, predicate revoked) | op error | lease released; attempt `failed` (spawn-failed); Parked{last = new}; Planner continues the new key or starts fresh |
| Worker reports the OLD attempt after the new prompt | Live{new}; old bound here, terminal | same outcome → idempotent; else `Conflict` naming the Live attempt |
| Predecessor's late `neige_task_done` after the bind | same | as above; its release is by attempt and finds nothing held |
| Worker starts a turn between admission and the write | Codex `active` | text steers/queues, exactly once (c10, k7) |
| Planner B message while A delivers | Live `dispatched` | B refused (`worker_starting`); A writes before `mark_running` |
| Two continuations of one key | second waits `InUse` | second refused at bind |
| Timeout/cancel of the new attempt | Live{new} | marker for new; predicate revoked; sweep kills, releases new's lease |
| Reaper finds the session dead mid-attempt | Live{new} | fails new (CAS vs report, 773) |
| Kernel restart (#2499) | no readable view | B refused `terminal_unreadable`; A refused at the pre-check, before any bind |
| Claude restart while bound | `carry_binding_tx` | the binding follows the conversation |

## 7. Planner prompt and docs

`crates/calm-server/prompts/planner.md` Tasks, last bullet (line 30) gains two sentences, plus one
bullet:

> … Point the next review at the new key. For a mechanical round (red gate, fmt, golden, a review
> nit) add `continues: "<old key>"`: the same worker takes it. A design change, a long or
> timed-out run, and every review get a fresh worker.
> * To correct a running codex/claude worker without stopping it, send `neige_terminal_input`
> action `message` by its `attempt_id`.

Budget: 6,581 B raw, ~6,883 rendered; +329 → ~7,212 of `PLANNER_PROMPT_MAX_BYTES` 7,500
(`planner_card.rs:383`). `tests/goldens/dev_planner_prompt.txt` regenerates
(`TASK_BLOCK_PROTOCOL_GOLDEN`, `planner_card.rs:62`, pins only the first bullet).
`docs/using-neige-calm.md`: "Read a task's execution history" (line 116) adds `continues` and its
refusal; the checkout paragraph (line 90) adds that a continuation takes the checkout like any
writer.

## 8. PR split, tests, mutations

Tests drive production entry points (MCP calls through the kernel socket, the operation runtime and
the real client pump/writer, real migrations, the browser for viewer behaviour); no copied
behaviour. Each mutation is single-factor; the listed tests are its complete predicted red set.

| PR | Scope | Tests | Mutations → red set |
|---|---|---|---|
| PR-1 S1 (L2) | column, index, view, migration + backfill, `worker_binding.rs`, rows #1–#25 (PR-3 anchors excepted), lease `attempt_id`, `carry_binding_tx`, scan | `worker_binding_backfill_matches_spawn_op_inference`; `first_spawn_binds_attempt_in_prepare_tx` (codex, claude, terminal); `bind_refuses_non_dispatched`; `one_attempt_per_session_is_enforced_by_sql`; `authority_follows_session_liveness`; `ended_worker_keeps_its_activity_and_run`; `report_for_another_attempt_names_the_bound_attempt`; `lease_release_is_keyed_by_attempt`; `regate_of_an_older_attempt_uses_its_own_lease`; `claude_restart_carries_the_binding` (real `claude-restart` op) | M1 view ignores `session_active` for Live → `authority_follows_session_liveness`; M2 drop `status='dispatched'` from the bind guard → `bind_refuses_non_dispatched`; M3 release by card → `lease_release_is_keyed_by_attempt`; M4 drop the unique index → `one_attempt_per_session_is_enforced_by_sql`; M5 activity reads authority instead of history → `ended_worker_keeps_its_activity_and_run`, backfill test; M6 restore one `worker_op_targets_card_tx` call → scan gate |
| PR-2 S2′ + B (L2) | `TuiInput`, `message` action, envelope, `may_write_tx(Planner)` on every agent write path, refusals, viewer-as-observer (§10), prompt/guide/tool text | `message_is_one_bracketed_paste_write`; `message_never_leads_with_slash_or_bang`; `message_text_refuses_controls`; `message_over_cap_is_refused`; `parked_worker_refuses_input_{done,verifying,failed}` (every action); `input_after_continuation_binding_is_accepted`; `codex_live_worker_takes_message_refuses_keys`; `claude_live_worker_takes_message_and_keys`; `plain_terminal_refuses_message`; `message_refused_when_bracketed_paste_off`; `waiting_on_{approval,user_input}_refuses_message`; `human_held_control_refuses_message`; `message_replay_writes_once`; `terminal_without_readable_view_refuses_message`; browser: `message_delivered_while_a_browser_views_the_worker`, `human_keystroke_takes_control` | M7 input treats Parked as Live → `parked_worker_refuses_input_{done,verifying,failed}`; M8 header omitted → `message_is_one_bracketed_paste_write`, `message_never_leads_with_slash_or_bang`; M9 ESC allowed → `message_text_refuses_controls`; M10 `SplitTrailingCr` → `message_is_one_bracketed_paste_write`; M11 Codex declares `BoundKeys::Accepted` → `codex_live_worker_takes_message_refuses_keys`; M12 viewer attaches as Owner → `message_delivered_while_a_browser_views_the_worker` |
| PR-3 A + prompt (L2) | `continues`, seq + index swap migration, admission, `worker-continue`, `may_write_tx(Continuation)`, anchors, attempt-owned run facts, worker/Planner prompts, docs | block/plan validation matrix; `continuation_binds_and_leases_before_delivery`; `continuation_refused_when_worker_gone` (card/session counts unchanged); `gate_red_then_continuation_reuses_card_and_session`; `done_then_continuation_reuses_card_and_session`; `second_continuation_of_one_key_refused`; `continuation_redrive_does_not_resend`; `crash_after_requested_delivers_nothing`; `continuation_write_revoked_by_cleanup_marker` (real pump path); `continuation_timeout_is_reaped_and_releases_its_lease`; `turn_ended_check_ignores_turns_before_the_attempt`; `quiet_wake_anchors_to_attempt_start`; `run_views_show_each_attempts_goal_and_prompt`; prompt goldens + budgets | M13 write before the bind commits → `continuation_binds_and_leases_before_delivery`; M14 cold start when the session is gone → `continuation_refused_when_worker_gone`; M15 drop the `requested` CAS → `continuation_redrive_does_not_resend`; M16 predicate ignores the marker → `continuation_write_revoked_by_cleanup_marker`; M17 drop the turn anchor → `turn_ended_check_ignores_turns_before_the_attempt`; M18 run view reads the card payload for every attempt → `run_views_show_each_attempts_goal_and_prompt` |

Each PR runs `scripts/local-ratchet-gates.sh`, `scripts/local-contract-gates.sh`, the whole
`-p calm-server` run (new SQL readers and tools hit the source-scan suites), quick Rust gates, and
for PR-2 the `fe` gates and browser tests; PR-3 regenerates OpenAPI/wire if the REST task row
gains `continues`.

**4140 acceptance (issue).** PR-1: every card→attempt read goes through the owning module, the scan
is green, `SELECT COUNT(*) FROM tasks WHERE worker_session_id IS NOT NULL` ≥ 282. PR-2: one message
each to a running codex and claude worker, with the grid open, arrives verbatim in the same or next
turn; a stuck worker wakes the Planner within ~60 s (#2507); input to a done worker is refused with
the `continues` hint. PR-3: after a red gate, a `continues` task runs in the same
card/session/conversation, its `neige_task_done` is accepted, the kernel commits and the gate runs;
continuing an ended worker is refused with its reason.

## 9. Rejected alternatives

- **New card + `thread/fork`:** still one card per attempt; copies context instead of modelling one
  worker over several attempts; ignored developer instructions, a fork/`turn/start` race, config
  re-supply after restart, and a separate Claude `--fork-session` path.
- **Same-key recovery attempt (0097 generation > 1):** the schema forbids the shape, it has no
  writer, and candidate/delivery are keyed by attempt (#2405).
- **Reopening a done task:** it was delivered and gated; dependents may already run.
- **Per-site exceptions** for "one card, one attempt": a patch at each of 25 readers.
- **Task workers in the harness:** per-provider protocol delivery, Planner-only harness semantics
  (TrackGoal, report diff, `mcp_role`, catch-up), two transcript models.
- **Codex `turn/start` / `turn/steer` from the kernel:** precise and receipted, but a second,
  Codex-only path. The TUI path gives up delivery without the viewer — optional
  (`codex_adapter/mod.rs:1297,1351`) and unreadable after a restart (#2499) — and a turn-id receipt.

## 10. Owner decisions and questions

1. **Viewing does not hold control (decision, implemented in PR-2 unless vetoed).** Today every
   browser viewer attaches with `role_hint: 'Owner'` and `wantedOwnerRef = true`
   (`fe/web/src/systems/terminal/xterm-view.tsx:143,431`) and re-claims whenever control becomes
   null (`:469-473`, `:594-597`); worker cards render through `TerminalCardView`
   (`systems/cards/builtins/codex.ts:32`, `claude.ts:29`), and the track grid keeps every card
   mounted after its first open (`features/track/grid/public.tsx:22-25,51-59`). So once the grid
   was opened, every worker terminal is browser-held and the kernel's claim-if-unowned refuses
   (`client_pump.rs:353-362`): A, B and today's Planner input all fail. Rule: control means
   "intends to type", not "is looking". A viewer attaches as Observer, sends `OwnerClaim` (a
   deliberate takeover: the human always wins) on its first keystroke, re-claims on owner-null only
   if it had claimed, and releases on blur or unmount. One rule for every terminal viewer; Planner
   terminals keep working (the Planner claims with `claim: true`; a human keystroke takes over, as
   the guide expects, `prompts/guides/terminal.md:12`). Consequence: `ResizeCommit` is owner-only
   (`calm-session/src/terminal_session.rs:239-240`), so an observing viewer shows the PTY at its
   last owner's size until it claims. The kernel rule "never take control from a human" stays.
   Veto: keep today's viewers and A/B only work with the grid never opened.
2. **Confirm:** a human typing into a Parked worker in the browser stays allowed (§4).
3. **Confirm:** `Unbound` as a fourth `WorkerBinding` state (§2); only input paths accept it.
4. **Confirm:** the 8,000-byte cap refuses 2 of 264 rendered worker prompts on 4140 (max 9,213 B);
   probe a larger paste only if refusals show up.

## 11. KNOWN GAPs

- Claude permission dialog: no sound signal; Enter would answer it. 0 occurrences on 4140.
- Residual composer text merges with the message (c3, k5); no safe clear key; the header shows the
  boundary.
- After a server restart, workers started before it take neither messages nor continuations until
  their terminal is reattached and readable (#2499). A Codex worker whose optional viewer failed or
  exited takes neither.
- No in-progress-turn check: a mid-turn paste steers (Codex) or queues (Claude).
- At-most-once: a crash after `requested` delivers nothing; outcome only by readback.
- The reused card keeps its first-round title.
- Untested: Codex `disable_paste_burst` and `tui.keymap.*`, Claude's Rewind selector, Codex Tab
  queue racing a kernel `turn/start`, other terminal sizes and `TERM`.
- Codex viewer starts a title-generation thread per turn (#2510).
