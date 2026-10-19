# Planner conversation handoff

Status: internal identity cutover and explicit forward activation are implemented internally;
final frozen-target verification is recorded in the per-attempt kernel gate logs. The transaction
write-authorization slice and durable system-observation acknowledgement foundation are
implemented, together with the internal durable pause preparation primitive described below.
This revision supersedes the blank-conversation
proposal and follows the existing-conversation investigation, completed event **402085**
(`7e93be68c1b94d6dac678f815f5602d9:existing-conversation-handoff-contracts`), and the
backend task context. Review tier **L2**: authority, isolation and persistence boundaries.

The internal durable coordinator composes these accepted owners; its complete state,
reservation, generation, cancellation and recovery contract is documented in
[Durable internal handoff coordinator](planner-handoff-coordinator.md). External
REST/MCP/AI/UI ingress remains outside this delivery. Migration 0175 adds only the
domain reservation and generation counters; migrations through 0174 remain frozen.

## Internal pause preparation primitive

The harness now exposes a Rust service API for single-session preparation only:
`pause_for_handoff(PauseRequest)`, `query_handoff_pause(PauseRequest)` and
`resume_from_handoff(PauseReceipt)`. No REST route, AI tool or user input creates a claim.
The internal cutover plan binds the Neige claims and exact card/session/Track requests.
No external caller is wired to this service.

Commands use the real run-loop channel, ingress fence, issuance lock and snapshot owner.
Only a current managed Planner or Assistant with a declared matching provider/contract can
pause. Idle admission rejects user queue entries, active/issuing/compacting work, held
responders, provider authentication holds, pending projection/rewind, unconfirmed maintenance
and sending/unknown native submissions. Running task workers are independent. System queue
entries are allowed. The transaction revalidates current SQL binding and native submission
ownership before persisting the marker; the ACK follows commit. It uses the existing snapshot
writer to checkpoint the original owner's complete queue and watermark together with the
marker, retaining its identity, contract and credential.

### Receipt, health and permission decision (533c304d follow-up)

L2: this revision changes persistence and the read/write authority boundary. The latest
claim alone lives in the existing snapshot; there is no receipt ledger or new migration.
`Released { receipt, outcome }` is a durable, non-active terminal result. A healthy outcome
records the releasing live owner's evidence; it is not a readiness promise for later owners.
`RecoveryHealthUnconfirmed { receipt }` explicitly means `HealthUnconfirmedAfterRecovery`,
not a fabricated ThreadSystemError. Rebuilding a paused owner never inherits the previous
owner's health proof. This task deliberately adds no provider health RPC/probe protocol. Future handoff callers
must abandon preparation on FaultedPaused, RecoveryHealthUnconfirmed, or either nonhealthy
release outcome, and request explicit recovery. A receipt alone proves no provider health.

| Durable state | Read original claim | Pause same claim | Release same claim | New claim |
| --- | --- | --- | --- | --- |
| legacy absent / Unpaused | Unknown (no result evidence) | current+active idle admission | Conflict | current+active idle admission |
| Paused | exact receipt | receipt, no write | Released/HealthyReleased, same transaction | Conflict |
| FaultedPaused | exact receipt + original provider fault | receipt, no write | Released/ReleasedWithBackendFault + Wedged/Failed, same transaction | Conflict |
| RecoveryHealthUnconfirmed | exact receipt + health uncertainty | receipt, no write | Released/HealthUnconfirmedAfterRecovery + Wedged/Failed, same transaction | Conflict |
| Released | exact receipt + original typed outcome | Conflict (never re-pause) | original outcome, no write | replace latest terminal only with current+active idle admission |

The repo-backed durable query does not require a live handle, registry, run-loop scheduling,
current pointer or active authority. Its single atomic SQL SELECT validates the exact requested session's
immutable card/Track/managed contract and projection storage relation, and validates the
snapshot receipt against that same original identity. It never redirects to a current newer
session. Missing/legacy result is typed Unknown; a different latest claim or bad relation is
an explicit conflict; database unknown/error is an error. Reading grants no write authority.
Every actual pause/release mutation retains current+active managed binding and snapshot CAS.
Same-claim terminal release is a read of the original result even on Failed/retired owners.
Terminal results do not impose the active-pause fence on normal system/snapshot writers.

