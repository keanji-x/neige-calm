# #2493 — Worker conversation: explicit attempt↔session binding and TUI message delivery

Status: design, final after L2 round 9 (round 3 narrowed delivery and continuation). Issue: #2493 (requirements, 4140 data, TUI probe). Code
references are to `f1ca87776`. Builds on 773 (worker lifecycle), 2053 (worker reports), 2003 and
`docs/conventions/agent-commands.md` (command surface).

**Scope (shipped, #2493):** only §4 (B, `message`). S1 (§1–§3) and A (§5) are deferred to #2520.
Without continuation one session serves one attempt, so today's target resolution
(`terminal_interaction/target.rs` `resolve_target`/`check_binding`: binding equality, session
active, task `running`) is the write rule wherever §4 says `may_write_tx`. Refusal codes are §4's
minus `binding_changed` (today's text stays); `worker_parked` names no `continues`; the keys refusal
is `worker_keys_refused`; a task-less terminal is `message_unsupported`; only the Planner messages
(`assistant_no_message`); `text`/`submit`/`key`/claim on a task worker share `message`'s typed write refusal.
Receipts: `written`, `unknown` (fences the connection's next write until it settles) or `refused`.

**A, continuation:** after a task ends (done or failed) the *same* worker (card, conversation, MCP
token) takes the next round. **B, message:** the Planner adds a line to a running worker. **S1**
makes "which attempt does this session serve" an explicit kernel fact with one owning module;
**S2′** is one generic TUI message path. B = S2′ to the current attempt's worker; A = a
continuation binding plus the new prompt via S2′. Pain (4140, 14 days): 66 of 72 next rounds found
the previous worker alive and cold-started anyway. Fixed owner decisions (comment "S1 评审意见"):
bind to the session; lease per attempt; Parked refuses agent input; typed resolver state.

## 1. Model

| Fact | Owner | Meaning |
|---|---|---|
| Attempt | `tasks` row | contract: a new key, a new `attempt_id` per round (unchanged); CAS terminal state (773) |
| Worker session | `worker_sessions` row | conversation: thread / Claude session, MCP token, terminal |
| **Binding** (new) | `tasks.worker_session_id` | history: which session executed this attempt; read whether or not the session still lives |
| **Authority** (derived) | `WorkerBinding` | may this session act for an attempt *now*: binding ∧ session liveness |

Today the binding is inferred: the spawn op keyed by the attempt (`task.rs:270-294`), a late
`COALESCE` stamp of `worker_card_id` (`task.rs:242-266`), the card payload's `idempotency_key`
(`runs.rs:85-95`), and an `owns_key` op proof for reports (`decision_sink.rs:116-132`).

A session outlives its attempt: reports touch only the task row and lease (`decision_sink.rs:100-190`),
gates never touch sessions. It ends on track close (`terminal_sweeper.rs:68-74`), a cancel/timeout
sweep (`scheduler/mod.rs:95-136,1650-1729`), the reaper (`reaper/mod.rs:95-372`), a Claude CLI exit
(`attach_reader.rs:141-150`) or card deletion. So continuation needs no keep-alive.

### Decisions

| Question | Decision | Rejected alternative |
|---|---|---|
| Where | `tasks.worker_session_id TEXT REFERENCES worker_sessions(id) ON DELETE SET NULL` (PR-1); `tasks.worker_bind_seq INTEGER CHECK (worker_bind_seq >= 1)` (PR-3, when the second writer arrives) | A table: the invariant needs `tasks.status`, so a trigger. A pointer `worker_sessions.attempt_id`: loses attempt→session history, which activity, run views and report idempotency read |
| Invariant | PR-1: `UNIQUE INDEX … ON tasks(worker_session_id) WHERE worker_session_id IS NOT NULL` (one attempt per session ever, as today). PR-3 replaces it with `UNIQUE (worker_session_id) WHERE … AND status IN ('dispatched','running')` plus `UNIQUE (worker_session_id, worker_bind_seq)`; `worker_bind_seq` is 1 for a first spawn, last + 1 for a continuation (not `created_at_ms`: ties within one commit) | Including `verifying`: `task_regate_tx` moves `failed → verifying` (`task_regate.rs:27-36`); regate is refused while the checkout is in use or a later task delivered (`task_verify_adapter/regate.rs:169-186`) |
| Card column | `tasks.worker_card_id` stays, written only by the binding writer in the same `UPDATE`; the `COALESCE` stamps in `task_mark_running_tx` and the three report flips go. Child-track flips keep writing `worker_card_id = NULL` (`scheduler/mod.rs:290,319,347`): harmless, a child-track row never binds | Drop it: ~45 files read attempt→card, which stays correct |
| Writer | `worker_binding::bind_attempt_tx` (calm-truth), in the transaction that creates or reuses the session: `prepare_tx` of `codex-worker` (`codex_adapter/mod.rs:777-897`), `claude-worker` (`claude_adapter/mod.rs:767-905`), `terminal-worker` (`terminal_adapter.rs:621-700`); PR-3's `worker-continue`. A unique-index violation maps to `Conflict` | Binding at `mark_running`: keeps the unstamped window and the op proof |
| Claude restart | `claude-restart` starts a new session row for the card (`claude_restart_adapter.rs:175-248`). It now refuses a card whose session has any binding: `claude-restart: card <id> runs task attempts (last <attempt_id>); declare a new task for a fresh worker`. The one intended behaviour change of PR-1; 0 such ops on 4140; KNOWN GAP | Move the binding to the new session: a second binding writer and history obligations for an unobserved path (round 2) |

Any future `worker_sessions` rebuild must preserve `tasks.worker_session_id` (the 0156 pattern).
`bind_attempt_tx` is one guarded statement: `UPDATE tasks SET worker_session_id=?s,
worker_card_id=?c WHERE id=?a AND status='dispatched' AND worker_session_id IS NULL` (PR-3 adds
`worker_bound_at_ms=?now`); 0 rows or a unique violation → `Conflict`.

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
pub enum WorkerOf<'a> { Session(&'a str), Attempt(&'a str) }
pub async fn worker_binding_tx(conn, of: WorkerOf<'_>) -> Result<WorkerBinding>;
pub async fn may_write_tx(conn, session: &str, attempt: &str) -> Result<WriteVerdict>;
pub enum WriteVerdict { Allowed, Parked { .. }, Ended { .. }, Starting { .. }, BindingChanged { .. } }
```

Card-keyed readers (#5, #13, #16, #23) are history reads of the view; no authority caller is
card-keyed, so there is no `Card` variant. `Unbound` extends the owner's three states: Planner-opened terminals are worker-role sessions with
no attempt, served by the input path (§10). `may_write_tx(session, attempt)` is the one kernel
write predicate (§4): Allowed iff Live{attempt} with `Running` (today's Running-only rule,
`target.rs:218-222`, moved here). Live `dispatched` → `Starting`; Parked → `Parked`; NoSession →
`Ended`; Live{other} → `BindingChanged`. The verdict carries what the refusal text needs. Planner
`text`/`submit`/`key`/`sequence` keep today's rule (`check_binding(write = true)`,
`target.rs:243-266`: the binding observed at read equals the current one), whose Live branch is
this predicate and whose Unbound branch admits plain terminals. A target `Attempt(a)` resolves
only while `a` is its session's latest binding, else `binding_changed` naming the current one;
target resolution also keeps today's key-current check (`target.rs:84-98`). PR-1 computes today's
single rule and text ("task or worker session is not running; terminal control refused",
`tests/cases/task_terminal.rs:494`) from `worker_binding_tx`; the typed `WriteVerdict` and the
refusal table land in PR-2.

Each reader asks one question. H = history (the binding fact, any session state); A = authority.
"Neutral" = identical result while each session has one attempt, ended sessions included.

| # | Reader | Today | New | Q / accepts | With a second attempt on the session |
|---|---|---|---|---|---|
| 1 | `decision_sink/worker_report.rs:14-73` | spawn-op keys ∪ `worker_card_id` of the card = one key | `Session(identity)`; identity is active at handshake (`handshake.rs:66-73`); a report racing a session end is refused | A: Live{a = reported} admits; a non-Live attempt bound to this session (verifying, done, failed) → same outcome idempotent, else `Conflict` (773); else refuse naming the bound attempt | new attempt's reports admitted |
| 2 | `decision_sink.rs:116-132`; `task.rs:329-420` flips' `worker_card_id IS NULL AND owns_key` arm | op proof | `TaskReporter::Session`; flips guard `worker_session_id = ?` (binding equality); the terminal-exit reporter (`complete_terminal_task`, `scheduler/mod.rs:2120-2230`) passes the card's session from the view, already exited (`attach_reader.rs:141-170`) | H (only row 1 requires Live) | correct row only |
| 3 | `task.rs:242-266` `COALESCE` stamp | late stamp | removed | — | — |
| 4 | `task.rs:270-327` op-proof helpers; callers `claude_restart_adapter.rs:152,196`, `scheduler/mod.rs:2145`, `worker_failure.rs:71` | creating spawn op | deleted; restart refuses a bound card (§1), so its head and MCP-config lookups go | H | restart refused |
| 5 | `read.rs:296-308` `task_for_worker_card`; callers `dispatcher/mod.rs:281-301`, `target.rs:193` | `LIMIT 2` → Conflict | deleted; suppress only when the card has a binding whose latest attempt is outside `dispatched|running`; no binding → wake, as today (`dispatcher/tests.rs:941-955`; Planner-opened terminals rely on it; 4140: 25 unbound terminal cards, 5 active) | H | no Conflict |
| 6 | `terminal_launch.rs:93-103`, `:220-231` | card's worker op `LIMIT 2`; `EXISTS tasks.worker_card_id` | the creating-op lookup by card stays (a creation fact; `worker-continue` is not in its kind list); only the `EXISTS` arm becomes the view | H | none: continuation never launches |
| 7 | `terminal_interaction/target.rs:109-238` | `spawn_op_id` → op key; Running-only control (`:218-222`); codex refusal (`:47-80`, `:216`) | reads: view (H); writes: `may_write_tx`; `keys_while_bound` is a per-action pre-check, not part of the predicate | reads H; writes A: Live Running (Unbound for `text`/`submit`) | input reaches the new attempt; Parked refused |
| 8 | `worker_quiet.rs:120-160` | through #7 | through #7; quiet from `max(last_output, running_started_at_ms)` (PR-3; both kernel-clock ms) | A: Live Running | no wake right after a bind |
| 9 | `codex_adapter/mod.rs:1195,1246` `persisted_turn_id` | first-spawn re-drive | unchanged; `worker-continue` never calls it | — | off the path |
| 10 | `scheduler/mod.rs:95-136` marker by card | `UPDATE … WHERE card_id` | the fail tx marks the session bound to the attempt it fails; none active → release that attempt's lease | A: Live (read before the flip) | cannot mark a newer attempt |
| 11 | `scheduler/mod.rs:1650-1729` sweep | kill card, release card's lease | kill; release the lease of `marker.task_id` | marker's attempt | per attempt |
| 12 | `scheduler/mod.rs:1752-1765`, `:1809-1825` | op fallback | the attempt row's binding | H | neutral |
| 13 | `scheduler/mod.rs:2037-2068` `on_terminal_exit` | card payload key | view for the card + status check | H | — (terminal kind never continues) |
| 14 | `scheduler/running_worker.rs:175-211` idle check ("each execution gets a fresh card and thread") | latest codex session of the card | the attempt's session row; comment rewritten; counts a turn only if `last_activity_ms` > `worker_bound_at_ms` (§3, PR-3) | H | old turn cannot fail the successor |
| 15 | `reaper/mod.rs:375-470` | `spawn_op_id` → attempt; release by card | `Session(s)` (active while converging): Live → fail it and release its lease, as today; other bound states release the latest bound attempt's held lease (as #16); unbound → nothing | A: Live | fails the new attempt |
| 16 | `workspace_lease/release.rs:54-72`, `:163-184`; callers `decision_sink.rs:185`, `reaper:433,461`, `scheduler:125,1701`, `routes/cards.rs:634`, `plugin_host/callbacks.rs:536` | card's newest active lease; `lease_attempt_tx` via owner op | lease by `attempt_id`; card delete resolves the card's latest bound attempt once (history, any status), before `fail_tasks_for_deleted_card_tx` and `card_delete_tx` (which deletes the sessions, `card.rs:514`, nulling the binding) (`routes/cards.rs:632-634`, `plugin_host/callbacks.rs:534-536`): fails it only if `dispatched|running`, and releases its held lease regardless (a cancel keeps it while cleanup is pending, `plan/cancel_running.rs:114-139`) | the ending attempt | late old report cannot release the new lease |
| 17 | `workspace_lease/facts.rs:50-81`; callers `plan.rs:589-600`, `git_delivery_settled.rs:53-61`, `git_candidate/view.rs:308-311`, `task_gate_run/admission.rs:72`, `task_verify_adapter/target.rs:219-223` | card's newest lease | PR-1: the attempt's lease; last commit stays the card's latest `worktree.committed` (`facts.rs:69-76`). PR-3: last commit scoped by the attempt's `delivery_id`; 19 legacy attempts on 4140 whose latest event has no `delivery_id` (41 such events) lose `last_commit` (accepted) | the attempt | old views never show the new lease |
| 18 | `task_verify_adapter/mod.rs:373-399` gate cwd | card's newest lease; terminal cwd from `terminal-worker` op `tx_output` by `idempotency_key` | only the lease arm moves to the attempt's lease; the terminal arm (frozen `terminal-worker` op output, a sanctioned op↔attempt read) is unchanged | the attempt | regate of an old attempt uses its own lease |
| 19 | `task_gate_run/admission.rs:48-57` | `worker_card_id == card` and Running | `run_target_tx` is shared with `task-gate-run` `prepare_tx` (no identity): `Attempt(attempt_id)` Live Running ∧ its session's card == payload card; `Session(identity)` only at MCP admission | A | neutral |
| 20 | `mcp_server/transport.rs:1039-1046` + `read.rs:970-985` `workspace_lease_for_card` (Worker forge cwd) | card's newest `held` lease | `Session(identity)` Live → its lease | A: Live | the running attempt's lease |
| 21 | `track_activity.rs:145-159`, `:186-215`; `track_activity/sql.rs:144-170` | every current row raises its `worker_card_id` | the view's attempt per session raises the card only when it is a `current_tasks` row (`track_activity/sql.rs:120-123`; a superseded attempt's card shows no verdict, as today) | H (ended sessions included) | old `failed` no longer outranks new `working` |
| 22 | `track_vcs/delta.rs:344-394`, `:405-412`, `runs.rs:85-95,373-395`, `track_fs_view/mod.rs:518`, `:1303-1312` | card payload key; first run with the card; run Markdown from card payload | run ↔ card from bindings; attempt facts per §5 | H | each run shows its own goal and prompt |
| 23 | `scheduler/worker_failure.rs:51-74` card delete | `worker_card_id` or op proof | the attempt resolved once by its caller (#16) | H | neutral |
| 24 | `terminal_sweeper.rs:68-74` | no current task with `worker_card_id = card` in `dispatched|running|verifying` | same statuses through the view, current rows only | H | neutral |
| 25 | `plan/cancel_running.rs:110-114`, `task.rs:131-151` | CAS on `worker_card_id` | CAS on `worker_session_id` | the attempt | neutral |
| 26 | `track_occupancy.rs:69-74` lease modes | lease's attempt via `operations.idempotency_key` | `wl.attempt_id IS NOT ?except`; the op join stays for `phase` | the attempt | neutral |
| 27 | `task_gate_run/mod.rs:148-151,273` gate checkpoint refs | `refs/neige/gate-runs/<track>/<card>/r<N>` ("one worker card serves one attempt"); `r<N>` restarts per attempt (`admission.rs:116-148`), `update-ref` overwrites (`forge_git.rs:81`) | PR-3: `<track>/<gate-run op id>`. The op id is a fresh hex id already in `prepare_tx`, unique per run; an attempt id (`<track>:<key>`) is not ref-safe (`mod.rs:148-149`). `FrozenRun.ref_name` is frozen at prepare and read back (`checkpoint.rs:55`, `finalize.rs:139`), so existing refs keep their names | the run | a successor's r1 cannot replace the predecessor's pin |

Unchanged: checkout admission, the liveness deadline, per-card capture/settings, attempt→card reads.

**Sweep method.** `git grep` over `crates/` (tests excluded) for `worker_card_id`, `FROM
workspace_leases`, `spawn_op_id`, op kinds in SQL, `find_by_kind_and_idempotency`, payload
`idempotency_key`, `session_projection_active_for_card`, `terminal_get_by_card`, `refs/neige` and
`format!(…card_id…)`; each hit read at its call site. Candidate refs carry the delivery id, hook
settings and forge dedup keys are per conversation; only row 27 assumed one attempt per card.

**Neutrality proof for PR-1.** (a) No production-reachable assertion changes except the
`claude-restart` refusal (§1; it also drops #2470's MCP flags for an unbound legacy Claude worker
card) and the backfill stamping one spawn-failed 4140 task (`06ff541a…:audit-planner-ui-final`),
whose card then shows a failed task; and binding at prepare makes `worker_card_id` visible while
`dispatched` (`neige_task_ls`, `Attempt` targets resolve with controllable false, activity). Re-fixtured through `bind_attempt_tx` (bind while
`dispatched`, then flip): the `seed_worker_op_target` tests (`tests/scheduler.rs:1682,1723,1756,
1807,1923,1964,2021,6902,6990,7093,7136`; `long_task_reliability.rs:36,71,135`; the four testing
the op proof become binding-forgery tests or go), `task_terminal.rs:502-577,652-693`, and the
restart tests (`reader_head_tests.rs:53`; `worker_mcp_tests.rs:368,406`). Deleted:
`task_terminal.rs:696` (deletes a bound row; production never does, `area.rs:212`,
`track.rs:413`, `task_projection.rs:1413`); `:580` stays green (key-current check). (b)
`worker_binding_backfill_matches_spawn_op_inference`: real migrator to N−1; spawn op / session /
task / lease per kind × attempt status × session state {running, exited, failed} with surviving
cards, a superseded attempt, an op-less legacy session, a deleted-session row; assert view, `worker_binding_tx`,
activity fold and occupancy against today's inference. (c) `session_binding_view_matches_is_active_authority`.
(d) The 4140 SELECTs. FE comments on the late stamp are updated (`fe/web/src/app/events/README.md:130`,
`queries.ts:458`, `fe/core/domain/report.ts:451-453`).

**Scan.** `scripts/gate-2493-worker-binding-inference.sh` (run by `local-ratchet-gates.sh`; a lint
step plus a `--selftest` step in `.github/workflows/ci.yml`): `git
grep` for `spawn_op_id` reads, `worker_op_targets`, `WORKER_SPAWN_OPS`,
`operation_idempotency_key_by_id`, `task_for_worker_card`, `owns_key`, card payload
`idempotency_key` reads, `worker_card_id =` in a `WHERE`, and `workspace_leases` filtered by
`card_id`. Exemptions with reasons, no count pins: released migrations, `worker_binding.rs`,
`session_mirror.rs`, row 6's worker-op-kind lookup. Op↔attempt by `idempotency_key` is sanctioned
(`operation/driver.rs:215`).

## 3. Lease, timeout and cleanup per attempt

- **Lease key.** `workspace_leases.attempt_id` (backfilled 273/273), a required `String` on the Rust
  row (fixtures pass an attempt id). `acquire_workspace_lease_tx` (`workspace_lease/mod.rs:145-166`)
  takes the attempt; `lease_owner` stays the op; `card_id`
  stays (path uniqueness, UI). Every release and fact read is by attempt (#16-#18, #20).
- **Worker timeout.** `mark_running` stamps start and deadline per attempt (`task.rs:242-266`);
  the anchor `max(started, progress)` (`task_liveness.rs:47-72`) hides the predecessor's progress.
- **Codex turn-ended check** (`running_worker.rs:104-127`) must not count the predecessor's last
  turn. A kernel-clock time boundary, not turn attribution: a candidate needs `last_activity_ms`
  (written with the resting status for every turn end, failed included, `liveness_feeder.rs:46-63,
  140-141`, `session_row.rs:443-460`) after the attempt's `worker_bound_at_ms`. One uniform anchor:
  a first spawn binds in `prepare_tx` before `thread_start` (so #1813's early failed turn stays
  detected), a continuation before any byte. One column, written by `bind_attempt_tx` (PR-3,
  backfilled from the session's `created_at_ms`: a first-spawn bind and its session insert share one
  `prepare_tx`, so the backfill is exact); rejected: fencing only `worker_bind_seq > 1`.
- **Cleanup marker.** Written only when failing a Live attempt (#10), with its id; the sweep releases
  that attempt's lease (#11); continuation admission refuses a marked session.

## 4. Message delivery (S2′)

**Declaration.** Typed and required on `calm_exec::WorkerProvider` (`calm-exec/src/provider.rs:40`),
implemented in `provider::worker`, registered in `calm-server/src/provider_registry.rs:28-58`:

```rust
pub struct TuiInput { pub message: MessageDelivery, pub keys_while_bound: BoundKeys }
pub enum MessageDelivery { BracketedPasteSubmit, Unsupported }
pub enum BoundKeys { Accepted, Refused }
```

Codex `{BracketedPasteSubmit, Refused}`, Claude `{BracketedPasteSubmit, Accepted}`, terminal and
managed `{Unsupported, Accepted}`. No dialog check: Codex task threads run `approval_policy:
"never"` (`shared_codex_appserver.rs:956`), `waitingOnUserInput` is unobserved, and Claude has no
sound signal (KNOWN GAP). `keys_while_bound` replaces
`card.kind == "codex"` (`target.rs:216`). Narrowing: the first release serves the existing
providers; target resolution's card-kind → session-kind map (`target.rs:148,159`) is a follow-up,
not a one-line change. Documented only: Codex trims surrounding whitespace, Claude turns a tab into
four spaces (probe c5/c7, k3).

**Envelope** (`terminal_interaction/actions.rs`, one encoder for A and B): one `WriteShape::Verbatim`
write of `ESC[200~` + header + `\n` + text + `ESC[201~` + `\r`. Fixed kernel headers control the
start of the inserted text, so `/` and `!` never lead an empty composer (probe c9, c10, k5); each
names only its attempt:
B `[neige] Planner message for attempt <attempt_id>:`; A `[neige] Next round: attempt
<attempt_id> of task <key>.` Text: non-empty; every `char::is_control` but `\n` and `\t` refused
(C0 incl. ESC and CR, DEL, C1). Cap **8,000 bytes** for header + text (the probe's 7,999-byte
paste arrived byte-exact on both TUIs). `submit` splits its CR into a second write
(`actions.rs:129-137`, `control_writer.rs:20-38`); a message never does.

**One kernel entry point: `TerminalInteraction::deliver(attempt, text)`**, used by B and A. It
attaches a kernel-private client whose hello sets `ClientCapabilities.kernel_originated_input:
true` (`calm-session/src/lib.rs:109-113`; today's Planner clients send `false`,
`terminal_interaction/client.rs:186`). The session admits `Input` from such a client as Observer
(`InputPermission::Kernel`, `calm-session/src/terminal_session.rs:176-188,226-236`); the pump admits
it only after the scope's `control` predicate (`input_authority.rs:84-99`); the WebSocket bridge
zeroes the flag for browsers (`ws/terminal.rs:299,396-404`). So `deliver` never claims or takes
control, has no observation anchor, and today's viewers are unchanged. Its scope `control` is
`may_write_tx(session, attempt)`, re-run by `WriteAuthority::admit` at the physical write. It
returns `Refused(reason)` (proven before any byte, including a handshake closed before
`ServerHello` and `INPUT_REVOKED_BEFORE_WRITE`, `terminal_interaction/client.rs:52`), `Written`, or
`Unknown` (ack lost, `control_writer.rs:94-125`, `receipts.rs:95-102`).

**Preconditions,** checked before the write; a refusal writes nothing:

1. `may_write_tx(session, attempt)` = Allowed (§2).
2. `message ≠ Unsupported` for the session's provider.
3. Terminal live and readable: renderer entry present, `observable()`, not exited (as
   `neige_terminal_show`'s `available`, `target.rs:268-279`), and DECSET 2004 bracketed paste on
   (`MODE_BRACKETPASTE` in `InputSurface.modes`, `calm-terminal-view/src/lib.rs:59-67`). This is the
   single liveness gate.

Timing: the pump queues writes, and `WriteAuthority::admit` (`input_authority.rs:90-99`) runs only
the scope callback when the writer dequeues, immediately before `WriteStdin`. So `deliver`'s
callback is 1 ∧ 3, read fresh: a predicate, exit or paste-mode change while queued refuses with
zero bytes.

Parked refusal covers every agent write path. A human typing in the browser connects as
`InteractiveUser` (`ws/terminal.rs:194-200`; `input_authority.rs:29-35` returns `true`) and keeps
that authority (§10.2). Follow-up (pre-existing, separate issue): Planner `text`/`submit` to a
Claude worker is refused while a browser viewer owns the terminal (`client_pump.rs:353-362`); route
bound-worker `text`/`submit` through the same kernel-input client so both Planner write paths share
one rule.

**Surface: action `message` of `neige_terminal_input`** (`input` = "send text or keys to a
terminal", agent-commands §3; `send` is mail only). Options: the target (`attempt_id`, or a
`terminal_id` resolving to a Live worker), `idempotency_key` (a replay returns the first receipt
from the caller connection's cache, `operations.rs:80-103`) and `read` with its wait arguments.
It uses (or creates, as `read` does) the caller's observation client for that cache and the
readback; `deliver` is only the write leg.
`observation_id`, `allow_output_since_observation`, `claim`, `release` get `-32602` listing the
valid options, as `detach` refuses `read` (`mcp_server/tools/terminal.rs:270-279`); the shared
target, replay and readback still justify one tool rather than a new verb. Schema
(`terminal.rs:49-58`): `["text","submit"]` → `["text","submit","message"]`; one description
sentence (1,331 of 2,048 B); PR-2 measures `SURFACE_MAX_BYTES` 30,884
(`mcp_server/tools/mod.rs:192-193`), never raising it. CLI: none (MCP-only, agent-commands §2).

Refusals (`-32403` like every terminal runtime failure, agent-commands §9; text and
`data.refusal` agree; prefix `neige_terminal_input: `):

| `data.refusal` | Text |
|---|---|
| `worker_parked` | `attempt <id> (task <key>) is <status>; its worker takes no input.` + by status: done/failed `Declare a task with "continues": "<key>", or a new task for a fresh worker.`; verifying `Wait for its gate.`; canceled `Declare a new task.` |
| `worker_ended` | `the worker of attempt <id> has ended (<state>); nothing was sent. Declare a new task.` |
| `worker_starting` | `attempt <id> is dispatched; its worker is starting. Read again, then send.` |
| `binding_changed` | `terminal <id> now serves attempt <new> (was <old>); show and read again.` (extends today's text, `target.rs:256-257`) |
| `message_unsupported` | `action "message" needs a task worker whose agent declares it (<providers>); terminal <id> runs <provider>. Use "text" or "submit".` |
| `worker_keys_refused` (was `codex_task_worker_input`) | `a <provider> task worker takes only action "message"; typed keys interrupt its turn without starting one.` |
| `terminal_unreadable` | `the worker's terminal has no live readable view (after a server restart until reattached, #2499), or bracketed paste is off; nothing was sent.` |
| `-32602` | invalid text (`U+001B at byte 12; only printable characters, newline and tab`), over the cap, or an option `message` does not take (`valid: attempt_id, terminal_id, idempotency_key, read, wait_*`) |

#1787 becomes "codex Live worker: `message` only"; guide item 2 becomes "correct codex/claude workers
via `message`" (guides 7,484 of 7,500). **Feature B** = this action; the readback confirms, the #2507
quiet wake catches a stuck worker. Receipts: `written`, `unknown`, or physical-write `refused` with its
reason; the typed policy refusals above are -32403 with `data.refusal`.

## 5. Continuation (feature A)

**Declaration.** Task-block field `continues: "<key>"` ("run in the worker of `<key>`'s current
attempt"), added like `start` (migration 0138): `TASK_FIELDS` (`report_blocks/kinds.rs:160-183`), a
validator beside `validate_task_start` (`task_execution.rs:138-178`), the block schema
(`track_report_blocks/contracts.rs:377-382`), projection (`task_projection.rs:874-878`,
`:1643-1686`), column `tasks.continues TEXT NULL`, `neige_task_ls` (`plan/list.rs:102`), CLI
render. Allowed kinds: the closed set {codex, claude} in calm-types, pinned by a calm-server test
to the task kinds whose provider declares `message ≠ Unsupported`. Rejected: `depends_on` (requires
`done`; a gate-red predecessor is `failed`), `resume` (Claude's meaning).

| Stage | Rule | Where |
|---|---|---|
| Block | kind in the set; `read_write`; default `spawn`; `start: checkout`; no `head`/`base`; not its own key | validator |
| Plan | named key exists, same kind, `read_write` (reviews neither continue nor are continued); `continues` is a graph edge for `find_cycle` | `report_blocks/tasks.rs:550-571,725-732`; diagnostics `unknown_continuation`, `continuation_mismatch` |
| Ready | the predecessor's current attempt is terminal; else waits like a dependency | `checkout_admission` (`track_occupancy.rs:115-150`) |
| Bind | in order: `Attempt(pred)` is Parked{last = pred} (else `worker ended …` / `already continued …`), pred `done` or `failed`, no cleanup marker; plan path == session terminal cwd; preconditions 2–3; prompt + header ≤ 8,000 bytes | `worker-continue` transaction |

A second continuation of one key waits on the checkout, then finds Parked{last ≠ pred}: refused.
No in-progress-turn rule: a paste near or during a turn arrived exactly once (c10, k7).

**`worker-continue` is a transaction-only operation, then one in-process delivery.** No durable
delivery phase exists:

0. Branch point: `build_worker_payload` (`scheduler/mod.rs:157-201`) returns `worker-continue` for a
   row with `continues`, and `drive_spawn` runs that kind via `commit_keyed`, so both dispatch and
   `resume_dispatched` take it and a re-drive can never cold-start a `codex-worker`.
1. The scheduler commits it with `OperationRuntime::commit_keyed` (`operation/driver.rs:182`; the
   `TxOnlyAdapter` path, `operation/tx_only.rs:1-30`), key = attempt id, payload a pure function of
   the frozen row (`scheduler/mod.rs:157-201`), listed in `TASK_BOUND_ADAPTER_KINDS`
   (`operation/mod.rs:77-84`). One `BEGIN IMMEDIATE`: `refuse_if_context_stale`
   (`operation/mod.rs:100-118`); admission (table); `bind_attempt_tx(seq = last + 1)`; lease with
   `attempt_id`; render the prompt for the new `attempt_id` into the op output; and
   `mark_acknowledged_running_tx` (`scheduler/mod.rs:2233-2248`), so `running_started_at_ms` and the
   deadline are stamped before any byte. It creates no card, session, thread, token or process.
   An admission refusal is a `Conflict` with the `refused: <word>: …` convention
   (`workspace_lease/worker.rs:157-161`); the transaction rolls back (no binding, lease or bytes)
   and `fail_spawn` settles the task `spawn-failed: refused: <word>: …`. Transient DB errors stay
   retryable.
2. Only the scheduler's single-flight path (`InflightGuard`, `scheduler/mod.rs:851-853`) delivers,
   and only after a fresh `KeyedCommit::Committed` (`driver.rs:195-200`, surfaced to the caller);
   a `Replay` delivers nothing. It calls, once and in process: `verify_worker_checkout` on the op
   output (branch, base, canonical path, `workspace_lease/worker.rs:469-480`; `verify_recorded_head`
   is a no-op without a declared head, `:456-466`), then `deliver(attempt, prompt)`.
3. `Refused` (before any byte) fails the Running attempt through the normal worker-failure path:
   `fail_worker_task_tx(…, "spawn-failed", "continuation refused: <reason>")`
   (`scheduler/worker_failure.rs:4-43`) plus the attempt's lease release with
   `ReleaseDelivery::Commit(AttemptOutcome::SpawnFailed)` (as worker compensation,
   `codex_adapter/mod.rs:1087`), in one transaction. `Written` → done. A crash after the commit →
   "Running without its prompt": nothing re-drives a Running attempt; in process the #2507 quiet
   wake fires (60 s); after a real restart the view is unreadable (#2499, `worker_quiet.rs:140-157`)
   and the idle window (`worker_liveness.rs:23`, 1 h) or a Planner cancel resolves it. `Unknown`
   stops the writer and marks the renderer barrier uncertain (`control_writer.rs:117-123`,
   `input_authority.rs:133-141`); later clients are dropped before `ServerHello`
   (`client_pump.rs:146-148`), so no new client can attach to that terminal (browsers, Planner
   reads) and resizes stop: the Planner cancels the task and that worker is lost (pre-existing,
   separate issue). At-most-once holds by
   construction.

Reasons name the alternative (ended: `declare the task without "continues"`; already continued:
`continue <key2>`; canceled, cleanup pending, checkout moved, prompt over 8,000 bytes, terminal).

**Dead sessions cannot bind:** the reaper converges only once the PTY is gone (`reaper/mod.rs:140-355`)
and precondition 3 needs a live PTY at bind; a PTY dying later gives `Refused` or `Unknown`.

**Attempt-owned facts.** Card payload goal/context/acceptance/prompt/key and title are first-round
facts (`codex_adapter/mod.rs:847-856`); a continuation never mutates them. Run views
(`track_fs_view/mod.rs:1303-1312`, `runs.rs`) read the attempt's `tasks` row and its op output's
prompt (`codex_adapter/mod.rs:889`, `claude_adapter/mod.rs:901`, or `worker-continue`) by
`idempotency_key` = attempt; `delta.rs:405-412` maps a card change to the runs bound to it.

**Worker contract.** `prompts/worker/head-{mcp,cli}.md` (line 3, "one job") gain: "A later round
may arrive as a `[neige] Next round` message: a new job with its own `attempt_id`; report it once
with that id. A Planner message naming an attempt you already reported is not work." Goldens
regenerate.

## 6. Races and crash windows

| Window | Kernel sees | Outcome |
|---|---|---|
| Crash (in process) after the continuation commit, before `deliver` | attempt Running, binding, lease | no write on recovery; quiet wake fires; Planner may `message` |
| Server restart after the commit, before `deliver` | Running, view unreadable (#2499) | no write; idle-window timeout or Planner cancel |
| Claim done, crash before the continuation commit | task `dispatched`, nothing bound | `resume_dispatched` re-runs `commit_keyed` (`worker-continue`, never a respawn) |
| Checkout moved between commit and `deliver` | `verify_worker_checkout` fails | `Refused`: attempt failed (spawn-failed), lease released, zero bytes |
| `deliver` refused before any byte (PTY gone, paste mode off, verdict) | `Refused` | attempt failed through the worker-failure path; Parked{last = new}; Planner continues the new key or starts fresh |
| Ack lost after the bytes left | `Unknown` | terminal input locked; lease kept; report, liveness or Planner cancel resolve it; the worker is lost |
| Worker (or a late predecessor report) reports the OLD attempt after the bind | Live{new}; old bound here, terminal | same outcome → idempotent; else `Conflict` naming the Live attempt; release is by attempt and finds nothing held |
| Planner B message during A's delivery, or before A's prompt | Live `running` | both are kernel writes through the renderer's one input sequence; each lands whole; a B arriving first is harmless (headers name the attempt) |
| Timeout/cancel of the new attempt | Live{new} | attempt terminal with its marker; sweep kills, releases new's lease |
| B message processed after the worker reported | Parked{last = a} | a queued/steered message may still reach the model; head prompt says it is not work; later input refused (`worker_parked`) |

## 7. Planner prompt and docs

`crates/calm-server/prompts/planner.md` Tasks, last bullet (line 30), plus one bullet:

> … Point the next review at the new key. For a mechanical round (red gate, fmt, golden, a review
> nit) add `continues: "<old key>"`: the same worker takes it. A design change, a long or
> timed-out run, and every review get a fresh worker.
> * To correct a running codex/claude worker without stopping it, send `neige_terminal_input`
> action `message` by its `attempt_id`.

Budget: ~6,883 B rendered + 329 → ~7,212 of 7,500 (`planner_card.rs:383`);
`tests/goldens/dev_planner_prompt.txt` regenerates. `docs/using-neige-calm.md` lines 90 and 116 add
`continues`, its refusal, and that a continuation takes the checkout like any writer.

## 8. PR split, tests, mutations

Reader-level neutrality is enforced by the existing suite, the backfill test and each PR's L2
review; this document fixes the model, invariants, per-reader accepts and the PR split.

Tests drive production entry points (MCP calls through the kernel socket, the operation runtime and
the real client pump/writer, real migrations); no copied behaviour. Each mutation is single-factor; its listed red set is a prediction, re-measured on the implemented
targets before the mutation run.

| PR | Scope | Tests | Mutations → red set |
|---|---|---|---|
| PR-1 S1 (L2) | column `worker_session_id`, index, view, migration + backfill, `worker_binding.rs`, rows #1–#26 (PR-3 anchors excepted), lease `attempt_id`, card-delete order, `claude-restart` refusal, scan | `worker_binding_backfill_matches_spawn_op_inference`; `session_binding_view_matches_is_active_authority`; `first_spawn_binds_attempt_in_prepare_tx` (codex, claude, terminal); `bind_refuses_non_dispatched`; `one_attempt_per_session_is_enforced_by_sql`; `authority_follows_session_liveness`; `ended_worker_keeps_its_activity_and_run`; `report_for_another_attempt_names_the_bound_attempt`; `parked_same_outcome_report_is_idempotent`; `flip_requires_the_bound_session`; `terminal_task_completes_after_its_session_is_exited`; `card_delete_fails_and_releases_the_live_attempt` and `card_delete_after_cancel_releases_the_held_lease` (REST route and plugin callback; free occupancy; lease released, delivery attributed); `claude_restart_refuses_a_bound_card` | M1 view ignores `session_active` for Live → `authority_follows_session_liveness`, `worker_binding_backfill_matches_spawn_op_inference`; M2 drop `status='dispatched'` from the bind guard → `bind_refuses_non_dispatched`; M4 drop the unique index → `one_attempt_per_session_is_enforced_by_sql`; M5 activity reads authority instead of history → `ended_worker_keeps_its_activity_and_run`, `worker_binding_backfill_matches_spawn_op_inference`; M6 release only when `dispatched|running` → `card_delete_after_cancel_releases_the_held_lease`; M7 restore one `worker_op_targets_card_tx` call → scan gate; M26 report admission ignores the session binding → `report_for_another_attempt_names_the_bound_attempt`, `parked_same_outcome_report_is_idempotent`; M27 drop the flip's session guard (equality by construction at every production caller) → `flip_requires_the_bound_session` (direct SQL unit test) |
| PR-2 S2′ + B (L2) | `TuiInput`, `deliver` (kernel-input client), `message` action, envelope, typed `WriteVerdict`, Parked refusal on every agent write path, refusals, prompt/guide/tool text | `message_is_one_bracketed_paste_write`; `message_never_leads_with_slash_or_bang`; `message_text_refuses_controls`; `message_over_cap_is_refused`; `message_refuses_anchor_and_control_options`; `parked_worker_refuses_input_{done,verifying,failed}` (every action); `unbound_terminal_takes_text_refuses_message`; `input_after_rebinding_reports_binding_changed`; `codex_live_worker_takes_message_refuses_keys`; `claude_live_worker_takes_message_and_keys`; `message_refused_when_bracketed_paste_off`; `queued_message_refused_when_paste_mode_turns_off` (pause after enqueue; 0 bytes); `message_delivered_while_a_browser_owns_the_terminal` (real pump, kernel-input client); the existing `ws_strips_kernel_originated_input_flag` (`tests/cases/ws_terminal_v2.rs:640-690`) covers browsers; `message_replay_writes_once`; `message_to_a_just_reported_attempt_is_refused`; `terminal_without_readable_view_refuses_message` | M8 `may_write_tx` returns `Allowed` for a `Parked` binding → `parked_worker_refuses_input_{done,verifying,failed}`, `message_to_a_just_reported_attempt_is_refused`, `task_terminal.rs:327` `task_completion_revokes_control_but_preserves_current_output`, `:368` `readback_reports_a_task_that_finished_during_the_wait` (`controllable` is the predicate's verdict), `:422` `input_queued_behind_a_readback_rechecks_write_authority_under_the_serial`; M9 header omitted → `message_is_one_bracketed_paste_write`, `message_never_leads_with_slash_or_bang`; M10 ESC allowed → `message_text_refuses_controls`; M11 `SplitTrailingCr` → `message_is_one_bracketed_paste_write`; M12 Codex declares `BoundKeys::Accepted` → `codex_live_worker_takes_message_refuses_keys` (PR-2 replaces `tests/cases/codex_worker_terminal_input.rs:55` with it); M13 `deliver` hello without `kernel_originated_input` → `message_delivered_while_a_browser_owns_the_terminal`, `message_is_one_bracketed_paste_write`, `message_never_leads_with_slash_or_bang`, `codex_live_worker_takes_message_refuses_keys`, `claude_live_worker_takes_message_and_keys`, `message_replay_writes_once`; M21 drop paste mode from `deliver`'s scope callback → `queued_message_refused_when_paste_mode_turns_off` |
| PR-3 A + prompt (L2) | `continues`, `worker_bound_at_ms`, seq migration (`worker_bind_seq = 1 WHERE worker_session_id IS NOT NULL`, `UNIQUE (worker_session_id, worker_bind_seq)`, index swap; no cross-column CHECK: `ON DELETE SET NULL` nulls only the session id, and SQLite cannot add one to a table with rows), admission, tx-only `worker-continue`, in-process delivery, turn fence, gate refs (row 27), attempt-owned run facts, worker/Planner prompts, docs | block/plan validation matrix; `seq_migration_backfills`; `one_live_attempt_per_session_is_enforced_by_sql` (raw second bind); `bind_order_is_unique_per_session`; `gate_runs_of_two_attempts_keep_their_own_refs` (through `neige_task_gate`: both attempts run r1, both refs keep their commits, an interrupted successor checkpoint cannot read the predecessor's pin); `card_delete_after_seq_migration_with_a_continued_session` (both entry points); `continuation_is_running_before_any_byte` (real pump/writer); `continuation_refused_when_worker_gone` (asserts the admission reason before any precondition, task `failed` and its `task.failed` reason; card/session counts unchanged); `crash_after_claim_before_commit_continues_not_respawns` (card/session counts unchanged); `concurrent_dispatch_and_recovery_write_once` (one initial write, zero recovery writes); `gate_red_then_continuation_reuses_card_and_session`; `done_then_continuation_reuses_card_and_session`; `second_continuation_of_one_key_refused`; `checkout_moved_before_delivery_writes_nothing`; `refused_delivery_fails_the_attempt_and_releases_its_lease`; `lost_ack_keeps_the_lease_and_runs`; `in_process_drop_after_commit_does_not_write_and_quiet_wake_fires`; `restart_after_commit_does_not_write_and_idle_window_fails`; `continuation_timeout_is_reaped_and_releases_its_lease`; `input_after_continuation_binding_is_accepted`; `lease_release_is_keyed_by_attempt`; `regate_of_an_older_attempt_uses_its_own_lease`; `turn_ended_ignores_a_turn_completed_before_the_bind`; `turn_ended_counts_a_turn_completed_after_the_bind`; `turn_ended_counts_a_failed_turn_after_the_bind`; `turn_ended_counts_a_first_spawn_turn_failed_before_the_running_stamp`; `quiet_wake_anchors_to_attempt_start`; `run_views_show_each_attempts_goal_and_prompt`; prompt goldens + budgets | M3 release by card → `lease_release_is_keyed_by_attempt`; M14 admission accepts NoSession → `continuation_refused_when_worker_gone`; M16 skip `verify_worker_checkout` → `checkout_moved_before_delivery_writes_nothing`; M17 release the lease on `Unknown` → `lost_ack_keeps_the_lease_and_runs`; M18 drop the turn fence → `turn_ended_ignores_a_turn_completed_before_the_bind`; M19 run view reads the card payload → `run_views_show_each_attempts_goal_and_prompt`; M20 fence on `last_turn_completed_ms` → `turn_ended_counts_a_failed_turn_after_the_bind`; M22 anchor on `running_started_at_ms` → `turn_ended_counts_a_first_spawn_turn_failed_before_the_running_stamp`; M23 drop the Live index → `one_live_attempt_per_session_is_enforced_by_sql`; M24 drop `UNIQUE (worker_session_id, worker_bind_seq)` → `bind_order_is_unique_per_session`; M25 restore card naming in `gate_run_ref_name` → `gate_runs_of_two_attempts_keep_their_own_refs` |

Each PR runs `scripts/local-ratchet-gates.sh`, `scripts/local-contract-gates.sh`, the whole
`-p calm-server` run (new SQL readers and tools hit the source-scan suites), quick Rust gates, and
PR-3 regenerates OpenAPI/wire if the REST task row
gains `continues`.

**4140 acceptance (issue).** PR-1: every card→attempt read goes through the owning module, the scan
is green, `SELECT COUNT(*) FROM tasks WHERE worker_session_id IS NOT NULL` ≥ 282. PR-2: one message
each to a running codex and claude worker, with the grid open, arrives verbatim in the same or next
turn; a stuck worker wakes the Planner within ~60 s (#2507); input to a done worker is refused with
the `continues` hint. PR-3: after a red gate, a `continues` task runs in the same
card/session/conversation, its `neige_task_done` is accepted, the kernel commits and the gate runs;
continuing an ended worker is refused with its reason.

## 9. Rejected alternatives

`thread/fork` (still one card per attempt); same-key recovery attempt (#2405); reopening a done
task; per-site exceptions; workers in the harness; kernel `turn/start`/`turn/steer` (receipted but
Codex-only; the TUI path gives up delivery without a viewer, `codex_adapter/mod.rs:1297,1351`).

## 10. Owner decisions and questions

1. **Resolved: viewers need no change.** Browser viewers hold control (`xterm-view.tsx:469-473,
   594-597`; the grid keeps cards mounted), which refuses a claim-based kernel write
   (`client_pump.rs:353-362`); `deliver` writes as Observer via the kernel-input capability (§4).
2. **Decided:** a human typing into a Parked worker in the browser stays allowed (§4). The safety
   boundary is the workspace: the gate samples HEAD and dirty state against its candidate
   (`gate-target-mismatch`, `task_verify_adapter/target.rs` `reasons()`), and the next prepare
   refuses a dirty tree (`ensure_clean_tree`). The Planner's parked refusal is guidance. The
   worker card states the parked attempt above the terminal and refuses nothing (#2526).
3. **Confirm:** `Unbound` as a fourth state (§2); the 8,000-byte cap, which refuses 2 of 264
   rendered prompts on 4140 (max 9,213 B).

## 11. KNOWN GAPs

- Residual composer text or concurrent human typing merges with the message (c3, k5; no clear key,
  no control fence); residual `/` or `!` runs as a command. An input-capturing prompt in either TUI
  takes the message and its Enter (Claude has no sound signal; 0 `permission_request` on 4140).
- After a server restart, workers started before it take neither messages nor continuations until
  reattached and readable (#2499); nor does a Codex worker whose optional viewer failed or exited.
- A crash after the continuation commit may leave the attempt Running without its prompt (after a
  restart, only the 1 h idle window or a cancel resolves it); a lost ack (A or B) locks input.
- Turn fence: any thread-status stamp after the bind (`liveness_feeder.rs:46-50`), a predecessor
  turn's included, makes the session a candidate (then the live recheck and grace path decide).
- A codex viewer whose thread died while its PTY lives is not reaped (progress waits for the idle
  window); the reused card keeps its first-round title, goal and prompt (card payload); a predecessor
  task row shows its card's current activity (`fe/core/view/track-page.ts:74-116`), i.e. the
  successor's; `claude-restart` refuses a bound task worker (a bound Claude worker whose CLI exited
  waits for the liveness timeout) and no longer gives an unbound legacy worker card MCP flags.
- The gate samples the workspace when it ends, so a change made and reverted during the gate (a
  human in the browser, §10.2) is invisible to it.
- A B message queued just before the worker reported may still reach the model (§6). Untested:
  Codex `disable_paste_burst`/`tui.keymap.*`, Claude Rewind, Tab queue vs `turn/start`, `TERM`;
  the Codex viewer starts a title thread per turn (#2510).
