# Workspace delivery and lifecycle protocol (S3)

Status: implementation proposal for independent L2 design review, 2026-10-16.
This is not an implemented capability. It refines the S3 contract in
[track-workspace.md](track-workspace.md); S1 remains unchanged. The approved
S2a identity/launch protocol supplies resource identity, binding versions,
launch tickets, executor drain receipts and resource fences. This document
introduces no alternative identity, scheduler, operation driver or reaper.

The delivery, migration and retention rules below are proposed decisions.
Section 7 and the [verification decision attachment](workspace-verification-decision.md)
identify a parent-contract change requiring explicit design review: preserve
logical cwd and Git-classified hot caches through a guarded verification
projection, instead of claiming plain worktree sampling proves no source write.
No candidate-bound capability is advertised before that mechanism is verified.

## 1. Real call paths and ownership

Reviewed implementation baseline: `13e895967`. Paths below are relative to the
repository root; these are evidence for the design, not a permanent caller list.

| Existing owner / path | Observed behavior and S3 change |
| --- | --- |
| `crates/calm-server/src/git_candidate/delivery.rs` | Writes a delivery row before forge submission, freezes its operation key, and settles candidate plus delivery in one transaction. Keep these settlement and scheduling owners; replace the shell-selected commit with a typed prepared delivery and extend that transaction with ledger CAS. |
| `crates/calm-types/src/forge_git.rs` | Delivery requires an attached worker branch; checkpoint soft-resets to lease base before add/commit; scripts are frozen in historical payloads. New work uses one kernel commit operation. Historical bytes stay unchanged. |
| `crates/calm-server/src/operation/workspace_lease/worker.rs` | Requires status-clean, samples HEAD, checks worker branch, selects newest done T and fresh fetched U, then runs `reset --keep U` inside prepare. Move filesystem/network work out of the DB transaction and use the resource plan below. |
| `crates/calm-server/src/operation/workspace_lease/upstream.rs` and `upstream_fetch.rs` | Reads branch config and HEAD to choose upstream; fetch success provenance is process-local. Replace post-conversion policy consumers with durable resource upstream receipts, retaining the bounded fetch mechanism. |
| `crates/calm-server/src/builtin_plugins/dev/publish.rs` and `crates/plugin/src/builtin/gitforge/publish.rs` | Kernel supplies path/branch/upstream/candidates through Services; Gitforge owns done-candidate publish policy. Supply typed facts and a verified publish target instead. |
| `crates/plugin/src/builtin/gitforge/git_actions.rs` | `git_commit` lowers to shell argv, accepts a branch label, and infers landed from a clean index. Replace this tool with a typed Services call; no kernel interception by plugin/tool name. |
| `crates/calm-server/src/operation/task_gate_run/checkpoint.rs` and `task_verify_adapter/target.rs` | Checkpoint is a synthetic first shell step; before/after evidence is provenance, HEAD and status. Prepare checkpoint before gate execution; retain logs and samples as observations, not a proof of no intermediate write. |
| `crates/calm-server/src/planner_hibernation.rs`, `routes/planner_recovery.rs`, `routes/tracks.rs`, `routes/areas.rs` | Sleep, wake, patch reopen, deletion and area compensation own domain transitions. They request the same resource lifecycle operation and consume its result. |
| `crates/calm-server/src/workspace_service/{worktree_clean,sweep,workspace_recycle}.rs` | Independent ignored cleanup, post-delete branch/candidate-ref sweep and seven-day managed trash collection exist. Converted resources use one persisted lifecycle driver; old helpers become mechanisms, not a second deletion authority. |

`workspace` owns snapshot, normalized tree storage, controlled Git effects,
staging/install inspection and physical receipts. `calm-truth` owns required
persisted shapes, uniqueness, references and CAS transactions. Kernel workspace
service owns admission, delivery, recovery and lifecycle policy using existing
operations. Gitforge owns message/tool/publish domain contracts; providers own
execution encoding and supported evidence mechanisms. None of these owners
infer policy from a template, plugin identity or display pathname.

