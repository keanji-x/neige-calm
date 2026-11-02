# Durable progress obligations: child bootstrap pilot

Review tier: L2. Adds persistent responsibility and an atomic notification path.
Implementation base: current main `32d01c38`.

## Outcome and scope

A child task's dispatch-to-running handoff has a durable obligation. At 15 minutes
without completing that handoff, persist one parent Planner notice and expose
`child-track-bootstrap-overdue` on the task. Expiry is advisory; it grants no
permission to cancel, recreate or restart the child. Existing canonical activation,
retained-child continuation and original-operation recovery remain authoritative.
Running Planner failure and explicit external waits are outside this first migration.

## Existing owners and the concrete gap

Main already has canonical durable event acceptance and historical prefix catch-up.
The pilot reuses both through the existing Planner ingress. It does not change their
protocol, filtering, recovery fences, event decoder or duplicate suppression.

The worker quiet detector requires a running task with an observable terminal and
excludes child tasks. Provider execution ownership grants session stop/release
rights for native operations and has no task-handoff deadline. Parked operation
deadlines cover individual operation resources, not the whole claim, child creation
and activation handoff. None supplies this obligation, so the new primitive belongs
beside task persistence rather than inside a provider or renderer.

## Shared primitive and task ownership

The primitive stores execution ID, purpose, immutable deadline and the one-shot
pending/notified/released state. It owns enrollment idempotency and expiry CAS;
callers own relevance and the action. Execution IDs name concrete attempts.
The task owner selects obsolete execution IDs and calls the shared release operation
in the same writer transaction; lifecycle state writes stay in the primitive.
Repeated scans cannot renew or rearm the same obligation. No callback registry,
heartbeat renewal or new application identity configuration is introduced.

Task claim enrolls the child purpose in its existing transaction. Reverse discovery
starts from current dispatched child tasks, repairs missing obligations once using
the stored updated time, and releases obsolete attempts. New claims use their exact
claim time; repair of pre-upgrade records is a one-time historical approximation.

Internal repair/release uses the existing no-event transaction helper. Notification
rechecks current allocation, route, status and parent existence before atomically
committing CAS, the task detail and the existing wake event. A crash between the two
phases leaves pending work for the next scan. All writes use the scoped writer lane;
helpers borrow the owner's current transaction. Reads use the read pool.

Every successful handoff branch uses the existing running stamp and releases its
obligation there. Explicit bootstrap failure releases in the failure transaction.
Cancellation, deletion and supersession are repaired by the next scan; a current
attempt check prevents them from committing a new stale notice.

## Independent supervision and existing delivery

One weak-owner timer scans durable obligations independently of scheduling, context
recovery and native starts. Delivery retries group committed event IDs by parent
Track. They try the existing per-Track push lock before spawning one background
attempt; a busy Track stays due. The background attempt keeps that same lock and
calls the authoritative under-lock ingress, including canonical acceptance and
historical wake/ledger ownership. A parent start fence therefore cannot block the
next durable scan or another Track, and repeated ticks cannot accumulate deliveries
for one blocked Track. No second inflight map or delivery ledger is needed.

The existing bounded event reader decodes the stored event by exact ID. A notification
is retried under the same ID until the handoff leaves scope; notified means committed,
not that the parent has completed recovery. The default event pruner preserves wake
events. Arbitrary manual history deletion and an unavailable parent are not repaired
by this pilot.

## Acceptance and permanent cost

Tests exercise real claim/sweep, rollback, missing enrollment, no renewal, completion,
cancellation and replaced attempts. Historical ingress tests hold the real push lock
while a second Track becomes overdue. Canonical tests drop the live listener and
verify one actual provider delivery through the existing acceptance owner.

The two primitive invariant tests join the existing contract selector by name. There
is no new list of files, variants or test names to maintain. A later purpose adds its
own lifecycle enrollment, relevance and notification policy while reusing the four
primitive operations. New lifecycle code stays in small modules; existing fixture
setup is reused instead of duplicated. Isolated review directories follow the
user-provided requirement for separate review worktrees.

Validation requires a red production regression, focused consumer tests, a single-
factor reverse-discovery mutation with the complete predicted red set, text gates,
quick Rust gates and full contract gates. Tier 1 stack tests cover the integrated
server path. Two independent review channels check all five architecture priorities.
