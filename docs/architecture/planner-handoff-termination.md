# Planner handoff termination protocol

Status: proposed follow-up to PR #2864; documentation only. Design tier: L2,
for authority and persistence boundaries. The handoff owner holds all policy.
This refines [the single workflow](planner-handoff-single-workflow.md): unresolved
claims/native Unknown must no longer retain exclusion indefinitely. It also
supersedes that document's migration-freeze statements **only for unreleased
0173–0178**, as authorized for this proposal; released migrations remain frozen.

## Evidence checked against this PR

1. Round 2, now fixed by `7a27f2b`: `handoff_state.rs` makes
   `SettledClaim::Released` carry `ReleaseOutcome`; its conversion accepts faulted
   and recovery-unconfirmed release. `handoff_coordinator_adapter.rs::compensate`
   skips already released claims and converts both claims to Finished. Previously
   the healthy-only conversion made that combination inescapable. The source and
   target regressions are in `handoff_coordinator_recovery_tests.rs`.
2. Round 3, still present: `operation/handoff_resources.rs::forward_resources`
   immediately returns AwaitingRetry for NativeIntent/Unknown. Intent commits
   before `mint_harness_resources`; bind has a later transaction. A crash before
   bind or a bind ABORT therefore leaves an intent with no proof. Retry takes the
   same branch. `handoff_coordinator.rs::cancel` only sets cancel_requested;
   the Committed dispatch and resource driver never consume it.
3. Round 3, still present: `routes/planner_cards.rs::fresh_start_planner_card`
   calls `start_harness_card`, hence `PlannerHarnessStartAdapter::prepare_tx` and
   `session_prepare_deferred_planner_tx` (`calm-truth/.../session_mirror.rs`).
   This supersedes/repoints the predecessor without settling its handoff.
   Compensation needs the original registry owner; even a surviving owner cannot
   release because `run_loop.rs::persist_snapshot_bound` requires
   `current_managed_binding_tx` for HandoffPause, including release. Likewise,
   installation precedes the final Activated transaction in `handoff_resources.rs`;
   resetting a Proven successor after that transaction fails invalidates the root,
   card/session and resource facts in `handoff_activation_sql.rs::current_facts`.

These are source-level confirmations, not new executed reproductions. Review
references supplied with the task: review-b2-2864 event 438675 and review-b3-2864
event 442932. 0175 blocks active-reservation close/delete/removal and admission
rejects another active owner; 0176 permits release only at Finished/Activated.

## Complete state × event contract

Cells specify **next state / reachable business terminal**, using the definitions
below. These are protocol transitions to implement, not descriptions of today's
AwaitingRetry behavior. Terminal rows are absorbing and excluded from the matrix.

- `A`: Abandoned / Abandoned, committed by the transaction below, without RPC.
- `F`: Finished / Finished, only after both claims are Untouched or typed Released.
- `V`: Activated / Activated, after final SQL proof; terminal and release atomic.
- `C`: Compensating / Finished or Abandoned; reconcile each original claim once,
  with a 30-second total deadline; failure, Unknown or deadline takes A.
- `P`: next Pausing checkpoint / Activated or Abandoned; source Intent → source
  Paused → target Intent → target Paused → identity cutover, original live drive
  only. Any fault takes A (or C when exact safe compensation is available).
- `I`: Committed(Unprepared) / Activated or Abandoned; identity and checkpoint
  share the cutover transaction, followed by the resource sequence below.
- `R`: Committed(Prepared) / Activated or Abandoned; persist preparation once.
- `N`: Committed(NativeIntent) / Activated or Abandoned; one native request, then
  Proven on a trusted result. No response by deadline takes A, never resend.
- `B`: Committed(Proven) / Activated or Abandoned; complete-row recovery/CAS and
  installation, then V. Installed is an observable durable boundary even though
  operation progress remains Proven; it must be exercised separately.
- `L(x)`: atomically fence/adopt the exact legacy owner into x, then that cell's
  terminal. Adoption failure/contradiction takes A using immutable original scope;
  it must not depend on decoding a healthy/current successor. Never run children.
- `D`: unchanged persisted state / Abandoned, unless read-back finds Finished or
  Activated already committed (return that terminal). Stop effects immediately;
  on the first successful repair/reopen settlement transaction take A.

