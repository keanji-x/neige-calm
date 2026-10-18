# Single-owner planner handoff

Design tier: L2, because the change crosses authorization and durable identity boundaries.
This supersedes the three-operation design in `planner-handoff-coordinator.md`.
The accepted outcome is one original user key, operation, progress sum type, explicit
retry generation and absorbing business terminal. There is no new executor, public
API, UI, provider protocol or approval queue. OperationRuntime retains leases,
checkpoint CAS, exact drive and generation-fenced explicit retry.

## Authoritative facts

| Fact | Authority and owning entry point | Retained evidence; copies to remove |
| --- | --- | --- |
| Original user scope, key and exact claims | Immutable versioned operation input admitted under IMMEDIATE; typed internal user begin | Keep original IDs, user audit ID, two PauseRequests, successor ID and approval generations. Remove reservation request hash, child keys and repeated scope in progress/results. |
| First live execution eligibility | Nonserializable capability scoped to the original begin drive, destroyed when that drive exits | Never reconstruct from Reserved, process registry, a fresh flag, lease or explicit retry. Recovery has a distinct typed execution context. No durable approval bit or TTL. |
| Binding generations | 0171 card/Track generation tables and their existing transition triggers | Approval contains the captured generation. It is not another current pointer. Original start compensation can call session_restore_from_superseded_runtime_tx, so timestamps/exact IDs cannot replace ABA generations. |
| Pause claim and health | Original serial harness pause/resolve/resume commands and durable snapshot | Workflow records attempted claims, exact receipts and typed release outcomes as recovery evidence. A receipt is never reconstructed or treated as live health. |
| Roles, current/root pointers, credentials | Existing cutover SQL facts and writes, profile/current-managed-binding and final decision gate | Identity mutation and this operation's Committed progress share one transaction and lease CAS. Cache publishes SQL roles after commit; cache failure cannot reverse identity. |
| Queue, floor, ACK and dedupe | Complete worker session row, existing snapshot writer and observation reader | No workflow queue or recovery copy. Original transferred queue below the floor remains pending. Installation consumes full-row/CAS recovery, Resumed and rewind. |
| Native sending intent and returned resource proof | Resource stage of this same operation plus immutable SQL proof | Intent precedes RPC. NativeUnknown never authorizes resend. A proven resource is recovered/installed from the complete row; never reminted. |
| Runner phase, lease and retry generation | OperationRuntime | Technical Failed/Stuck does not override a committed business terminal. Only this domain opts into safe original-generation continuation. Ordinary adapters retain their contracts. |
| Track exclusion and cancellation | Thin domain reservation indexed by Track and operation ID, monotone cancel flag | Keep exclusion across keys/processes. It is not another work log. Terminal progress and reservation release are atomic. Unknown and unresolved claims keep it active. |

## Required state and execution matrix

Progress is a versioned tagged sum, with required data in each variant. It is not a
stage plus unrelated optional plan, committed result, receipts and booleans. Input
owns immutable scope; committed progress owns captured configuration. Resource state
distinguishes unprepared, prepared, native intent, unknown and proven resources.
Compensation distinguishes no effect, uncertain exact claim, owned receipt and typed
release independently for the two sessions. Runtime errors are diagnostic, not an
alternative source of business facts.

