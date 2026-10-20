# New blank Planner conversation in the same Track (#2774 v1)

Status: proposal for the v1 scope given at the top of #2774 (owner decision, 2026-10-19). The person starts a new, blank Planner conversation in the same open Track. The previous conversation becomes read-only history and stops receiving Planner wakes. The entry point, the UI and the backend ship in one delivery.

Out of scope: promoting an Assistant, `/planner`, AI self-nomination, approved-waiting-for-idle, and every mechanism from #2856, #2864 and #2892.

Review tier **L2**: the change crosses authority and persistence boundaries and adds one migration.

## 1. Why the existing restart is not enough

`POST /api/cards/{id}/planner/restart` (`crates/calm-server/src/routes/planner_cards.rs:597`) calls `fresh_start_planner_card` (`:627`). That function takes the `CardStartFence`, runs one `planner-harness-start` through `start_harness_card` (`:657`, `:707`), and then submits a separate `planner-harness-shutdown` (`:663`).

The start's `prepare_tx` (`crates/calm-server/src/operation/planner_harness_start_adapter.rs:717`) already does most of a cutover in one transaction:

- captures the active predecessor (`:830-843`);
- inherits its fused queue and `push_watermark` (`:884`);
- harvests stranded human sentences (`:944`);
- empties the predecessor's queue as a move (`:1042`);
- calls `session_prepare_deferred_planner_tx` (`:1056`), which supersedes the old row, stamps `queue_harvested_at_ms`, inserts the starting placeholder, and repoints `cards.session_id` and the track root (`crates/calm-truth/src/db/sqlite/session_mirror.rs:338-379`).

Where restart falls short of this feature:

1. **Not blank, and no history boundary.** `Restart` keeps the transcript: the transcript-clearing flag is set only for `Reset` (`planner_cards.rs:696-697`, `:739`). The transcript is read per card with no conversation boundary (`crates/calm-server/src/routes/cards.rs:189-208`), so the new thread's rows continue the old view. The FE says so in `RESTARTED_NOTICE` (`fe/core/domain/conversation-restart.ts`): "Earlier messages stay here".
2. **No idle admission.** Restart is a recovery tool. It retires an active runtime whatever its turn state and inherits pending user messages (`planner_harness_start_adapter.rs:838-911`) instead of refusing them. It never checks provider holds or unknown activity.
3. **Not idempotent.** Each call mints `operation_key: new_id()` with `idempotency_key: None` (`planner_cards.rs:753-754`, `:774-775`). A replay after a lost response starts a second session.
4. **It resurrects the old session.** A start that fails after its commit plans `restore_old_runtime` (`planner_harness_start_adapter.rs:1706`, `:2441`), which hands authority back to the superseded session. The issue requires a durable cutover, with recovery handling failures after it.
5. **Not person-only.** The route accepts any `Actor` (`planner_cards.rs:599`). Archive is the precedent for a person-only lifecycle write (`crates/calm-server/src/routes/conversation_archive.rs:52-57`).
6. **No quiescence.** The old live owner runs until the second operation stops it. Observations it already acknowledged into memory (`crates/calm-server/src/harness/run_loop.rs:585-613`, acknowledged before they are persisted) can be passed by the dispatcher cursor (`crates/calm-server/src/dispatcher/mod.rs:1347-1350`). They are missing from the persisted snapshot that `prepare_tx` moves.

## 2. Decision: same card, new session, a durable conversation floor

The Track's Planner **card** stays the Planner identity. The new conversation is a new **worker session** on that card, with a new provider thread. A persisted **floor** splits the card's transcript into conversations.

A new Planner card was considered and rejected:

- `idx_cards_one_planner_per_track` (migration 0083) would force the old card off `role='planner'`, and no existing role fits a retired Planner. A new `CardRole` variant would touch about 14 files with exhaustive matches, plus the wire format, and would mutate the role cache while requests are in flight.
- Every card-keyed authority fact would need rebinding: forge approvals keyed on `AiPlanner(card)` (`crates/calm-server/src/ask/forge_approval.rs:42-80`), the per-card open asks, dispatcher routing through `verify_role` (`dispatcher/mod.rs:1401-1420`), the per-card push cursor, the card-payload provider and model settings, and boot-recovery SQL that filters `c.role='planner'`.