Proposed finite policy: one 30-second budget per external reconciliation/install,
and a persisted overall deadline of 5 minutes from admission, also for legacy
adoption from its first settlement admission. A handoff-owned lifecycle callback
on the existing runtime wakes overdue owners; boot enumerates active reservations
and takes A before ordinary harness recovery. No generic runtime handoff branches,
new executor or second journal. Store deadlines in the existing operation's typed
progress/input contract; retries and restarts cannot extend them. Even an original
live drive that never receives another event expires to A.

Storage liveness is necessarily conditional: no protocol can commit a terminal
while SQLite is permanently unwritable or the service never runs again. D has a
finite recovery action (one successful settlement transaction), not a claim of
bounded wall time during outage. An unavailable database rejects lifecycle actions;
no volatile unlock is reported. All other bounds assume writable storage and fair
owner scheduling. Accept this premise explicitly rather than promising impossible
unconditional storage-failure liveness.

Columns: `OK` = successful next effect/read-back; `Fail` = definitive failure;
`?` = Unknown/missing answer; `DB` = storage failure; `Boot` = process restart;
`Cancel` = audited user cancellation. `S-*` and `T-*` are source/target lifecycle
actions; T also includes the created successor. Each lifecycle cell takes A before
its action, subject to the admission rules below. Concurrent events serialize;
a committed terminal wins. A nonsensical effect for a row is a protocol failure → A.

| Persisted nonterminal / boundary | OK | Fail | ? | DB | Boot | Cancel | S-reset | S-restart | S-archive | S-hibernate | S-delete | T-reset | T-restart | T-archive | T-hibernate | T-delete |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Admitted operation, no tx_output | P | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Reserved | P | C | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Pausing: source Intent | P | C | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Pausing: source Paused, target Untouched | P | C | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Pausing: target Intent | P | C | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Pausing: both Paused / validated cutover | I | C | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Compensating, either claim Untouched | C | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Compensating, either claim Intent | C | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Compensating, either claim Paused | C | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Compensating, either claim Released(any outcome) | C | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Compensating, both settled | F | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Committed, Unprepared | R | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Committed, Prepared | N | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Committed, NativeIntent (call still live) | B | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Committed, NativeIntent (orphaned call) | A | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Committed, Unknown | A | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Committed, Blocked(resume Prepared) | N | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Committed, Proven | B | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Committed, Blocked(resume Proven) | B | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Installed, final checkpoint absent | V | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy coordinator: Reserved | L(C) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy coordinator: SourceIntent | L(C) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy coordinator: SourcePaused | L(C) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy coordinator: TargetIntent | L(C) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy coordinator: BothPaused | L(C) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy coordinator: PlanRecorded | L(C) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy coordinator: CommitIntent, cutover absent | L(C) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy coordinator: CommitIntent, cutover found | L(I) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy coordinator: Committed | L(I) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy coordinator: Compensating | L(C) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy ReleasedNeedsRecovery | L(C) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy Finished, active/unsettled reservation | L(C) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy cutover: uncommitted intent | L(C) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy cutover: committed, activation absent | L(I) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy activation: AcceptedPending | L(R) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy activation: Preparing / Blocked(false) | L(N) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy activation: ResourceRequestRecorded / Unknown | A | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy activation: ResourcesCheckpointed / Blocked(true) | L(B) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy activation: NeedsRecovery | L(R or B from proof) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy installed, final activation absent | L(V) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |
| Legacy terminal receipt, active reservation | L(F or V from receipt) | A | A | D | A | A | A | A | A | A | A | A | A | A | A | A |

Claim rows apply independently to source × target: all 16 Claim combinations,
all three ReleaseOutcome variants, and Paused/FaultedPaused/RecoveryHealthUnconfirmed
snapshots. Both-settled takes precedence; any unsettled claim uses C. An impossible
Pausing combination or contradictory/malformed legacy progress takes A, preserving
raw evidence. NeedsRecovery with sending intent uses A; without intent/proof uses R;
with valid immutable proof uses B. Technical Pending/Failed/Stuck/AwaitingRetry and
lease loss overlay every row; none is a business terminal or resets its deadline.
Cutover commit-unknown requires authoritative SQL read-back before interpreting
absence: found Committed continues I, found terminal returns it, read error uses D.
Historical Finished/Activated with inactive reservation never changes to Abandoned.

## Abandoned: authority, transaction and recovery