| Durable state | Original live drive | Recovery / explicit original-generation drive | Query / terminal behavior |
| --- | --- | --- | --- |
| Reserved | Original live capability may start source intent after final generation/cancel checks | No pause or cutover; finish without effects | Preparing until resolved; no effect |
| Source intent | Send exactly original source claim once | Query original SQL claim plus serial absence barrier; exact compensation only | Unknown keeps reservation |
| Source receipt / target intent | Original live drive may attempt original target; busy immediately refuses | Reconcile both original claims independently; no new pause | Exact original receipts retained |
| Both paused / validated configuration | Revalidate exact healthy claims and generations in identity transaction | Compensate; never regain original approval from persisted state | No effects from query |
| Committed, resources unprepared | Forward prepare from committed identity | Safe explicit forward prepare on same key/generation after storage repair | Identity never compensated; active reservation |
| Prepared, no native intent | Check readiness, persist intent, then one resource request | Safe explicit same-key forward; hold release alone does not retry | Preparing/Blocked, active reservation |
| Native intent or Unknown, no durable resource proof | Resolve returned result if original call is still live | Unknown only; no native resend | Retain original intent and reservation |
| Proven resources | Existing complete-row recovery/CAS and install | Explicit forward recovery/install using the proven row, including Resumed/rewind | Never remint or overwrite ACK/suffix from operation snapshot |
| Installed, final checkpoint not committed | Final authority/proof validation and terminal transaction | Explicit forward using same resource and complete recovery owner | Keep reservation until terminal transaction |
| Activated plus inactive reservation, atomically committed | Return historical success | Return historical success before readiness/current-owner validation, resource installation or error rewrite | Absorbing even after lost answer, runner tail failure, later hold or subsequent legal handoff |
| Compensating, unresolved original claim | Resolve/release exact claim | Same exact resolve/release only | Reservation remains active; nonhealthy is not Healthy |
| All required releases healthy / no effects | Commit cancelled/refused and inactive reservation atomically | Historical cancelled/refused only | Absorbing |
| Typed released but faulted/unconfirmed health | Retain typed evidence and recovery diagnosis | Ordinary owner/provider recovery required; cannot report healthy | Do not silently use a storage error or Unknown as permission to abandon reservation |

Every recoverable postcommit storage failure has a specific safe continuation:
unprepared retries preparation; prepared-before-intent retries readiness and intent;
intent without proof remains Unknown; bind transaction failure leaves intent and must
not resend; durable resource proof retries full-row recovery/install; final checkpoint
failure retries terminalization. Committed and Activated read-back precede any attempt
to write a staged before-image. Explicit retry uses the original runtime attempt and
lease CAS; boot does not rearm AwaitingRetry or Stuck.

## Transaction and deletion plan

1. Admission fixes the original scope/claims and inserts one workflow operation and
   thin reservation. Immediate driver contention refuses before admission.
2. Each pause intent and receipt is checkpointed under the existing runtime lease.
   There is no persistent fresh/live assertion.
3. Reuse cutover facts/writes inside the workflow transaction and checkpoint Committed
   in that same transaction. Remove separate cutover submission, recorded-key query,
   commit-unknown child protocol and generic claimed-tx-only wrapper.
4. Reuse briefing, resource mint, credential binding and installation owners. Store
   intent/proof/install progress on the original operation. New SQL proof relates the
   resource directly to this owner, without joining coordinator JSON or a child row.
5. Commit Activated and inactive reservation in the same transaction. Return it before
   checking current role/root/readiness, since a later valid handoff is independent.

| Delete from the normal path | Replacement / retained reason |
| --- | --- |
| Coordinator → keyed cutover → keyed activation operation stack | One workflow adapter and original operation |
| Independent cutover submit/query_recorded/commit_recorded and claimed tx-only composition | Existing cutover SQL called in original leased checkpoint transaction |
| Independent activation enqueue, retry key and normal service registration | Resource stage and explicit retry of original workflow key |
| Activation's reverse coordinator JSON join to release reservation | Exact original operation ID owned by terminal transaction |
| Coordinator's duplicated plan/result and reservation request hash | Required variant data and original immutable input/payload hash |
| PlanRecorded then CommitIntent then empty Committed rewrite | Required pause intents; one atomic identity/Committed checkpoint |
| Runtime phase-first business result interpretation | Domain terminal first, technical diagnostics second |

Retain thin Track exclusion, cancellation, exact pause intent/receipt/release evidence,
binding generations, native intent, immutable resource proof and full-row recovery.
Do not remove released migration bytes or SQL history/late-write/retirement fences.
New duties belong in files of at most 800 lines; no handoff policy enters runtime.

## Legacy boundary and verification

Migrations 0171 and earlier are byte-frozen. A new migration must define versioned
single-owner proof while preserving historical keyed receipts and immutable legacy
audit/resource rows. Legacy three-operation data is explicitly decoded as legacy,
never silently interpreted as the new sum state. In-flight legacy preparation can
only reconcile original claims; committed legacy work needs a bounded explicit
original-scope/generation recovery owner or typed one-time adoption preserving native
intent and proof. It must not submit children or replay old effects. Merely removing
external ingress or normal registration does not authorize forgetting persisted work.

