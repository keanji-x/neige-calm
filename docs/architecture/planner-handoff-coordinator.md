# Durable internal handoff coordinator

Design tier: L2 (authorization, persistent ownership and cancellation boundaries).
The outcome is an internal user-audited request composing the existing exact pause,
validated SQL cutover and explicit activation. No REST, MCP, AI or UI ingress is added.

## Durable ownership

Admission must atomically bind the immutable audited request, both exact original
sessions, two freshly allocated PauseRequests, a cutover key/plan ID and successor
session ID to an OperationRuntime operation and one active reservation per Track.
A same-key identical request returns this journal; changed scope conflicts. Another
key refuses immediately. Neither reservation expiry nor process shutdown releases it.
Track/root and both card/session pointer generations are required audited input. SQL
triggers monotonically increment counters on pointer/role/lifecycle transitions, so the
existing start compensation restoring an old session cannot recreate an old approval.
Admission and final cutover reuse the owning profile and SQL binding validators.
Initial SQL admission uses the same durable quiet-checkpoint, current managed binding,
native-receipt and hold facts as cutover. The live pause owner remains the final
idle/readiness authority. The internal entry takes the existing runtime drive mutex with
try_lock and drives only its exact accepted operation through the existing phase runner;
it refuses a busy driver instead of persisting approval behind other pending work. The
same repository claim query supports batch and exact selection. Existing workers continue
to run independently, and ordinary background drive keeps its existing batch behavior. Busy refusal ends
preparation; replay must not wait for a previously refused owner to become idle.

All effects use persisted identities. The journal stores an intent before each pause,
the returned/query receipt before advancing, the complete validated cutover plan before
commit and the original cutover operation before activation. OperationRuntime owns
leases, checkpoints, keyed replay and generation-fenced retry. A typed adapter opt-in
also permits explicit retry of an interrupted Stuck preparation/interaction checkpoint;
the generic claim requires the original generation, phase origin, output and exact key.
Other adapters retain their existing terminal semantics. The reservation is a
domain uniqueness constraint, not another work queue or provider executor.

## State and recovery matrix

| Boundary | Cancellation / crash / missing answer | Permitted continuation |
| --- | --- | --- |
| Admission not committed | no accepted request or effect | retry original key; SQL decides admission |
| Reserved, no pause intent | journal and reservation survive caller drop | cancellation can terminate without effects |
| Source pause intent | query original request, never allocate another claim | original active claim is precisely compensated; unresolved fact retains NeedsRecovery |
| Source receipt, target intent | query both original requests | release only exact owned claims; target busy never queues approval for later idle |
| Both receipts | exact current binding and healthy receipt rechecked by existing precheck | cancellation/fault abandons cutover and compensates |
| Validated plan persisted | complete original plan and successor ID remain fixed | no commit until cancel flag and SQL facts are rechecked |
| Cutover commit intent | query original cutover key before any release | success forwards; query/storage uncertainty retains reservation and cannot compensate |
| Cutover known rolled back | query proved original key absent after completed call | exact precommit compensation only |
| Cutover committed | retired source/target and successor are authoritative | cancellation only reports new owner; never restores role, queue or credentials |
| Activation requested | existing activation key and resource intent own replay | query forwards actual pending/blocked/unknown/recovery/Activated state |
| Activation Blocked | hold is not approval to retry | explicit original-generation retry only |
| Activation Unknown | native request might have succeeded | no automatic native resend |
| Compensation release intent | query original exact receipt | consume typed ReleaseOutcome; fault/unconfirmed never means Healthy |
| Compensation storage unknown | preserve original progress, claims and reservation | explicit recovery; no blind release of reservation |
| Proven healthy compensation | durable released outcomes for every possible owned claim | terminal cancelled/refused; release reservation atomically |
| All exact releases confirmed, one nonhealthy | durable typed outcomes and original progress retained | terminal ReleasedNeedsRecovery; release reservation, require ordinary provider/owner recovery |
| Activated | existing activation proves installed exact successor | terminal reservation may be released |

Cancellation is a durable flag, not a dropped future. An accepted command cannot be
undone by dropping its answer. Any post-pause error first reconciles original facts.
An unresolved pause intent is never converted to an absence/healthy assertion from a
plain SQL read. A serial query barrier on the exact current original run-loop owner can
prove no accepted command remains and no durable claim exists. It may then record no
effect, without inventing a healthy release; unavailable/closed owners require recovery.
Reentry before commit is conservative compensation, not renewed approval to pause.
A commit-intent reentry queries first and never sends a new cutover plan.

## Lifetime and lock order

No SQL transaction or Track mutex crosses a provider RPC. Preserve the established
card-start -> Track-delete -> registry -> issuance order. Reservation admission is a
short IMMEDIATE transaction. Ordinary workers, attempts, task claims, verdicts, approvals,
worktrees and transcript item/session identities are outside coordinator ownership.
Track close/delete must refuse while a reservation remains active; terminal Track
cleanup may remove reservation metadata while keyed operation evidence stays permanent.
Process exit changes neither reservation nor claims. Recovery does not trust process
registry presence as proof of a successful release or cutover.

System observations continue through the existing durable eligible-prefix delivery,
complete queue/watermark CAS and activation catch-up. Coordinator must not introduce
another notification buffer or discard pending entries beneath the watermark.

## Verification and delivery boundary