Abandoned means failed closed, not successful compensation or provider health.
Automatic triggers are an untrustworthy continuation, Unknown, lost owner/binding,
expired budget, boot interruption or unrecoverable validation failure. Audited
cancel and admitted destructive lifecycle actions trigger it immediately in every
nonterminal, including Committed. No trusted provider reconciliation is required:
we revoke local authority rather than assert the external effect did not happen.
Unknown remains Unknown evidence. Provider cleanup is best effort, never a gate.

The handoff owner exposes a proposed `abandon_tx` to domain lifecycle composition.
Under BEGIN IMMEDIATE it reads the exact reservation/input/progress and committed
cutover facts, fences the old operation lease/attempt, and performs these writes:

1. Preserve original scope, claims, receipts, typed releases, configuration,
   native intent, immutable resource/adoption/cutover proof, complete session
   rows, transcripts, queue/ACK/floor and diagnostic error. Abandoned carries the
   prior typed progress, reason, initiator and time in the SAME operation result;
   legacy rows remain immutable evidence through the existing adoption mapping.
2. Set owned source/target/successor rows to superseded; null their token hashes,
   remove their card-token grants, exclude dispatch/recovery/install,
   and fence outstanding pause, resource-bind and snapshot writers. Do not resume
   a paused owner, clear an Unknown fact, invent Released or delete provider data.
   Already-created provider threads may remain orphaned; known IDs are retained.
   Unbound credentials from a late mint never pass final SQL authorization.
3. Preserve the single Planner **card** selected by committed SQL: source before
   cutover; target after cutover. Preserve the source history role after cutover;
   never promote the old target assistant or restore retired source privileges.
   Keep its retired root/current pointer as the non-executable recovery anchor;
   remove the pending-activation payload marker under exact abandonment proof.
   A newer valid current Planner stays current: never overwrite its pointers or
   revoke its unrelated grants. Never roll back a committed role from progress.
4. Checkpoint absorbing Abandoned and set reservation active=0, checking exactly
   one affected row/read-back. SQL terminal admission checks exact original scope,
   owned-session retirement/token revocation and the surviving Planner anchor;
   a JSON state label alone cannot release exclusion. All writes commit together; any
   ignored write, CAS loss or error rolls everything back. Unknown commit answers
   read the terminal before retry. Old and new drivers cannot both own authority.

For a surviving open Track, use the existing history-preserving recovery route:
`POST /api/cards/{id}/planner/restart` → `fresh_start_planner_card` →
`start_harness_card` → `planner-harness-start` → `prepare_tx` →
`session_prepare_deferred_planner_tx`, then normal mint/install. Explicit reset
uses the same path with transcript clearing enabled; retained handoff evidence must
survive transcript clearing (immutable session facts/tombstones, no second log).
The proposal must extend this owner's retirement/recovery admission to the retired
anchor, including formerly pending_activation rows; today's pending guard is not
silently bypassed. Abandoned predecessor restoration in start compensation is
forbidden: failed recovery leaves the same card retryable with a fresh session.
Lazy/boot recovery must not reinstall an abandoned session. Recovery transfers the
complete queue once using existing harvest/CAS rules; it never copies stale output.

Exactly one Planner role/recovery anchor survives; normal start fencing, current
binding generations and final SQL authorization grant at most one fresh executable
Planner. There can be zero live Planners during failed-closed recovery. Claiming
otherwise would contradict revocation under provider failure. Availability requires
an ordinary restart and a working provider; there is no permanently ownerless Track
or handoff-specific repair command. Deleted/closed Tracks need no live Planner.

## Lifecycle coordination and SQL changes

Choose same-transaction abandonment before session replacement, not rejection of
reset/restart. Domain callers invoke abandon_tx before any supersede, queue harvest,
archive/hibernate or destructive write; normal SQL guards reject callers that skip
it. No provider I/O occurs under the transaction. Stop/detach old handles after
commit; all their writes are already fenced. Cancellation must drive settlement,
not just persist a flag. Old current-binding health is not a prerequisite for A.

Preflight ordinary lifecycle permissions first. For an admitted action on either
participant/successor, abandon and mutate atomically. If an ordinary guard refuses
(e.g. deleting the surviving Planner/history card), leave the action unchanged;
the user can cancel to A, then restart the surviving Planner or close/delete the
Track normally. Archive/hibernate must preserve its recovery anchor; restore/wake
precedes ordinary restart. Never cascade-delete handoff facts; retain tombstones
and immutable evidence even when the Track is explicitly deleted. This extends
today's evidence lifetime where session/Track cascades would otherwise erase it.
User-visible cost: interruption cancels transfer, retires affected sessions and
may require restarting an assistant too. No stale permissions are restored.

