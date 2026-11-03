# Close a Track and its descendants (#3464)

Review tier: L2, because cancellation crosses authorization, persistence,
runtime isolation and workspace lease boundaries.

The outcome is a stopped, closed subtree. REST and MCP retain their existing
direct-target authorization, then call one server close transaction. Only
`tracks.parent_track_id` grants descendant membership; creator provenance does
not. The existing bounded tree queries validate depth, ancestry and area before
any change. Closed intermediate nodes remain members. Invalid or oversized trees
fail atomically rather than completing a truncated walk.

The writer transaction closes all members, cancels pending tasks and safely
cancelable workers through the existing cancellation owner, and cancels active
sub-track assignments, including the selected child's external parent binding.
Unsupported inflight tasks refuse the entire transaction. Child-assignment gates
use the existing Pipe disposal decision and execution driver, preserving the
observer's lease until its original owner settles. Existing worker
timeout cleanup markers retain their attempt identity and lease ordering.
Planner cleanup obligations are recorded in a dedicated table, keyed by the exact
session and close request. A live snapshot rewrite cannot erase that obligation.
Completion, handback and gate compare-and-set guards must continue
to reject canceled attempts. Explicit child success remains a separate outcome
from user cancellation and uses its original writer.

After commit the existing scheduler worker cleanup and Planner lifecycle owners
are triggered. Planner cleanup takes the card/start, operation and track fences,
revalidates the exact session and marker, then releases SQLite before stopping
the harness/backend. Only confirmed shutdown clears the obligation. Failure is
retryable by the regular sweep. MCP admission remains held through response
flush so asynchronous self-close cleanup cannot interrupt its own response.
Closed tracks and sessions with pending cleanup cannot restart or derive work;
reopening does not restore canceled assignments.

Verification starts with minimal REST and authorized own-track MCP reproductions,
then expands within the supplied focused selector to subtree boundaries, closed
intermediates, rollback, late results and lifecycle races. Load-bearing scope or
late-result guards receive single-factor production mutations with complete
predicted and actual failure sets. The kernel gate owns text, Rust quick, full
contracts, focused replay and Tier 1; CI owns broad suites. The Planner arranges
the two independent formal review channels.

Maintenance cost: one new cleanup-table migration; no frozen migration changes,
new policy registry, test-name allowlist or duplicate cancellation implementation.
New behavior tests live in the existing test modules and run once in the focused
gate replay and once in the CI nextest partition that owns them. The existing
head-schema filename inventory also gains the migration name; adding a later
migration still edits its SQL file and that fixture, and its completeness check
runs once in CI nextest. There is no additional CI check or repetition. Ordinary
new tasks and track variants require no update to a close-specific list.
The existing MCP registry golden changes only the close description hash. A later
description edit changes its prompt Markdown and regenerates that golden through
the registry's existing generator; the registry check still runs once in CI.

## Persistence and shutdown evidence

The historical run loop serializes a complete snapshot in
[`persist_guarded_snapshot`](../../crates/calm-server/src/harness/run_loop.rs),
then calls `session_set_handle_state_tx`. Both
[`session_set_handle_state_mirror_tx`](../../crates/calm-truth/src/db/sqlite/session_mirror.rs)
and the harvest writer in
[`session_projection`](../../crates/calm-truth/src/db/sqlite/session_projection.rs)
assign the whole JSON value. A close-only field added to that JSON could therefore
disappear during an ordinary active turn. The existing
[`hibernation owner`](../../crates/calm-server/src/planner_hibernation.rs) requires
an idle session, no active turn and an idle snapshot; it cannot represent a
committed request to stop an active Planner. Worker timeout markers belong to
worker attempt and lease cleanup, rather than Planner snapshot ownership.

[`0213_track_close_cleanup.sql`](../../crates/calm-truth/migrations/0213_track_close_cleanup.sql)
keeps the obligation independent of these snapshot writers. The close transaction
records the current Planner's exact session, card, track and request timestamp.
Child assignment cancellation is bound separately to its task id and validated
`child_track_id`/`parent_track_id` edge in that same transaction. Worker admission
and timeout cleanup share
[`session_owns_attempt_tx`](../../crates/calm-truth/src/worker_binding.rs): the
spawn operation's idempotency key, or the existing legacy latest-attempt rule,
supplies attempt ownership. No process identity is reconstructed.
The factory's stop-binding check is also shared by close admission and timeout
cleanup replay: a live Codex worker needs its recorded native thread, and a live
Claude worker needs its exact terminal. Missing stop authority refuses close;
if discovered during replay, the marker and lease remain retryable.

[`close_cleanup`](../../crates/calm-server/src/planner_hibernation/close_cleanup.rs)
rechecks all four obligation fields under the existing start and lifecycle
fences. It calls the installed harness's strict shutdown, joins its run loop,
and retires only that same registry handle. A registry miss uses the exact
persisted session through `PlannerFactory` and the registered provider Stop
owner. The generic best-effort replay path cannot retire this obligation after
an unconfirmed interrupt. Stop errors and registry replacement retain the row;
the regular terminal sweep retries it. Only confirmed shutdown permits deletion
with all four fields as a compare-and-set guard and completion of that exact
session. Neither lookup nor completion follows the card to a successor session.

The tool admission guard survives through the Unix socket response flush. Cleanup
waits for that admission to drain asynchronously before it takes lifecycle locks,
so an MCP self-close receives its result before its own harness stops. The short
SQLite revalidation transaction ends before native Stop or run-loop joining.
The worker cleanup owner likewise confirms Stop before clearing its exact
attempt marker and releasing its lease; the general settled-worker sweep skips
sessions carrying that marker.

Historical native recovery rechecks the same start fence in
[`HistoricalRecovery::validate_tx`](../../crates/calm-server/src/harness/backend/factory/recovery.rs)
before read/resume RPCs. Canonical activation and issuance validate it in their
existing admission transactions. A Human input ACK records a durable message;
it grants no startup authority. The lifecycle regression waits for the existing
canonical wake completion boundary before checking that no native thread or
turn started. A referenced child's reopen remains refused; parent reopen does
not replay canceled child creation or continuation.

The lifecycle regressions exercise response-flush ordering, failed-Stop retry,
successor-session isolation, a SQLite write during blocked native Stop, and a
real worker process whose lease remains held until cleanup confirms shutdown.
The behavioral evidence and mutation sets belong in the delivery result, not in
a permanent test-name registry.
