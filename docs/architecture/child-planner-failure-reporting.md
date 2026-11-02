# Running child Planner failure reporting

Review tier: L2, because failure-episode identity and parent notification cross a
persistence boundary. This extends the [bootstrap pilot](progress-obligations-pilot.md)
without introducing another recovery owner. Conversation recovery remains #3372.

## Outcome and ownership

A current Running sub-track task whose authoritative current Planner session is
Failed produces one durable parent observation per task attempt, session and
failure episode. Idle/completed turns and inactivity do not qualify. Notification
neither changes task status nor restarts, closes, cancels or replays child work.

The task owner requires an open parent and child, the child's parent relation,
`current_tasks` allocation, child's root session, matching card/session/track links,
exactly one Planner role and the existing `PlannerBinding` provider/session contract.
Qualification reads only typed session contract and owned identity facts; it does
not decode opaque provider snapshots, whose corruption must not hide a confirmed
failure or suppress other children. Qualification and the obligation's
pending-to-notified CAS share the transaction
that persists the existing `TrackWakeRequested` event.

## Stable failure identity

Session IDs can survive explicit system-error recovery. Mutable timestamps and
snapshots change without a new failure, and some failure writers emit no event.
The internal SQLite `failure_epoch` increments on each nonfailed-to-failed state
transition, atomically with that transition. Migration backfills currently Failed
rows to one; inserts already Failed start at one. Repeated Failed/snapshot writes
do not increment. Rollback also restores the epoch. The metadata stays internal:
no session wire model, provider convention or failure/recovery policy is added.

The obligation execution identity is the JSON tuple of task attempt ID, session ID
and epoch. A new episode gets a fresh identity; released identities never rearm.
The new purpose is already due once failure is observed: zero is an immutable due
sentinel, not an invented failure timestamp.

## Supervision, delivery and historical facts

Existing supervision reverse-discovers current failures, repairs missing enrollment
and releases retired task/session/failure identities. Closure or temporary binding
unavailability suppresses enrollment and delivery but does not retire an existing
obligation; reopening or repairing the binding resumes the same identity. Live Failed-status/Wedged-phase events accelerate
the same scan before scheduler permits; durable state remains authoritative.
Eventless writes, dropped events and restart converge at the next existing scan
(default five minutes with a healthy loop), without an inactivity window. Failure
that has already recovered before observation is outside this current-failure
contract; the epoch counter is not a historical failure journal.

The existing per-Track push lock, durable ingress and same event ID own delivery.
Active watchdog retries stop after recovery, closure, supersession or rebinding.
A committed notification remains a factual historical observation and can still
arrive through existing event-prefix catch-up after recovery or Planner replacement.
Its text gives exact identities and observation transaction time, says state may have
changed and requires rereading current facts. It carries no recovery authority.
No ingress protocol, decoder, filter, watermark or duplicate-suppression policy changes.

## Acceptance and maintenance cost

Production-entry tests cover eventless failure discovery and scheduler reconstruction,
current binding/allocation negatives, normal Idle, transaction rollback, duplicate
suppression, same-session recovery/refailure and temporarily held parent ingress.
The migration test upgrades a real released file database through normal startup
and checks insertion, state changes, rollback and unrelated writes.

Generic session state writers need no new registration: database transition metadata
captures their writes. New reporting purposes still provide owner-local enrollment,
relevance and presentation over the same one-shot primitive. The failure observer
adds no bus, callback list, lifecycle controller or CI inventory. Notification text
contains Neige identities and observed state, without provider errors or snapshots.