Legacy adoption must be transactional and write-once. Its source is the exact persisted
scope, operation attempt, claims and immutable audit/proof, not caller-supplied replacement
configuration. The adopted workflow may retain a typed reference to legacy immutable
evidence; it must not keep advancing three journals. A mapping fences the prior live
drivers before the new owner can continue. Retrying an adopted child must report its
adoption instead of rearming it. Existing succeeded receipts remain byte-for-byte evidence;
new query logic must recognize historical Activated before consulting technical phase or
current ownership. Adoption and exclusion use existing IMMEDIATE/lease CAS primitives.

| Persisted legacy boundary | Permitted adoption / compatibility behavior |
| --- | --- |
| Historical succeeded cutover or Activated | Read the original versioned receipt independently of today's root, role or hold. Do not rewrite it. |
| Coordinator Reserved or precommit with no committed cutover | Original claims only, recovery-only compensation or no-effect termination. No live capability is constructed. |
| Coordinator CommitIntent | Serialize read-back of the exact recorded child key. Proven absence permits original-claim compensation; query error remains unresolved. A found committed identity admits only forward adoption. |
| Committed identity, activation absent or prepare rolled back | Adopt unprepared forward state with original successor/configuration. No enqueue of a child. Original key and explicit generation own the retry. |
| Prepared activation, no sending intent | Reuse the prepared complete successor row and original briefing; adoption cannot insert the briefing a second time. |
| Native intent/Unknown with no returned proof | Adopt Unknown as evidence. No new native request, even if provider readiness later improves. |
| Immutable resource proof exists | Reference original proof and consume complete-row recovery/CAS. Never regenerate the resource or derive a snapshot from old operation output. |
| Activated committed but runner unfinished | Consume historical success and inactive reservation. No install/readiness pass. |
| Standalone committed legacy cutover without coordinator | A bounded explicit recovery-only adoption may create a single owner keyed by the Neige-owned original operation identity, using original immutable audit. It cannot acquire precommit authority or forget retired identities. |

The implementation must enumerate these version transitions and test pending adoption,
compensation, Unknown and already-proven resources. The implementation and actual evidence are recorded below and in the verification document.

The original regressions reproduce through real begin, SQLite and the native stub:
Reserved crash without cancellation followed by same-session busy/idle change;
postcommit activation-prepare ABORT followed by storage repair/file reopen/original-key
explicit retry; and committed Activated with lost answer/runner-tail error followed by
hold or successor handoff. Each regression checks exact roles/claims/native counts and
reservation close/delete behavior. No test seeds a successful receipt.

Worker iteration uses only the task's complete focused selection and necessary fmt/lint
fixes. Predict complete red sets before the few live-only, absorbing-terminal and
postcommit-recovery single-factor production mutations; run the entire selection,
compare exact failures, restore exact hashes/diff, then restore green. Kernel owns
the six frozen-target gates; Planner owns two fresh L2 reviews. No real provider,
Tier 2, workspace suite, public API or visible UI verification is claimed. No API
generation is needed unless implementation actually changes schema/generated inputs.

An ordinary new provider extends the existing profile/resource owner, not this workflow.
A new state changes its owning type and relevant tests; a new test changes only its
module. No permanent test list or extra CI pass is added. Checkpoint counts and actual
deletions must be measured from the final production path, not inferred from line count.

## Implemented ownership and compatibility

The version-two registered kind is `handoff-workflow`. The existing internal
`handoff_coordinator::begin/query/cancel/retry` entry points now select this one
operation. Both application and dispatcher registries remove the cutover and
activation adapters; the declared non-task adapter contract removes those two kinds.
No legacy adapter is retained in a fixture registry. `HandoffCutoverAdapter` is a
transaction-domain test helper only, and the old activation module contains receipt
compatibility and explicit legacy adoption, with no native executor or enqueue.

Admission returns an in-memory `Admission::Original` or `Admission::Replay`. Only
the original branch scopes the private, nonserializable `LiveAdmission` around the
existing exact-target runner. The scope is task-local to that original future, is
not inherited by spawned/background tasks, and is destroyed on cancellation.
Neither a Reserved row, a new lease, retry nor adoption can construct that scope.
The admission transaction also distinguishes a concurrent same-key replay before
returning, so a race after the initial read cannot mint a second live capability.