| Failure / recovery boundary | Required state and action |
| --- | --- |
| observed provider fault, commit succeeds | quiet FaultedPaused, original fault retained across every rebuild |
| observed provider fault, trigger ABORT / DB unknown | sticky in-memory fault, stop old owner writes; never undo the observed fact |
| replacement / actual file reopen sees healthy Paused | recovery owner converts to RecoveryHealthUnconfirmed, persists under complete source CAS before install |
| replacement sees persisted fault or health uncertainty | preserve kind; never infer health from SQL phase or backend open |
| release commits, reply dropped / caller cancelled / owner exits | exact Released outcome remains independently readable, including Failed |
| release write/status abort | transaction rolls back result+phase+status; full before-image reconciliation before any continued write |
| release DB unknown | stop writes; durable query resolves original claim or reports DB error, never guesses healthy |
| wrong claim/session/card/Track | conflict, no retarget or authority grant |
| new normal claim supersedes latest Released | old query conflicts; no full history retained |

Decoder compatibility explicitly accepts missing legacy Unpaused, old healthy Paused and
old FaultedPaused. Null, malformed identity, missing required outcome/fault, illegal quiet
checkpoint refuse recovery. Released outcomes
remain historical facts when normal subsequent writers change phase; they are not a new
phase fence. Both fault and unconfirmed-health release use the existing failed recovery
owner; health uncertainty has its own reason and never invents a provider error.

### Pause writer transition audit

| Writer / event | Existing target and field ownership | Paused transition |
| --- | --- | --- |
| ThreadSystemError | Thread identity filter; no turn-target exemption | Healthy pause -> faulted pause; repeated fault stays faulted; durable commit before another command |
| ThreadStarted / ThreadIdle | Only PendingThreadStart / Resumed can become Idle | Quiet phase is unchanged; never clears fault |
| TurnStarted | Issuance/active/resumed/maintenance expected target; Idle without history is bootstrap only | Bootstrap acceptance requires unpaused; pause has no admitted issuance/maintenance target, so late starts cannot create running work |
| TurnCompleted / TurnAborted | Active/interrupt/maintenance exact target; Wedged system-error terminal settlement only when unpaused | Quiet pause has no matching active target; duplicate/late terminal events cannot change phase or clear fault |
| Item / PlanUpdated / ReplyDelta | Existing thread, rewind, maintenance and projection filters; transcript/live owners | Preserve claim and fault; no phase transition |
| ContextReplaced / TokenUsage | Existing user/maintenance boundary and turn filters; usage/compact facts | Preserve claim and fault; no unconfirmed compact intent is admitted |
| Watchdog / timeout / budget | Requires running/issuing/compacting/resumed state or admitted interrupt deadline | Quiet checkpoint has none; no timeout or automatic health normalization |
| Issue / interrupt / replace / compact / queue edits / human ingress / hibernate | Existing final unpaused admission and issuance owner | Both pause variants refuse; stop target remains fixed |
| System observation / public snapshot | Complete queue-watermark snapshot and marker CAS | Both variants retained; stale healthy capture cannot overwrite a faulted claim |
| Boot / lazy / replacement / ACP recovery | Exact row binding, source checkpoint CAS, native receipt ownership | Conservatively persist health uncertainty for healthy Paused before install; preserve existing fault kind |
| Start replay / harvest / fork / give-back | Row-owned queue and marker; fresh session identity owns a fresh initial snapshot | Replays preserve the complete paused row; new sessions never inherit predecessor claim/fault |
| Failed-system-error settlement / recovery quiesce | Existing Wedged system-error owner, failed-row writer | Unreachable until exact faulted release; then retains original unpaused error contract |

