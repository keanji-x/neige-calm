# Close a Track and its descendants (#3464)

Review tier: L2, because cancellation crosses authorization, persistence,
runtime isolation and workspace lease boundaries.

## Completion owner correction after the complete 6b reviews

Installed cleanup only queried a qualified in-memory END, while detached
shutdown additionally recovered the original END. Source loss downgrades native
correlation, so a still-installed canonical owner could not converge even after
explicit reconnection to its original peer.

Close completion has one sequence under its exact four-field cleanup witness:
freeze and validate the original carrier; confirm physical Stop; recover any
required original END; consume it through the original input transition owner;
retire that exact handle and cleanup obligation. Registry residency selects only
physical control and the live provider observer. It must not select whether an
END may be recovered. PlannerFactory owns provider-specific factual recovery for
both callers, while input_dispatch retains the sole canonical END consumer.
ACP continues to use its immutable receipt owner, Claude its existing observer,
and Codex its accepted-native history reconciliation. No provider state
classifier or cancellation framework is duplicated.

Close observes an already-ready client without reconnecting or replaying work.
It retains the existing ready-history transition/replay coordination contract,
including source/activity, original Accepted proof/backend and cleanup admission
revalidation inside the writer transaction. Ordinary explicit recovery is
unchanged. Only an exact stopped hint may recover its lost association from the
original Accepted proof and full terminal history; this grants no live control.

The installed regression loses its Accepted turn's source, explicitly reconnects
to the same peer, and closes/retries without a new END notification. It then
reopens and sends one new Human input. Wrong, Unknown and incomplete original
history and source/activity replacement retain the obligation. The load-bearing
stopped-association assertion
uses production mutation evidence; broader controls remain in existing owners.

Known performance limitation: same-backend END history observations remain
serialized by the existing recovery coordination. A slow history read delays
other such observations until it returns or reaches the protocol's default
30-second request deadline. Independent history observation is deferred; this
change does not claim that a second cleanup can finish while that read is held.

Maintenance stays in the existing completion, native history and input owners.
A new provider edits its factory/observer and tests; a new test edits its owning
module and, when needed, Rust module registration. There is no central test list,
new durable field, migration or CI pass. Tests use their existing CI partition;
experiment inventories and review dispositions remain delivery artifacts.

## Stop owner follow-up after the d3bd reviews

The complete owner chain includes worker timeout cleanup, installed shutdown
and detached shutdown. Three gaps require production-entry counterexamples:
the worker scanner awaits each Stop while holding the global operation drive;
native strict Stop reaches generic connection recovery before its bounded END
read; a restarted native owner has no local slot for a historical first start
whose delivery is Unknown, and mistakes that cache miss for physical Stop.
The eighteen prior dispositions and the original source/END fences still apply.

Strict maintenance uses the native owner's connected-source confirmation entry;
ordinary shutdown retains the existing warning-only interrupt/recovery entry.
Only the confirmation entry can retire a cleanup obligation. Strict maintenance
must sample one already-connected native source, control
only the selected thread's original activity and read facts on that same source.
Neither fallback interrupt nor liveness observation may reconnect or replay.
Ordinary explicit recovery retains its existing admission and replay contract.
An empty local cache is unknown, including after hot takeover. Stop requires
an acknowledgement for the selected activity, trustworthy original-thread idle
or not-loaded evidence, or confirmed process absence. A terminal historical turn
unrelated to the selected execution is insufficient. Stop never supplies the
separate immutable original END required by a current canonical execution.

Worker maintenance must schedule independent resources independently. Its short
driver/writer checkpoint freezes the exact session, task, marker and terminal;
the network/process wait cannot retain the global drive. Replacement protection
must remain in the existing resource owner: native source/turn generation and
Legacy terminal reuse need explicit fences through Stop and the final marker
CAS. The existing durable worker cleanup marker is that resource fence: Legacy
restart checks it before replacing a session, and terminal launch preparation
checks it before clearing exit state or starting a new generation. The driver
still serializes these short checkpoints with cleanup's snapshot. UI attachment
already uses the terminal launch owner's attach-only contract for worker-owned
terminals; a genuinely unbound or fresh launch must also check the same marker.
A busy resource refuses without waiting under drive. No second renderer lock
or cancellation framework is needed. Cleanup serializes concurrent passes by card,
then polls independent cards concurrently, preserving synchronous sweep completion.
It still releases only the ended attempt's lease after confirmed Stop and exact
CAS. A cleanup with no lease event must commit its metadata write through the
existing optional-event transaction pattern; the generic eventized writer keeps
its nonempty-event contract.