`handoff_state` owns the required-data sum type. Pausing and compensation
carry independently typed original claim states. Committed owns configuration and
consumed receipts; it reconstructs the transient cutover plan/result from the
immutable input instead of storing both. Resources distinguish unprepared,
prepared, sending, unknown, proven and blocked-with-a-typed-resume-state. The
resource stage uses the existing briefing, snapshot CAS, native mint, credential,
transport and full-row recovery/install functions. Technical storage diagnostics
use the original operation's existing `last_error`; they do not replace progress.

Migration 0172 preserves all prior migration bytes, original operation payloads and
legacy activation evidence. It removes reservation `request_hash`, adds direct
single-owner resource proof, and makes the domain terminal absorbing in SQL as well
as in the reader/driver. Retired identity fences start at the identity checkpoint,
including the interval before resource preparation. Pending activation accepts only
the leased version-two owner, proven resources, original consumed claims,
configuration and current SQL authority. There is no reverse coordinator JSON join.

Version-one rows are decoded explicitly by legacy kind and contract. The migration
parks nonterminal old journals as requiring explicit adoption. Adoption is an
IMMEDIATE, original-scope/generation transaction: it preserves immutable input,
claims, intent, configuration and proof, creates one recovery-only version-two
owner, and records a write-once mapping that fences writes by the old drivers.
An active reservation transfers only through that mapping. The original user
idempotency key and payload hash select the new owner; its internal globally unique
operation key is Neige-owned (`handoff-adoption:<owner-id>`), since the old keyed
operation remains permanent. Standalone already-committed legacy cutovers use the
bounded `legacy-cutover:<original-id>` recovery key. Historical Activated is read
before current authority/readiness and never reinstalls an owner. Legacy native
intent without proof remains Unknown. No general version migration framework was
added, and none of the old three journals is driven after adoption.

The removed mechanisms are independent cutover commit/query composition, activation
submit/enqueue and retry owner, the claimed-tx-only runtime wrapper, PlanRecorded /
CommitIntent / empty Committed rewrites, duplicate persisted plan/result, and
reservation request hash. The retained SQL retirement/resource proof is immutable
evidence; the thin reservation is cross-key exclusion and cancellation; generations
remain necessary for pointer ABA; full snapshot data remains owned by its existing
session row and recovery CAS. These are distinct from runner phases.

The normal domain path has one initial Reserved output and nine leased checkpoints:
two pause intents, two pause receipts, atomic Committed, prepared resources, native
intent, bound proof, and Activated with reservation release. The prior path had the
coordinator's initial output plus eight checkpoints, one separate cutover result,
and activation's initial output plus three checkpoints (14 versus 10 domain output
writes, three operation rows versus one). Generic runner phase writes are separate
from these domain checkpoints. Fault/compensation paths retain necessary evidence
and can write additional checkpoints. Verification records runtime measurement and
actual logs rather than making the exact count another permanent CI invariant.

An ordinary new provider still extends profile/resource/recovery owners. An ordinary
new test changes its module, and a new domain state changes this sum type and its
callers. No new permanent test-name list or CI pass was added. The inherited head
schema inventory gains the new migration name; subsequent migrations keep its
existing two-file maintenance contract. New responsibilities remain below 800 lines.

The final state model narrows Finished to settled claims: untouched or an exact
HealthyReleased receipt. Intent, paused and nonhealthy outcomes cannot deserialize
as that terminal. Error read-back takes IMMEDIATE and reads the original row before
compensation or forward work. Terminal reservation updates prove their result in
that transaction, including ignored-write detection. A late exact original pause
is fenced after settlement; invalid non-handoff JSON is left to its existing recovery
contract, rather than being parsed or accepted as a pause by this domain trigger.
The adoption write fence performs one indexed mapping lookup on operation updates;
the pause fence reads JSON on handle-state updates and checks completed original
claims for valid pause candidates. These costs enforce original-owner settlement,
rather than adding another executor or approval journal. Generations still write
only on their existing pointer/role/root/lifecycle transitions. Adding an ordinary
provider or test does not extend an application-identity list in runtime.