Notification fault staging changes only the typed pause field; on storage failure it retains
the irreversible observed fault in memory and closes ingress for recovery, so no later command can overwrite
an uncertain commit. Release stages marker and state together. Its error reconciliation must
compare the complete checkpoint, including queue/watermark, before permitting more writes;
an unknown result closes ingress rather than restoring only one field. Ordinary future tests
need only edit their owning test module; adding a fault kind edits the owning enum, decoder,
release mapping and transition tests. A new recoverable failure reason also updates the shared
failure vocabulary and the existing SQL/route recovery predicate; no provider-specific
assumption is added to generic lifecycle code. No growing list or additional CI pass is added.

Public stop requests now carry the target sampled by `PlannerHarness::interrupt` into the
run-loop owner. A bounded stop command lane is served both between loop iterations and by
the issuance owner while awaiting `turn/start`; this preserves stop-before-start-return without
reacquiring issuance or waiting on a command that only the blocked loop could serve. Watchdog
stops call the same owner implementation directly. Final admission rejects a paused or retired
owner and requires the sampled turn (or exact start-in-flight issuance) still to match before
changing state, interruption intent, issued id or deadline. A late known target never resolves
to a later turn. Paused stops refuse even with no active target; stale unpaused targets are
no-ops. Provider interruption follows the durable interrupt checkpoint. Checkpoint failure
restores all staged fields, closes ingress and stops further issuance writes for recovery;
it never sends an interrupt on an unconfirmed checkpoint. Provider RPC failure after a confirmed
checkpoint retains the existing completion deadline and timeout intent semantics.

A paused owner remains installed. It does not issue new model turns or accept human sends,
steers, stops, replacements, compaction or queue edits. System deliveries continue through the
existing durable eligible-prefix ACK contract, updating the same snapshot without dropping
the claim. Boot, lazy and replacement recovery reload the original row, conservatively mark unproven health
through catch-up/source CAS, install the backend and remain quiet. Start-operation replay
adopts pause together with the row-owned queue/watermark; a newly minted session starts
unpaused and never inherits an old session's claim. Ordinary live snapshot writes compare
the stored claim inside their transaction, rejecting a stale capture that would erase it.
This adds a row lookup to each live snapshot transaction, not a tick-time global scan.

Identical pause replay while active returns the original persisted receipt without a write.
Released same-claim replay cannot re-pause. Duplicate release/query returns the original
persisted typed result. Cancellation does not cancel the serial owner's accepted command.
No dropped receiver, Drop implementation, process flag or TTL releases a claim. Unknown
commit results close writes until recovery; legacy Unpaused never proves HealthyReleased.

The pause primitive also supports A pausing successfully, B refusing, and compensation
resuming A with A's original receipt. This primitive alone is not a global reservation,
cutover or worker authorization change. The internal operation below composes two exact
healthy receipts into one SQL identity transition; it does not prepare native resources.

The implementation extends existing large run-loop/snapshot/start-adapter files only at their
owning capture, command and replay points; new types, command logic and tests are separate
files below 800 lines. No growing provider/test registry or new CI pass is introduced. Later
providers extend existing backend registration; ordinary test additions need no policy list.
The worker runs the focused selection and single-factor production mutations of final pause
and target admission (using separate isolation cases so one guard cannot mask the other); the kernel
gate owns text, quick, contracts, focused replay and transaction authorization regression.
The Planner arranges the two independent L2 reviews, and CI owns broad suites.

## Explicit forward activation contract

Review tier L2: activation crosses authority, credential and durable resource boundaries.
The accepted cutover result is the sole authority source. An internal explicit operation
binds its original succeeded cutover row, validated plan and user audit, exact new current
Planner session, root, ordinary open Track and frozen configuration. An operation row alone
is not approval. No REST/MCP/AI/UI entry is added; the internal coordinator is described separately. Generic startup,
reset, lazy recovery and ACP recovery keep rejecting PendingActivation and history.

