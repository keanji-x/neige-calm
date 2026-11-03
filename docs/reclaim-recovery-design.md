# Reclaim recovery design

## Outcome and boundaries

Task-bound operation startup may be unable to acquire a launch fence, a
location fence, or the global operation driver while a workspace reclaim is
live.  That condition is recoverable.  The scheduler must leave the task
`dispatched`, preserve its operation id, idempotency key, payload hash and
resource reservation, and return from that scheduling turn.  It must not turn
the condition into `failed` and must not keep the Track lock or scheduler
permit while waiting.

The operation owner records a versioned blocker in a `pending` operation's
existing `phase_detail_json`.  The detail contains resource facts (blocker
class, owning scope, resource identity and retry condition), not rendered user
copy.  A retry claims that same pending operation without clearing its blocker;
successful preparation clears the detail atomically with its phase transition.
Missing detail is an old ordinary pending operation.  Malformed detail fails
closed.

Only task-bound startup consumes this deferred result.  Invocation-specific
adapters continue to retain their live caller authority in
`execute_with_adapter`; no generic background retry reconstructs a Human or
retained invocation source.

## Bounded drive and recovery

The scheduler uses one exact-operation entry point: insert/deduplicate, try the
global drive fence once, drive/observe that operation, then return either a
settled result, an owned parked handoff, or the persisted blocker.  Every
scheduler retry first tries the ordinary scheduler permit without waiting. Inline
steady recovery returns when capacity is unavailable; boot's existing detached
owner retains its single-flight guard while awaiting capacity. A resource blocker
still returns the permit. Every retry repeats the task/attempt admission and
operation claim checks. Existing keyed replay,
single-flight task guards, attempt-startable checks, and result CAS remain the
authority for one effect.

The current workspace retirement contract (#3495) supersedes online cleanup of
unconfined resources. New retirement and unadmitted historical reclaim never
run Git preflight, admit a deletion, or install a background cleanup observer.
They retain the authoritative provision's deferred retirement reservation.

Already admitted legacy executions retain their original identity and removal
protection. The existing parked owner performs short drive/drain/settlement
probes and reconciles durable successful completion; scans do not wait for
physical cleanup. Unknown outcomes, failed waits and failed settlement do not
release a removal reservation. No new owner or scheduler reconstructs a
physical cleanup capability from a retired path or an explicit null link.

## Location protection

Retired targets remain durably unavailable for reassignment through the workspace
owner's existing retirement inventory. They are excluded from execution-owner
inventory and do not become global terminal blockers. Admission to a retired
root is a final, actionable rejection rather than a recoverable Pending blocker.
An explicit null reclaim link contributes only through the authoritative
provision receipt, never through a reconstructed destructive effect.

A legacy effect whose Track and path decode but whose link, consumer, or settlement
record is corrupt remains an indeterminate removal at that path.  Admission is
refused only for overlapping location consumers; global terminal admission
remains fail-closed because terminals are not filesystem-isolated.  If the
target identity cannot be decoded, its scope is unknown and admission is
conservatively refused.  For execution roots, a corrupt root already selected
by the requested owning scope does not broaden that known occupancy; corrupt
roots outside the owning scope remain fail-closed.  Settled historical records
do not regain protection.

## Diagnostics and verification

`TaskRecoveryView.blocking_reason` projects a dispatched task's structured
pending blocker, naming the operation/resource source and the durable condition
that must settle.  Clearing the real blocker resumes the original
task/operation/key automatically.

Regression tests use the `reclaim_recovery_` prefix and exercise real scheduler,
terminal, legacy reclaim, retirement, boot/steady recovery and persistence entry points. They
cover launch/global-drive contention, durable removal, ordinary/typed/Pipe and
default/explicit cwd protection, restart/deferred-null-link/Unknown/stop/settlement
failure, damaged-record scope, late closed/deleted attempts, forward progress
with one permit, and exactly-once identity after release.  Production
single-factor mutations must make the predicted complete observer set red,
then the restored tree must pass the same selection.

This is an L2 change: it crosses authority, persistence and isolation
boundaries.  Rollback is the whole change; partially reverting blocker
projection, bounded driving, reclaim ownership or durable inventory would
violate the combined contract.