All kernel calls carry the S2a principal, purpose and authorized binding. Human
reads use the visitor principal; accepted shared execution uses its frozen
common-execution authorization binding, not a message author's private grants.
An operation persists that binding and WorkspaceRef; replay revalidates it and
does not select a current member or a different workspace. Revision, GitFacts
and delivery receipts are the evidence consumed by collaboration. They do not
authorize execution and do not prove personal credential isolation.

## 2. Required resource facts and their lifetimes

A new migration extends the S2a resource row; released migrations, historical
events, operation inputs/outputs and hashes are byte-frozen. Required values
exist only in the typed state that has established them; NULL is not a default
tip, epoch, baseline, normalized tree or upstream decision.

| Fact | Required shape / authority |
| --- | --- |
| Delivery protocol | `Legacy` or `Converting(operation_id, version)` or `LedgerV1(receipt)`; only LedgerV1 can accept new S3 writes. |
| Baseline B | `Known(commit, owned_pin)` from S2a materialization, or `LegacyUnknown`; immutable after materialization/conversion. Unknown never means current HEAD. |
| Delivered state | `DeliveredTip { sequence, commit, tree, settlement_id }`; the resource ledger is the single authority. Initial sequence zero is the conversion/materialization receipt. |
| Installed state D | `InstalledBaseline { epoch, tree, delivered_sequence, install_receipt }`; outside a baseline-install operation its tree equals the settled delivery tree. HEAD/index are separate execution projections. |
| Upstream | `NoneSelected` or `Configured { selection_version, remote, merge_ref, resolved_url, tracking_ref }`; explicit absence is a decision. Fetch observations are immutable receipts, not this selection. |
| Branch projection | Exact owned branch ref, expected OID and `InSync(receipt)` / `Stale(reason, protected_observed_oid)` / `Absent`; never delivery authority. |
| Revision | Immutable snapshot identity, resource, install epoch, logical baseline sequence, normalized tree, normalization digest, source-content handle, changes and overlap acknowledgement. |
| Active dependencies | Registered operation phases declare resource, execution, revision and protected-content dependencies through their existing owner contract. Permanent audit identity alone retains neither storage nor pins. |

Protocol ref names are Neige-owned identifiers made from validated IDs. Use
`refs/neige/workspaces/<workspace_id>/delivery` for the delivery pin/intent;
candidate and run refs retain their domain owner and names where compatible.
Ref names, full OIDs, object format and expected previous values are frozen in
the operation intent before the first ref effect. OID types support Git's
reported object format rather than assuming forty hexadecimal characters.

| Reference owner | Retained until |
| --- | --- |
| Known B pin | Resource retention has ended and no restore/evidence dependency uses B. |
| Delivery pin | A newer settled delivery replaces it atomically, or final resource expiration releases the last protected-content dependency. |
| Revision/object staging pin | Snapshot consumers and any prepared operation have settled or canceled; no consumer can outlive its content. |
| Run pin | Its verification, candidate-binding or explicitly retained evidence dependency is released. Ending a worker alone does not release it. |
| Candidate pin | Existing candidate/evidence/PR dependencies are explicitly released; domain row deletion does not implicitly drop the pin. |
| Archive pin | Its frozen archive deadline has passed and no restore, legal retention or other explicit content dependency remains. |
| Divergent ref/source preservation pin | Recovery has a durable resolution; never expires merely because the originating attempt ended. |
| Publish branch | No checkout uses it and no candidate, unsettled operation, open/unknown PR or other protected branch dependency remains. |

Reference release uses expected-OID CAS and records its receipt. A reference
with an unexpected OID is retained for recovery. A terminal keyed-operation
identity can replay its original result after physical release without resolving
the current track or rematerializing content; it is not a hidden storage lease.

## 3. ImmutableRevision and normalization

Snapshot compares the full logical source tree with the delivered baseline;
status-clean and equal HEAD are insufficient. Include tracked paths even if
ignored, new nonignored files, deletions, executable mode and symlink values.
Exclude internal Git metadata, kernel artifact storage and newly ignored
outputs. Unsupported source types and unresolved Git operations/conflicts are
explicit refusals. Gitlink/submodule handling remains Unsupported unless a
shared contract pins its complete source relation; it is not silently flattened.

