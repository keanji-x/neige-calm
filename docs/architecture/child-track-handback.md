# Child Track handback

Outcome: a child Planner can return an unmet assignment without closing its Track.
This is L2: it changes parent/child authority and transactional task admission.
Continuation reuses the child-track operation; no new API, recovery generation,
replacement task, or reopen-policy change is needed.

## Ownership and transaction

`neige_track_handback({blocked_on})` is a Planner tool listed only on child
Tracks. `blocked_on` is a nonempty human explanation, not a machine protocol.
The handler resolves the caller and uses the existing event write transaction.
In that transaction it checks recorder admission, the live parent edge and an
active (`dispatched` or `running`) parent task bound to this child. It rejects
inflight child work using `RUNNING_WORK_STATUSES_SQL` with -32409. Pending work
may remain. A guarded UPDATE flips the bound parent attempt to failed, and the
same transaction emits kernel TaskFailed to the parent (not a Planner verdict). It leaves child.closed_at NULL.
Concurrent scheduler/handback calls serialize at the writer and only one
transition emits a terminal event. No free-form reason string is parsed.

## Result reference

One child-result helper reads the report card and its CRDT document revision,
and the latest immutable task_candidates row (created_at_ms, candidate_id tie
break), inside the writer transaction. Both scheduler success and handback use:

```
{source:'child-track', outcome:'met'|'handed_back', child_track_id,
 report:{card_id,rev}, candidate:null|<complete task_candidates row>, blocked_on?}
```

TaskFailed.details is this object itself. TaskCompleted.result uses the same
shape with outcome=met. The Planner cannot supply candidate/report pointers.
Report revision comes from the report document reader, not a guessed payload
field. Candidate serialization belongs to the existing candidate owner.
The existing failure receipt points to the exact execution record; the parent
reads events.failed.payload.details there, without parsing the reason.

## Scheduling and close authority

The authoritative pending claim SQL admits a child task only while its Track
has an active parent binding; root Tracks retain normal admission. This fences
pending work after handback and also closes the race between a scheduling read
and its claim. Parent close authority is checked in the close transaction: a
parent can close its own child once no active parent binding remains, without
Area admin. Existing authority for non-parent callers remains in force.

Seed observations include a Parent section with parent_track_id, task key and
attempt_id; at creation the child title uses the parent task key read from the task row.
Planner guidance teaches self-close only for a met child goal; unmet goals,
including budget cutoff, hand back. After `handed_back`, the parent either
declares a **new task key** with
`spawn:"sub-track", child_track:{"continue":"<child_track_id>"}` or closes the
returned child. `continue` is exclusive with `provider`, `model`, and
`reasoning_effort`; the existing Planner configuration remains authoritative.
There is no child continue API.

Declaration projection includes eligible returned children in its existing single-statement
snapshot. Admission requires the live parent edge, an open child, an explicit
`failed/child-track-handed-back` assignment, no active parent binding, and no
inflight child work. The existing inflight status set is owned by truth task
lifecycle and re-exported by Planner hibernation, so close/handback/continue
share it. All settled bindings must be handbacks: continuation does
not recover some other failure or reopen a completed assignment. It uses the
existing `invalid_declaration` diagnostic instead of another diagnostic vocabulary.

`ChildTrackAdapter::create_tx` branches before creation-only depth/member admission,
parent model selection, HEAD resolution, workspace materialization, Track creation
and initialization. The writer repeats dynamic admission and requires a new key.
Creation and continuation call `task_bind_child_track_tx`; its guarded UPDATE checks
the live edge and open state, and independently uses `NOT EXISTS` over other
`dispatched/running` bindings. SQLite's writer transaction serializes competing
assignments. Migration 0195 replaces the old all-history unique child index with
a nonunique lookup index, allowing historical bindings to remain. Active
exclusivity stays in the one production binding writer under SQLite's serialized
transaction; no additional partial unique index is introduced. The lookup index
still serves historical-reference reads. Released migrations remain unchanged.

In the same transaction, continuation appends a child-scoped kernel
`TrackWakeRequested` with `source:"parent_task"`, `key:task_id` and
`text:render_child_seed(new_payload,new_key)`. Failure to append rolls back the
binding. The ordinary mail/objective wake channel (Dispatcher durable catch-up,
canonical System input, retained Planner wake) delivers it exactly once. Scheduler
marks the new parent task running without `planner-harness-start`; operation replay
returns the recorded result without another wake. The output carries neither a
parent HEAD nor a workspace creation plan. Child checkout, uncommitted files,
Report, model selection and Planner session remain in place. Closing the child
settles the new binding through the existing scheduler success path.

## Verification and maintenance

Production MCP, scheduler and claim entry tests cover authority, result fields,
active-work refusal, pending admission, seed/title, and event atomicity. New
tests include child_track in their names for gate replay. The focused red/green
run precedes implementation. Single-factor production claim-guard and binding-guard mutations
predict the complete red set, run the full relevant target/filter, then are
safely restored and confirmed green.

The worker runs focused tests and formatter fixes. The kernel gate runs quick,
text and contract checks once plus the intended child_track replay. L2 review
uses two independent channels arranged by the Planner. Adding ordinary tests or
variants needs no permanent name list; a new tool updates the existing registry
and generated golden through its generator. No new per-CI check is introduced.

The published Report kind schema and types declaration validator both describe
continuation; their existing parity test covers acceptance and exclusion. The
permanent transactional checks live in truth's
shared candidate SQL/binding writer and the adapter/scheduler branch. Ordinary
new tests join the existing `child_track` gate filter automatically; adding or
renaming one edits only its test module (and normal module registration for a
new file). A new migration updates the existing head-schema inventory fixture;
no additional migration inventory is introduced. No test-name pin or new CI step is introduced. Declaration shape changes keep
the published Report schema and validator in sync and run the existing
`npm run gen:api` generator for any affected wire artifacts;
changing Planner guidance regenerates its existing prompt golden, and a tool
description change regenerates the existing MCP registry golden. Text and
contract gates keep their existing once-per-gate-run placement; the focused
worker tests are replayed once by the kernel gate. In CI, each test runs in
its assigned partition of the existing eight-way nextest suite; continuation
adds no second CI invocation.
