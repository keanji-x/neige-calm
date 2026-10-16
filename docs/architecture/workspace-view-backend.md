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
normalization/classification, the RuleView contract in section 5, durable
content and all installation policy.
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

## 4. Stable VFS inodes, private copy-up and bounded live visibility

### Actual FUSE boundary

There is no kernel overlay mount. LOOKUP selects a stable virtual inode for a
verified object/alias group. OPEN receives that inode and returns a handle and
flags; it cannot replace the inode selected by LOOKUP. This follows the
[low-level API](https://libfuse.github.io/doxygen/structfuse__lowlevel__ops.html)
and Linux's [open reply](https://github.com/torvalds/linux/blob/v6.12/include/uapi/linux/fuse.h#L752).
Cached pages belong to the VFS inode, not a private image per fh. There is no
immutable-per-open or OPEN-time inode-swap promise.

An inode record has a stable mount-local ID/generation, verified source object
identity or owned object ID, alias/path provenance, backing state and reference
counts. It never reuses an ID while any kernel lookup, handle, callback or
manifest reference remains. Source objects use retained descriptor identities;
a reused pathname or numeric inode alone cannot resurrect a forgotten object.
Aliases of the same source inode join one group. Every fh refers to that group
and its access rights, not a permanent read FD bypass to the old source.
A source-to-private transition changes the group's backing, not its VFS inode.

Creation allocates a private group before CREATE returns. Hardlink creates
another name for that group. In-root source hardlinks share one group even if
one name is discovered later: after a write via a, a read via b must use the
same private content, never an independent source FD. Path ownership/provenance
is separate from object ownership; a newly observed external name alone does
not become delivery W. Snapshot uses D's complete tracked names and owned path
effects to represent affected aliases. Unknown attribution remains overlap.
Rename/unlink changes names; open FDs retain their group and cannot address a
replacement inode installed at that path. Atomic external replacement creates
a new source identity for fresh lookup, while an owned name remains private.

### Visibility and cache contract

Source-backed ordinary READ requests use the verified read-only source FD;
metadata lookup/stat/readdir never copy file payloads. Kernel page-cache misses
may observe host changes. Atomic replacement/new names are revalidated through
fresh namespace lookup with zero entry/negative retention. For in-place host
writes to the same inode, existing cached pages, fresh opens and mappings may
still show older bytes until an idle sync. No per-open immutable image or
strict close-to-open byte guarantee is claimed. TTL, mtime, polling or inotify
are hints only; source observations can mix concurrent states and are never
Revision, a whole-tree snapshot or gate evidence.

Use cached read/write-through mode, ordinary executable/private/shared mmap,
no DAX/passthrough and no FUSE writeback-cache mode for the initial profile.
[The FUSE I/O contract](https://docs.kernel.org/filesystems/fuse/fuse-io.html)
distinguishes these modes. New opens do not request KEEP_CACHE, so the kernel
attempts data invalidation; this is a refresh attempt, not positive evidence
that every mapped/pinned page was evicted. Linux's
[open implementation](https://github.com/torvalds/linux/blob/v6.12/fs/fuse/file.c#L217)
keeps the same inode throughout. Source mmap is a cached live observation.
MAP_PRIVATE writes affect process memory only; writable shared mappings require
an authorized writable open that has already made the group private.
FOPEN_DIRECT_IO is not a substitute that disables ordinary mmap/exec/builds.

After copy-up, old read-only FDs and read-only shared mappings are allowed to
observe subsequent writes to that same private inode. They are not promised
the old source image; they gain no write rights. Already faulted pages can
remain cached during a concurrent transition, as can ordinary mapped data
during concurrent writes/truncation. All subsequent daemon READ/WRITE/metadata
callbacks route through the group's current private backing. Page cache is
shared by all its aliases. Do not pin an old source backing per fh or use a
new fh to pretend it is a separate cached inode. Private writes, append and
truncate follow the qualified kernel's inode/page-cache ordering; truncate
occurs only after copy-up, including SETATTR-before-OPEN and O_TRUNC paths.
Concurrent ownership transitions serialize on the group, not a path string.

An idle sync uses the existing full-scope drain, finishes pending FUSE work,
flushes private data, releases old content handles and rebuilds the mount through
the existing executor/holder transition. This removes old source cache/lookup
state without an unprovable notification ACK. Resume uses the new generation;
W and private Git remain owned and durable. A busy mapping/descendant keeps
sync Busy. No lazy unmount/storage reuse. Opening/read caching during activity
need not wait for that full refresh, but in-place changes have no stronger
visibility promise. This is an explicit product exception to immediate live
visibility, not a claim of full POSIX cache equivalence.

### Copy-up, lifetime and cost

Only the first writable intent or mutating metadata operation for a source group
copies its file bytes to independent owned staging. Serialize against group
callbacks, bind the exact source identity and current RuleView, reserve typed
storage budget, copy bytes/mode with conservative overlap attribution, fsync
ownership/data/parents and publish one Owned backing before granting mutation.
No source FD is ever writable, inherited by user code or exposed as backing.
Concurrent source edits during that copy are ordinary external-read uncertainty,
not a claim of a coherent source snapshot. Uncertain capture is marked overlap
or refused; later strong snapshot reads only the completed owned backing.
An O_TRUNC target need not copy discarded payload, but still records ownership,
before identity and overlap before truncate is acknowledged.

Old cached source pages can differ from the concurrently copied backing. Cache
invalidation is requested without holding a lock needed by related requests;
it does not prove that all pinned pages disappeared. Live readers have the
weaker semantics above. Before a strong snapshot, S3's stability permit seals
mutations, settles kernel writes/mappings through the existing execution facts,
flushes private backing and freezes its manifest. An unaccounted writer or
unreconciled cache-dependent source effect is Busy, never proof. In particular,
SameSourceProjection cannot be signed from cached live reads or a notify return
code. Full drain/install is available when the effect owner cannot prove its
unchanged-source contract; it is not an exemption for an awaiting provider.

The owned union manifest records before/after object IDs and staging identities;
it is storage state, not another execution/delivery ledger. A crash between
publication and settlement preserves both objects for inspect/recovery. Never
discard an unknown private object or fall back to source. Ordinary later writes
obey storage fsync semantics, not an invented per-write exactly-once promise.

Account separately for LOOKUP reply nlookup/FORGET, open/RELEASE (including
mappings), pending callbacks, namespace names and persistent manifest references.
FORGET is independent of close; neither alone authorizes reclaim. After unlink
or rename, referenced inodes still answer requests. Clean unmount implicitly
forgets lookup references, but lost replies/unknown mount access remain recovery.
Whole-resource destruction still uses the single S3 lifecycle.

For N metadata lookups or read-only opens of a file of size B, this mechanism
performs no N*B payload capture and allocates no immutable read-image tree.
Reads allocate normal bounded kernel page cache; repeated opens can reread
pages but do not duplicate full files on disk. First private ownership costs at
most one copy of the group's current payload plus bounded staging, until a
real source installation creates a new group. Hardlinks share that one copy.
The typed resource budget bounds private W/cache bytes, staging bytes, inode/
handle entries and concurrent copy reservations; existing S3 pins keep their
separate accounting. Reserve before copy or file growth. Exhaustion returns
ENOSPC/typed capacity refusal without partial ownership or source fallback;
recoverable staging is charged until reconciled. READ data need not be copied
into private storage merely because the private-write quota is full. Metadata
scans remain proportional to entries, not total payload bytes. Hot builds reuse
visible source/owned ignored caches at their logical location; benchmark their
actual FUSE/page-cache and first-write cost without claiming zero-copy ext4.

Use descriptor-relative containment from
[openat2](https://man7.org/linux/man-pages/man2/openat2.2.html). Normal in-root
symlinks remain content; neither end of rename/link/copy can escape the granted
root or reach administration/backing paths. The supported type contract covers
regular files, directories, in-root symlinks and accounted in-root hardlink
groups. Nested mounts, special files and Gitlinks remain the existing explicit
unsupported type boundary. A type/identity race enters Busy/recovery rather
than redirecting the operation or choosing another backend.

## 5. One RuleView for visible rules, Git and evidence

S3's existing normalization/effect owner must provide a required versioned
RuleView; this is its missing S2b consumer contract, not a second rule engine.
The required receipt binds rule sequence, installed/source version, exact
working-byte rules plus authoritative absences, effective config/excludes/
attributes, tracked membership/index semantic version, classifier and native
Git projection identities. Snapshot, D/W classification, private Git and every
verification preparation consume the same receipt. A digest alone is not it.
The existing S3 content owner retains those rule bytes/absences, config and
index/classifier inputs as part of the required source/environment closure;
reopen/gate preparation never reconstructs them from deleted private Git or
today's source rules. RuleView has no independent artifact TTL or content store.

The Git owner registers input roles by Git's official
[ignore](https://git-scm.com/docs/gitignore) and
[attribute](https://git-scm.com/docs/gitattributes) contracts, including scoped
inputs at every relevant directory and inputs absent in the selected version.
It owns the protocol identifiers and config/index precedence. Generic FUSE
asks that role resolver; kernel/adapters do not know filenames or maintain an
inventory of current rule files. Newly created contextual rule inputs are
recognized too. Registered RuleInput paths resolve only to RuleView's owned
bytes/absence, never live source. The private Git's global/system/config paths
are frozen scoped projections with no host HOME/config fallback.
The role applies to both destinations and object/FD aliases: link, rename,
open-by-FD or a lockfile publication cannot bypass it through another name.
Source hardlinks do not make a controlled rule projection an alias of live
source. Rule-change activation drains old handles and replaces the whole
projection; there is no old rule FD sharing an inode with a new active version.

This is an intentional live-source exception: external edits/additions/deletions
of registered rules are pending rule drift, even while ordinary source/ignored
files remain observable. Native Git cannot read new live rules behind the
classifier. Initial RuleView uses D's in-tree rules, the approved frozen external
rule inputs and the owned index contract. Dirty source rule bytes are retained
as a proposed change with provenance/overlap and visible pending status; they
are not silently used, discarded or inserted into C. Adoption requires the
controlled operation below. Source HEAD/rule observation can request that
operation at idle sync; it cannot publish a version by sampling alone.

Direct filesystem edits of registered rule inputs and classification-changing
Git metadata are not an alternative publisher. Such mutation returns a precise
controlled-rule error before any visible bytes change. Ordinary reads/status
remain native. Authorized callers use one typed RuleChange operation through
kernel Services, reusing S3 SourceInstallPlan and the existing operation driver.
This product limitation also covers shell attempts to force-add an ignored
cache path or alter rule config. No caller/plugin-name interception is used.
The agreement concerns the registered workspace repository and its canonical
configuration. An arbitrary program or an explicit ad-hoc Git command-line
override can interpret files differently; its output has no RuleView/evidence
authority and cannot change the canonical projection. Do not turn this into a
launcher filename/argv denylist or infer authority from a native Git success.

Private Git index/blob/stat-cache writes stage privately. The Git owner validates
their actual publication at the registered metadata role; generic FUSE does
not parse Git's binary index or grant a raw replacement. Ordinary index updates
and add/remove of already nonignored source paths may advance the RuleView's
index/metadata sequence through sealed SameSourceProjection when the owner
proves visible rule bytes, fallback behavior and every effective source/cache
classification plus normalization unchanged. This preserves ordinary native
add/commit without requiring a drain for a pure metadata update. Such proof
does not use equality of normalized trees alone or an adapter classification
flag. Native-visible index and its owner receipt publish under the prepared
projection permit; partial receipt/FS effects close snapshot/publication until
that same metadata intent reconciles.

An effective semantic change, such as force-tracking an ignored cache path,
requires RuleChange. Failure leaves the old projection intact and the native
command receives an explicit controlled-rule error. Private commits on an
accepted index remain nonauthoritative history. No caller gets an exception
from proof or a raw-index escape hatch. Adding an ordinary nonignored source
file does not itself create a new product refusal.

RuleChange is a frozen proposed RuleView plus owned raw rule bytes, explicit
tracked-membership changes, affected content roots and per-path dispositions.
A source-dirty in-tree rule adopted for delivery becomes owned W with overlap
confirmation and an explicit tracked entry; it is not implicitly omitted by
its own ignore patterns. Preview returns exact pending scope and cost. Apply
uses the existing Accepted/old-call-ended/full-drain/install continuation,
including provider/native Git processes and rule/index FDs. No synchronous
request waits for itself to drain and no current owner is exempt.

The S3 owner computes the complete source/cache transition with Git's matcher:

| Existing content | Required disposition before the new version can activate |
| --- | --- |
| Owned cache becomes nonignored/source | Explicit promotion pins its raw bytes/provenance as W and requires overlap approval as appropriate; or an explicitly authorized discard removes that exact owned cache object. Never silently add all historical build output to C. |
| Owned W/source would become ignored | Preserve it as source through explicit tracked membership in the new private index/RuleView, or complete an authorized delivery/discard first. Never silently exclude owned source/archive bytes. |
| Still ignored cache | Retain its owned group only under the new classifier version; aliases/mappings have already drained. |
| Unowned live source context | No automatic W ownership or delivery. It uses the new classifier on subsequent observations; dirty rule bytes still need explicit adoption. |

The plan protects both old/new content roots, records expected RuleView and
manifest/index versions and stages all new projections while admission is
sealed. After drain, install rule bytes/authoritative absences, private Git
index/config, D/W/cache dispositions and classifier under one recoverable
operation. They need not be one FS syscall: until all component receipts match,
no reader, launch, Git command, snapshot or gate is admitted. The final Ready
CAS publishes one RuleView and source epoch. Crash replays that frozen plan or
remains sealed in recovery; it never resumes with mixed versions. Budget or
confirmation failure occurs before activation and retains the old version.
This is SourceInstallPlan, not SameSourceProjection, whenever effective rule/
classification/source facts change. An index update with proven identical
effective semantics keeps the existing metadata-only owner contract.

Snapshot seals the RuleView/version together with the owned manifest. Native
Git's visible rule bytes and index semantics, the classifier and gate's rule
projection therefore agree for that version. Verification prepares C with its
exact RuleView and working-byte closure, without live rule passthrough. If a
proposed rule change would enable a forbidden conversion such as a custom
filter, the existing view type/normalization boundary refuses the entire plan,
not one consumer's interpretation. Adding an ordinary file/test needs no rule
registration edit; changing Git's external contract belongs to its owner.

## 6. Installed D, private W and delivery

The backend owns a versioned union manifest, not an extra delivery ledger.
Its effects and roots bind one RuleView and belong to the existing operation
intent/receipt. It does not infer W by scanning an overlay upper or interpreting private Git
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
owned manifest. Outstanding writable handles (and therefore possible mappings)
or effects refuse Busy; FUSE cannot infer that an open writable FD has no mmap. Capture
one manifest version and refuse on concurrent changes or unresolved source-copy
ownership. A waiting provider may remain live for the already specified
same-source metadata path; snapshot does not require that call to drain itself.
Real SourceInstallPlan and destructive lifecycle still require full S3 scope
drain. S3 then pins the normalized tree and exact raw-byte closure. Backend `.git` status is not this
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

## 7. Git metadata and source HEAD

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
effective ignore/exclude/attribute rules, tracked membership and approved
configuration from the same RuleView. Its controlled publication boundary in
section 5 also applies to native Git, index fallback and config changes.
Repository code/hooks run only in the authorized execution sandbox, never SourceGitAdmin
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
refresh the mount only after full drain as described in section 4. Rule drift
remains pending for RuleChange and cannot bypass the selected RuleView. No
inotify fingerprint decides
whether sync runs. A changed source HEAD pauses new launches and emits a typed
confirmation requirement. Confirmation binds old/new observed HEAD plus source
and binding versions; it updates observed HEAD only. B, D and delivery tip do
not change. Rebuild is archive plus a new track, not reset of the user's tree.
An already live interactive PTY is not promised command-by-command interception.

## 8. Creation, close, delete and recovery ship together

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

## 9. Public capability and real entry points

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

## 10. Implementation sequence and qualification

The atomic release surface is one selectable backend with create, usable
no-provider entry points, close/delete/recover and complete content/evidence
boundaries. Internal commits may separate mechanism, truth/API and callers,
but no intermediate selectable half-backend merges. Start only once S2a's
real executor/identity interfaces and S3's real effect/content/verification
owners are available; these names are contract responsibilities, not unused
public scaffolding to add early.

1. Review section 1's selected mechanism/visibility amendment and section 4's
   stable-inode/cache contract and section 5's RuleView through both full L2
   channels. If the mode cannot support ordinary builds/mmap safely, narrow the declared platform profile before
   implementation; successful unrelated probes are not qualification.
2. Implement the shared path-mediator mechanism and owned manifest algebra
   behind S3/S2b owners, with the required RuleView/classifier interface. Keep
   filesystem request handling separate from resource policy and private Git plumbing.
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
| VFS inode/lifetime | Exercise LOOKUP then writable OPEN and SETATTR/O_TRUNC without an inode swap; hold old RO FD/MAP_SHARED, copy up through alias a and read/write through hardlink b, reopen by FD, concurrently open/link/rename/unlink, and prove every daemon callback uses the same private group. Old RO readers may see private changes but cannot write source. Cache tests separately cover atomic replacement, stale in-place pages and full-drain remount. FORGET-before/after-RELEASE and unlink with nlookup retain the correct object. |
| RuleView | Start with rules ignoring out, externally remove that rule, and reverse the directions; native canonical Git, owned file classification, Revision and gate keep the old version until explicit RuleChange. Also test view-side rule edits, new nested/previously absent rule inputs, attributes/index fallback/config, force-add and alias attempts. Promotion/retain/discard handles every affected owned cache/source; crash between each projection receipt never opens mixed versions or silently loses source. |
| Cost/capacity | Scan metadata and repeatedly open/read a large source cache with reflink disabled; assert no full-file capture/staging is created. First private write copies once per inode group, aliases reuse it, truncate avoids discarded bytes, and reservations cover concurrent copies/growth. Exhaust byte/inode/staging quota, restart and recover: no source write, uncharged orphan, fallback or loss of completed W. |
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

## 11. Architecture and permanence cost

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