For worktrees, perform content normalization in an authorized launcher using an
isolated temporary index and the repository's effective attributes, filters,
eol and fileMode behavior. Repository-selected clean/smudge execution receives
the untrusted allowlisted environment and no kernel credentials. Do not mutate
the user's index to take a snapshot. Freeze normalized blobs/tree and their
content handle once: retry never reruns a nondeterministic clean filter to
silently produce a different C. Record effective attributes/config and ignore
rule sources/digests, including Git-reported exclude paths and kernel exclusions.
Index flags cannot hide tracked modifications from the full tree comparison.

The trusted object/ref writer accepts already normalized objects and fixed
typed commands. It disables hooks, filters, signing and fsmonitor, does not
inherit repository-selected executable configuration, and receives no arbitrary
argv. Git-reported common/git directories establish repository identity; no
external provider's private layout participates.

Snapshot holds a source read permit and stable-content capture. If source bytes,
source classification or installation change during capture, retry or Busy;
never publish a torn tree. A committed private tree still differs from the
ledger when its source differs. Private commit ancestry is input provenance
only. Future view snapshots use D plus agent W and report overlap; S3 does not
implement or select a view. Unacknowledged overlap is refused by the kernel.

## 4. Free conversion and ordinary checkout preparation

Conversion is one recoverable operation per resource. Before starting, seal new
mutating admission under the resource fence, reconcile S2a launch facts and
drain every legacy task/reader/writer, gate, forge/delivery and source-mutating
execution. Checkout admission must be Free; Readers is not Free. A live terminal
is not generally a worker-admission blocker, but it must drain for conversion.
Idle provider state without executor evidence cannot establish this condition.

Old operation payloads drain through their original registered codec. A pending
old operation that would launch new legacy writes stays held until it is either
explicitly canceled or drained before conversion. Its keyed result remains
replayable. No legacy mutator may execute after the protocol CAS. Missing legacy
identity or unknown process state requires recovery; upgrades do not kill PTYs.

After Free, the conversion owner records exactly once: resource/directory and
repository identity, current worker branch tip H, observed symbolic HEAD/index
and source snapshot, chosen upstream config plus effective URL, projection ref
and expected OID, known B or LegacyUnknown, target epoch and expected ledger
version. The adopted delivery tip is H from the existing owned worker branch,
not a private detached HEAD or a newly fetched U. If that branch cannot be
verified, conversion refuses rather than guessing from HEAD. Existing attached
tracks without an owned worker worktree keep their explicit legacy refusal;
conversion never detaches or edits a borrowed human checkout. Managed repositories
use their verified owned branch, and unborn repositories require their explicit
materialization contract rather than an invented H. Preserve dirty
source as W relative to H. Conversion may proceed with W, but a subsequent
ordinary worker claim will refuse it until Planner commit or explicit discard.

Persist the immutable conversion intent before creating delivery/source pins.
Install detached execution HEAD/index at H without rewriting source bytes,
then CAS the protocol, delivery sequence zero, D and upstream receipt together.
The CAS is the switch: every consumer now reads GitFacts, and legacy branch
config is no longer an upstream authority. A crash before CAS leaves admission
sealed; inspect pins/install receipts and complete the same intent. A retry
never samples a newer branch tip or upstream and never adopts twice. A new
materialization enters LedgerV1 from its actual ready receipt by this same
publication path, without changing S2a selection timing.

For an ordinary lease, checkout_admission first reserves the existing
ReadWrite workspace lease owner for preparation; readers use their existing
compatible policy. Snapshot must be empty relative to settled tip P; private
commits with identical trees are empty. The kernel then freezes a prepare
intent and sets detached HEAD/index to P, preserving source/ignored files,
outside the DB writer transaction. Only its completed receipt permits the
lease's prepared/spawn-ready transaction. Expected epoch/sequence and binding
version are rechecked there. Failure remains a prepared/recoverable owner and
cannot admit a second writer. A failed identity/launch check still maps to
spawn-failed, not a successful attempt or delivered candidate.

A declared reader head must match the protected ledger commit and revision
tree under its read lease; arbitrary private HEAD cannot change this test.
The read pin is retained for the reader's actual lifetime and rechecked at
launch/replay. These tests replace attached-branch provenance and raw HEAD
equality in verify_worker_checkout/verify_worktree_base; directory identity,
registration, epoch and frozen source evidence remain required.

## 5. One candidate construction, runs and Planner commits