Implementation migration scope is **only unreleased 0173–0178**, no 0179 and no
edits now. Update their existing definitions together:

- 0173/0174: narrowly permit pending_activation retirement and current-link changes
  for exact abandonment; retain activation proof, never a generic start bypass.
  Retain existing identity rows as tombstones instead of evidence-erasing cascades.
- 0175: retain active uniqueness, no-rearm and active close/delete fences; allow
  only the owner's atomic terminal path to release, and guard lifecycle bypasses.
- 0176: add Abandoned to terminal absorption/release; immutable retirement scope
  must survive its wrapped prior progress, even when resources were never inserted.
  Final activation/resource binding still needs original intent, live lease,
  active reservation, exact current roles/root/configuration and immutable proof.
- 0177/0178: Compensating may advance only to Compensating, Finished or Abandoned,
  never back to Pausing/Committed. Include Abandoned/adopted legacy owners in
  exact-request late-claim fences and prevent late resource proofs/installs or
  old start compensation from restoring abandoned sessions. Freeze terminal scope.
  Preserve existing-receipt health evidence without allowing new authority.

SQL writer serialization chooses activation or abandonment; losing callbacks may
record diagnostics outside immutable proof but cannot grant credentials, change
roles or recreate claims. Keep final role/current-binding authorization in SQL;
caches and provider cleanup do not authorize. Policy stays in handoff, lifecycle
owners compose its transaction contract, and generic kernel stays domain-neutral.

## Additional gaps exposed by the matrix

- Blocked(Prepared/Proven) has no deadline; readiness failure can retain exclusion.
- Missing registry owner, resolve failure or changed binding can keep Compensating
  forever, even without a user reset; technical retries do not repair ownership.
- An admitted operation without output or a lost lease needs scheduled settlement;
  read-only query and dropped original callers cannot provide liveness.
- Archive/configuration/root changes invalidate forward proof just like reset;
  the Installed window makes an already-live successor a revocation obligation.
- Strict legacy adoption rejects contradictory proof/current binding; it needs the
  same evidence-preserving abandonment path, including standalone cutovers and
  terminal legacy receipts whose reservation/claims are still unsettled.
- Repeated storage ABORTs and failed recovery-start compensation must not erase
  abandonment, restore old sessions, or reset budgets. Track delete must preserve
  audit/resource evidence after exclusion releases.

## Verification and decisions

Add one parameterized production-entry test in the existing handoff run-loop
suite: begin through real audited coordinator admission and SQLite/provider stub;
reach each matrix boundary using seams, not seeded successful receipts. Enumerate
Claim pairs/outcomes, resource variants and explicit legacy decoding cases; inject
one column event/fault per case (both lifecycle sides and successor separately).
Drive the owner with a fake clock through the maximum budget; reopen file-backed
SQLite for boot/DB cases, remove the injected storage failure, then assert a
business terminal and active=0 within the finite drive bound. Assert zero resends,
absorbing replay, immutable facts, no old credential/current-role authority and
exactly one recoverable Planner card. Restart that card through the real route
and assert one fresh Planner; exercise close/delete and retained audit separately.
Race late pause/bind/install writers on both sides of the terminal transaction.

Use exhaustive matches over owning Rust enums so a new variant requires a case;
keep event cases local to this domain test, no global inventory of test names.
Legacy versions have explicit fixtures. Ordinary variants change their owner and
this test; a protocol/event addition also updates this matrix. Run the focused
suite once locally and once in its existing gate replay, not a second permanent
CI job. Mutation-verify release atomicity and late-writer rejection with predicted
complete red sets across the whole focused target. Broad CI remains unchanged.

Planner decisions requested: approve immediate Unknown abandonment versus a
provider reconciliation grace period; approve 30-second/5-minute budgets and boot
abandonment; approve retiring even untouched participants; approve tombstone
retention after deletion and the explicit-restart availability premise. The
proposed defaults above are coherent but intentionally trade continuity for
bounded exclusion. Implementation must receive L2 review of the complete call path.

Non-goals: user entry points/UI are a later PR; no code/schema changes here, new
provider idempotency protocol, guaranteed external resource deletion, restored old
authority, second journal, generic kernel special cases or automatic healthy-provider
claim. This design supplies the internal settlement contract for that later work.