| State | Durable evidence | Explicit next action |
| --- | --- | --- |
| CommittedPendingActivation | original cutover result and inert new snapshot | register the one activation for that cutover |
| Blocked | provider owner's readiness/authentication refusal; no resources granted | explicitly retry the original activation generation after owner clears hold |
| Preparing | trusted briefing and complete pending queue/watermark frozen before issuance | check exact binding and owner readiness before effects |
| ResourceRequestRecorded | durable intent before native RPC | a crash or missing reply is Unknown; never repeat an unproven request |
| ResourcesCheckpointed | returned provider identity and fresh credential binding committed with operation checkpoint | reuse only this proven resource, catch up queue and install |
| NeedsRecovery / Unknown | original operation and last known checkpoint remain queryable | explicit forward recovery; never restore old roles, tokens or task identities |
| Activated | final SQL binding/resource/checkpoint proof and successful harness installation | ordinary issuance contract, including later asynchronous authentication holds |

The adapter exposes internal `activate`, read-only `query`, and generation-fenced `retry`.
`query` reads the original operation without driving it, including accepted Pending and
stored recovery failures. The operation's original audited cutover, prepared briefing and
resource proof are permanent. The new session stays PendingActivation through Blocked or
Unknown before binding. Resource binding commits Starting, credentials, transport metadata
and the operation checkpoint together. Starting is an active SQL binding, not an installation
success. `Activated` records a completed installation and final exact SQL validation; it is
historical evidence, not a promise of current provider readiness. Blocked separately records
whether resources were already checkpointed. Later authentication holds still fence issuance.

Cancellation or a dropped reply never deletes accepted durable work. A post-cutover
failure has no backward compensation. Native resource side effects and SQL commit cannot
be one atomic transaction: installation/cache publication does not authorize the final SQL
write, and a missing RPC result proves neither success nor absence. Codex thread/start has
no durable client-key receipt in the current public contract; an interrupted request must
remain Unknown until a separate explicit recovery can prove its result. Local providers
retain their existing first-turn native creation and credential ownership.

| Briefing / queue input | Owner and persistence rule |
| --- | --- |
| Source/target identity | original cutover plan/result; label both cards and retired sessions plus new session |
| Report, template and task facts | existing Report/template/task repositories; server reads only, never caller JSON authority |
| History excerpts | existing transcript repository, read only; combined <=12000 Unicode characters with source references and explicit truncation boundary |
| First input | trusted SystemContext, <=32768 Unicode characters total, durable in new snapshot before any issuance; explicitly says context reconstructed and native history not retained |
| Pending observations <= cutover floor | preserve entire cutover queue, deduplicate by existing envelope ownership; never discard merely because watermark covers them |
| Activation suffix | existing eligible-prefix event reader and complete source-checkpoint CAS; re-read after quiescence, never overwrite an ACK |
| Reopen / replay | row-owned snapshot wins over operation's older capture; first briefing is inserted once and never reinserted after ACK |

The shared start adapter owns minting, transport payload normalization and harness installation.
Its registry reservation quiesces any prior installed owner before activation catch-up and
complete source CAS; row adoption then precedes installation under that same reservation.
The final Activated result is also checkpointed before advancing OperationRuntime, respecting
its existing refusal to overwrite a third callback checkpoint. Generic startup keeps its
original admission and does not request activation catch-up. Migration 0174 narrows only the
explicit transition and permanently fences both retired identities; 0173 is unchanged.
A pending claim alone, a native cache entry, or registry installation never grants SQL authority.

### Start capture and same-session restoration ownership

The ordinary start operation owns its initial phase/thread capture; its row owns queue,
ACK watermark and report deduplication. Activation installation, including the first install,
is same-session recovery: after registry quiescence the complete current durable snapshot
owns **all** fields (phase, turn/thread, usage, rewind, projection, interruption, pause,
queue metadata, floor and deduplication). The operation supplies exact resource/binding
proof only. Separate typed installation purposes select these contracts; a shared boolean
must not decide phase/thread ownership. Boot/lazy replacement and activation share the
recovery candidate owner, ACP reconciliation, pause-health invalidation, eligible suffix
catch-up and complete-source CAS. The exact accepted candidate goes straight to installation;
a later row read or field merge cannot replace it. CAS refusal leaves recovery to the owner.