Every worker attempt freezes LeaseBase P from the settled tip. Catch-up first
settles U and then creates P=U. A checkpoint/run never advances P. Each distinct
run has its own key and Revision; retries of that run reuse its exact commit.

The only construction is snapshot -> normalized tree -> trusted `commit-tree`
with exactly one parent P -> owned pin -> execution projection. The intent
freezes full commit bytes, including author/committer identity, timestamps,
message, parent and tree, before object/ref mutation. C is therefore identical
after restart. An unchanged source tree still uses this explicit construction;
it never silently adopts a private commit. Gate, ungated delivery, Planner
commit and archive use this construction owner with their declared commit
purpose. Older archives retain independent pins; they need no extra parent and
never update delivered tip.

A run creates/pins C and installs execution HEAD/index=C while leaving D at
tree(P): D is the settled logical baseline, not the index tree. Run two after a
fix again has parent P, never run one's C. A `done` submission freezes the
accepted Revision and C; a passing gate is reusable only for that exact tree,
candidate, normalization/environment evidence and required policy. Report
transaction, delivery row, final candidate and acceptance remain distinct:
neither done text nor a passing command is the delivery linearization point.
When a passing run supplies C, final delivery reuses its exact frozen commit
bytes and only creates the final pin; it must not regenerate C with a new
timestamp/message after verification. An ungated attempt constructs C once
through the same owner before delivery.

When a run ends without delivery, the attempt retains its workspace lease.
On actual lease release, an idempotent projection intent restores HEAD/index
to the current settled tip while preserving work files. Dirty source remains
visible W and blocks a later ordinary writer/read-head claim. It is not lost
and not implicitly delivered. `DiscardWorkspaceChanges { reference,
expected_revision, expected_delivery_sequence }` is a typed kernel operation,
authorized as a destructive source edit, which protects the discarded revision
before installation. It refuses changed evidence and never runs on the human
source checkout. No bare reset helper is an authorization entry point.

Gitforge's Planner-only `git_commit` becomes a typed Services method carrying
message/idempotency input and authorized WorkspaceRef; remove the branch
argument from its descriptor and producers. Registry role checks and kernel
principal/purpose validation both apply. Worker callers receive a clear refusal
to use `done` delivery. Under checkout_admission Free the commit operation
registers the same ReadWrite lease/owner and holds it through settlement;
Readers, another writer and unsettled delivery are Busy. Do not add a plugin
name switch in forge_action_adapter or duplicate concurrency SQL in Gitforge.

Planner commits use the exact C/ref/install/DB path below and advance ledger
tip, but have `PlannerCommit` provenance, not a fabricated done candidate.
Gitforge publish consequently still rejects a tip that is not a done worker
candidate. Repeated idempotency keys compare the original typed request and
replay its original result without constructing a new commit.

Synthetic shell checkpoint step is removed. Kernel checkpoint preparation is
a separate operation phase/event, not user step zero. Gate log step numbers
now start with the first declared GateSpec step at one. Version persisted log
metadata so old step-one `neige-checkpoint` logs remain readable; do not rewrite
old logs or silently shift their existing failure indices. GateSpec itself and
declared cwd/user step semantics are unchanged.

## 6. Delivery, refs, installation and upstream

The existing operation intent/tx_output stores Revision, P, C, expected ledger
sequence, all owned ref names/OIDs, projection observation, target epoch and
install plan. Do not add a shadow operation journal. S2a resource/binding fences
serialize mutation and install; existing lock order precedes workspace fences,
then Git common-metadata locks. Never hold a DB writer transaction over Git,
filesystem, drain, provider or network work.

Final delivery atomically CASes the final candidate pin plus the owned delivery
ref P->C in one Git update-ref transaction. Pins for provisional runs are not
the delivery ref. A Git ref transaction is the Git-side intent commit; it is
not the settled resource tip. The final DB transaction CASes the ledger sequence
and D/install receipt and writes candidate plus one-time delivery settlement.
That transaction is the authoritative delivery linearization point. Write
admission stays sealed between these points; reads/identity resolution may run.