The required focused selection is the task's complete calm-server selection (including
handoff_coordinator, pause, cutover, activation, dispatcher and recovery). Regressions
must use registered production operations, real pause commands and SQLite file reopen.
Critical single-factor production mutations cover persisted intent before pause and
no compensation after a possibly committed cutover or durable uniqueness. Predict the
complete red set before mutation, run the entire observing selection, compare it,
restore from an exact before-image, verify hashes/diff and restore green.

The kernel owns text, quick Rust, contracts, focused replay, transaction authorization
and FE consumer gates. The worker owns focused iteration and necessary formatter/lint
fixes. Planner owns two fresh independent L2 reviews. External ingress remains a later
task, without preventing delivery of the complete internal service.

## Maintenance cost

The original `planner_harness_start_adapter` compensation can call
`session_restore_from_superseded_runtime_tx`; exact session IDs alone therefore do not
prove absence of pointer ABA. Migration 0175's counters add one small SQL update only
when a card's session/role or Track's root/lifecycle changes, plus initialization on
creation. There is no tick scan, TTL or worker admission fence. Later providers continue
to extend the existing profile/provider owner; coordinator has no provider list.

A new coordinator stage changes its owning enum and relevant transition tests. A new
test changes its owning module only. No test-name registry or additional CI workflow
pass is added. The reservation adds an indexed uniqueness lookup to admission; each
checkpoint uses the existing lease/output CAS. Final cutover also requires the original coordinator live lease inside the existing keyed
transaction before any business write. Final activation atomically terminates
only the reservation naming the exact committed cutover. A nonhealthy release preserves
NeedsRecovery and its evidence. Once every possible claim has a confirmed typed release,
the reservation can terminate without asserting Healthy. Unknown/unreleased claims retain
the reservation and require explicit coordinator retry.

The old large run-loop and runtime modules only receive owning registration/command
hooks. New coordinator responsibility and test files remain under 800 lines. No public
wire type, generated API schema or visible UI changes, so API generation/browser preview
are not applicable to this slice. Long-term external authorized ingress and evidence-based
native Unknown recovery remain distinct tasks; Unknown never mints again automatically.

## Unique fact owners and structural scope

| Owner | Sole facts and effects | Facts it must consume from another owner |
| --- | --- | --- |
| Coordinator | original typed user-audit scope, Track reservation/cancel flag, original claim requests, persisted plan/key and workflow progress | original pause/release facts, atomic cutover result and actual activation result; it cannot manufacture approval or health |
| Harness pause owner | exact session ingress/issuance fence, durable pause marker, receipt and typed release health outcome | current SQL managed binding and existing backend readiness; a receipt alone is not health |
| Cutover | final SQL revalidation, role/current-pointer transition, retirement and complete queue/watermark transfer in one keyed transaction | two exact healthy pause facts and the original coordinator commit intent/lease; it grants no native readiness |
| Activation | provider readiness, native resource intent/proof, credential binding, recovery catch-up and installation result | original succeeded audited cutover; Unknown never authorizes another native request |
| OperationRuntime | keyed row identity, lease, phase runner, checkpoint CAS and explicit retry generation | adapter-declared domain contracts; it knows no handoff/provider policy |

Provider eligibility/configuration remains in the existing profile/provider owner. Initial
admission and cutover reuse its SQL facts; coordinator does not copy a provider whitelist,
authentication decoder, task queue or authorization executor. Production reservation SQL
is private to the operation domain; tests have a cfg(test) scheduling seam calling that
same admission implementation. The public internal service is the typed `begin` path.

An operation key only deduplicates one request; it cannot exclude a different key on the
same Track. The runtime mutex is process-local and cannot survive restart or be held as a
Track/RPC reservation. Per-session pause markers prove individual claims, not a durable
pair reservation. Hence the small reservation table/partial unique index is necessary.
The operations row remains the only workflow journal and work queue.

Existing session IDs are insufficient against committed pointer restoration:
`session_restore_from_superseded_runtime_tx` can restore an old exact session. Existing
update timestamps are not monotone pointer generations; queue/snapshot revisions do not
own card/root/lifecycle changes. The two small generation tables therefore record only
Neige-owned binding transitions. They are not a provider health protocol or an event ledger.

The generic runtime delta has three concrete causes, each kept in its existing owner:

- Existing batch drive can advance unrelated pending work before this request. The
  immediate internal entry refuses a busy drive mutex and uses the same claim query and
  phase runner with an exact operation selector. A domain-local driver would duplicate
  kernel lease/phase rules. Batch behavior remains owned by the original driver.
- A tx-only cutover nested inside a coordinator must validate the coordinator's original
  lease in the business transaction. The small claimed-commit wrapper calls the existing
  generic live-claim predicate; it adds no domain policy or second commit implementation.
- Storage failure can leave an owned checkpoint Stuck. Re-enqueueing a new operation would
  orphan original claims. An explicit, adapter-opted-in continuation reuses the original
  key, generation and exact stored checkpoint CAS. Default adapters do not opt in, and
  native activation's Unknown protocol is not broadened.

These are lifecycle-contract mismatches, not reasons to add per-error retries or more
scan/trigger registries. The broader alternative is to change the runtime's submission and
interrupted-checkpoint contract for all adapters; that is deliberately outside this task.
A future simplification review can assess that unified contract and remove the private
composition helpers if it supplies the same guarantees. Domain code must not take over
kernel lease updates or create a replacement queue merely to reduce this diff.

Planner message `b256c8aea7104a37a893ae24010f6b76` requests an overall architecture
simplification review after this task settles, before external ingress. The worker adds no
review channel or external surface. If that review finds a lifecycle ownership conflict,
the next change should delete/narrow mechanisms around the shared contract, not add
another conditional retry or inferred readiness flag.
