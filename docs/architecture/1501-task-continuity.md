# Task continuity: starting, recovering and delivering delegated work

Status: design in progress for #1501. This document does not claim that the
behavior below is implemented. Implementation and validation status are recorded
in the delivery ledger at the end. Baseline: `4360896b`.

## Outcome

A task is a continuing delegation. After execution is authorized, the system
advances that work until its contract is satisfied, the user withdraws it, or an
explicit blocker needs a decision outside the current authority. An execution
failure is evidence about one attempt; recovery preserves the business identity,
reliable completed work, history and outstanding obligations.

The first integrated acceptance story is deliberately small:

1. Declare A and B in parallel, with C depending on both, and authorize execution.
2. A succeeds. B encounters a controlled failure. C explains that B blocks it.
3. Recover B under the same business identity. Do not re-declare B, change C's
   dependency keys, or repeat A's successful work.
4. Retain B's failed attempt and evidence. The recovery creates a distinct attempt
   whose callbacks cannot be confused with the old attempt.
5. When B satisfies its contract, C receives the precise eligible A/B results
   bound for its execution and proceeds. Input preparation failure cannot start C.

## Responsibility

| Responsibility | Authority |
| --- | --- |
| Goals, requirements, dependencies, acceptance policy | Existing authorized report task author |
| Business decomposition, semantic assessment, recovery strategy | Planner within user authorization |
| Claim, identity binding, bounded technical retries, input preparation | Kernel scheduler and existing Operation machinery |
| Candidate work and self-report | Bound Worker for one execution attempt |
| Machine checks against frozen input | Verifier; no Planner/Worker credentials |
| Withdrawal, additional permission, unresolved requirement decisions | User or specifically authorized decision owner |

The framework must execute an authorized recovery decision to completion. Planner
should not manually reconstruct keys, copy directories or repair dependency edges
to express "recover B and keep A". It must not invent semantic acceptance or new
requirements on the Planner's behalf.

## Behavior before storage

### Starting

Declaration and permission to run are distinct. An author can draft work without
authorizing execution. Existing `declare-and-wait` and user-owned declarations
remain effective authority boundaries.

Once authorized, resource waiting, workspace provisioning and input preparation
are runtime responsibilities. They do not require the user to manually advance
additional lifecycle flags. A pending task explains the actual blocking condition
and its owner. "Started" means business execution was allowed to begin after
preparation; accepting a start request alone is not proof of execution.

### Recovering

Recovery preserves the task's author identity and creates a new execution only
when the old attempt cannot safely continue. Reattaching a live provider or
replaying an Operation does not create a business attempt. Recovery never changes
the failed attempt's terminal outcome, its logs, or already-frozen downstream
input bindings.

Initial recovery is explicit, scoped to one failed attempt and the unchanged
contract. It is not permission for an unlimited retry loop. A changed contract
requires an author revision with explicit invalidation/re-execution semantics.
Completed or user-canceled work cannot be retried through the failed-work command.

A recovery request supplies the logical task, expected failed attempt, a reason
and an idempotency key. Admission rechecks caller authority, exact attempt,
contract, declaration validity, withdrawal and Track state in one transaction.
Concurrent duplicate requests converge on one new attempt. A request made against
an obsolete attempt cannot replace a newer execution. Transport retries preserve
the idempotency key and return the original result.

Admission does not prove replay safety. Before replacement execution starts,
reconcile and fence the prior execution so it can no longer write the same input
or repeat an external action concurrently. A failed task row can precede process
cleanup. Request idempotency and late-callback rejection do not stop that process.
Uncertain prior external effects require an actionable blocker or an explicitly
authorized recovery decision; they must not be replayed merely because an attempt
row says failed.

Initial authority is deliberately bounded:

| Requester/declaration | Recovery authority |
| --- | --- |
| User through the recovery action | One recovery of the exact failed attempt per explicit request |
| Planner, its own `spec` declaration under `auto-declare` | At most one recovery (generation 2) in the initial implementation |
| Planner, user-owned declaration or `declare-and-wait` | Not granted by the old start/release; requires a user recovery action |
| Worker | No recovery authority |

This initial bound is a support policy, not a new configurable author contract.
Later policy configuration must specify budgets and authority explicitly. A user
can issue a new recovery for a later failed attempt; repeating the same request
still returns its old receipt. Withdrawn readiness/release stops an admitted but
unstarted recovery, and checks run again before launch. In-flight withdrawal uses
the existing cancellation/reconciliation protocol; it is not a promise that an
arbitrary external side effect can be undone.

The recovery transaction records the new attempt and its provenance; the existing
Operation saga prepares and starts it. A crash between these actions must be
recoverable without a second attempt, loss of provenance or duplicate side effects.

S1 provides execution continuity only. The public repair experience additionally
requires the immutable recovery manifest in S2: unchanged requirements and exact
inputs, rejected candidate when one is usable, failure evidence, and corrective
instructions selected by the Planner. Corrective instructions do not edit acceptance
requirements or gates. A requirements change needs a new author revision. The
manifest is frozen with the recovery and its replay identity; no mutable path or
latest-HEAD fallback may stand in for unavailable evidence.

