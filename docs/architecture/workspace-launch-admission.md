# Workspace launch, materialization and admission bridge (S2a)

Status: L2 implementation proposal, awaiting two independent reviews. Baseline
`ebf4e6d4e5e6bf04a654347d3928220e2d99a1ad`; no implementation or qualification
is claimed. This refines [workspace-identity-launch.md](workspace-identity-launch.md)
and [track-workspace.md](track-workspace.md), keeping S1 behavior, S3 delivery
policy and S4 provider qualification at their existing owners.

## Outcome and current boundary

Produce a real kernel `PreparedLaunch`: verified authority and purpose, a
qualified current source object, frozen binding/WorkspaceRef, durable operation
intent, registered executor boot and immutable specification/ticket. It must
reach a production process/provider caller. A directory observation or a
constructed generation number is not a substitute for this bridge.

The foundation under revision stores a fresh logical selection identity and
binding-local Pending/Observed/CAS facts only. It does not publish WorkspaceRef,
canonical physical identity, admission claims, generation or runtime authority.
Its earlier persistent dev/inode or selection-key canonicalization is not
restored. The first actual Ready transition below establishes a real resource.

Executor checkpoint `2a68f5c27bef611e3e1789b69814de285d822aa7` is internal work,
not a releasable dependency. Its old-base boundary is `d80c8c7be`; integrate only
the executor commits onto the narrowed foundation, not that old foundation.
Real focused replay was last 11/13, followed by unverified readiness/reservation
and protocol edits. Final replay, gates, mutation and fresh L2 review remain.
The source contract reference is `/tmp/neige-2666-executor-checkpoint.md`.

## Existing owners and production call paths

Paths identify reviewed evidence, not a maintained completeness/file allowlist.

| Owner / evidence | Bridge obligation |
|---|---|
| `auth/session.rs`, `auth.rs` | Reuse SessionIdentity, SessionAuthority and authenticated auth::Principal. Authorization policy remains here and at the existing feature gate. |
| `routes/tracks/create.rs:596-717`, `routes/tracks.rs` | Create/ensure happens after domain commit; keyed retries preserve original selection. Repoint seals old activity and must cancel old materialization before deletion/rebind. |
| `workspace_service/track_worktree.rs`, `workspace_materialize.rs` | Preserve upstream refresh/ensure timing and managed-only materialization. Extend their mechanisms with intent, incarnation, current-object receipt and B pin. |
| `operation/workspace_lease/worker.rs:204-269` | Current prepare selects cwd, may materialize, checks Git/HEAD and supersedes by path inside a DB transaction. Move slow effects into its existing operation stages; freeze the source binding rather than deriving it again. |
| `checkout_admission.rs`, truth `track_occupancy.rs` | One task scheduling/claim/projection policy; resource aggregation is activated once, after source bindings and replay codecs close. |
| `scheduler/mod.rs:156-204,1075-1140` | Frozen task payloads and hashes must replay unchanged. A current track path is not the historical task's source binding. |
| `operation/driver.rs:130-174`, `terminal_launch.rs` | Key lookup precedes validation; existing operation launch ownership, compensation and result replay own the new launch intent. |
| `operation/mod.rs` SpawnCtx, terminal/provider/direct process callers | Consume PreparedLaunch and exact receipts. Raw cwd/Command construction is no longer a track execution authority path. |
| Workspace recycle/clean/sweep and area compensation | Consume independent release/cancel intents, existing 7-day managed trash and 10-minute eligible ignored cleanup; no second reaper or S3 retention policy. |

## Authority: consume C1, do not invent identity

C1's `auth::SessionIdentity` contains user_id/display_name/role.
`Session` additionally contains required SessionAuthority, currently
PasswordLogin or PairedDevice. `auth::Principal` is the authenticated identity
and session ID; it does **not** carry credential provenance. A management check
that needs provenance obtains it through the existing session issuer/store,
as the existing mobile-management owner does. A role string or matching owner
user ID cannot promote PairedDevice into password-login management authority.