Projection is best effort. Before writing the owned publish branch, inspect its
exact expected OID and `git worktree list --porcelain`. If another worktree
has it checked out or its OID differs, protect any observed foreign commit,
record Stale and skip writing it. This cannot stop settlement or ordinary task
admission. HEAD/index changes, attached or detached, are source inputs, never
projection drift. All kernel HEAD writes use update-ref --no-deref; they never
move an attached user branch. Publish refuses a Stale projection with recovery
information; it never publishes the deviating tip.

`install_delivered` writes HEAD/index=C and preserves all working files. It
records D=tree(C) and W=current source minus C, preserving later source edits.
An epoch/install receipt is required even when source bytes need no rewrite.
Ordinary projection changes do not bump the resource generation. An install
epoch change invalidates previous source-specific handles as S2a requires.

Catch-up freezes H=settled tip, newest done candidate T, fresh successful fetch
receipt U and merge base M(T,U), original source revision and target epoch
before any mutation. Refuse if source is dirty, Git operation/conflict remains,
T is absent, history cannot establish M, or the selected upstream lacks a
fresh success receipt. Shallow/unknown history is not guessed. Fetch is bounded
and outside DB prepare; its receipt binds exact selected remote/merge/URL,
selection version, fetched OID and owned pin under the existing single-flight
Git lock. A later failed fetch never silently replaces a frozen successful U.

Under the exclusive owner, CAS delivery pin H->U, project best effort, stage
and install source tree U, and settle ledger tip/D=U before publishing the
lease with P=U. Keep ignored outputs unless a target tracked path would
overwrite one: preserve conflicting bytes and refuse/recover explicitly.
Worker prompt retains M/T/H/U and asks the agent to replay M..T and resolve
conflicts. Kernel does not replay, pick a new T on retry or reset the human
checkout. Consecutive catch-ups freeze their own H and fetch receipts; they
cannot infer H from an abandoned run or current HEAD.

Source-writing installs use an operation-owned staging manifest. Freeze target
working bytes through the same authorized smudge/materialization rules, and
record each path's before/target identity/digest plus expected index/HEAD and
target epoch. Fs effects are descriptor-relative, fsynced and inspectable. A
path already at target completes its receipt; one still at before can advance;
anything else is protected as unexpected source and requires recovery instead
of overwrite. Source-only files removed by the install get the same treatment.
Crash halfway cannot open admission, claim Ready or discard unknown bytes.
Changing source projection never installs a mutable branch name as a target.

Post-conversion `GitFacts` supplies selected upstream and latest durable fetch
receipt to ensure/lease preparation, scheduler refresh, readers, staleness,
catch-up and publish. None calls head_upstream/branch_upstream as a fallback.
Changing upstream is a typed authorized operation with expected selection
version; subsequent fetches use a new provenance key. Explicit no-upstream and
fetch unavailable remain distinguishable. Publish consumes authoritative tip,
candidate provenance, ancestry and projection receipt, pushes the exact frozen
OID and records the remote/PR receipt under existing approval policy. Gitforge
keeps done-candidate domain policy; `worktree.is_some()` and user current branch
cannot determine publish eligibility.

| Durable point reached before crash | Recovery / next linearization |
| --- | --- |
| Snapshot/commit intent, no object/ref | Recreate exact frozen object bytes or fail for missing protected input; never resnapshot under this key. |
| Object exists, run pin absent | CAS original expected ref to frozen C, then record run receipt. Unreferenced object creation alone proves no completed run. |
| Run pin=C, receipt absent | Validate object/parent/tree and original intent, then receipt; no delivery advancement. |
| Final refs transaction uncertain | Both refs old: retry transaction. Both at frozen targets: continue. Anything else: DeliveryRecoveryRequired after protecting observed objects; never accept partial/foreign values. |
| Final refs=C, branch projection pending | Probe exact branch and checkout receipts; project or mark Stale, then continue. Branch staleness does not roll back the candidate refs. |
| HEAD/index partly projected, DB still P | Reapply no-source-write projection to the frozen C, validate install receipt and complete original settlement transaction. |
| Install receipt exists, DB settlement missing | CAS sequence P->C with original settlement/candidate; duplicate transaction observes identical result. |
| DB already C, reply missing | Return original settlement and release owner once; do not construct C again. |
| Catch-up pin already U, source install unfinished | Inspect per-path manifest and finish original U installation before ledger/lease publication; do not fetch/select again. |
| Catch-up source/epoch installed, DB or lease absent | Validate original receipt; atomically settle U and publish P=U lease exactly once. A spawn failure leaves U settled and T protected. |