### Handling errors

Error evidence records what failed; recovery policy determines what happens next.
Do not infer that business work began from a preparation error. Do not collapse
rejected output, gate configuration failure, unknown provider exit and permission
failure into an unexplained red task.

| Situation | Required behavior |
| --- | --- |
| Retryable transport/preparation failure within configured authority | Retry within a bounded policy, preserving identity |
| Failed business implementation/check | Retain candidate evidence; Planner can select repair/re-execution |
| Missing permission or conflicting requirements | Explain the specific decision and owner; preserve unrelated work |
| Unknown state or uncertain external side effect | Reconcile existing evidence; do not blindly replay non-idempotent work |
| Exhausted recovery policy | Explain the attempted recovery and remaining blocker |

Automatic recovery policy is a later capability, not a default introduced by
adding an explicit recovery command. Existing technical recovery remains intact.

## Identity and author continuity

The existing author identity is Track + task key. Keep it stable across recovery;
dependencies continue to address that identity. Report block identity locates the
declaration for editing and context resolution; it is not a substitute for task
identity. Forks create independent Track-scoped work and copy no execution state.

Execution identity must be separate from the author key. Legacy `tasks` rows,
Operation idempotency keys, Worker ownership and gate attempt numbers must retain
their historical meanings. No released terminal row is revived, and similar
historical names are never guessed to represent retries.

Only the current selected execution contributes scheduling and report state for a
logical task. All executions remain addressable as evidence. This selection is
kernel-owned runtime state, not another editable source of task goals. The
physical mapping below preserves this separation.

### S1 storage decision

Keep `tasks.id` as the attempt ID and `(track_id, key)` as author identity.
Remove the old per-key uniqueness from `tasks` through a new migration and add
append-only `task_attempt_allocations`. Each allocation identifies an attempt,
logical key and monotonically increasing generation. Initial and recovery origins
are structurally distinct. Recovery requires predecessor, request key/fingerprint,
reason, actor provenance and a versioned contract constraint; initial allocations
do not fabricate those recovery fields.

Enforce one generation per logical task, one successor per predecessor, and one
stable result per scoped recovery request key. The current attempt is the maximum
allocated generation, even when its pending `tasks` projection is absent. Existing
projection legitimately removes pending rows when declarations cease to qualify.
Re-enabling an unchanged declaration rebuilds the allocated attempt ID, without
falling back to the old failed attempt or silently allocating another attempt.

Initial task producers must register their initial identity through the production
storage boundary. Test fixtures cannot invent missing allocation metadata. Old
rows migrate one-to-one as initial attempts with all existing columns unchanged;
there is no name-based association between historical tasks.

Recovery contract constraints persist the previous frozen context evidence plus
execution route and author identity. They must not use only the projection drift
field list, which omits parts of the contract. Compare the relevant source hashes
and identities at recovery and again at claim; current authorization and withdrawal
remain separate checks. Old attempts lacking sufficient evidence are explicitly
ineligible for same-contract recovery. Existing root hash bytes remain unchanged.

REST and Planner MCP share one service and return the stable receipt:

```text
request: { expected_attempt_id, idempotency_key, reason }
receipt: { key, previous_attempt_id, attempt_id, generation }
```

Logical Track/key are supplied by the route or caller scope. Current/history reads
expose exact attempt identities so clients never reconstruct them from a key.
History redacts private gate bodies and retains original execution evidence.
The initial operation supports only failed `in-wave` attempts; sub-Track recovery
needs its own parent/child ownership protocol. Full repair/revision and automatic
retry policies are later slices.

### Caller and migration obligations

| Surface | Required change |
| --- | --- |
| Task repository | Unambiguous current-by-logical-key and history/by-attempt-ID APIs |
| Report projection and read state | Current allocation only; preserve single-snapshot reads and rebuild equality |
| Scheduler ready/claim/budget/sweep | Evaluate current executions; dependencies continue to use author keys |
| Worker/gate/terminal/reaper | Keep exact attempt ownership and terminal CAS; old results cannot change the successor |
| Operation prepare/boot recovery | Reject obsolete unstarted work; still reconcile/clean already-started old work |
| Plan list/cancel and file views | Resolve logical key to selected attempt; preserve direct historical lookups |
| Live/boot notifications | Resolve author key from recorded attempt data, never parse opaque attempt IDs |
| Tree accounting, Track/area delete, replay | Preserve current budget semantics, historical ownership and explicit cleanup |

Rebuilding SQLite `tasks` must preserve all current columns, indexes, incoming
references and terminal cleanup triggers. Save and restore `task_ref_index` across
the rebuild and verify `foreign_key_check`; changing foreign_keys inside the
migration transaction is not a valid substitute. No released migration is edited.

An old binary cannot interpret multiple attempts per key. Rollback after adopting
this schema requires a compatible forward fix or an offline restore of the
pre-migration backup; do not advertise simple binary downgrade as safe.

## Reliable delivery