That is exactly the cache-grant churn the issue warns against. With the same card, none of those facts move: authority changes only through the session supersede that restart already performs and tests.

The floor needs one migration (`0177_planner_conversation_starts.sql`, the next free number):

```sql
CREATE TABLE planner_conversation_starts (
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 1),
  after_item_id INTEGER NOT NULL CHECK (after_item_id >= 0),
  started_at_ms INTEGER NOT NULL,
  PRIMARY KEY (card_id, ordinal)
);
```

- Conversation 0 is implicit. Conversation `k` holds transcript rows with `after_item_id(k) < id <= after_item_id(k+1)`. The current conversation is the highest ordinal and is unbounded above.
- The transcript table's row id is `AUTOINCREMENT` (migration 0032, later only renamed), so ids are never reused and every later row falls in the current conversation.
- `after_item_id` is the card's maximum transcript row id, read inside the cutover's write transaction.
- `/planner/reset` needs no change: it deletes all of the card's rows, and a conversation with no rows is never listed.

## 3. Admission: `POST /api/cards/{id}/planner/conversations`

The route lives in a new file, because `planner_cards.rs` is already 903 lines. Steps, in order:

1. **Person only.** Call `require_rest_user_actor_for` and `track_access::require_detail_access`. The write transaction rechecks access with `require_detail_access_tx`, as archive does (`conversation_archive.rs:52-57`, `:159`): the route drives the start through `CardStartFence::start_new_conversation`, an invocation of the start adapter that holds the request's `Principal` (`StartInvocation::NewConversation`; never persisted), and the cutover reads that person's current access before its first write (§4). Also require a Planner binding, an open track that is not area-chat, an unarchived card, and an `Idempotency-Key`.
2. **Replay before admission.** Scope the key as `planner-new-conversation:{card_id}:{key}` and look it up with `find_by_idempotency_key` (`crates/calm-server/src/operation/mod.rs:887`). If an operation already exists, wait for it and return its stored outcome without admitting or quiescing; doing either again would act on the *new* conversation. The `payload_hash` covers `{actor, card_id, "new_conversation"}` and excludes the expected session, so a replay hashes the same. Different content under the same key returns 409 `idempotency_key_reused`.
3. **Fence, settle, readiness.** Hold `CardStartFence::lock` for the whole request. It is the same lock that sends, lazy recovery and Planner wake take (`crates/calm-server/src/planner_hibernation.rs:180-205`), and the dispatcher holds it from `observe_envelope` through the cursor bump (`dispatcher/mod.rs:1325-1350`). Then call `require_settled()` (`crates/calm-server/src/operation/planner_start_fence.rs:64`) and check provider readiness with `ReadinessFor::StartOperation` (`planner_harness_start_adapter.rs:713`). A failure in this step returns 409 and changes nothing.
4. **Idle, with a live owner.** Run the existing hibernation quiesce on the owner loop (`crates/calm-server/src/harness/run_loop/hibernation.rs:30-130`). Under the issuance mutex it requires `idle_for_lifecycle` (`:132-152`): empty ingress, a state of Idle, TurnCompleted or Resumed, an empty pending queue, nothing steered, and no provider-active turn. It then persists the owned snapshot through the guarded writer and closes the observation gate. If it refuses, the route returns 409. A request abandoned after a successful quiesce leaves an ordinary hibernated Planner, which the existing wake path revives.
5. **Idle, without a live owner.** The persisted snapshot must parse (`HarnessSnapshot::parse_known`). An active status without an owner is unknown activity, the same rule archive applies (`conversation_archive.rs:111-124`).
6. **Refusals: 409, and nothing changes.** Refuse each of these:
   - an active turn or compaction;
   - pending **user-authored** or steered entries;
   - an open `hold` ask from the current session (the provider holds; `ASK_OPEN_SQL`, `crates/calm-server/src/ask.rs:48-58`);
   - a card with canonical conversation storage (#2843): its accepted inputs, pending ones included, stay with the current carrier, and the start moves only historical storage, so the route refuses it before step 3's readiness check and step 4's quiesce;
   - unknown activity: an unparseable snapshot, an active row without an owner, an unsettled start or compensation, an `acp_submissions` row in `sending` or `unknown`, a declared `unknown_outcome` recovery, or a `failed` current session.

   Refusing a `failed` session is a v1 narrowing: the person recovers it with restart first. Pending **system** observations are not a reason to refuse when no owner is live; they are moved. A **live** owner with pending system observations is refused with 409, because the quiesce requires an empty pending queue (`idle_for_lifecycle`). This is also a v1 narrowing: the owner delivers them, and the person asks again.
7. **Submit.** Submit `planner-harness-start` with the restart payload (`force_new_thread: true`, `goal: None`, no model or provider fields) plus one optional field, `new_conversation: Some({ expected_session_id })`. Serde skips the field when it is `None`, as it already does for `opening_briefing`, so the payloads and hashes of all existing starts stay byte-identical. The test `the_persisted_payload_field_names_are_frozen` gains this name once.

## 4. The cutover transaction (inside the existing `prepare_tx`)

There is one `BEGIN IMMEDIATE` transaction and no new phase, operation kind or second transaction. When `new_conversation` is set, restart's `prepare_tx` gains these steps:

- **CAS preconditions**, read in the transaction; any mismatch fails the operation with 409 and nothing written:
  - `cards.session_id == expected_session_id`, and that row is active;
  - its persisted snapshot holds no user-authored entries;
  - the session has no open hold ask;
  - `queue_harvested_at_ms IS NULL`.
- **Requester access**, read in the same transaction after the CAS and before the first write: `require_detail_access_tx` for the invocation's `Principal`. A start driven without the request (a boot or lease re-drive before `TxCommitted`) has no requester and is refused; nothing is written.

  The existing guards then make the move exactly-once: `session_supersede_active_tx` updates only active rows and errors on 0 rows (`session_mirror.rs:381-406`), `session_mark_queue_harvested_tx` stamps only once (`crates/calm-truth/src/db/sqlite/session_projection.rs:370-386`), and `ws_one_active_per_card` (migration 0163:108) allows one active row per card.
- **Transfer, via the existing inherit and harvest rules.** The whole persisted queue (system observations only, given admission) and its `push_watermark` move together into the placeholder snapshot (`:884`). The predecessor's queue is emptied in the same transaction (`:1042-1055`). Nothing replays all history, and nothing skips to the latest event. Events above the moved watermark arrive through the normal dispatcher push and catch-up (`crates/calm-server/src/harness/mod.rs:396`).
- **Retire and repoint.** `session_prepare_deferred_planner_tx` supersedes the old row, inserts the starting row, and repoints `cards.session_id` and the track root. The new row has no token yet.
- **Floor.** Insert `planner_conversation_starts(card, max_ordinal + 1, MAX(id), now)`.
- **Event.** Emit `CardUpdated`, as archive does, so the FE invalidates the run, transcript and list queries. No new event kind.

Unchanged by construction: the Report, tasks, attempts, workers, claims, gate runs, deliveries, user release decisions, open `wake` asks and forge approvals. The transaction writes only `worker_sessions`, `cards`, `tracks.root_session_id`, `planner_conversation_starts` and appended `events`. After the commit, the route submits the existing `planner-harness-shutdown` for the old runtime, as restart does (`planner_cards.rs:657-664`).

## 5. Credentials, requests admitted before the cutover, approval bindings

- **New requests.** Codex Planner calls resolve per call through `session_projection_active_by_thread`, which matches only active states (`crates/calm-server/src/mcp_server/transport.rs:1747-1790`). Card-bound connections recheck `is_active_authority` on every call (`:1650-1668`). The supersede clears the token hash for Claude and OpenCode Planners (`session_mirror.rs:391`). After the commit, the old thread and the old token resolve to nothing.
- **Requests admitted before the cutover.** Every gated event append re-resolves the session actor from the live `worker_sessions` row in the caller's write transaction (`crates/calm-truth/src/db/sqlite/events.rs:56-73`) and refuses a session that is not active (`crates/calm-truth/src/decision_gate.rs:85-89`). SQLite serializes writers, so a pre-admitted write commits either before the cutover (it is then a fact the new conversation sees in the Report and tasks) or after it (refused, nothing written).
- **Persisted bindings, not cache grants.** The only cache read on that path is the role check at `decision_gate.rs:111`, and this design never changes the role. Review must sweep for any Planner tool that commits rows without a gated event in the same transaction; fix each one at its owner.
- **Sweep result.** Of the Planner-visible kernel tools, only `neige_source_capture` committed rows (`report_sources` insert and quote append) in a plain transaction; both transactions now check `ToolCallIdentity::session_is_active` before writing, as `neige_mail_send` does. `neige_mail_cat`'s read stamp already checks it. Task, report, track, ask, terminal-open and calendar writes append gated decision events as the session actor. Preview add/remove is in-memory, and terminal input and control are process effects re-checked per write leg. Card-bound external effects (forge approvals and actions, admin GC/vacuum, `track_add` resume) are tracked in #2932.
- **Approval bindings stay as they are.** Forge approvals stay bound to `(track, AiPlanner(card), idempotency_key, payload_hash)`. Open `wake` asks stay open, and answering one wakes the current conversation. `hold` asks block admission, so none is orphaned. No ask is rewritten, withdrawn or rebound.

## 6. Provider, settings, permissions

The start reads:

- the provider from the card's sticky `planner_provider`, and the model and effort from the card payload (`startable_provider`, `planner_harness_start_adapter.rs:329`);
- the instructions from the Track's bound template (`bound_template`, `:171`);
- the MCP tool surface from the persisted role.

This is the same adapter path, and therefore the same creation policy, as every Planner start. The request body has no fields, so it cannot widen anything.

## 7. Failure, boot recovery, lost responses

- **Before the commit.** Refusals and readiness failures write nothing.
- **Crash before `TxCommitted`.** Boot re-drives the operation without the request, so the requester access check fails it with nothing written; the key answers that failure, and the person asks again.
- **Install catch-up.** The new session's install catches up on events above its own row's watermark, for every provider: an ACP provider's full checkpoint, or the queue and watermark Codex and Claude adopt together from the row. A re-drive after an earlier drive persisted its catch-up therefore replays nothing twice.
- **After the commit.** The cutover is final. For `new_conversation`, `plan_compensation` (`:1633-1716`) omits `restore_old_runtime` and keeps `abort_harness_task`, `interrupt_thread` and `fail_runtime`. The new session stays current as a failed carrier (`session_projection.rs:575-595`) holding the moved queue and watermark. The harvest journal is empty, because admission refused user entries.
- **Recovering a failed new conversation.** The existing restart runs inside the new conversation. It leaves the floor alone and inherits the queue and watermark (the #2192 path, `:845-911`). The old session is never restored.
- **Lost response.** A replay (§3, step 2) of a successful operation returns the same `{card_id, conversation, new_thread_id}`, with no second floor and no second session. A failed operation is final for its key; the FE mints a new key for each confirmed click.

## 8. Read-only history and wake routing

- **Reads.** `GET …/harness/items` returns the current conversation by default (`id > after_item_id(max)`), so the pane's existing windowed history needs no client change. An optional `conversation=<k>` selects an earlier range. `GET /api/cards/{id}/planner/conversations` lists the earlier conversations that have rows, each with `ordinal`, `started_at_ms` and `ended_at_ms`.
- **Writes.** Sends, queue edits and steering reach only the current session. Replace and edit look up turns on the current thread only (`crates/calm-server/src/harness/run_loop/replace_command.rs:56-93`), so a turn from an earlier conversation returns `planner_turn_not_replaceable`. Transcript writers are fenced (§10).
- **Wakes.** The dispatcher resolves the Track's Planner card, then that card's active session (`dispatcher/mod.rs:1267-1300`, `:1422-1442`). After the cutover that is only the new session. Running tasks continue; their results, gate outcomes and deliveries wake the new conversation. A superseded session is never recovered, hibernated, woken or pushed to.

## 9. UI (`fe/`, following `fe/AGENTS.md` layering and the design system)

- **Entry.** Add a `DropdownMenuItem` labelled "New conversation" to the Planner row's existing actions menu. In `ContextConversationItem` (`fe/web/src/features/chat/list/public.tsx:155-167`), that menu is currently offered only for `track-assistant`. Mobile history reuses the `ConversationArchiveAction` `DropdownMenu` with the `more` icon (`:104-121`).
- **Confirmation.** Use the existing `ConfirmDialog` (`fe/web/src/ui/dialog/public.tsx`) with the text "Start a new Planner conversation? This conversation becomes read-only history. The Report, tasks and running work continue." and the buttons "Start new conversation" and "Cancel".
- **Busy and error states.** The item is disabled while its request is pending. A 409 shows one `role="alert"` line, as archive errors do. The working, queued and sending states also disable the item, but only as a hint; the server decides.
- **On success.** `registry.requestOpen(plannerCard.id)` shows the existing empty state and focuses the composer.
- **History rows.** Earlier conversations are read-only rows in the existing collapsed Archived group of `ChatList` and in mobile history: "Earlier Planner conversation · <date>". In `core/domain` they have kind `planner-history` and key `${cardId}#${ordinal}`.
- **Reading history.** Opening a history row renders the same thread view from `harness/items?conversation=k`. Where the composer would be, an `archiveNotice`-styled notice says "This is an earlier Planner conversation. It is read-only.", following the pattern at `fe/web/src/app/router/public.tsx:790`. There is no restore action, and no run or queue query is made.
- **Reuse.** Motion uses `ui/motion/spring.ts` for list and dialog presence. No new tokens or icons.
- **Code placement.** New logic goes in small owner modules: the operation in `core/domain`, the row and notice in `features/chat/list`, the wiring in `app/conversations`. `app/router/public.tsx` (1650 lines) only gains calls.

## 10. Commit 291227e: are transcript writes fenced by the current session?

**Conclusion: the same class of risk exists here, so an equivalent but narrow fix is needed.**

On main, every transcript mutation is keyed by card, not by the writing session:

- `insert_transcript_row` is a plain `INSERT … VALUES` (`crates/calm-truth/src/db/sqlite/transcript_projection.rs:9-41`);
- projection upgrade and delete match `card_id + client_id` (`crates/calm-truth/src/db/sqlite/out_of_domain.rs:551`, `:584`, `:16`);
- the thread-suffix delete matches card and thread (`:76`);
- the turn-outcome put inserts without a check (`:507`);
- receipt `restore` deletes by session, card and thread (`transcript_projection.rs:43-110`).

This design keeps both conversations on one card. A late write from the old run loop could therefore alter the read-only history: an echo upgrade (`run_loop.rs:2575`), a cleanup delete, or a provider-history outcome backfill. The key-based upgrade and delete are card-wide. The quiesce in §3 removes the ordinary writers, but the issue requires revoking authority even for work admitted before the cutover, and only a database predicate does that regardless of timing.

The fix ports only the predicate from 291227e, not its handoff machinery. One shared `CURRENT_SESSION_BINDING` gates the six primitives:

```sql
cards.session_id = ws.id AND ws.card_id = card AND ws.track_id = track
  AND ws.state <> 'superseded'
```

Upgrade and delete gain `worker_session_id` and `track_id` parameters. A non-current write affects 0 rows and is logged at debug level. A current failed owner may still settle. This also closes the same latent gap for restart; the existing tests in `planner_restart.rs` and `planner_transcript_projection.rs` must stay green.

The writers #2843 added reuse these primitives, so they carry the same predicate: the canonical native item writer (`observe_native_item_tx`, `crates/calm-truth/src/db/sqlite/conversation/presentation.rs`) inserts through `insert_transcript_row` for a carrier it loads as the card's current session in the same transaction; the canonical content observer upgrades projections through the fenced upgrade; and receipt restore stays the fenced `restore`.

## 11. Focused tests (production entry points) and mutation verification

New cases go in `crates/calm-server/tests/cases/planner_new_conversation.rs`, registered once in the existing case list, plus unit tests at the owners. They use the real route, operation and MCP transport with fake providers and no real Codex.

1. **Admission.** Each refusal reason in §3 step 6 is refused. So are a non-user actor, wrong-track access and a missing key. After every refusal, the data image and the old live owner are unchanged.
2. **Stale session and cached credentials.** After the cutover, these are refused: the old Codex thread, the old Claude or OpenCode token, and an old card-bound connection that is still open. A Planner MCP write (task create, report write) is paused at a seam after identity resolution and resumed after the commit; it is refused, with no row and no event.
3. **Cutover races.** A send either lands before the cutover (and causes a refusal) or lands on the new session. A dispatcher push racing the quiesce is neither lost nor doubled. Two concurrent requests with different keys give one cutover and one 409. A changed `expected_session_id` fails the CAS.
4. **Queue and watermark transfer.** System observations and the watermark move exactly once, and the old row ends empty and stamped. An event committed while the new row is `starting` arrives exactly once. If that case is red, the fix belongs in the shared install path.
5. **Preserved identities.** A data image of every table outside the allowlist (`worker_sessions`, `cards`, `tracks`, `planner_conversation_starts`, `operations`, append-only `events`) is byte-identical before and after. Tables added later are covered without edits. The forge-approval lookup for the identical call is still Approved. A running task's result wakes only the new session.
6. **Failure and boot recovery.** A spawn that fails after the commit runs no `restore_old_runtime`; the old session stays superseded; the new session is a failed carrier holding the moved queue. Restart then recovers the new conversation in place. A crash after `TxCommitted`, followed by boot, reaches the same result.
7. **Lost-response idempotency.** A replay after success returns the same body, with no second floor, session or quiesce. Different content under the same key returns 409. A replay while the first request is in flight joins it.
8. **History.** Reads default to the new conversation, and `conversation=k` returns the old rows. Empty conversations are not listed. Replace or edit of an old turn is refused. Each fenced primitive drops a write when the session is superseded or repointed, or the card or track is wrong.
9. **FE.** Vitest covers the operation schema, menu gating and history-row derivation. The browser test covers the menu, the confirmation, the empty pane, opening a history row read-only, and the 409 alert at desktop and 390px widths. Run the credential-free Tier 1 E2E.

**Single-factor mutations** (production code only, each with the complete red set predicted across `planner_new_conversation` and the owner unit module):

| Mutation | Predicted red |
| --- | --- |
| Remove the `is_active_authority` check (`decision_gate.rs:85`) | Test 2, paused write |
| Remove the supersede from the cutover | Test 2, stale credentials |
| Remove the `expected_session_id` CAS | Test 3, CAS |
| Remove the user-actor check | Test 1, non-user |
| Move the replay lookup after admission | Test 7, no second quiesce |
| Plan `restore_old_runtime` again | Test 6 |
| Remove `CURRENT_SESSION_BINDING` from upgrade | Test 8, fence |

## 12. Expected change set and size

| Area | Files | Approx. lines |
| --- | --- | --- |
| Migration and head-schema inventory | 2 | 15 |
| Route (new file), router and OpenAPI registration | 3 | 220 |
| Start adapter: `new_conversation` admission, floor and compensation, in a new submodule beside `input_transfer` | 2 | 180 |
| Transcript reads (floor, list) | 3 | 90 |
| Transcript write fence (6 primitives and their callers) | 6 | 120 |
| Rust tests (case file and owner unit tests) | 3 | 700 |
| FE operation, row and notice, app wiring, tests | 7 | 450 |
| `openapi.json` and `wire.ts` from the real generator | 2 | generated |

Every hand-written file stays at or below 800 lines. The oversized existing files (the start adapter at 3400 lines, `planner_cards.rs` at 903, `router/public.tsx` at 1650) gain only calls into new modules. Each new check runs once per CI workflow, inside the existing nextest, Vitest and browser jobs. A later ordinary change edits only its owner: a table the cutover should write joins the test-5 allowlist, and any new transcript primitive must use `CURRENT_SESSION_BINDING`.
