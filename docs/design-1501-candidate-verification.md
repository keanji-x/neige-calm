# A1 candidate verification implementation contract

A1 adds explicit candidate producer/consumer roles to frozen task context. The
producer declares a bounded ordinary-file set and required machine checks. The
policy states declared-checks-only scope; successful commands do not establish
test discovery or semantic review. Reviewer-required and repair are unsupported.
Existing JSON contracts, receipts, bindings and artifact manifests remain unchanged.

The publication Operation retains a candidate after exact execution stop. Migration
0103 adds immutable candidate and consumer-binding records. Candidate existence
never grants delivery eligibility. A separate candidate-verify Operation freezes
candidate identity and policy in tx_output, journals process identity before release,
and retains actual verification evidence in its result. No independent accepted bit.

Gate process mechanics are extracted from task-verify: held shell, environment
allowlist, process-group identity and cleanup, wait-status verdict, timeout and
restart exit evidence. Task-specific admission and result writes stay separate.
Candidate release checks lease, current attempt, withdrawal, frozen contract,
source authority and exact materialization in a guarded transaction. Verification
runs in an owned working copy outside sealed storage. This is the existing trusted
host gate surface, not a malicious-code filesystem sandbox.

Consumer claim and preturn both require the exact successful verification Operation
and candidate relation, as well as current authority. Materialization supplies sealed
bytes, never producer files. Replay and technical recovery retain the same snapshot
and frozen policy. Track capacity includes active verification and cleanup; the
shared launch semaphore bounds starts. Worker terminal fences remain unchanged.

Publication settlement retains its exact semantics. Verification settlement requires
its own event and corresponding version, compatibility, dispatch, replay and generated
schema sweep. Candidate projections distinguish sealed, checking, failed and qualified.

Acceptance uses production authoring/scheduling/Operations with only Codex faked at
the external boundary and real harmless shell checks. It covers immutable bytes,
failed gates, absent discovery claims, corrupt input, withdrawal, lease/release,
recovery and capacity. No real model suites or shared-service experiments.

## Capacity and authoring authority

An immutable verification allocation is reserved in the scheduler transaction
before Operation submission. It binds publication, Track and operation key. Missing
Operations after interruption still consume capacity; replay submits that allocation.
The same transaction observes current task costs and existing reservations. Consumer
and ordinary worker claim includes reserved verification capacity. Compensating and
stuck operations retain their reservations; successful/failed operations release cost.

Host checks are authored in the same Planner/User task declaration context as ordinary
gates, not by Worker output. Existing task declaration projection enforces actor,
ready and automation release. `validate_frozen_contract_tx` revalidates the frozen
context and current declaration including `released_by_user` under declare-and-wait;
candidate release invokes it through retained-source authorization. Producer Done and
accepted report are stop/publication prerequisites, not host command authorization.
Terminal-tool approval modes govern interactive terminal invocation; ordinary gate
execution has its separate declaration/scheduler authority path. A1 uses that path
and retains its shell shape checks; no terminal-tool authorization is inferred.

## Approved frontend contract change

Parent/orchestrator approved on 2026-09-08 the narrow core/api and core/events CR
for `task.candidate_verification_settled`: decode, generated wire/OpenAPI, contract
tests and invalidation. `fe/module-file-inventory.yaml` assigns readonly core/api
(section 6.3) and core/events (section 6.5a); app/providers is writable. No ownership
or layer rule changes. Actual readonly changes require these commit/PR trailers:

```
OWNERSHIP-CHANGE: fe/core/api/schemas.ts — decode approved candidate verification settlement (#1501)
OWNERSHIP-CHANGE: fe/core/api/schemas.contract.test.ts — pin approved candidate verification identities (#1501)
OWNERSHIP-CHANGE: fe/core/api/generated/wire.ts — generate approved candidate verification event (#1501)
OWNERSHIP-CHANGE: fe/core/events/invalidation-plan.ts — refresh candidate verification evidence (#1501)
OWNERSHIP-CHANGE: fe/core/events/invalidation-plan.test.ts — pin candidate verification invalidation (#1501)
```

## Bounded review fixes: deletion and process completion

Track and Area deletion now refuse unresolved candidate-verification allocations,
including reservations whose Operation has not yet been submitted. Both route
preflights and repository transactions use the shared guard; migration 0103's
allocation-delete trigger backs raw cascades. Terminal succeeded/failed Operations
permit ordinary deletion. A scheduler sweep replays missing Operations independently
of current task status and Track scheduling eligibility, so cancellation/withdrawal
settles rejected reservations through admission rather than leaking capacity.

Live gate observation retains the Tokio Child without polling its reaping wait:
Linux `waitid(WEXITED | WNOHANG | WNOWAIT)` observes actual exit, then verified
group cleanup runs before reaping. Candidate observation retains the Child through
the terminal transaction, retrying failed writes with its real wait verdict held;
only then does it reap. This prevents a concurrent dead-work sweep from replacing
the actual verdict with exit-file inference. Tokio 1.52.3's Unix Reaper reaps on poll or drop,
not merely on SIGCHLD notification while the handle is retained. Kernel wait status
remains authoritative over completion files. The shared live wait mechanism also
benefits ordinary gates; their existing timeout, extraction, and recovery tests
remain required.

Candidate terminal completion, dead boot recovery, and compensation separately
require a successfully inspected recorded group with no executable members.
Zombies are stopped; inability to inspect is unresolved. After observer drop or
restart, a missing leader never authorizes a group signal. If live members remain,
verification stays unresolved and retains capacity. If the group is proven stopped
(or its recorded boot ended), recovery may use the existing dead-work exit evidence.
This is a same-recorded-process-group contract, not a namespace sandbox or a claim
about descendants which change groups.

#1501 follow-up: ordinary task-verify's pre-existing dead-leader boot/recovery and
compensation semantics have not acquired candidate's new quiescence fence. Do not
claim all ordinary-gate descendant cleanup is solved. No host-wide subreaper or
new process-supervision architecture is introduced in A1.

Read projections preserve historical verification state/verdict while reporting
current qualification and its authority-failure reason separately. Unexpected
storage/serialization errors remain read errors. Candidate-consumer recovery
inherits the immutable file-set and verification binding; JSON-consumer recovery
inherits its JSON binding. Failed candidate outputs remain unsupported recovery
inputs (F5).