| Captured durable phase / evidence | Same-session restoration | Issuance constraint |
| --- | --- | --- |
| Idle / TurnCompleted | original `state_from_snapshot` | preserve committed rewind and apply it before replacement input |
| TurnRunning / IssuingInterrupt / Resumed | original Resumed reconciliation | shutdown does not prove native termination; no immediate duplicate turn |
| IssuingTurn | original state restoration using durable turn identity | preserve projection/queue claims; no invented terminal receipt |
| Compacting / Wedged | original conservative recovery | no forced Idle |
| Paused / faulted pause | complete row plus unconfirmed recovered health | original pause/health owner gates release and issuance |
| ACP sending / unknown | ACP receipt recovery | retire receipt-owned input and preserve typed unknown outcome |
| ResourceRequestRecorded / Unknown without resource proof | activation operation stays Unknown | never retry native resource creation blindly |
| Cancel/crash after proven resource binding or failed Activated checkpoint | explicit original-operation recovery | quiesce, complete snapshot CAS, then install; no resource/briefing remint |

Registry reservation remains after predecessor shutdown, before candidate capture and through
installation. Card start/mint, Track deletion and issuance retain their existing lock order;
no SQL grant or pending-session admission is broadened. A swallowed shutdown interrupt error
only means the old local loop stopped. It cannot justify discarding a live turn's identity.

The new direct binding snapshot write joins the existing writer classification as row-owned.
Preparation uses the existing snapshot writer, with complete source comparison and read-back
under BEGIN IMMEDIATE; an ordinary later direct writer updates that inventory once. Migration 0174 joins the existing
head-schema fixture once. Adapter registration updates the existing non-task classification.
These existing checks keep their existing CI pass count; no new scan or growing provider/test
list is introduced. Existing large start-adapter methods are extracted in their owning module
so both callers share them; new responsibility and test files remain below 800 lines.

Worker focused verification uses real OperationRuntime, SQLite, registered backend stubs
and notification seams. Critical production mutations cover exact authorization, unknown
resource non-repetition and durable queue/briefing ownership with predicted complete red
sets. Kernel owns quick/text/contracts/focused replay and generated consumer gates; CI
owns broad suites. Planner owns independent L2 review. No growing test/provider allowlist
or extra CI pass is introduced by this contract.

## Atomic cutover scope and stages

Review tier L2: the internal kernel changes persisted identity and write authority.
This slice exposes only a typed internal user-audited service and registered tx-only
OperationRuntime adapter. The internal coordinator composes it through the same keyed
transaction owner; REST, MCP, AI and UI ingress remain absent.
Running workers and all task, attempt, claim, gate, delivery, approval and user-release
identities are outside this transaction's write set.

1. **Validated plan:** capture immutable configuration through the profile, model,
   template and permission owners. Bind Track, exact source Planner and target managed
   Assistant card/session IDs, both exact healthy pause receipts, plan/key identity and
   a fresh kernel session ID. Same provider is required. Target keeps its own model,
   effort and ever-set choices; source supplies its validated template context and the
   Track retains its installed template. Planner permission uses mint_at_creation.
   Precheck creates no native resource, token, thread, harness or model turn.
2. **CutoverCommittedPendingActivation:** one BEGIN IMMEDIATE re-reads the complete
   SQL binding, configuration, ordinary open Track, unique source Planner, both active
   current managed identities, exact healthy pause checkpoint and unresolved provider
   receipts. Missing, malformed or unsupported facts refuse without defaults. Healthy
   Paused records the serial issuance/hold owner's pause-time admission; it is not
   proof that a later provider-wide authentication hold is absent. Cutover makes no
   such readiness promise; activation and issuance retain their own hold checks.
   Faulted, recovered-unconfirmed and released claims cannot authorize cutover.
   Snapshot CAS, native submission checks and final SQL authorization fence stale work.