`calm_types::worker::Principal` is a different existing execution vocabulary
(User/Kernel/Agent); its User variant alone does not identify a human or prove
HTTP access. Agent authority is derived from the authenticated worker/session
row at the owning gate. ActorId/Actor headers are audit metadata, not either
authorization source. Do not add another user/role/membership table or infer
grant authority from terminal/card/session display names.

The existing authorization owner issues a sealed execution/access conclusion
referencing its accepted identity, target and purpose. Kernel workspace service
consumes that conclusion and current binding; workspace and executor receive
only opaque grant/binding/version inputs. An internal kernel call names its
own registered operation authority and frozen target, not an invented human.
HTTP reads use the actual visitor. Task/Planner execution retains its accepted
execution authorization; restart revalidates it without choosing another user.

The kernel owns typed purpose: content read, content write, execute and
lifecycle control. These are distinct required variants, not booleans supplied
by a caller. Existing task access and credential-provenance restrictions still
apply; purpose does not create permission. Read authority cannot mint stdin,
launch or destructive-control permission.

AreaEnvironmentBinding belongs to collaboration authorization. Its identity,
version, resources and approved credential inputs are distinct from a workspace
binding, source incarnation, runtime generation or executor grant. The current
C1 code does not implement Area membership/common-execution grants. C4 must
deliver that owner conclusion before shared execution is admitted. An adapter
must not fill the gap using a message author's HOME, provider config, Actor or
deployment-owner identity. Single-user work remains available through existing
verified authorization, without claiming C4 completion.

## Identity: selection, resource and object are separate

| Required fact | Meaning and owner |
|---|---|
| SelectionId | Foundation logical ID plus immutable selected inputs and binding-local version. An observation/audit identity, never executor ResourceKey. |
| CanonicalWorkspaceId | Opaque resource identity allocated/selected by this workspace owner after a current-object proof. Public WorkspaceRef.workspace_id and the executor Worktree WorkspaceId both name this one resource. |
| WorkspaceBindingId/version | Domain Track/selection authorization association; freezes resource, relative cwd, selection version and release/cancel version. |
| SourceIncarnationId | One installed/adopted source-object lifetime with its proof and ownership disposition. A copied marker or a reused pathname does not preserve it. |
| Runtime generation | Real version advanced by a reconciled resource recovery/holder transition; established by its durable receipt. Not an observed inode field. |
| Source epoch | Real initial adoption/materialization or later source-install receipt sequence. Separate from S3 projection/baseline sequence and immutable content evidence. |
| Area authorization / ExecutionId | Respectively the existing authorization owner's grant and the executor's exact process incarnation; neither is a workspace identity. |

Selection IDs stay internal to selection/binding audit. Before first Ready they
may converge to one CanonicalWorkspaceId only when the live-object mechanism
below proves the same root/Git layout. Distinct linked worktree roots do not
merge because their common Git directory matches. The selection-to-resource
association is CAS-persisted; it is not a global path/inode uniqueness index.
A new Track gets a new selection/binding, never an old deleted Track's audit row.

Public `WorkspaceRef { workspace_id, cwd: WorkspaceCwd }` is constructed only
from a frozen verified binding to that canonical resource. It has no running
generation. There is one resource admission/lifecycle owner and, in S3, one
delivered-tip ledger; logical aliases do not get parallel tips or lock domains.
All operation sidecars freeze the same association. This preserves the merged
public WorkspaceRef contract rather than making workspace_id mean selection
in one API and executor resource in another.

The independent resource/binding rows already required by S2a carry these
states and references. Do not add another ledger, lock manager or keeper daemon.
The kernel retains controlled object leases; the existing executor retains its
registered root handles. The existing per-resource fence serializes their state.

Within that owner, candidate objects are opened before its short identity-index
critical section. The section atomically reserves or joins one resource/adoption
slot for the proven held object; its held witness stays alive through pending
Ready/cancellation. Concurrent aliases cannot each publish a different resource.
Release this index guard before resource fences, Git, DB/FS waits or executor
RPC. Revalidate the reserved adoption/version under the resource fence. This
is the resource owner's one live index, not a persisted physical-key canonical
table or an independent per-path lock service.