DeliveryRecoveryRequired blocks new writes and settlement, not identity resolve
or merely starting Planner. Source-mutating execution still needs current
admission. WorkspaceRecoveryRequired separately denotes missing/unverified
storage/runtime identity and may prevent handles. A kernel repair operation
uses original intents and ledger expectations: automatic repair only preserves
all bytes/objects; destructive choices require explicit authorized confirmation
against the frozen recovery plan. Never adopt a divergent ref as a new delivery.

## 7. Verification evidence and the remaining proof conflict

`VerificationHandle` binds Revision, tree, C, resource/epoch, execution identity,
normalization, environment digest and `SourceWriteProof`. The digest hashes
versioned declared execution inputs: GateSpec, typed cwd, normalized tree,
toolchain/runtime contract, allowlisted environment names and digested effective
values, provider/executor version, frozen Git refs and ignore/config rules.
Secret values remain private and are not emitted. Ignored caches are expressly
mutable; digest does not claim reproducible builds or hash their full contents.

Kernel holds the attempt's exclusive write owner, drains unrelated source
writers, prepares C, and arms proof before any user step. Proof remains active
through gate descendants' positive drain and final source sampling. A successful
seal requires the same epoch/tree, complete proof coverage and no tracked or
nonignored creation/deletion/mode/symlink mutation, even if later restored.
Ignored output writes and cache reuse remain legal. Tracked files remain source
even beneath ignored directories. A source-writing gate returns failed/unbound
source evidence with its logs and preserves edits for fixing/rerunning; it
cannot accept the candidate. Unknown descendants or lost evidence never bound.