Verification first drives real subtree Close with an unanswered worker interrupt,
real Close on a disconnected but reconnectable peer with unrelated cached
history, and real historical first-input Unknown followed by a new owner on the
same database and original peer. Known idle/notLoaded/absence and original,
wrong and unknown END are controls. Required mutation evidence and raw outcomes
are delivery artifacts, not permanent test inventories. Regressions remain in
the existing owner modules and CI partitions; no migration, provider identity
list, test-name registry or extra CI pass is added. Any resource-fence extension
must document its participating reuse callers and their ordinary edit cost.

## Review correction boundaries

The 9f initial reviews identified eighteen hypotheses across four owner
boundaries. Each needs source adjudication and a stable production-entry
counterexample before a behavioral correction. Review agreement is not a test
result; the final candidate still requires two fresh independent L2 reviews.

Authorization and cancellation inventory are checked before any effect commits.
REST must consume the current share capability in its write transaction. MCP
must preserve caller Area and child-return authority. Child bindings must be
unique; both directions of live card/session bindings and existing cleanup
identities must agree. Worker native identity and child gate metadata must belong
to the selected attempt. The existing gate inventory is authoritative for
worker gate activity: an unsafe active gate refuses the whole Close, without
adding another gate cancellation mechanism.

A closed Track's projection records its valid declarations as terminal canceled
rows in the same transaction. Close refreshes that projection for every member,
including declarations previously outside execution capacity. Later template,
Report and policy refreshes share the same rule, so Reopen cannot materialize an
old declaration. A new declaration after Reopen remains admissible. Mixed PATCH
events expose only the final committed root state and each changed descendant.
Template persistence, authorization and projection remain in their existing owners.

Admission drains the calls already accepted when Close commits. A live waiter
has priority over later calls; an abandoned waiter grants no lasting fence.
The original guards continue through response flush, write failure or task
cancellation. Cleanup retains the per-card and per-track fences while native
Stop runs, but must release the global operation drive lock first. Each session's
cleanup progresses independently, and ordinary hibernation must not bypass that
same reply fence.

Physical Stop and the original END remain separate facts. A false readiness bit
cannot prove process absence. Detached local providers must reuse their existing
execution and legacy-absence owners; cold END recovery must reuse immutable
receipt/proof qualification, without issuing prompts. Reopen inputs retain their
actual current source through the old cleanup and subsequent activation; neither
an ACK nor a durable input row reconstructs execution authority.

These are corrections to the existing server/lifecycle and provider owners, not
new policy in the generic truth row writer. The bounded tree and its one depth
constant remain shared. The cleanup table is still needed because live snapshots
are replaced in full. New behavior tests use existing modules and CI partitions;
no permanent list of test names, providers, or extra CI pass is introduced.

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
intermediates, rollback, late results and lifecycle races. Worker iteration owns
focused red/green, necessary single-factor production mutations, and formatter or
lint fixes. The kernel freezes one candidate and owns four steps: text gates,
Rust quick against the checkpoint's exact parent SHA, full contracts, and one
nonempty focused replay. CI owns broad suites and Tier 1. The Planner arranges
two fresh independent complete L2 reviews after behavioral corrections. Historical
P1 runs are provenance, not verification of a rebased dependency tree.

Cold cleanup observes only the accepted original execution. A shared four-field
Close witness revalidates the committed request and carrier at every native/write
boundary. Codex consumes an existing qualified terminal first, otherwise reads
only the exact turn from an already-connected peer through the existing END
reconciliation owner. Close does not reconnect, replay subscriptions, resume,
install or issue work to obtain that fact. ACP reuses its original canonical
receipt association and Completed recovery after confirmed Stop; Unknown or a
foreign receipt keeps the obligation. A reopened Human's live invocation retains
its actual source while the old cleanup is pending, then re-enters the original
retained activation owner with current authority and exact carrier checks.

The Close inventory checks every non-child attempt's existing gate inventory,
including terminal attempts whose independent worker gate may still run. This
cost is limited to explicit Close admission and uses the gate owner's indexed
attempt keys; ordinary MCP replies and native notifications do not scan it.
No second gate classifier or cancellation policy is installed in worker helpers.

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