Recovery alone is insufficient if a downstream worker receives the wrong files.
For artifact dependencies, bind exact eligible content, not a mutable path or a
global "latest successful" result. Keep pure ordering dependencies supported.

The first delivery implementation is limited to explicitly supported isolated Git
workspaces and declared output slots. Kernel sealing must establish a consistent
snapshot after a proven write boundary; Worker self-report and lease release alone
are not that proof. Unsupported files/workspaces fail explicitly. Public output
requirements belong to the contract; private gates may conceal checks, not add
undisclosed delivery obligations.

The successful-candidate ordering is:

```text
frozen execution contract and inputs
  -> Worker candidate report
  -> kernel sealing with durable immutable content identity
  -> verification against that exact version
  -> eligibility under the contract's required acceptance policy
  -> consumer claim freezes inputs and retention references atomically
  -> Operation materializes and checks inputs
  -> consumer execution starts
```

Sealing, preparation and external work use recoverable Operations. Database facts
and their events commit atomically. External sealing failure cannot publish an
eligible result. Referenced artifacts survive workspace release and GC. A replay
cannot discover a new source HEAD and silently substitute it for a prior binding.

Verifier build outputs live in its own writable workspace; consumers materialize
the sealed source, not the mutated verification checkout. A merged result has a
new identity and needs its own verification. Conflicting parallel outputs become
an explicit integration decision instead of an implicit overwrite.

## Review and remaining obligations

Failure may happen before a Worker reports any candidate. After establishing the
write boundary, the failure path must preserve recoverable workspace content and
validation evidence before disposal, or explicitly record that no trustworthy
snapshot exists. Preserve the distinction between retained unverified work,
validated partial work and a fully eligible result. A retained B snapshot may
seed repair; it does not by itself satisfy C's dependency. The integrated product
experiment must include useful B work before failure, not merely prove that A was
left untouched.

Accepting a review report does not accept the reviewed implementation. Findings
identify the exact subject/version, evidence, unresolved requirement and owner.
They survive completion of the review task and travel with relevant repair inputs.

An authorized review/repair consumer may bind a sealed rejected candidate without
changing its acceptance status. Ordinary delivery/publishing requires its proper
acceptance scope. Workers cannot change consumption purpose to grant themselves
that exception. Final acceptance requires evidence or authorized disposition for
each blocking obligation; green tests that enumerate known differences do not
prove those differences were resolved.

## What readers need

The shared read model answers "what remains, why has it stopped, who can act and
what action is allowed?" It is derived from existing facts and never writes state.
Expose the current execution and navigable history together with the author task.
Actions are hints from a snapshot; all writes recheck authority and exact versions.

Keep business status, execution phase and acceptance scope distinct without making
the user interpret their cross product. Use recorded source times, collection
times and liveness times honestly; historical transcript backfill is not current
progress. Unknown provider activity stays unknown. Private verifier information
must not leak through a shared snapshot to Workers.

The continuing-task read state remains open after an attempt failure. Its displayed
reason identifies whether recovery is available or a decision is needed. While an
admitted recovery waits, show the preparation/dependency/authorization blocker and
the new attempt; after eligibility is established, show delivery complete while
retaining failed history. This is derived state, not a second persisted status
writer or a reinterpretation of the historical failed row.

Recovery composes the already-permitted transition to Working for an appropriate
nonterminal Track. It must not strand ordinary failure behind an additional manual
lifecycle command. A user-blocked or terminal Track expresses separate intent;
Planner recovery cannot silently reopen it. Capability replies must state that
specific prerequisite, and user reopening keeps the existing lifecycle authority.

## Delivery ledger

Every implementation PR stays focused, updates this ledger and links its actual
tests and experiments. #1501 remains open until the defined integrated outcome is
verified; merging a design or backend foundation does not complete that outcome.

| Slice | Acceptance | Status |
| --- | --- | --- |
| S0: protocol decisions | Reviewed identity, authorities, transitions, migration/caller map | In progress |
| S1: continuing task execution | Explicit recovery retains old attempts, preserves keys and sibling results; late messages fenced | Pending |
| S2: reliable inputs | Sealed eligible inputs bound and prepared before start, with crash recovery and retention | Pending |
| S3: actionable task experience | Shared current/history view, recovery entry point and clear blockers; browser + product experiment | Pending |
| S4: repair obligations and bounded supervision | Review/repair purpose and findings, explicitly bounded intervention policies | Pending |

Tests must exercise production authoring, claim, reporting and recovery entry
points. Critical invariants require production mutations with predicted complete
red sets in exclusive worktrees. Relevant cases include concurrent recovery,
stale actor callbacks, changed/withdrawn contracts, wrong Track/role, lost response,
restart between claim and spawn, exact downstream inputs and GC retention.

Product experiments use an isolated instance, data and Git workspace, bounded
concurrency and a versioned observer log. Real Codex experiments are supervised
dogfooding, never the prohibited real Codex E2E suite on the shared host. Distinguish
mechanical handoffs, recovery work, business decisions and audit interviews in
operation counts. Record failed hypotheses and evidence limits as well as success.
