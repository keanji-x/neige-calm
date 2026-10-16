# Linux same-path workspace mechanism (S2b)

Status: mechanism proposal for issue #2666, 2026-10-16. Not implemented or
qualified. L2: this crosses filesystem isolation and recoverable storage
boundaries. Read with the [parent](track-workspace.md), the S2a
[identity/launch contract](workspace-identity-launch.md), and S3's
[delivery](workspace-delivery-protocol.md) and
[verification](workspace-verification-decision.md) protocols. S1 stays unchanged.

## 1. Mechanism decision and compatibility change

The historical experiment mounts kernel overlayfs directly over a writable,
externally edited checkout. That cannot be the supported implementation.
[Linux's overlayfs contract](https://docs.kernel.org/filesystems/overlayfs.html#changes-to-underlying-filesystems)
excludes underlying-tree changes while mounted. Offline lower changes also
require that metacopy, index, xino and redirect_dir have not been used.
An idle remount does not legalize changes that happened while it was mounted;
no stress-test result changes that contract. The same restriction applies to
external source edits, atomic replacement, rename/delete, branch checkout and
source Git maintenance, not only unusually destructive operations.

The PM selected direct mediated COW to preserve the parent's live-source goal.
This is an explicit mechanism amendment requiring fresh dual L2 review:

| Route | Supported promise | Cost and limitation |
| --- | --- | --- |
| Direct mediated COW | Keep ordinary live source observations and private first-write ownership. Implement the union through the S3 FUSE mechanism, without overlayfs over a changing tree. | Expands the shared mediator beyond verification. Requires a reviewed live-read/cache/mmap contract and real FUSE qualification. |
| Immutable source epoch | Capture source through an actual atomic filesystem-snapshot capability, mount a private immutable image, and refresh only after full quiescence. | New external source changes appear only after sync. Dirty/ignored capture includes their storage cost; arbitrary ext4 checkouts do not acquire an atomic tree-snapshot capability through copying. |

The immutable-epoch alternative is not the initial route or a hidden fallback.
A future registered source provider may offer genuine atomic snapshots with
its declared visibility contract. A plain recursive copy, two equal
enumerations, timestamps, inotify or hash samples are not an atomic tree
snapshot in the presence of independent writers. Per-file
[FICLONE](https://man7.org/linux/man-pages/man2/ioctl_ficlonerange.2.html)
can atomically separate one supported file's bytes; it does not snapshot a
directory tree and may be unsupported. Do not silently select a weaker route.

Sections below specify the selected direct-mediator ownership boundaries and
implementation obligations. The cache/mmap
qualification in section 4 is a release condition, not an assertion that FUSE
alone solves external consistency. There is no supported live-overlay variant.
The persisted backend must be versioned for the actual mechanism; do not name
a new mediated implementation `LinuxOverlay` and preserve a false wire promise.
An unshipped name may be corrected in one migration/schema sweep. A released
descriptor needs an explicit new variant/decoder; never reinterpret old bytes.

## 2. Evidence obtained here

These are disposable local mechanism probes, not production acceptance:

| Probe | Actual observation on Linux 6.1.0-37-amd64 |
| --- | --- |
| User/mount/PID namespace, `userxattr` overlay, read source file then replace it atomically outside the overlay | Source read `atomic-new`; merged path still read `initial`. |
| Look up missing merged path, then create it in source | Merged path still appeared missing. |
| Fully unmount, remount the same upper against changed lower | New source bytes became visible and the private agent file survived. No agent file appeared in source. |
| Overlay an independent immutable copy, then edit the original source | The immutable view retained old source bytes; a view write changed neither original source nor immutable copy. |
| FICLONE in the local scratch filesystem | EOPNOTSUPP, errno 95. No zero-copy capability established. |
| Enter outer mount user namespace then nested real-identity user namespace | Inner uid/gid were the caller's 1000/1000, mapped to outer 0/0. This did not test any provider sandbox. |
| Overlay a scratch Git linked worktree with an external `.git` file | Git still resolved the external common directory; a scratch `update-ref` through the view changed that source repository. |
| Independent `git clone --bare --no-local` scratch repository, then remove source Git storage | Imported commit remained readable. This validates the small probe's independence, not S3's full durable-content receipt. |

All temporary mounted fixtures were unmounted and removed in `finally` blocks.
Probe source and stdout provenance live under `/tmp/neige-2666-s2b-probe/` for
this session; no production checkout, credentials, provider or model was used.
The host has no `/dev/fuse`; no FUSE daemon, cache/mmap behavior, benchmark or
supported backend was tested. Missing FUSE is an explicit Unsupported result,
not a passing skipped mechanism test.

## 3. One resource, one holder, one executor protocol

`workspace` owns filesystem mechanisms. Kernel workspace service owns selection,
authorization, lifecycle, source-HEAD confirmation and effect plans. Truth owns
their durable intents and CAS. The S2a executor owns actual execution receipts,
fork/revoke linearization, descendants and controls. S3 owns Revision,
normalization/classification, durable content and all installation policy.
There is no backend scheduler, lock-by-path, source ledger, reaper, provider
session manager or alternate shell execution service.

The view descriptor binds WorkspaceId, backend/version, source directory and
Git identities, logical root/cwd scope, owned storage identity, materialization
operation, generation, install epoch, and backend manifest versions. Display
paths are not identities or capabilities. Two views over one source have
different WorkspaceIds/private state; legacy aliases of one real checkout
remain the S2a shared resource. Same-looking cwd does not select a lock domain.

A trusted holder retains the private user/mount namespace and the registered
mediator mount. It is passive infrastructure: no arbitrary-command RPC, user
stdin or repository-code execution. It exposes only the authenticated mechanism
protocol and owned namespace FDs. The boot nonce, namespace device/inode/type,
mount/root identity, mediator instance and source/storage descriptors are in
its receipt. A PID or `/proc/<pid>/ns/...` pathname supplied by an adapter is
never accepted as authority. Kernel/executor opens, verifies and retains the
owned namespace capability; `setns` uses that capability.

Bootstrap must be authorized by the existing materialization intent through
the same S2a executor journal before Ready. S2a currently has registered
Worktree and ExternalDir targets, without namespace FDs. S2b adds an explicit
execution-envelope version and required bootstrap/registered-view target
variants; it cannot impersonate ExternalDir or prematurely make a view Ready.
This extends the executor owner, not a second ticket service. Its receipt is
bound to the operation/resource/generation and inspected on replay. Ordinary
launches require Ready, resolve the same LaunchHandle, consume the same
single-use ticket immediately before fork, and record the same ExecutionId.
Controls retain the required exact `ExecutionControl` envelope, including
resize, signal, observation and cleanup; a namespace identifier grants none.

Joining namespaces uses the executor's dedicated single-threaded trampoline,
not `setns` on a multithreaded server. The
[setns contract](https://man7.org/linux/man-pages/man2/setns.2.html) constrains
user-namespace membership and PID-namespace effects on subsequent children.
The trampoline enters the controlled mount environment and creates the actual
execution scope through S2a. All user execution sees its verified real uid/gid
through the typed mapping plan, drops bootstrap capabilities and supplementary
groups, closes administrative FDs, and starts with an explicit environment.
Mapping is derived from the authorized OS identity; 1000 is probe data only.
Namespace root used for mount construction is not the program's user identity.

The holder/mediator is accounted infrastructure, not a live user turn silently
subtracted from occupancy. Generic executor evidence distinguishes passive
runtime infrastructure from executions that can mutate source or issue tools.
An idle transition seals content/launch RPCs and positively drains every such
execution and content permit before mechanism maintenance. Infrastructure must
itself stop and reconcile before its mount/storage can be destroyed. A dead or
replaced holder/mediator, unresolved bootstrap, lost namespace FD or unknown
descendant yields Busy/recovery, never guessed drain or immediate recreation.

The sandbox reuses `crates/sandbox`'s typed system mounts, private proc/tmp/dev
and explicit binds. Its current lexical host-path validation is insufficient
for source/storage authority: mechanism-bound descriptors must be identity
verified before the owning bind operation. No source checkout alias, real
Git administration, private backing directory, global kernel artifact root,
credential root or host namespace/control FD reaches user code. An independent
PID namespace prevents host `/proc` from recovering those backing paths. A
provider may further restrict the plan, never widen it; S4 owns qualification.

## 4. Live reads and private mutations

There is no kernel overlay mount in this route. The mediator resolves the union
itself against a read-only source capability plus owned installed and private
state. The real source is neither writable backing nor a mount exposed to
children. All mutations resolve both ends, including rename-exchange, link,
metadata operations, ioctl/copy offload, mmap and delayed writeback.
Unsupported mutating operations fail explicitly; a generic passthrough may not
hand out a writable source FD or recreate a host pathname from a request.

Live observation has an explicit close-to-open, open-scoped contract, not full
POSIX cache equivalence or a simultaneous tree snapshot. A fresh pathname
resolution/open captures the source file's observed bytes and metadata into an
independent immutable read object. Subsequent reads and source mappings through
that handle use that object until release. New opens after an external change
capture a new observation; old FDs and mappings may retain the old observation
after both atomic replacement and in-place writes. There is no promise that an
existing mmap synchronously reflects host writes. Source mutation during the
capture may affect its ordinary read or return Busy; this acquisition is not
a claimed atomic whole-source state and is not a Revision.

The implementation must negotiate and qualify its FUSE I/O mode explicitly.
[Kernel FUSE I/O modes](https://docs.kernel.org/filesystems/fuse/fuse-io.html)
make this material: direct I/O avoids the page cache but shared mmap is
disabled unless its negotiated capability permits it; writeback caching
assumes changes go through FUSE. The
[libfuse cache configuration](https://libfuse.github.io/doxygen/fuse_8h_source.html)
also distinguishes data retention from open-time invalidation. Neither
`auto_cache` nor notification alone proves external mutation visibility.
Direct I/O for all FUSE opens is rejected as an easy answer: it must not make
ordinary mmap/exec/build behavior disappear from the supported product.

Use immutable source-observation objects with ordinary cached read/mmap support;
the FUSE page cache then refers to bytes with no external writable alias. Each
observed content/metadata version has its own virtual inode/observation ID.
Fresh namespace resolution revalidates source by descriptor and actual capture,
with zero negative/entry/attribute retention and explicit namespace invalidation;
TTL, stat equality and inotify do not authorize reusing mutable source bytes.
An acquisition/open race binds the already captured observation or returns
Busy/stale and retries; it cannot rewrite the old observation's backing image.
An older open remains attached to its old virtual inode when a fresh lookup
finds a changed version. The extra inode/cache semantics are intentional and
must be visible in the backend's support contract.

Read-only shared and private source mmap and executable mappings operate on
that immutable image. MAP_PRIVATE writes affect only memory. A writable open
or mutating metadata operation first creates independently owned mutable W;
a later shared writable mmap uses W, never the original source or its immutable
observation. The descriptor/inode transition and cache invalidation are
serialized before the writable handle is exposed. Owned mutable W/cache files
use the negotiated write-through/mmap mode, with no external backing writer.
Every delayed callback retains its original handle/object identity through
path replacement, unlink and ownership changes. Old read-only handles never
gain write permission through the transition.

This requires actual capture of opened source files, without eagerly copying
the whole checkout. It adds read amplification, immutable observation storage,
copy-before-write and cache-invalidation cost; safe reflink is an optional
optimization, not assumed on ext4. Observation objects are mechanism storage
references until all FDs/mappings release, not delivery pins or W ownership.
Preserve source metadata needed by hot builds; repeat-build acceptance measures
the cost and verifies ignored cache reuse at its original logical location.
Kernel/FUSE mmap/copy-up/invalidation tests must pass before selection. Refusing
an entire ordinary compile path merely because the implementation chose
FOPEN_DIRECT_IO does not satisfy this design. Optimizations need separate
qualification and cannot silently change the visibility contract.

First source mutation never modifies a source inode. Under the mediator's
per-entry serialization, resolve the current source object, copy its bytes,
mode/link value and provenance into owned staging, fsync as required, then
atomically install an owned W entry before acknowledging the mutation. Safe
reflink is optional; hardlinking source into private mutable state is forbidden.
Concurrent source replacement or an unprovable copy records conservative
overlap or refuses the operation, never grants source write authority. A
later snapshot reads only the completed owned W object, not a mutable source
alias. Source read uncertainty cannot be promoted to a clean provenance proof.

Persist entry ownership before exposing a writable handle. The owned manifest
binds stable object IDs, expected prior version and staging/result identities;
it contains union state, not execution/delivery authority. File/directory
creation, rename, masks and removal have inspectable before/after identities.
Use same-filesystem atomic publication and fsync the ownership metadata and
parents before its receipt. A crash between data publication and manifest
settlement preserves both and enters inspect/recovery; it never discards an
unrecognized object or falls back to source. Snapshot needs a mediator write
barrier and positive accounting for writable mappings and delayed writeback;
uncontrolled outstanding mappings return Busy rather than an unsafe seal.
Flush/fsync completed owned content and seal one manifest version before S3
capture. Ordinary later file writes obey that storage's
documented fsync durability, not an invented per-write exactly-once operation.

Open handles retain owned object identity through rename/unlink/path reuse.
A delayed writeback cannot re-resolve a reused path into source or another
entry. Directory enumeration resolves a bounded merged namespace; per-handle
enumeration may be a snapshot, while fresh enumeration revalidates source.
Source directory movement cannot redirect an already verified descriptor into
an unrelated root. Source-root identity replacement closes new admission and
requires recovery; display-path equality does not authorize adoption.

Use descriptor-relative containment and explicit no-magic-link/no-cross-mount
rules from [openat2](https://man7.org/linux/man-pages/man2/openat2.2.html).
Normal in-root symlink values remain content; following them must stay within
the granted view capability. Absolute/out-of-root aliases cannot reveal source
backing, another workspace or private storage. Initial support rejects nested
mounts, special files, Gitlinks and hardlink aliases outside the supported
capability. A runtime type/alias change refuses the affected operation and
source evidence rather than silently selecting another backend.

## 5. Installed D, private W and delivery

The backend owns a versioned union manifest, not an extra delivery ledger.
Its effects and roots belong to the existing operation intent/receipt. It
does not infer W by scanning an overlay upper or interpreting private Git
commits. Logical D starts from B and later names the installed delivery/catch-up
source supplied by S3. Unmodified live source dirt is observational context,
not an automatic contribution to Revision. W records source changes owned by
the view; newly ignored data remains the owned cache class.

| Entry or effect | Required record and behavior |
| --- | --- |
| Owned source content | Validated relative path, owned byte object, mode/link, origin/source identity, first-write evidence and overlap status. |
| Deletion/whiteout | An explicit W tombstone against its expected entry identity. It hides later lower reappearance until an authorized W change; not a device node copied into a Git tree. |
| Opaque directory | Explicit namespace ownership/masking plus the affected descendant set. A rename/removal of a merged directory captures a stable plan or refuses Busy; it cannot acquire arbitrary future source children by accident. |
| Installed override | S3 SourceInstallPlan's exact D target, before/after footprint and manifest sequence, separate from W and ignored cache. |
| Cache entry | Git classifier/rule version and owned cache object. It is retained across D/W bookkeeping when rules still permit; tracked precedence can promote a path to source only through the S3 effect owner. |

Lookup uses W ownership/tombstones, installed overrides/masks and then source
observations. Tree evidence is exactly D composed with owned source W, including
mode/link/delete, never the whole currently visible lower. Source-only dirty
paths do not enter delivery merely because a new sync saw them. A rename is one
owned effect with both endpoints; subtree masks and source-origin metadata must
survive restart. No xattr filename convention becomes an adapter API.

Snapshot uses S3's sole classifier/normalizer and MaterializedSource owner.
Tracked paths override ignore; untracked paths use the frozen effective Git
rules through that owner. S2b does not implement ignore patterns or add familiar
cache directory names. Before snapshot, use S3's owner/stability permit and the
mediator write barrier, settle pending writes/copies and complete/flush the
owned manifest. Unaccounted writable mappings or effects refuse Busy. Capture
one manifest version and refuse on concurrent changes or unresolved source-copy
ownership. A waiting provider may remain live for the already specified
same-source metadata path; snapshot does not require that call to drain itself.
Real SourceInstallPlan and destructive lifecycle still require full S3 scope
drain. S3 then pins the
normalized tree and exact raw-byte closure. Backend `.git` status is not this
evidence, and source changes cannot alter a captured W object after that point.

Overlap includes copied source dirty bytes relative to the expected installed
source, unknown attribution, and conflicting external observations. Kernel
owns confirmation policy. A typed confirmation operation binds principal,
authorization/binding version, exact Revision and overlap digest; any Revision
change invalidates it. UI/adapters cannot clear a manifest flag. Delivery
without required confirmation refuses through the same S3 checkout admission.

`install_delivered` consumes S3's sealed effect plan. ProvenSameSourceProjection
may reclassify W into logical D only if the owner proves identical visibility,
bytes, modes/links, rule/classification, permissions and root/mount identity.
Metadata receipts advance their own sequence and do not advance source epoch.
If removing W would reveal different live source bytes, an index update changes
classification, or opaque masks change visibility, that proof is unavailable.
Use the full SourceInstallPlan, Accepted/drain continuation and new epoch;
never clear W merely because normalized trees match. Ignored cache survives
only where the same plan proves its retained capability and classification.

## 6. Git metadata and source HEAD

Initial support requires a local supported Git checkout with real internal
`.git` directory, no external gitdir/common directory, external alternates or
unsupported conversion configuration. Query identities and object format via
[Git-reported paths](https://git-scm.com/docs/git-rev-parse); do not parse a
guessed worktree administration layout. Reject `.git` files/symlinks pointing
outside the selected root, submodules, unsafe config/includes or filesystem
types before selection and on replay/recovery. No non-Git fallback.

The view never exposes the source `.git`, even read/write through a union.
Create an independent private execution Git repository from the authorized
S3 scoped export, containing only required closure/refs. No hardlinks,
alternates, source gitdir pointer or resource-store administrative path reaches
it. The
[Git clone contract](https://git-scm.com/docs/git-clone#Documentation/git-clone.txt---no-local)
illustrates why local sharing and concurrent-source races need care; the
production path uses S3's verified content receipt, not a claim that one clone
command supplies all durability/authorization. Source `git gc` cannot erase
already imported private or durable objects.

Private config/index/HEAD are execution projections. HEAD is detached; kernel
projection writes use S3's no-deref rule. Initialize frozen normalization,
effective ignore/exclude rules and approved configuration from S3. Repository
code/hooks run only in the authorized execution sandbox, never SourceGitAdmin
or the trusted object writer. Reject initial view repositories with custom
clean/smudge/LFS, working-tree-encoding or eol/autocrlf transformations, and
recheck at snapshot/prepare/recovery. Private Git edits do not redefine B,
upstream receipts, delivery tip, W ownership or the shared classifier.

SourceGitAdmin only observes the source identity, HEAD/config and required
objects for kernel preparation; its typed operations cannot run provider or
repository code and cannot mutate user refs/index/checkout. Materialization
freezes actual B and imports/pins its required closure before Ready. Concurrent
source checkout/gc or missing objects retry against that same intent or refuse;
never pick a new HEAD under an old key. Source identity and owned runtime Git
identity remain distinct in every receipt.

At every full quiescent sync, unconditionally resample source identity/HEAD and
revalidate the mediator's source observations. No inotify fingerprint decides
whether sync runs. A changed source HEAD pauses new launches and emits a typed
confirmation requirement. Confirmation binds old/new observed HEAD plus source
and binding versions; it updates observed HEAD only. B, D and delivery tip do
not change. Rebuild is archive plus a new track, not reset of the user's tree.
An already live interactive PTY is not promised command-by-command interception.

## 7. Creation, close, delete and recovery ship together

View selection is explicit and freezes with the existing selection/frozen_at
writer. The creating transaction persists the backend/version, source,
authorization and requested service mode, not a guessed baseline. The existing
materialization operation then validates support/extents, freezes/imports B,
creates owned staging and private Git, starts registered infrastructure, probes
the real mount/content/launch capability and CAS-publishes Ready. No failed
stage returns a host cwd. Replay inspects the original operation and ownership
marker; it may remove only its own known incomplete resources.

The source, private storage, content artifact stores, provider state and
credential extents must be identity-disjoint, including ancestry, aliases,
hardlinks and mount traversal. Validate controlled registered extents through
their owner, not string-prefix checks or a `.git`/directory-name exclusion list.
The source root may never be an ancestor containing destroyable private state.
Any unprovable overlap refuses create/recover. A DestroyPlan can be built only
from an owned storage capability and expected marker/version; it has no field
capable of naming source storage or SourceGitAdmin.

Use S3's single lifecycle and independent tombstones. Close seals new admission,
revokes unused tickets and waits for complete execution/content quiescence.
Unknown descendants, dead mediator or source-copy effect remain snapshot_pending
or recovery. Archive through the shared Revision/content owner, verify/fsync
both normalized and raw closures outside all private runtime state, then record
ArchiveReady. No archive receipt means no reclaim. Seven/ninety-day deadlines
are frozen by S3's typed settings, not a backend timer.

Reclaim closes the registered infrastructure, reconciles its exact receipts,
unmounts only the owned mount and recycles/removes only owned private state via
the existing managed recycle mechanism. Do not lazy-unmount then reuse its
storage while an old mount/FD can still access it. A namespace/mount held by an
unknown process keeps references and blocks deletion. Source checkout, Git
registration/refs and branches are never destruction or compensation targets.

Recover never identifies a holder by PID alone. Reconcile the existing executor
journal and operation; verify marker, directory/root, namespace and manifest
identities. If old access cannot be excluded, remain Busy/recovery. Once old
scope is positively drained, rebuild infrastructure through the same intent,
increment generation and reject old handles/tickets. Source install epoch
changes only under S3's SourceInstallPlan. A dead holder alone is not permission
to start a second writer on its private storage.

Reopen before reclaim verifies/cancels stale release intents through CAS.
After reclaim, restore D/tip and A's raw W through durable content receipts,
without source `.git`, smudge, network or private filter cache. Preserve the
archive's original bytes/modes/links; missing/expired closure refuses restore.
Source context needed for further live observation must be explicitly verified
again; archive restoration cannot silently adopt a replacement source root.
Area rollback restores bindings/references and the existing restoration gate;
stale cleanup cannot delete a restored resource.

## 8. Public capability and real entry points

S2b supplies the explicit view selection API, wire/OpenAPI/generated artifacts,
capability/refusal projection and content ports. A successful capability badge
is descriptive; every action prepares a typed result under current authorization
and runtime facts. Selection is not a provider qualification claim.

Creation names either a qualified provider profile or explicit no-provider
mode. If a Planner/conversation/worker is requested without S4 ViewSupport,
refuse before selection. Explicit no-provider tracks can use qualified terminal,
content, gate and controlled forge paths; do not auto-start a Planner during
create, wake, recovery or UI navigation. Existing worktree behavior is unchanged.
View parent/child creation and out-of-root cwd remain explicitly Unsupported.

All read/list/raw/status/diff and authorized content writes consume the S2a
WorkspaceContent capability and operate through the same mediator/root identity.
No server-side merge implementation or host `std::fs` fallback is added.
Revision reads use their immutable content handle; live view reads say that
they are live. Preview/image/HTML/report/attachment paths keep their owning
authorization, MIME, size and managed-only rules. Whiteouts return the same
absence in every port, including raw/preview; encoded traversal/symlink inputs
cannot recover source/private backing. Display path remains presentation only.

Gate preparation uses C's frozen MaterializedSource plus S3's shared immutable
verification mode and dynamically classified owned ignored cache. It does not
run against live source or infer immutability from this COW backend. External
source edits after preparation cannot affect tested source; existing tracked
entries inside ignored directories remain immutable. Gate proof, invalidation,
VFS-pre-denied mmap semantics and descendant seal are exactly the S3 contract.
No second ignore parser, output directory setting or source-write event ledger.

Controlled forge probes/actions launch through the exact workspace capability.
Source-mutating installs and Planner commits use typed kernel Services and
S3's effect owner, not plugin/action-name interception. Publish gets a scoped
export containing exact C and frozen remote/PR authority; it cannot inspect or
write another resource store. Native Git shell cannot update user common-dir
refs through a hidden alias. A forge path that still requires host cwd is
Unsupported for a view until migrated, not sent to the source checkout.

## 9. Implementation sequence and qualification

The atomic release surface is one selectable backend with create, usable
no-provider entry points, close/delete/recover and complete content/evidence
boundaries. Internal commits may separate mechanism, truth/API and callers,
but no intermediate selectable half-backend merges. Start only once S2a's
real executor/identity interfaces and S3's real effect/content/verification
owners are available; these names are contract responsibilities, not unused
public scaffolding to add early.

1. Review section 1's selected mechanism/visibility amendment and section 4's
   open-scoped I/O mode through both full L2 channels. If the mode cannot support
   ordinary builds/mmap safely, narrow the declared platform profile before
   implementation; successful unrelated probes are not qualification.
2. Implement the shared path-mediator mechanism and owned manifest algebra
   behind S3/S2b owners, with one Git classifier interface. Keep filesystem
   request handling separate from resource policy and private Git plumbing.
3. Add the controlled executor namespace capability/bootstrap extension to the
   existing protocol and journal, then creation/recovery/extents validation.
   Do not merge a direct raw-spawn path while waiting for S2a.
4. Integrate existing lifecycle operations, S3 content/effects and real content,
   terminal/gate/forge ports. Generate all API/wire artifacts from their owner.
5. Run the provisioned mechanism job plus focused production-entry replay,
   then both fresh L2 reviews; fix blockers and rerun invalidated checks.

Required acceptance covers these invariants, not a permanent list of current
test names or applications:

| Boundary | Required production-path observations |
| --- | --- |
| Source separation | Two resources at the same logical path see private writes independently. External in-place/atomic writes, create/delete/rename, branch checkout and source gc obey the selected documented visibility model; no operation writes source. |
| Manifest/source evidence | D/W independence, source dirty exclusion, copy-up overlap, delete/reappearance, opaque subtree/rename, ignored/tracked transitions, modes/symlinks, open-unlink-reuse and delayed mmap/writeback preserve exact owned object identity. |
| Gate | Frozen C remains unchanged during every external source mutation; arbitrary Git ignore/negation/new cache paths and tracked-in-ignored work. Actual source writes cannot occur; mediator loss or unknown descendant cannot seal. |
| Process | Exact uid/gid, PTY and pipe, fork/revoke/namespace swap races, setsid/double-fork descendants, holder/mediator death, old ticket/control after recovery and multiple alias contenders use S2a receipts without host execution. |
| Lifecycle | Create immediately followed by close/delete, crash at every staging/pin/Ready/mount/manifest/archive/recycle receipt, day-eight whole-private-tree deletion/reopen and day-ninety expiry preserve durable raw source and never select source in DestroyPlan. |
| Capability/API | Explicit no-provider mode never launches Planner; provider request refuses until qualified. Every content/preview/raw port sees the same whiteout and traversal denial, and authorization revocation reaches the exact binding scope. |
| Storage/export | Extent ancestry/alias collision refuses; no source Gitdir/alternate/private administrative FD escapes. Authorized scoped export cannot observe another workspace/Area's closure. |

Provision a dedicated ephemeral Linux CI job with actual `/dev/fuse`, supported
libfuse/kernel, user/mount/PID namespaces, the real executor envelope and a
scratch Git fixture. Perform real mount, ordinary read/build/mmap, source
mutation, cache mutation, proof sealing, crash/recovery and positive drain.
Missing capabilities fail that qualification job. Generic runners may test
Unsupported separately. Do not install dependencies or widen namespace/FUSE
policy on the shared production host. No model or credentials are needed.
Record actual runtime/platform identities in the capability receipt and rerun
the mechanism contract when its owned mechanism or supported platform changes.

Critical mutations target source routing, stale generation/ticket rejection,
W ownership and archive-before-destroy in an exclusive recoverable worktree.
Predict all red names, run the whole relevant target/filter, restore exact
production bytes and prove green. Ordinary tests need no mutation ceremony.
Worker runs focused tests/formatter; PM gate runs text, contract, quick Rust
preflight and one nonempty focused replay. CI owns broad suites. Visible API/UI
integration also needs real browser preview and relevant browser tests.

## 10. Architecture and permanence cost

The shared mediator owns containment/FD identity and union operations; S3 owns
Git classification and source evidence; kernel owns authorization/lifecycle;
provider owns external runtime qualification. No layer knows plugin names or
guesses external session/Git layout. All process work uses S2a's single journal
and control protocol. Do not merge a second implementation of ignore, locking,
delivery, filesystem routes or process drain to make one backend easier.

Adding a normal source/cache path or a test requires no configuration or gate
allowlist edit. New filesystem operations change their owning mediator module
and invariant tests; new backend variants update the typed registry/decoder and
shared backend contract. Existing per-workflow text/contract/quick checks stay
one each, focused worker plus PM replay is deliberate, and the provisioned
mechanism job runs once for changes selected by its owning package/surface.
Probe evidence belongs in review, not a permanent current-file/test inventory.