[`0222_track_close_cleanup.sql`](../../crates/calm-truth/migrations/0222_track_close_cleanup.sql)
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

## Close, END and Reopen lifecycle contract (PR #3527)

Closing stops physical work; it does not terminate a resumable conversation.
In particular, cleanup must not call `session_complete_tx(Failed)` on an ordinary
Planner. That writes `completed_at_ms`, excludes the original carrier from both
historical recovery and canonical activation, and makes a later Human input ACK
look successful while no turn can run. A confirmed idle original remains an
active-authority idle carrier. Reopen and a new authorized Human source use the
original recovery/start owner and original thread. They never reissue a prior
attempt, weaken Failed admission, or restore canceled child assignments.

Native interrupt acknowledgement and execution END are different facts. An ACK
can confirm the Stop request, but cannot clear a canonical active claim, its
delivery receipt or retained interruption intent. The existing END transition
owner must qualify and consume the original immutable END. An unresolved claim
keeps its cleanup obligation; retry is factual observation, never prompt replay.
The original timeout END transition is also the only owner allowed to restore
an `interrupt_timeout` Failed carrier to idle. Other Failed states keep their
existing recovery requirements.

Close admission includes a current Failed Planner whose `completed_at_ms` is
still null: the real Stop deadline can wedge the carrier without proving the
native execution stopped. The exact session/card/track binding is validated
inside the close transaction. If no safe Stop identity exists, the entire close
rolls back. A canonical active claim without an addressable native turn also
refuses Close; Unknown input is retained rather than interpreted as no execution.
A later Stop error or identity change retains the existing obligation.

The transaction reports a newly recorded cleanup obligation separately from its
presentation events, through the required `ClosedSubtree.cleanup_requested`
field. REST and MCP commit that effect even when the Track was already closed
and emits no `TrackUpdated`. This covers both Planner and ended-worker cleanup.
In particular, an already-closed row left by the former Failed-selection gap
can acquire its missing Stop obligation. A repeated Close keeps its original
closed time and emits no duplicate Track update. Both entry points poke their
existing cleanup owners after the transaction; event presence is not a write
or scheduling authority.

These entry points compose the existing `write_in_tx_typed` transaction with
`append_actor_events_in_tx` for a nonempty batch and `broadcast_actor_events`
after successful commit, as existing mixed-effect owners already do. Event
authorization, history projection and rollback stay in their original helpers.
The ordinary eventized writers retain their nonempty-batch requirement.

For a current canonical execution, the factory also asks the existing
`original_input_target_tx` owner for the actual Turn-to-Initial association and
decodes its typed native scope. The selected Stop target must match that
original execution. A previous header's turn ID cannot address a new crossing
whose native turn is still unknown. Admission refuses that case atomically;
cleanup revalidates the same contract. A moved predecessor retains its own
persisted Stop identity and cannot consume the successor's input facts.

A detached Planner uses the Planner identity contract. Codex thread identity
comes from the existing `effective_runtime_thread_id`, including a valid
`snapshot.last_thread_id` when the inline column is missing or blank. Conflicting
inline/snapshot identity and a known foreign native owner must refuse Stop.
The persisted last turn remains a Stop fallback; Worker timeout conventions
must not silently treat a Planner with missing inline thread as already stopped.
Planner and Worker callers share a native Stop confirmation function with a
required thread argument. Its original daemon-down, NotLoaded and ended-turn
facts prove physical Stop, without granting canonical END or input admission.
Installed Codex shutdown uses that same confirmation owner after joining its
run loop and sampling the last turn under the existing issuance lock. An idle
registered harness can outlive its completed native turn: an old-turn interrupt
refusal must be checked against those native facts, rather than retaining an
obligation forever after the original END has already settled. A transport error
while the daemon remains Running, an unreadable thread, or facts showing only
active/unknown work still leave Stop unconfirmed. Best-effort shutdown replay
retains its warning-only raw interrupt contract and cannot clear a Close
obligation. Detached Close and worker timeout cleanup already use the strict
Stop confirmation owner. No caller infers END from Stop confirmation.
Claude and ACP continue through their exact-session provider Stop owners.