3. **Activated:** the separate internal `handoff-activation` operation consumes this
   committed result, creates fresh resources through the shared Planner start owner on
   the fixed new session, and installs its harness. Activation failure requires forward
   recovery and cannot restore old roles. The internal coordinator invokes this owner; no external entry exists.

The sole commit demotes source to explicit planner_history before promoting target,
retaining the unique Planner index. History has no write, startup or recovery profile.
Server-owned history metadata replaces the source planner_harness marker. Both old
sessions are superseded and their credentials revoked. Their pause receipts become
ConsumedByHandoff with original binding, plan and new owner, never HealthyReleased.
Original receipt queries remain read-only on the exact retired session.

Target card, transcript physical rows, item IDs and attachments stay in place. Any
pending target user entry refuses. Unsupported target system queue ownership refuses
rather than silently discarding valid observations. The fresh Planner session receives
all source pending system QueueEntries with original identities, envelope/observation
references, report dedupe and trusted push watermark, including entries at/below that
floor. Fresh snapshot has no predecessor claim, thread or token. Suffix catch-up uses
the existing snapshot owner and cannot read beyond its triggering event.

Operation keyed outcome/checkpoint and all identity changes share this transaction.
Same key and scope returns the original exact IDs/configuration/snapshot references,
including after lost reply and database reopen; changed scope conflicts. Stale plans
cannot replace a later owner. Errors at any stage roll back all business changes and
operation rows. Cache publication follows success only; final SQL reads remain the
write authority, including for both old pre-admitted sessions. No rollback may publish
a grant. The result names pending activation explicitly and never claims readiness.

Migration 0173 follows the actual delivery base (0168),
with every released migration byte-frozen. Extend role declarations, validators,
triggers and generated consumers through their owning contracts. Ordinary additions
need no growing provider/test registry. Activation and coordination now compose these primitives internally. External entry
points remain separate work.

### Provider admission belongs to activation and issuance

SQL cutover commits identity only. A provider-wide authentication hold can arrive after
both pause acknowledgements without changing their SQL markers. Cutover may still commit
`CutoverCommittedPendingActivation`: it promises neither provider readiness nor permission
to start a thread or issue a turn. The external authentication owner is asynchronous and
is not an atomic prerequisite for this SQL transaction. No cross-store health protocol,
authentication-file decoder, new lock or copied provider rules belong in this adapter.

The original `PlannerBackend::issuance_hold` and provider activation checks remain in
place. The explicit activation owner rechecks them before creating resources or
credentials. Its failure requires forward recovery on the new identity; it cannot restore
old roles. The persisted `PendingActivation` session carries no active authority, thread,
token or harness. Generic boot, lazy, ACP and start paths cannot activate or replace it.

The registered internal tx-only operation and typed service implement the identity stage.
There is no external REST/MCP/AI/UI entry point, nor any provider allocation in this slice.

## Durable system delivery

Extend existing observe/snapshot ownership with durable acceptance and exact current-binding
validation. A successful in-memory channel send is not an acknowledgement. Dispatcher may
advance its cursor only after entry and watermark persist under that binding. A late old
owner rejects and dispatcher resolves the current owner again. Reuse its original eligibility,
catch-up, stale-stop and deferred-self-report policies; add no second
replay dispatcher. Transfer queue contents independently of watermark, then catch up only
the suffix beyond the transferred floor.

The watermark promises a **durably processed eligible prefix**, not merely the highest
individually accepted event. Under the existing track push fence, every mapped live envelope
(including ordinary Forge notifications and settlements) reads the current owner's SQL floor
and uses `catch_up::observations_since` through that committed envelope, inclusive. The same
eligibility, deferred-self-report, stale-worker-stop, role and observation resolver select the
prefix for live delivery and boot catch-up. IDs are globally ordered but need not be adjacent.
The cache is reseeded from SQL and cannot skip a gap, including after reset or owner replacement. Deliver each
selected entry in order and wait for its durable ACK before sending the next. Any lookup,
mapping or ACK failure stops the prefix; a later eligible envelope retries from SQL without
requiring redelivery of the failed broadcast. Entries already queued at/below the floor remain
pending. No separate retry queue or processed-event ledger is introduced.