## Current-object proof that can actually be established

Initial supported proof is **controlled directory-FD transfer with continuous
live retention**, on a qualified local filesystem/mount namespace. Open the selected root and Git-reported
worktree/gitdir/common-dir objects through controlled descriptors, retaining
the relevant entry/indirection facts. Transfer those actual descriptors with
SCM_RIGHTS on the authenticated registered Unix connection; the executor validates
and retains the received descriptors, then acknowledges the bound registration.
The kernel retains its lease until this acknowledgment. A sender crash after
transfer leaves the object held by the socket message/receiver; a crash before
transfer cannot produce Registered/Ready. Sending a path/stat tuple while
intending to keep an FD until ACK does not prove that overlap actually happened.
No unpinned gap may be bridged by a serialized device/inode pair.

The session transport gains a required versioned root-attachment operation:
resource/incarnation, actual executor boot, registration/transfer nonce and a
closed typed role shape (worktree root plus its required Git-layout objects, or
ExternalDir root). The descriptor count/order must match that shape. The library
frames metadata and ancillary rights together with recvmsg; truncated control
data, missing/extra rights, wrong object kind or mismatched nonce is refusal,
closing all received FDs. Receive CLOEXEC atomically and never reconstruct a
missing descriptor from its serialized integer or pathname. Only the registered
kernel connection can attach such authorized objects; this is neither an
arbitrary namespace/PID interface nor a trusted pathname broker. Prepare binds
the resulting registration witness. [unix(7)](https://man7.org/linux/man-pages/man7/unix.7.html)
defines SCM_RIGHTS transfer of open-file references.

During this overlap, dev/inode plus the qualified filesystem/mount context can
compare the already pinned objects. They cannot be reused for another object
while that object is still held. Git layout/indirection is rechecked under the
same registration operation; ordinary HEAD/index contents are not root identity.
Replaced `.git` layout or a changed named root requires recovery. Fork resolves
relative cwd from the pinned registered root and fchdir, not a late host-path
current_dir call. A current pathname must still name that object when required
by the launch plan; rename/substitution is refusal, not permission to use the
replacement. This proves root continuity, not source-content immutability or
confinement of every future path used by an ordinary worktree process.

The object comparison index is only a lookup over **currently held** witnesses
under the existing resource-registration fence. Any scalar bucket key is
ephemeral and verified against those live objects before alias convergence.
Retired/unwitnessed entries are absent from that index. Persisted dev/inode,
path, canonicalized path, btime, selection digest or Git HEAD never recreate it.
Qualified filesystem/mount semantics are required; unsupported comparison is
a typed host/resource capability failure, not guessed equality.
After kernel restart, rebuild this live index from reconciled active receipts
and their actual transferred handles before enabling resolve/admission. Unknown
old source owners are not guessed disjoint from new selections; their legacy
cutover remains closed until the owner/dependency closure below is established.

[Linux open(2)](https://man7.org/linux/man-pages/man2/open.2.html) establishes
that an open description survives pathname removal/replacement. A small actual
`/tmp` probe retained a directory FD, renamed it, created a new directory at
the old path, and read different old/new sentinels through the two live roots;
their pinned identities differed. Evidence: `/tmp/neige-2666-live-fd-proof.json`.
It used no production data, credentials, subprocess, model or host configuration.
It proves only that live case, not crash continuity or stable-handle support.

An alternative persistent filesystem-handle mechanism is **not** an initial
dependency. It needs qualified filesystem identity/export semantics and handle
staleness detection; mount IDs alone are not persistent.
[open_by_handle_at(2)](https://man7.org/linux/man-pages/man2/open_by_handle_at.2.html)
requires CAP_DAC_READ_SEARCH and may be unsupported/stale. Do not grant broad
host capabilities merely to make this design work. A future registered mechanism
can supply such a proof through the same owner, with its own platform contract.

## Owned materialization and borrowed adoption

An owned create/ensure operation persists a random source-incarnation nonce and
the original selection/binding/version before external effects. Workspace
materialization owns the versioned Neige marker/claim protocol, staging and
rename; existing legacy owner markers remain readable by their explicit decoder.
Never treat a marker containing only Track ID as a new incarnation receipt.
The random token records owned lifecycle provenance; it is copyable and is not
standalone cross-lifetime filesystem identity. Ready additionally requires the
live object proof, original effect receipts and expected binding CAS.

At actual materialization, retain the current upstream/branch selection timing.
After bounded refresh, persist chosen B and expected owned pin/ref before its
Git effect, then write/verify that exact pin, materialize and register the
root. A branch created by a failed worktree add is inspected against that same
intent on retry; do not choose B again. If only a legacy initial baseline is
unknown, encode LegacyUnknown explicitly; never take today's HEAD as original B.
Known B evidence and its owned pin are separate from root identity and remain
subject to the original cancellation/release operation.

Borrowed existing checkouts get **no marker write, directory claim or destroy
authority**. A LegacyAdoption operation freezes the authorized existing selection,
opens/pins its current object, reconciles old executions/source bindings, assigns
the real SourceIncarnationId, initial epoch/generation receipt and canonical
resource association, and then permits Register/PreparedLaunch. The initial
epoch means this actual adopted execution source; it does not assert an immutable
tree or a known creation B. Git/lease facts are captured separately by their
existing owner. No constant zero, dummy generation or observation-only Ready.

Adoption is an existing kernel recovery/materialization operation, not a new
policy manager. It can finish automatically for a current authorized Legacy
selection with no unresolved old execution, ticket, prepared source binding or
source-evidence consumer. Thus ordinary old worktrees need neither a privileged
filesystem handle nor a permanent Unsupported status. Explicit uncertain old
work remains recoverable/Busy until settled or canceled at its owner.

Owned legacy directories may be adopted only through their existing ownership
policy plus a current live proof and a recorded adoption decision. If previous
object continuity was lost, retaining/copying a marker does not prove ownership
of a replacement. Use explicit authorized ownership recovery/adoption, or permit
only a separately authorized borrowed-current-object execution; never silently
grant deletion of unverified replacement data.

## Crash/restart and retirement boundary

| Available evidence | Allowed recovery |
|---|---|
| Kernel/root lease remains live; executor restarts | Transfer the retained descriptors when registering the same pinned object to the new authenticated executor boot. Old execution/ticket state must first reconcile; the new boot does not adopt old numeric PIDs. |
| Kernel restarts; original executor boot retains its original registered FD | Authenticate that boot, verify its live provenance and recover descriptors through the same controlled ancillary transfer in the reverse direction before Ready. Do not replace this with path-open after a claimed liveness check; the executor could die in that gap. |
| Both lost all live handles | Persisted stat/path/marker is Unwitnessed. Do not deserialize Ready or bind old frozen work to the current pathname. |
| Old facts terminal and no old source-proof consumers; active Legacy selection remains authorized | Explicit new adoption can establish a new SourceIncarnation/epoch under that active resource binding, after sealing the old incarnation. It claims a new current object, not proven historical continuity. |
| Old execution/ticket or frozen source consumer unresolved | RecoveryRequired/Busy; observe/reconcile/cancel that exact old scope first. A new adoption must not route it to replacement storage. |
| Track deleted and new Track selects the same path | Fresh selection/binding and proof. No lookup revives the old audit resource just because stored stat/path values match. |

The same canonical resource may undergo explicit recovered source-incarnation
replacement only through its active binding/lifecycle transition, preserving
one ledger and invalidating old runtime handles. S3 LedgerV1/source-proof
consumers require their own restore/repair evidence; the Legacy adoption shortcut
does not rebaseline their history. Completed receipts remain historical and can
be replayed without any current object. A resource with no remaining binding
does not automatically become the target of a later selection.

Root leases hold the object until release/ownership effects settle. Unknown
identity postpones destructive work, including GC; seven-day/ten-minute policy
deadlines are not identity evidence. This preserves those policy/mechanism
owners and durations, not an unsafe promise to delete on time after proof loss.
Borrowed roots are always excluded from destroy targets. Source-proof pins and
permanent audit references are counted separately from active object leases.

## Required kernel states, CAS and lock order

Foundation Pending/Observed remains observational. New runtime materialization
is a separate required state, not extra nullable evidence fields:

| State | Required data / allowed next transition |
|---|---|
| Planned | Existing operation ID, selection/binding/version, authority/purpose, target descriptor, ownership disposition and cancellation version. |
| Materializing / Adopting | Frozen chosen B/pin or explicit LegacyUnknown, SourceIncarnationId, owned nonce where applicable, external effect intent and live-object lease. |
| Registered | Canonical resource association plus actual ExecutorBootId and live-root registration receipt; no launch ticket yet. |
| Ready | Materialization/adoption receipt, object-proof mechanism/keeper boot, real generation/epoch and binding CAS. Can resolve opaque handles, not rewrite operation source history. |
| Canceling / Releasing | Sealed binding version, pending effect IDs, exact pins/ownership references and release intent independent of domain row deletion. |
| Unwitnessed / RecoveryRequired | Missing/ambiguous lifetime or execution facts. No new authority is manufactured from observations. |

An opaque kernel `SourceObjectLease` owns actual root/Git descriptors and the
current keeper boot; it cannot be deserialized from a stat observation. A
`RegisteredObjectWitness` binds resource, SourceIncarnation, executor boot,
registration sequence and overlap-lease nonce. The sequence/nonce is receipt
identity, not proof without the held descriptors. Planned transitions reserve
their genuine generation/epoch sequence before Register; only the committed
Ready receipt turns those reserved versions into a usable runtime handle.

Use existing card-start/per-card fence -> operation drive -> track lifecycle ->
resume-replay order, then resource fences in canonical-resource ID order. An
alias-unification/adoption operation acquires the involved existing Track fences
in stable ID order before resource fences. Its short DB CAS cannot wait for
Git, object opening, executor RPC, process drain or filesystem IO. Git metadata
locks follow resource fences. Never acquire an outer card/track/drive lock while
holding a resource/executor gate. Intent effects run outside DB transactions,
with expected-version checks before each effect and at final receipt CAS.
Freeze/recheck the resource binding-set version as well as individual bindings.
If concurrent alias attachment changes the required outer lock set, release
and retry in order; never fetch a missing Track lock under a resource fence.

Delete/repoint first durably cancels pending materialization and closes its
binding's admission/version under that same fence. Revoke pending tickets and
wait for exact acknowledgement/known execution drain before removing/rebinding
owned storage. An ensure retry rechecks cancellation before every effect and
Ready CAS, so it cannot recreate an unbound directory. One binding's cancellation
does not close another binding or bump its resource generation. Area rollback
uses a new binding authorization version and cancels stale release intents;
old drivers cannot act on the restored binding.

## Real PreparedLaunch and frozen executor seam

`resolve(verified_authority, purpose, WorkspaceRef)` validates the frozen
binding/version, current object witness, lifecycle and relevant source evidence.
It yields an opaque LaunchHandle holding the object/registration lease and
current generation/epoch. The handle has no public cwd-to-capability constructor.
Each invocation revalidates admission and the originating task/operation owner;
resolving once cannot authorize an indefinitely delayed spawn.

The actual frozen executor protocol supplies
`RegisteredExecutorConnection::connect(endpoint, kernel_client_secret)` and
`.executor_boot()`, then Register / Prepare / Launch / Revoke / Control.
PrepareExecution requires exact ExecutionIdentity, ticket, separate
observe/interactive/lifecycle tokens, optional explicit alias handoff, and the
complete program/args/environment/io specification. Preserve those required
inputs; do not wrap a raw-cwd launch with fabricated IDs or empty authority.

Production ordering is: authenticate/qualify boot -> descriptor-transfer Register
and receipt -> durable kernel launch intent -> Prepare -> exact-ticket Launch
-> immutable receipt settlement in the existing operation. The launch intent
freezes canonical resource, binding, SourceIncarnation/registration proof,
generation/epoch, authorization, relative cwd, executor boot, ExecutionId,
ticket/specification IDs, full specification digest and handoff slot version.
Store secrets in scoped protected kernel state, not events/debug artifacts.
The existing operation references this state; it is not another operation ledger.

Prepare may be retried only with immutable equality. Launch/revoke share the
same exact ticket and executor resource gate. Lost receipt keeps occupancy;
replay retrieves that original execution, never allocates another ticket because
the caller disconnected. Definite NoChild/Revoked/Drained permits settlement or
alias handoff; Exited and Unknown do not. Domain deletion cannot erase these
independent execution/release facts. Every control carries exact receipt/slot,
purpose and token; late stdin/resize/signal/cleanup never follows an alias to its
replacement. Historical observation has a narrower artifact capability.

Necessary changes to the frozen executor before this bridge can qualify:

1. Resource WorkspaceId means CanonicalWorkspaceId above, not a foundation
   logical selection. Bind the source-incarnation/registration provenance into
   registration/intent/receipt equality; preserve one resource gate across aliases.
2. Report whether a registration is continuously held by this live boot. Its
   startup `mod.rs` currently reopens persisted registration paths; those are
   Unwitnessed and admission-closed, not restored source-object certificates.
3. Add controlled ancillary descriptor attachment and recovery export. Preserve
   the original live FD until successor Register has received/validated/retained
   its matching descriptors and acknowledged. Release/removal cannot create an
   unpinned handoff gap, including sender failure before receiver path opening.
4. Qualify Linux pidfd/subreaper/FD handling and non-Linux compilation/refusal
   explicitly. Re-run the unverified checkpoint edits and all affected controls.

The registered protocol must distinguish a continuation of an original live
RegisteredObjectWitness from a new adoption with its frozen intent/incarnation.
Register acknowledgments bind that witness and actual boot; restored scalar
registrations cannot answer as continuously held. Add this as a required
negotiated/versioned Neige contract before publishing PreparedLaunch. This is
an explicit integration change to the internal checkpoint, not a claim that its
current bare `Registered` reply already supplies that evidence. A path/devino-only
RegisteredV1 peer cannot qualify the stronger PreparedLaunch contract. The new
typed attachment has no missing-FD/default fallback; released explicit legacy
worktree control remains a separate negotiated contract.

No executor source is changed by this document. Existing same-boot path/devino
validation is useful only with the actual lifetime witness; it does not satisfy
the new cross-boot provenance requirement by itself. The registered connection
must refuse an unsupported stronger contract, not silently fall back on EOF.

## ExternalDir, legacy executors and providers

ExternalDir is a distinct authorized current directory registration with its
own ID/version/live root lease. It has no WorkspaceRef, source epoch, resource
ledger, B pin or candidate-bound evidence. Use the executor's distinct ExternalDir
shape, never a fake WorkspaceId/generation to fit Worktree. Validate explicit
root-external cwd at its owning API; view mode rejects this target. Ordinary
non-workspace service artifacts use their separate KernelArtifact capability.

Released supervisor v2 is accepted only by the positive Probe/frame negotiation;
v3 explicitly negotiates LegacyWorktree. That preserves existing worktree
control, not a typed Ready/receipt or a view guarantee. Unknown version, EOF,
parse failure or a missing configured executor cannot authorize legacy fallback.
Known legacy processes finish/reconcile through their negotiated legacy scope;
they are not forcibly restarted or adopted by numeric PID to upgrade a resource.
Hosts without qualified registered execution can retain the explicit legacy
worktree path while reporting typed/view support unavailable.

Provider and direct kernel launches consume the same PreparedLaunch. Direct
pipe processes need the same executor-owned helper/receipt/control path, not a
bare Command exception. Shared worktree provider services use explicit service
authority for service artifacts and per-session bound authority for project
actions; S2a does not claim S4 view isolation. S4's scoped service/managed console
continues to require its whole-service/tools/helper qualification. Provider
configuration, attachment and content readers consume WorkspaceContent and
their original visitor/common-execution authorization, not a displayed cwd.

## One admission cutover after replay/source closure

Launch protocol migration and resource task-admission activation are different
axes. A real typed terminal launch can land before resource-wide scheduling,
while task scheduling remains explicitly Legacy and destructive lifecycle counts
both old and new execution facts. Do not advertise shared-resource admission
from a foundation ID, a single migrated caller or a terminal-only integration.

Cutover is the existing resource recovery operation:
`Legacy -> Converting(operation/version) -> ResourceAdmissionV1(receipt)`.
It seals new claims for all proven aliases, reconciles old task/lease/operation
source owners and their execution facts, then atomically enables the new domain.
Until closure, no partial path-vs-resource double scheduler. Ordinary existing
work can finish under explicit legacy semantics; unknown old work remains Busy.

Each operation adapter declares source/dependency codecs and active phase
references through its existing registration contract. Use exhaustive typed
declarations for workspace-bearing versus workspace-free work, not kind/file
inventories or an optional field that silently defaults to no dependency.
Frozen task/lease/terminal/provider/forge/gate/child outputs and scheduler payload
replay are covered through those owners, including old catch-up/reader leases.

New operations carry required typed targets. Historical input/output/event/hash
bytes do not change. An owner decoder writes a CAS sidecar keyed by original
OperationId plus payload hash and codec version, freezing the original selection,
binding, canonical resource, source-incarnation/evidence and authorization that
can actually be proved. It never looks up the current Track as a missing-source
fallback. Unprepared legacy work may adopt its original still-authorized frozen
selection at its documented prepare point; a previously prepared source binding
without continuity cannot be silently rebound. Finish/cancel it in its legacy
contract or require recovery. S3 source-proof consumers impose their stronger
evidence independently of current directory readability.

Completed keyed replay is checked before any source resolution/materialization
or current Track access. It returns the original stored result/tombstone after
Track deletion and physical release, subject to original result-access authority.
It does not call ensure, create storage, rebind a sidecar or execute again.
Terminal operation completion releases active execution dependencies, while
real source-evidence consumers retain their own pins. Permanent dedup history
alone retains neither a root lease nor storage.

Once cutover is complete, scheduler ready-set, claim transaction, supersede,
lease uniqueness and busy projection consume the same resource aggregation of
the existing checkout_admission rule. Readers may share and writer fairness
remains. An ordinary terminal is not a new worker exclusion but does block
destructive quiescence while it or descendants can act. Source/object identity,
task admission and positive destruction readiness stay separate conclusions.

## Independent release and cancellation

Before deleting the last domain binding, persist the existing S2a release intent
with complete owned descriptor, incarnation/proof, pending materialization IDs,
exact pin names/OIDs, policy deadline/disposition and active references. Existing
recycle, worktree clean, sweep and area compensation consume it and record
inspectable receipts. A missing expected resource may settle; unknown identity
cannot. Unexpected ref values are retained for recovery. Partial sibling failures
do not discard the rest of the release job. Borrowed directories are not targets.

S2a retains seven-day managed trash and ten-minute eligible ignored cleanup,
and existing worktree deletion policy. It adds no S3 7/90 archive behavior and
does not make a pin in a reclaimed private repository durable S3 retention.
No GC may derive authority from a completed old operation or resurrect its
selection. Repoint and restore use the same cancellation/release CAS so a stale
cleanup cannot touch new adoption, binding, incarnation or execution.

## Six C4 seams and delivery status

| Shared seam | Owner / actual delivery requirement |
|---|---|
| Persistent WorkspaceRef | Workspace kernel/truth: canonical Ready binding and owner codecs. Foundation selection/observation is not this delivered API. |
| resolve(principal,purpose,ref) | Existing auth/C1 validates identity/provenance; C4 supplies accepted Area common grant; kernel workspace service checks both target and purpose. No C4 grant is fabricated here. |
| Launch permission | This kernel bridge plus qualified executor: object witness, real versions, durable intent, exact ticket/receipt and all controls. Internal executor code alone is not delivery. |
| WorkspaceContent | Workspace service and existing Track file/attachment/frontend ports; readers use visitor authorization and shared execution uses its frozen grant. Caller migration remains required. |
| Occupancy/quiescence | Existing checkout admission/lifecycle plus exact executor drain; switch only after legacy source-owner/codec closure. Collaboration owns message batching, not another lease policy. |
| Revision/GitFacts/results | S3 owns immutable source evidence and the one delivered-tip ledger; C4 references them. S2a root/adoption observations never impersonate them. |

S2a supplies the workspace side of C4 integration, not membership, conversation
batching, common credential isolation or multi-user release approval. Those
authorization/feature owners must integrate their real conclusions into these
ports before multi-user execution is opened. S1 remains unchanged.

## Smallest implementation slices and acceptance

1. **Object/materialization bridge and one real terminal consumer.** Extend the
   narrowed foundation with canonical/source-incarnation states and live-root
   proof, materialization/adoption intents/B pin, cancel/release and qualified
   Register provenance. Introduce real PreparedLaunch at the existing terminal
   operation/renderer start path. Preserve explicit legacy other callers and
   admission mode. Prove pipe/PTY startup, retry, controls, same-uid authentication
   and positive descendant drain through the production entry point.
2. **All launch/content and persistence owners.** Migrate create/ensure/repoint,
   Planner/conversation/provider/direct processes, worker/gate/forge/checkpoint,
   system/today/child and terminal restart/attach callers through the same kernel
   bridge. Their owning codecs bind all old/new task/lease/session/operation
   sources and result replay. Reuse existing operation registration to enforce
   closure; a new variant declares its source/dependencies at that owner once.
3. **Atomic admission activation and frontend/content finish.** Reconcile proven
   aliases and old owners, switch the one occupancy/claim/lease/projection path,
   migrate Track file/preview consumers and run integrated UI checks. Only this
   completed closure can claim the resource-admission part of S2a. S3/S2b/S4
   capability and delivery claims remain separate.

These are reviewable implementation outcomes with real consumers, not three
new managers. Dependencies/cancellation for every earlier slice are real before
merge; later phases cannot excuse an unsafe active release or fake cap now.

S2b's proposed RuleView is the single source/classification/rule-version owner
when that backend is implemented; it is not a current production API. Future
Ready/source-epoch plans consume that owner contract rather than introducing
another rule digest/classifier here. The S2a live root certificate still is not
a RuleView, Revision or immutable SourceProof.

Critical contracts: overlapping Register vs rename/replacement; two aliases of
one held object vs two different worktree roots; copied marker/stat reuse not
restoring identity; kernel-only/executor-only/both restarts; ordinary borrowed
adoption; old unknown execution blocking new adoption; pin-before-Ready then
delete/repoint; area compensation and stale release; exact retry after alias
handoff; owner codecs/hash stability; completed-key replay after deletion; and
ordinary terminal allowing worker admission while preventing destroy.
Include sender death before/after ancillary enqueue, receiver death before/after
Registered, descriptor truncation/substitution and reverse-export crash gaps.
No typed Ready may be recovered from a root lease that was never actually held.

Run focused production tests/mutations at their owning implementation slice,
text/contract/quick gates as applicable and CI broad suites. Register tests
through existing targets/contracts; do not maintain test-name/file/handler
allowlists. Exact-binary/platform contracts run on the mechanism change and
upgrade, not as unrelated permanent per-file checks. New variant ownership is
an exhaustive typed contract; adding a test/file does not require editing a
second census. This document runs only text/diff/link checks, no Cargo or model.
