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