The production sampler currently cannot provide this proof. For example,
`cp tracked saved; modify tracked; test; cp saved tracked` can leave the same
HEAD/status and final tree; create/test/delete of a new nonignored file also
disappears. Recursive inotify can catch these ordinary operations, but it is not
a complete solution: Linux documents missed mmap writes, new-directory races,
network-filesystem limitations and queue overflow. Timestamps are observations,
not an immutable write history. See the primary
[inotify contract](https://man7.org/linux/man-pages/man7/inotify.7.html),
[mmap timestamp contract](https://man7.org/linux/man-pages/man2/mmap.2.html)
and [statx field consistency](https://man7.org/linux/man-pages/man2/statx.2.html).

Architectural blocker S3-V1: a complete source-write constraint/audit mechanism
for arbitrary existing worktree gate commands has not been established by the
parent or current code. The design deliberately does not rename inotify plus
hashing to proof. Implement the evidence API with required typed
`Complete(proof)` / `Unavailable(reason)`; unavailable refuses candidate-bound
verification. It may run an explicitly unbound diagnostic gate, but cannot
silently downgrade a required bound gate or mark a task accepted.

The proposed resolution is the guarded projection in the
[verification decision attachment](workspace-verification-decision.md), including
its precise supported set, actual /tmp probes and proposed parent wording.
It covers immutable source and dynamically classified ignored outputs without
a directory allowlist, but changes the original worktree inode/mount contract
and requires an additional Linux mechanism. Independent design review must
accept that amendment before implementation. Unsupported hosts/resources remain
on their explicit legacy protocol; they are not converted and silently made
unbound. A source watcher remains supplementary invalidation, not the proof.
No implementation PR may claim full S3 completion while this blocker is open.

## 8. One lifecycle: archive, close, reopen and deletion

The resource row owns `Active -> Quiescing(reason, close_id) -> ClosedRetained
-> Reclaimed -> Expired`; DeletePending is a tombstone disposition through the
same transitions, not a second reaper. RecoveryRequired retains the underlying
state/intent. Binding close seals only that binding; a shared resource cannot
be archived destructively while another binding/execution uses it. Final
resource retirement requires the last active binding/storage reference.

Planner hibernation preserves its existing session-sleep policy. Resource
quiescence additionally requires checkout Free (including no readers), closed
launch admission, no unresolved tickets, and positive drain of gate/forge,
snapshot/IO permits, local process descendants and remote tools. Unknown is
Busy. Ordinary terminals can coexist with workers but prevent destructive
close/archive/reclaim; waiting for input is not process exit. The caller sees
snapshot_pending and the actual blocking execution, with no forced kill.

When quiescent, freeze a close intent and Revision; construct/pin archive A
from that source with the settled delivery tip as primary parent. Persist
archive receipt before any recycle/reclaim. Archive does not advance tip.
Freeze private retention at close+7 days and source archive at close+90 days
from typed settings; later settings changes do not move existing deadlines.
Close timestamps identify a unique cycle. Repeated driver retries reuse it;
an actual authorized reopen/close creates a new cycle and snapshot/deadlines.
Archives of older cycles keep their own references until separately released.

Successful archive yields ClosedRetained. Existing managed recycle is the
physical quarantine mechanism; no copy/delete cross-device fallback. The seven
day private layer includes ignored caches and directory state. At expiry,
require successful archive, no active execution/storage references or mounts,
and identity-verified destruction plan, then reclaim owned storage. Borrowed
legacy directories and user source paths are never destruction targets.
For worktrees, registration/directory release and branch release are distinct
operations. The old ten-minute ignored cleanup and unconditional sweep are
disabled for LedgerV1 resources; both schedule this driver instead.

Every explicit wake path, including human send/ask, recovery and patch
`closed=false`, calls the same `reopenable` operation before changing closed
state or restarting Planner. Under the lifecycle fence it cancels stale reclaim
intents by CAS, validates retained storage or stages archive restoration, checks
all bindings and executor facts, then advances generation and reopens admission.
Reclaimed restore uses A's source with a new runtime generation/installation
epoch; ignored caches rebuild. It preserves the old settled delivery tip and
installs D=tree(tip), W=tree(A)-D. It does not mislabel archive A as a delivered
candidate. Archive missing/expired returns explicit unrecoverable status and
does not create an empty workspace. The UI's can_reopen derives from this state.

If the domain track stays closed during an explicit authorized activity, its
end reenters Quiescing and captures a new close cycle. A dormant Planner alone
does not keep storage forever; a live PTY delays quiescence until positively
drained. Reopen cannot revive an old ticket or an old binding version.

Delete persists an independent tombstone plus per-resource release jobs before
removing track/area rows. It contains descriptor/storage identity, binding
versions, close/archive receipts/deadlines, operation dependencies, exact owned
refs and pending materialization IDs. A deleted row is never needed to resume
the job. Domain deletion may complete while archive/reclaim is pending, but
failure cannot permit destructive release of unsnapshotted source.

Area deletion retains the existing saga's complete membership snapshot and
per-resource compensation report. Rollback cancels release intents by version
and restores the same descriptor, resource/archive references and recovery
fence. If a directory was moved, restore only into an absent original path;
an occupied path stays quarantined and needs recovery. Retry compensates every
sibling independently; one failure never erases another item's receipt. A
reaper must recheck the cancellation/version fence before each physical effect,
so stale deletion cannot destroy a restored area resource.

At archive deadline release only refs with no active content dependency.
Published branch deletion additionally requires authoritative evidence that all
known PRs are closed/merged and no open or unknown PR association remains.
Remote unavailable/unknown means defer and display why. A PR pin/receipt binds
repo, remote branch, PR identity and exact protected source OID; branch name
alone is not content protection. Releasing an archive does not delete candidate
pins or publish branches. Shared common-dir resources may release only their
owned refs using expected values, never a broad namespace sweep.

| Lifecycle durable point | Recovery rule |
| --- | --- |
| Quiescing/close intent, no snapshot | Keep private layer and sealed destructive admission; reconcile blockers and retry same close. |
| Archive object/ref exists, receipt missing | Validate exact close intent/object and pin, then record receipt; no reclaim beforehand. |
| Archive failed | Retain private layer indefinitely pending repair; no ignored cleanup or expiry escape hatch. |
| Tombstone committed, rows deleted, storage unmoved | Resume independent resource job from descriptor; no track lookup and no rematerialization. |
| Managed rename complete, receipt missing | Probe original/trash identity and operation marker, then complete receipt; unknown/occupied paths require recovery. |
| Area rollback after physical move | CAS cancel job, restore authorized references and compensate by original report; do not overwrite a replacement path. |
| Reclaim partly complete | Reprobe only exact owned identities; expected absence can complete, unexpected identity cannot. Keep archive pinned. |
| Reopen races reclaim | Same fence and expected lifecycle version chooses one winner; loser rechecks persisted result before effects. |
| Ref release/physical delete succeeds, reply lost | Expected absence plus original release intent completes receipt; do not infer expiry of sibling refs. |

## 9. Production acceptance and implementation gates

Exercise real kernel operations and Gitforge/worker/gate/lifecycle entry points;
test helpers may create repositories and inject crashes, but must not implement
snapshot, delivery, admission or retention policy. The following are acceptance
scenarios, not a permanent list of test function names or source files.

| Outcome | Required observable production evidence |
| --- | --- |
| One parent chain | Agent privately commits, gated run fails, second run passes, done settles; every C has frozen P, only settlement advances tip, next attempt starts there. Planner commit advances tip but cannot pass done-candidate publish policy. |
| Admission | Planner commit while Readers/writer/unsettled delivery returns Busy; ordinary terminal does not block worker scheduling but does block conversion/reclaim. Unreleased run source survives with HEAD/index reset and blocks next dirty claim. |
| Normalization | Actual clean/smudge/eol/fileMode fixtures execute in launcher with no kernel credentials; frozen C and verification tree match, ignored tracked source remains source, retry does not rerun filter to choose C. |
| Projection | Detached and attached private HEAD changes never move a user branch. Foreign publish-branch OID/checkout records Stale, preserves foreign OID and allows settlement; publish refuses. Foreign delivery ref enters delivery recovery without making identity resolve unavailable. |
| Conversion | Crash before/after pins, detach and DB switch; original H/upstream are adopted once, B remains LegacyUnknown when unknown, dirty source survives, old payload bytes/hash stay identical and no legacy write crosses switch. |
| Installation/catch-up | Crash after H->U pin, mid-path install, after HEAD/index, after install receipt, before DB/lease and after settlement reply; re-drive exact U/C once, keep T and ignored cache, no original checkout modification. Repeat catch-up after failed spawn. |
| Upstream/publish | Detached ensure/fetch/staleness/reader/publish all use durable selected upstream; restart and remote force-push do not change frozen receipts; no complete config is explicit absence. Verify exact OID push plus PR receipt and candidate ancestry. |
| Verification | Tracked write/restore, create/delete nonignored, mmap restore, hardlink alias write, directory rename/new directory, queue/monitor failure, background descendants and restart cannot produce bound success. Ordinary ignored hot builds pass under the approved complete proof; root-external gates remain explicitly unbound. S3-V1 must close first. |
| Retention | Close captures source after quiescence; archive failure prevents all destruction. Seven-day private and ninety-day archive deadlines are frozen; settings changes do not shorten them. Reclaimed reopen restores source as W over delivered D; expired archive refuses instead of empty recreation. |
| Deletion/area | Delete then restart uses tombstone without domain rows; partial area failure compensates every sibling, occupied restore path is preserved, stale reaper cannot delete restored binding. Candidate/open/unknown PR refs survive physical private-layer release. |

Mutation verification uniquely pins ledger-parent authority, source-proof
failure closure, archive-before-reclaim and stale-intent cancellation. Predict
the complete red set, mutate one production factor in an exclusive recoverable
worktree, run the whole affected target/filter, restore safely and confirm green.
No transient mutation may be observed by another writer/reviewer.

Implementation uses L2: two fresh independent channels after each behavioral
fix, explicitly checking ownership, duplicate policy, application hardcoding,
external facts and permanence cost. Runtime contracts register scenarios by the
owning mechanism; no caller/test-name inventory or path allowlist is added.
Ordinary new tests/files require no registry edit outside their natural module.

Worker runs focused tests and formatter fixes. The kernel gate runs one text
ratchet, contract and quick Rust preflight plus one nonempty focused replay;
CI owns broad suites. Crash injection belongs to production operation/effect
boundaries, not an alternate fake delivery algorithm. No real provider E2E runs
on the shared host. This documentation proposal runs text ratchet and diff
checks only; those checks do not establish the future acceptance scenarios.
