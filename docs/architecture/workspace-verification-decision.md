# S3 verification mechanism decision

Status: parent-aligned implementation protocol awaiting the third fresh full
two-channel L2 design review, 2026-10-16. Companion to
[workspace-delivery-protocol.md](workspace-delivery-protocol.md), section 7.
The probes below ran only against disposable /tmp repositories, without a
provider, Cargo, network access or production source mutations. They falsify
two cheaper designs; they do not validate the proposed FUSE implementation.

## Decision and compatibility change

Recommend a kernel-owned, short-lived verification filesystem projection:
immutable working bytes from C's bound MaterializedSource manifest plus an
operation-aware Git-classified ignored cache store,
mounted at the same absolute logical cwd inside the authorized gate namespace.
Use a path-mediating filesystem, initially Linux FUSE, for the mixed namespace.
Do not approximate it with a mount for each familiar output directory. The
ordinary working workspace remains Worktree; this is a VerificationHandle's
execution environment, not a selectable new workspace backend.

This preserves cwd strings, declared GateSpec steps and ignored output/cache
locations. It changes underlying inode/device identities, mount topology and
the fact that source writes used to be physically possible during a gate.
A mediator-visible source mutation request fails candidate evidence even when
the command catches its denial and returns zero. The guarantee is no actual
source mutation, not observation of every rejected syscall: a failure rejected
by VFS before FUSE may be handled without invalidating otherwise sound evidence.
This therefore needs an explicit parent amendment; calling it unchanged
physical in-place worktree execution would be inaccurate.
Users are not asked to add output-directory declarations or new GateSpec fields.

Capability is checked before S3 conversion and before verification preparation.
Unsupported resources/hosts retain their explicit Legacy protocol, with their
existing semantics and visible upgrade refusal. Do not automatically convert
them and then downgrade required bound gates to unbound. New LedgerV1 work
requiring bound verification refuses preparation if its negotiated complete
guard disappears. A diagnostic-only unbound gate must be explicitly requested.
This is a platform support boundary, not a claim that S3 has already shipped.

## Why ordinary sampling and bind mounts are insufficient

