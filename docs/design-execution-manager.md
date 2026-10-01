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
