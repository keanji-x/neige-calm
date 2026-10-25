# Child Track handback

Outcome: a child Planner can return an unmet assignment without closing its Track.
This is L2: it changes parent/child authority and transactional task admission.
No migration, new state, continue operation, or reopen-policy change is needed.

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
attempt_id; the child title uses the parent task key read from the task row.
Planner guidance teaches self-close only for a met child goal; unmet goals,
including budget cutoff, hand back. The parent decides whether to continue its
work or close the handed-back child. There is no child continue API.

## Verification and maintenance

Production MCP, scheduler and claim entry tests cover authority, result fields,
active-work refusal, pending admission, seed/title, and event atomicity. New
tests include child_track in their names for gate replay. The focused red/green
run precedes implementation. A single-factor production claim-guard mutation
predicts the complete red set, runs the full relevant target/filter, then is
safely restored and confirmed green.

The worker runs focused tests and formatter fixes. The kernel gate runs quick,
text and contract checks once plus the intended child_track replay. L2 review
uses two independent channels arranged by the Planner. Adding ordinary tests or
variants needs no permanent name list; a new tool updates the existing registry
and generated golden through its generator. No new per-CI check is introduced.