The ordinary live path adds one track/kind event-slice query bounded in SQL by
`floor < id <= envelope_id`; it normally returns only the current event. A cold owner reads its
unconsumed suffix, never the already acknowledged history or events after the triggering
envelope. Role, deferred-task, stale-hook and mapping lookups retain their existing per-kind
costs. The initial live eligibility checks still run, so task/hook triggers also pay the
shared prefix's deferred/role/stale consultations for their current entry. The existing current
observation resolver still runs before provider wake, preserving no-wake behavior for unmapped
hold answers; the prefix resolver re-reads enrichment, so completions/settlements also pay a
second result-contract/retained-task lookup for that entry. Each selected entry
retains one snapshot transaction/ACK; prefix lookup currently
materializes the bounded suffix before delivery. Boot catch-up has no upper bound and reads
the suffix after its durable floor. Event kinds and eligibility remain in their existing owners,
so adding a kind does not require a second prefix whitelist or a new CI test-name registry.

The crate-private `observe_envelope` is the low-level operation for an already proven prefix
entry; only Dispatcher uses it in production. Fixture-only access tests snapshot ownership
and failure contracts, and cannot be used by production integrations. It awaits the existing
run-loop command owner. Queue and push watermark
commit in one snapshot transaction, which checks active session, current card/session pointer,
same Track and SQL Planner role. Dispatcher advances its cursor after that durable ACK; its
recovery floor comes from SQL rather than staged memory. A stale binding allows one bounded
retry only when the resolved card/session owner changes. Storage failures preserve replay;
staging restores queue, watermark, debounce, report hash and hook dedupe together. An unknown
commit outcome is compared against the complete persisted snapshot; an unresolvable outcome
refuses further writes. Duplicate envelopes retain pending entries at/below the watermark.
Public snapshot writes share the existing durable-ingress and issuance fences; envelope
staging holds issuance through commit even if its ACK receiver disappears. The snapshot owner
serializes full-snapshot capture through commit, including notification and external writes. Ordinary non-enveloped `observe` retains
its existing asynchronous enqueue contract, as do folding, filtering and backpressure policies.

Recovery uses the same durable ownership boundary across boot, deferred and lazy/replacement
entry points. A projection supplied by a caller is an eligibility hint. Before any catch-up
snapshot write, recovery takes the track deletion fence, validates lifecycle/readiness and
claims the registry slot. Deferred recovery skips an occupied slot without replay writes;
replacement waits for the previous live owner's ingress and issuance shutdown. Only then
is the original session's complete SQL snapshot reloaded, including pending entries below
its watermark. ACP receipt reconciliation also runs after this claim and reload.

Catch-up only builds a candidate in memory. Its final write transaction compares the complete
source checkpoint and the card's exact current session with the captured session; it never
looks up a newer current session as a write destination. A changed checkpoint/current session
returns an explicit skipped recovery and installs no candidate. The next normal recovery
attempt must reload; there is no partial queue merge or silent success on conflict. The
installed harness receives the same snapshot accepted by that transaction. Deferred readiness
is checked again after catch-up before writing. Recovery keeps card-start -> track-delete ->
registry order, adds no SQL-only mutex, and never acquires a card-start fence while holding
the track fence. No process registry map guard or SQL transaction spans provider RPC.

The existing live-loop snapshot lock still owns capture-through-commit inside one harness;
it is not a global lock on every session writer. Recovery's complete-source comparison fences
other writers at the durable boundary. Metadata/queue-transfer writers read and modify the
owning session's row inside their existing transactions; they do not retarget a carried recovery
snapshot. Start-operation replay keeps the row's queue, push watermark and report dedupe
state together while applying its own phase/thread decisions. Dev replay rechecks a missing
checkpoint inside its write transaction rather than replacing a checkpoint a live ACK repaired.
This observation owner does not promise automatic in-process recovery from an unknown
storage outcome. Explicit activation reuses its suffix catch-up and complete-source CAS.

