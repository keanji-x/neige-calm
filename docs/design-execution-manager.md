# Execution ownership before concurrent read tasks

Status: replacement design for the draft guard implementation in PR #1921.
The current branch remains incomplete. Its backend stop primitives and regression
cases are useful evidence; its distributed lifecycle wiring is not the intended
public contract. The existing draft must not merge before this boundary converges.

## Outcome

Task, Planner, Terminal and Forge submit execution plans to one execution manager.
The manager owns resource reservation, durable execution identity, launch,
cancellation, recovery and release. Business task completion records a report;
it grants no authority to release resources.

The manager extends the existing operation/runtime ownership boundary. It must
not introduce a parallel workflow FSM, another scheduler, or a second recovery
owner. Persistent leases remain the storage mechanism for resource exclusion.

## Contracts

A plan contains the required access mode, canonical resource, authenticated owner
and backend selection. The manager creates `ReadPermit` or `WritePermit` only
inside successful admission. Their constructors and durable lease identity are
private. Permits are not Clone or Copy, and backend launch consumes the permit.
Dropping a permit or losing a reply never proves the execution stopped.

Backends are registered behind the manager. Mutating native RPCs and process
launch methods are private to their backend implementations. Ordinary callers
cannot obtain the raw launch client through a general SpawnCtx or daemon getter.
Read-only provider inspection is a separate interface without launch authority.

A backend implements launch, stop and recovery for its own execution identity.
It returns a sealed execution-scoped stop observation: confirmed stopped or
unconfirmed. PID/group checks, native request correlation, background terminal
rosters and transport uncertainty stay inside the backend. Observations cannot
be transferred between execution IDs or generations. Backends never update the
resource lease's released state themselves.

The manager is the only lease releaser. It records positive stop evidence and
releases the execution's resource reservation in one transaction. Unknown launch
or stop outcomes retain the reservation and a recoverable explanation. Existing
terminal, CLI and native-turn proof helpers can be reused under this contract.

## Consumers

The scheduler asks the manager whether a plan can enter and submits the plan.
Pending diagnostics use the same admission result. A report handler changes only
the task's business state. Git delivery waits for the producer execution to stop,
then acquires its own execution permit; it never borrows a live producer's root.

Deletion and relocation request scope shutdown and inspect the same durable
execution state before changing ownership or removing resources. Their final
transaction checks that state again. They do not interpret backend process or
turn facts, and they do not release leases.

Interactive TUI clients must connect through the manager's session ingress,
which owns their native control requests. The CLI can select another thread
inside a remote session: its starting directory and `resume SESSION_ID` do not
freeze the scope. Stopping the CLI also does not prove its remote turns stopped.
A session write permit alone therefore cannot authorize direct daemon access.

The ingress admits only supported inspection requests directly. Thread creation,
resume, turn launch and control enter the manager, which validates the frozen
scope and permission policy and persists each execution before forwarding it.
Unknown mutating requests fail closed. The raw daemon endpoint stays private to
execution backends. Cancellation closes ingress, stops the client, then settles
its managed native executions using provider evidence before releasing resources.
Read tasks expose status and reports without a write-capable TUI. The manager
owns permission-profile selection and validation; callers do not select a weaker
sandbox to start a managed read execution.

## Runtime settlement

OperationRuntime owns one bounded, coalesced execution reconciliation entry.
Completion and reconnect notifications only wake it; they never constitute stop
proof. The existing Dispatcher reconciliation calls the same entry when a
notification is lost or a subscriber lags. The scheduler has no independent native
execution scan or lease-release authority.

Startup scans held executions even when their business Operation is terminal.
An unavailable provider retains the reservation, and reconnection wakes the same
entry. Remote probes run outside the global operation drive mutex. Observers use
weak owners and unregister when their runtime ends; dropping an observer cannot
prove that a remote execution stopped.

Stop confirmation persists the release and its events atomically. Only after
commit does the manager publish the exact recorded envelopes, including workspace
release and delivery handoff. Harness outcome, input restoration and snapshot
ordering stay with their existing business owner.

## Bounded migration

1. Freeze further per-caller guard additions. Preserve all existing changes and
   evidence in the draft branch; do not merge a partial manager alongside the
   distributed ownership paths.
2. Implement one complete native execution vertical slice through the existing
   runtime: reserve, launch, cancel, uncertain-reply recovery and confirmed stop.
   Both Task and Planner use it. Close raw native launch access as part of this
   slice, before migrating the next backend.
3. Move Terminal/CLI and Forge onto the same manager contract, retaining their
   specific stop primitives. Move scheduler, report cleanup, deletion, relocation
   and Git delivery onto manager status/shutdown APIs and remove their manual
   release and backend-fact interpretation paths.
4. Review the resulting public boundary and complete call graph, then run fresh
   independent reviews and the invalidated checks. Rebase and merge only that
   converged result. Declaration/usage refinements follow the agreed interface.