The field extraction behind `effective_runtime_thread_id` is shared with the
factual END sampler. The sampler reads a `WorkerSession`, while recovery reads
a projection; both now use the same inline-first identity rule. This lets an
active snapshot-only execution consume its actual original END after Stop.
The existing provider/profile, immutable proof and current-carrier CAS checks
still qualify that fact. Startup keeps its original active-only and retained
association checks. A detached local canonical execution without an original
qualifying END observer retains its obligation after physical Stop.

The canonical and released historical regressions drive real native RPCs through
HTTP Close/Reopen and new Human inputs. The historical snapshot-only case also
exercises the existing recovery owner. Existing MCP lifecycle tests use an
external protocol peer, sharing the original unit-test transport publisher; the
peer returns RPCs/notifications and records requests, never input claims or FSM
transitions. Stop errors retain obligations, and held response tests still
require the caller's response before Stop while another Track makes progress.

Unsafe-identity refusal and valid retry use independent, genuinely timed-out
carriers. A negative case ends with its deliberately damaged identity retained;
restoring only a test SQL row cannot synchronously restore a live owner's cached
projection. Both cases retain their original input and atomicity assertions.

Consumer fixtures that exercise hibernation seed an already-closed candidate
without requesting user Close/Stop. Fixtures that exercise clean versus issuance
reopen after clean has sampled/registered its candidate, preserving the actual
Track lock and teardown races. Replay, input order, lease, cancellation and
negative identity assertions remain behavioral checks. The schema disappearance
inventory registers only the exact incoming
`track_close_cleanups.worker_session_id -> worker_sessions.id ON DELETE CASCADE`
edge: deleting a carrier removes its cleanup metadata; that edge cannot delete
the carrier. The unregistered-cascade and deleting-trigger counterexamples remain.

Maintenance cost: these regressions join their existing modules and CI
partitions; there is no test-name registry or extra CI pass. The Close fixture
uses the existing `_tests.rs` convention while retaining its `close_p1` module
identity. Its deliberate snapshot damage stays outside the production writer
inventory; that inventory and its negative probes are unchanged. Stop identity
checks run on the selected Planner at Close admission and cleanup retry, rather
than scanning every conversation. New tests and providers using an existing protocol
need no close-specific registration. A newly exercised native RPC updates only
the protocol peer that returns it; the fixture publisher is shared with the
existing native unit tests. Adding a migration
with a new incoming carrier reference continues to require its SQL and the
existing exact-edge inventory. Experiment logs and mutation failure sets remain
delivery evidence under ignored `target/pr3527-p1/`, not permanent source checks.
This follow-up adds a transient transaction-result field and shares the existing
thread decoder; it adds no durable field or migration. The affected worker
fixture uses the current session terminology, tightening the existing
terminology occurrence baseline. Later references edit their owning query/test
and that same baseline; the CI lint scan and its self-test keep their existing
invocation counts.

[`close_cleanup`](../../crates/calm-server/src/planner_hibernation/close_cleanup.rs)
rechecks all four obligation fields under the existing start and lifecycle
fences. It calls the installed harness's strict shutdown, joins its run loop,
and retires only that same registry handle. A registry miss uses the exact
persisted session through `PlannerFactory` and the registered provider Stop
owner. The generic best-effort replay path cannot retire this obligation after
an unconfirmed interrupt. Stop errors and registry replacement retain the row;
the regular terminal sweep retries it. Only confirmed shutdown permits deletion
with all four fields as a compare-and-set guard. The original conversation
remains resumable; shutdown never completes it as Failed. Neither lookup nor
END settlement follows the card to a successor session.

The tool admission guard survives through the Unix socket response flush. When a
close finds a busy session, the admission owner registers one waiter for that
session and the sweep continues with other Tracks. Releasing the last call or
rotation guard hands the idle fence to that waiter. It retries the original
cleanup row under the same session and request checks. Ordinary replies perform
no cleanup scan. The periodic sweep retries durable obligations after a failed
Stop or process restart. An MCP self-close receives its result before its own
harness stops.
The short SQLite revalidation transaction ends before native Stop or run-loop joining.
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

The lifecycle regressions exercise response-flush ordering, another Track's real
harness retirement and exact original END settlement while the oldest reply is held, failed-Stop retry,
successor-session isolation, a SQLite write during blocked native Stop, and a
real worker process whose lease remains held until cleanup confirms shutdown.
The behavioral evidence and mutation sets belong in the delivery result, not in
a permanent test-name registry.