This observation foundation is used by current production delivery. The internal cutover
reuses its complete queue and watermark representation. Neither owner installs a candidate
provider or exposes an external handoff entry point.

## Verification and maintenance boundaries

The internal service commits through the real production-registered `OperationRuntime`.
`handoff_cutover` tests exercise exact SQL pause/identity/configuration revalidation,
full-store rollback, both old token and admitted transcript writes, complete queue transfer
at/below the watermark, concurrent keyed replay, changed scope, lost-answer file reopen,
and a real authentication-owner hold arriving after pause. All registered provider wrappers
pause and commit without native starts; a handoff-specific suffix test uses the shared reader.
Boot/replacement skips the new inert identity. The existing focused recovery tests cover the shared suffix/catch-up owner.
The accepted explicit activation owner supplies fresh resources; the internal durable
coordinator now composes the complete authorized flow. External entry remains deferred.

No new provider list, test list or CI pass is introduced. A later session state changes its
owning Rust enum/SQL schema and typed consumers; a later test changes only its module.
The new inactive state does not join generic active-authority or recovery predicates.
REST revision 34 protects bundles against new enum values; no new event wire is emitted.
History authorization belongs to the shared role/profile owners. SQL guards reject late
card/transcript mutation and generic replacement of a pending identity. Content remains
read-only while FK cleanup and deletion of its owning Track retain their existing contract. The session owner
links the inactive Planner root without mirroring any token. The target keeps physical
transcript rows, item IDs and attachments; unsupported Assistant queue ownership refuses.

Worker runs only that focused filter and `calm-truth --lib decision_gate::tests` with
`NEIGE_CODEX_BIN` unset, `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=6`, `--locked --no-tests fail
--no-fail-fast --test-threads 8`. For critical healthy-receipt and queue/watermark assertions,
predict the complete red test-name set before each single-factor production mutation, run
the entire observing module, compare exact actual failures, restore safely and confirm green.

Run real `npm --prefix fe run gen:api` for schema changes and include all generated outputs;
read FE layer guidance before changing its consumers. Worker fixes formatting/lint. The
kernel gate owns text gates, Rust quick/contracts, focused replays, FE lint/build/tests. CI owns broad suites. Never run real Codex/Tier2 on the shared
host. Two fresh L2 review channels are arranged by the Planner, not this worker. No permanent
provider/test-name pin is needed; future providers extend their owning registration.

## Compliant replay foundation

The delivery foundation is upstream `e5a2e4f1793e93af5451c7b8ef05339f648367a1`.
The replay carries the complete Track delta through `5699bcdae1342552c7047d0a033c05bda33e7ad8`,
including authorization, durable notifications, pause receipts, cutover, generated contracts and tests.
The kernel-requested `ceb5802..c5827e16` replay is followed by the remaining retained work;
it is not a last-commit-only replay. Both patches use `git apply --reject`; rejected files use
`git merge-file` with source versions from `git show`. Upstream input inspection and start-adapter
tests coexist with handoff behavior. Terminology conflicts retain the final counted baseline.
All replayed files match the old candidate byte for byte before this documentation update.
Upstream is already an ancestor of that candidate: the new foundation changes delivery ancestry,
not runtime semantics. In that replay, migration 0173 was the only new migration; released files were unchanged.

The kernel creates one commit above upstream with exact frozen-path ownership trailers.
No audit policy, allowlist or injected audit base changes. The new head must pass all six gate
steps; old green steps do not establish that verdict. The verification record and completion
report identify the actual run. That replay delivered the PendingActivation boundary. Accepted activation and the
current internal coordinator extend it; external API/UI and the Planner's two fresh
L2 review channels remain separate work.

FE consumer runs on the new foundation exposed fork-pressure failures in unchanged upstream
tests. Vitest's owning configuration caps workers at two without changing selections,
timeouts, assertions or audit policy. This testing-only difference is documented with the
minimal reproduction and final gate reference in the verification record.