The current `task_verify_adapter/target.rs` samples provenance, HEAD and status
at endpoints. A command may edit source, test it, and restore the original bytes;
new nonignored source can be created and removed. Final tree equality does not
identify what source the command tested. Inotify captures ordinary mutations,
but its documented omissions include mmap, remote filesystems and new-directory
races; lost evidence must fail closed. Timestamps are not a durable write log.
The primary contracts are
[inotify(7)](https://man7.org/linux/man-pages/man7/inotify.7.html),
[mmap(2)](https://man7.org/linux/man-pages/man2/mmap.2.html) and
[statx(2)](https://man7.org/linux/man-pages/man2/statx.2.html).
The [mprotect](https://man7.org/linux/man-pages/man2/mprotect.2.html) and
[ftruncate](https://man7.org/linux/man-pages/man2/truncate.2.html) contracts
also establish permission failures on read-only access; those failures do not
imply a userspace filesystem observed the attempted call.

An immutable C checkout with writable ignored directories solves only a subset.
A tracked file may live under an ignored directory, so mounting that whole
directory writable exposes source. A pattern such as `*.cache` permits new
files beside source; mounting only existing ignored files denies legitimate new
outputs. A read-only root plus finite mounts cannot dynamically grant exactly
the future paths of arbitrary Git ignore patterns.

An immutable lower plus disposable overlay improves the ordinary mmap case:
writable opens copy source into upper even when bytes are restored. It still
does not log every temporary new source path. In an actual user-namespace probe,
writing only an ignored `src/build.cache` and writing that same cache plus
creating/deleting `src/transient.rs` left the same final upper entries, content
and xattr kinds. Both final source trees were unchanged. Directory timestamps
cannot distinguish the transient source from legitimate cache mutations.

Unprivileged mount-wide fanotify is not the missing log. The same user namespace
successfully created a FID/DFID/name reporting fanotify group, but mount and
filesystem marks both failed EPERM. Independently of that host result, the
[fanotify mark contract](https://man7.org/linux/man-pages/man2/fanotify_mark.2.html)
does not permit create/delete FID events with a mount mark; filesystem-wide
marks require CAP_SYS_ADMIN. The
[unprivileged group contract](https://man7.org/linux/man-pages/man2/fanotify_init.2.html)
limits marks to inodes. A privileged host audit service with complete event
coverage would be a different trusted mechanism, not a userns configuration.
See also the [kernel overlay contract](https://docs.kernel.org/filesystems/overlayfs.html)
for copy-up semantics. This rejects overlay upper inspection plus ordinary
unprivileged notification as a complete source-write proof.

| Actual probe | Observed result / implication |
| --- | --- |
| Python ctypes inotify, watched source directory, tracked write then restore | 32 bytes of modification events; final SHA-256 unchanged. Endpoint hash misses the transient write. |
| Same inode, shared writable mmap, mutate then restore, flush and unmap | Zero inotify event bytes; final SHA-256 unchanged. An inotify-only proof falsely succeeds. |
| New nonignored file created then deleted | 96 event bytes, no final path. Source enumeration only at endpoints misses it. |
| Real Git fixture: ignore `build/`, force-track `build/source.rs` | Git check-ignore with no-index says ignored, but ls-files proves tracked. Tracked classification must take precedence. |
| bwrap read-only C root plus writable original build directory | Writing `build/source.rs` succeeds. Directory passthrough breaks the source fence. |
| bwrap read-only C root plus writable existing old.cache file | Appending existing cache succeeds; creating valid new.cache is denied. File passthrough breaks existing dynamic ignored output semantics. |
| bwrap read-only physical copy of C after original source changed | Gate reads original candidate source; writable open returns EROFS (30), shared writable mmap returns EACCES (13). Snapshot decouples source from external original writers. |
| Source copied preserving metadata | Source mtime_ns equals its original value. This is necessary, not sufficient, for hot-build compatibility. |
| Disposable userns overlay, mmap source then restore | Upper retains `src/source`; this catches this mmap case but not every transient new source path. |
| Disposable userns overlay, cache-only vs cache plus create/delete transient source | Both uppers contain only directory `src` and file `src/build.cache`, with equal bytes and xattr kinds. Upper inspection cannot distinguish them. |
| Same userns fanotify FID/DFID/name group then mount/filesystem marks | Init succeeds; both marks return EPERM (1). Userns root is insufficient for the proposed complete audit. |

The initial bwrap probe failed inside the tool sandbox when mounting proc. The
same short-lived read-only namespace probe succeeded after automatic escalation
review; subsequent probes used that reviewed path and wrote only their /tmp
fixtures. This host has fusermount tools but no /dev/fuse. No FUSE mount, mixed
path mediator, full cache benchmark or production verification was run.
The first overlay probe's optional mount-option combination returned EINVAL;
the rerun with the essential `userxattr` option mounted successfully. Failed
probes are not counted as evidence of mechanism support.

## Proposed complete mechanism contract

1. Under the S2a resource fence, reserve the existing attempt/verification owner,
   block unrelated source writers and reconcile tickets/processes. Capture
   immutable Revision/C with the shared normalizer and its required working-byte
   manifest. That same content owner pins normalized and raw-byte closures for
   run/candidate/archive/install consumers; verification has no separate archive
   or materialization policy. Snapshot-owned source
   bytes have no writable alias to the original checkout: copy or safe reflink,
   never hardlink. Authorized source arrives through a scoped content export;
   the gate receives no durable artifact-store path, administrative FD, alternate
   or other workspace/Area's objects. It cannot access mutable backing handles.
   Use its frozen path/byte/mode/link/metadata manifest, including source mtimes,
   bound to normalized tree and rule identity, in environment evidence. Do not retimestamp every file
   to gate preparation time and thereby invalidate every hot build.
2. Build the authoritative classifier from C's tracked set, frozen effective
   Git ignore/exclude/config rules and kernel-owned exclusions. A typed fixed
   `git check-ignore --no-index` query against that frozen rule environment is
   the authority for untracked paths; tracked paths always select source. Cache
   exact-path results within that immutable classifier version. Do not copy
   pattern matching into the adapter or keep an output-directory inventory.
   Gate writes to source rule files are denied; private copies freeze external
   Git config/excludes. A changed normalization/rule version at final snapshot
   or a later verification request invalidates reuse of the old proof.
3. The mediator resolves every request it receives by validated relative path
   and descriptor identity. Source paths read only from C's immutable frozen
   working-byte objects; Git pointer blobs cannot substitute for source. Initial
   normalization/materialization runs before acknowledgement; preparing or
   restoring this already captured source never reruns smudge/LFS, uses network
   retrieval or needs deleted private Git/filter caches. Missing manifests/raw
   bytes fail typed preparation, not a pointer-only or unbound fallback. Source
   mutation requests reaching the mediator are denied and latch
   ObservedSourceMutationRequest; catching that error cannot clear the latch.
   Source writable FDs are never issued. A read-only source FD cannot acquire
   writable MAP_SHARED pages, including via mprotect; VFS may reject mmap,
   mprotect or ftruncate on that FD before FUSE receives anything. Such failures
   do not modify source and generate no invented daemon receipt/latch. If the
   command handles them, ordinary exit/evidence rules apply and it may pass.
   Writable MAP_PRIVATE pages do not change filesystem source and remain legal.
   Ignored cache FDs map/write through their distinct cache capability.
4. Existing ignored data stays in its original owned cache store, accessed only
   through the mediator. Cache content, mtimes and stable IDs within one gate
   are preserved; inode equality with a previous unsandboxed run is not promised.
   New ignored files or directories are admitted by the same frozen classifier,
   including patterns beside source and previously absent ignored subtrees.
   Intermediate directories are virtual navigation nodes; empty directories
   have no Git source entry. Granting creation does not grant writes to
   arbitrary descendants. Newly nonignored paths are
   denied even if a sibling name matches an ignore rule.
5. Rename, exchange, link, symlink, unlink, chmod, truncate and other mutating
   requests reaching the mediator resolve both ends. Cross-classification
   rename/link and source mutation are denied and latch invalidation there;
   ignored-to-ignored operations remain permitted with identity checks. There is no pathname-only symlink
   following into source or outside the cache capability. Open cache handles
   retain their cache identity through rename/unlink and never become a source
   capability after a pathname is reused. All FUSE writeback/mmap callbacks use
   that original handle identity, not the pathname's current classification.
6. The gate namespace uses the existing minimal sandbox mounts and explicit
   environment. Its logical workspace cwd is the mediator mount. Original
   mutable source, source staging, Git administration and mediator control FDs
   are not exposed as writable aliases. No raw backing-source FDs are inherited.
   Gate Git metadata is private to the verification environment, binds C and
   frozen required refs, and cannot move the original worktree's HEAD/refs.
   Index stat-cache refresh may occur privately; semantic source/required-ref
   mutation invalidates evidence. Kernel normalization before preparation still
   runs repository conversions in an authorized launcher, never the trusted
   writer. Do not infer tool session paths from an external private layout.
7. The mediator's operation identity, boot identity and mounted root are bound
   into the S2a executor receipt. It stays alive while any gate descendant can
   access the mount/cache capability. A background descendant, unresolved
   launch or dead/disconnected mediator prevents Complete. No response from a
   replaced mediator can seal another instance's proof. Unknown execution keeps
   occupancy; a lost proof requires a new run, not reconstructed success.
8. Seal only after positive executor drain, unchanged Revision/epoch/C and an
   authenticated mediator receipt with no invalidation or unaccounted received
   request. The receipt attests immutable source and mediation continuity,
   not a complete history of calls rejected earlier by VFS.
   Since C is physically immutable to gate operations, external writers in the
   original working checkout cannot change the source tested through this
   environment. Cache changes do not advance candidate or ledger source. They
   remain owned workspace content, not a side-channel authorization grant.

## Initial Supported set and refusal policy

Support Linux hosts whose namespace, FUSE mount/FD transport, sandbox and
executor descendant-drain contracts pass. The controlled source/cache roots
must be local identity-verified owned storage with supported durability and
normalization. Source regular files, directories and symlink values use the
shared snapshot contract; unsupported Gitlinks/special files refuse explicitly.
An absent FUSE device or denied namespace is Unsupported, never a test skip
counted as success or a request to fall back to host cwd.

Cache regular files/directories are supported, including ignored writable mmap.
Cache symlinks must resolve wholly inside the typed ignored cache capability;
escapes to source or unrelated host files refuse. Pre-existing hardlinks with
an alias outside that cache capability refuse until explicitly separated;
kernel must not unlink/copy them silently just to pass admission. Cache special
files and nested mounts refuse unless a later mechanism contract supports
their complete effects. These are supported filesystem types/capabilities, not
application identities or named build directories.

Actual source writes are impossible under the supported capability. Observed
source mutation requests, cache path escapes, metadata corruption, unsupported
mediated operations, mediator loss and unknown descendants fail source evidence.
They do not leave an apparently bound green verdict. Existing ignored caches
with supported types do not require a clean rebuild or a new output path.
Metadata-sensitive tools may still expose a semantic change from namespace/
inode projection: benchmark those through actual representative production
entry points, and refuse the capability if its contract is not met. Do not
claim universal compatibility from preserved mtimes alone.

## Parent mapping awaiting full review

The worktree verification paragraph in track-workspace section 2.2 now maps:

> Worktree verification retains the same logical cwd, declared user steps and
> Git-classified ignored output locations. A kernel-owned verification
> projection reads C's frozen working-byte manifest and mediates ignored cache writes
> dynamically, with tracked paths taking precedence. This changes physical
> inode/mount identity and prevents actual source writes. Source mutation
> requests reaching the mediator are denied and latched; VFS-rejected calls
> with no source change have no invented receipt and may be handled by a
> successful gate. This is not a complete attempted-syscall audit. It requires
> no new GateSpec/output directory. Only a complete, independently tested
> mechanism may produce bound evidence; unsupported hosts/resources remain
> explicitly unconverted or refuse preparation, without silent unbound fallback.

Approve this amendment only with review of the complete mediator boundary; it
adds a significant trusted mechanism and platform requirement to S3. The cheap
alternative is to explicitly narrow the parent to endpoint equality, which
would no longer prove tested tree equals delivered tree for arbitrary commands;
that alternative is not recommended. Reusing the later overlay workspace alone
does not solve arbitrary ignored-name mediation or transient source writes.

Required production contracts cover every probe counterexample above plus
ignored new paths/pattern negation, tracked-in-ignored, source symlinks, cache
alias escapes, mmap/mprotect, rename-exchange, open-unlink-reuse, concurrent
outside source writers, live descendants, mediator crash, filtered/eol
materialization and repeated hot builds. Exercise a real pointer-producing
filter through production capture and restore after deleting the whole private
Git directory and external payload supply; raw working bytes must come from
the retained manifest or preparation/ArchiveReady/reclaim must refuse. This is
future production acceptance, not an experiment already performed here.
Test read-only-FD writable mmap,
mprotect and ftruncate rejected before FUSE separately: assert no source
mutation, no fabricated mediator event, and normal gate success/failure
according to handled error and exit status. These are invariants owned by the
verification mechanism, not a caller/test-name list. The mechanism must be
implemented and verified before full S3 completion can be asserted.

Run that mechanism contract in an explicitly provisioned Linux CI job with
/dev/fuse, controlled user/mount/PID namespaces and the same executor envelope
as production. The job must execute an actual mount, source/ignored writes,
writable mmap, proof seal and descendant cleanup, and fail if any capability
is missing. A generic CI host may test explicit Unsupported results, but those
tests do not substitute for the provisioned mechanism job. Record supported
kernel/FUSE/runtime versions in the capability receipt and rerun the mechanism
job when those contracts change; do not enable the production capability from
a config flag without its live probe. No model invocation is needed for this
job, and no production host capability should be broadened just to run it.

The main protocol's parent amendment additionally relocates retained content
and writable ref projections outside private/runtime and human-common-dir Git.
This attachment changes verification semantics only; it must not be described
as the sole parent change or as an implementation of those other boundaries.
Its prepare/metadata effects consume the main protocol's sealed effect-owner
classification. A live provider awaiting a proven same-source checkpoint is
not drained; changed source/environment requires the Accepted/full-drain/new-
execution continuation. Verification adds no caller exemption or epoch policy.