## Acceptance checks

- A compile-fail test proves ordinary consumers cannot construct a permit or
  call a backend's mutating launch method. A source boundary check covers every
  production native RPC/process launch route without accepting caller promises.
- A newly registered fake backend automatically receives admission, cancellation,
  recovery and release behavior through the manager. Adding it requires no
  scheduler, deletion or Git-delivery lifecycle changes.
- Concurrent readers share a resource; overlapping writers wait. An uncertain
  launch, early business report or failed stop cannot release it. Exact positive
  stop releases once, and a late acknowledgement cannot revive stopped execution.
- Interactive clients and startup recovery obey the same ownership boundary.
  Raw UI input or an upgraded pre-existing execution cannot enter outside it.
- Scheduler diagnostics, scope shutdown and delivery observe one durable state.
  Existing race/proof regressions remain meaningful at production entry points;
  tests do not reproduce lifecycle behavior in hand-written fixtures.

Completion means the next execution path is constrained by types and module
visibility by default, rather than relying on a reviewer to remember a guard.


## Managed session ingress implementation boundary

The session ingress is a manager-private WebSocket-over-UDS protocol adapter.
Its durable record freezes the session execution, terminal, card, track,
canonical directory, permission selection, and provider/socket endpoints.
Existing lease phases remain the lifecycle state; there is no gateway FSM.
Session-to-thread and session-to-execution associations are stored before a
control request is forwarded. Unknown launch replies retain the native record
and its existing nonce. The transport preserves complete replies; backend
associated outputs reach callers only after the manager commits launch identity.

Admission checks the held session and frozen owner in the same immediate
transaction that associates a native reservation. Closing uses the same durable
session stopping fence, then quiesces transport, confirms client stop, and settles
all associated native references using existing backend provider observations.
Only the manager releases the session reservation. Recovery reconstructs the
socket from the persisted endpoint after validating ownership. A generation-bound
unmanaged-client observation permanently rejects attachment and retains its
reservation; an empty known-reference set cannot prove a legacy client stopped.

A dedicated provider connection preserves initialization capabilities and thread
subscriptions, including a newly created thread before its first rollout exists.
The ingress forwards only declared inspection methods. Thread creation/resume,
turn launch/control, and supported approval or input responses pass manager
scope and permission checks. Unknown mutations and uncorrelated responses are
refused. Cross-worktree and foreign-owner resumes direct the user to the owning
card instead of expanding session resources. Exact terminal/runtime attribution
reuses the existing pending registry binding implementation without FIFO guesses.

Verification uses fake UDS WebSocket and supervisor peers through production
entry points, covering protocol envelopes and bidirectional IDs, durable
issuance before forwarding, scope rejection, uncertain reply recovery, stop
races, socket recovery, and the absence of raw daemon endpoints in client
commands or public card metadata. No test invokes a model.

## Native recovery cleanup fence

Permanent projection cleanup closes the native scope in the same durable admission
boundary used to mint launch capabilities. Ordinary turn interruption leaves it
open for the next generation. Discovery can refine a closed scope's actual cwd;
it cannot reopen it. Missing provider facts retain the closed recovery barrier.

Both provider histories must agree with the acknowledged generation. A lost target
after interrupt, or a nonce mapped to another acknowledged turn, supplies no stop
proof. Session cleanup distinguishes an atomically unclaimed capability from a
claimed session; the caller's old launch snapshot supplies no release authority.
The final projection deletion transaction rechecks live references and unknown
native scope state. Positive legacy stop records evidence without acquiring a
writer. Only its exact current terminal task's durable read intent can receive
the stop handoff after all live references are gone.

## Ingress provider proof and validation limits

Native admission associates its existing reference with the unique same-owner,
same-directory managed session in the reservation transaction. Explicit session
claims must match exactly; a stopping session rejects both forms. Known unissued
clients cannot release while admitted remote references remain unresolved.

Provider stop proof covers the complete descendant roster, including archived
and non-archived pages and the loaded-thread roster. Parent chains, canonical
directories, active turns, background executions, and the final roster must all
agree. A new descendant or missing provider fact retains the parent lease. This
uses backend observations without introducing child task scheduling or nonces.

Protocol fixtures use the offline codex-cli 0.159.2 JSON schema generated with
`codex app-server generate-json-schema --experimental`. Nullable defaults and
empty configuration are accepted where declared; non-null scope or capability
overrides are rejected. Full input, result, and provider error data survive the
manager boundary except its frozen policy, directory roots, and launch nonce.
These fake protocol tests do not validate a live TUI or model lifecycle.

This checkpoint does not close the public Shared thread-bootstrap facades, which
still lack manager launch permits, or SharedDaemonStatus.sock publication. Those
remaining production surfaces require integration work before claiming that the
complete native execution graph has no unmanaged entry point.
