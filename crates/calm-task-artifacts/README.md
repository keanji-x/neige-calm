# calm-task-artifacts

A synchronous, Linux-only filesystem library for #1501's restricted local Git
artifact delivery. It captures exact ordinary file bytes, keeps immutable
snapshots independent of source workspaces, and prepares verified input slots or
whole failed candidates. It does not implement scheduling, database state,
acceptance, recovery authority, or provider stopping.

## Required caller contract

The kernel caller must:

- Establish a trusted write boundary before capture and keep the source quiescent
  until capture finishes. `QuiescentSource.boundary_id` is a required assertion
  identifying that proof; the library does not verify a provider stopped.
- Supply an isolated local Git worktree and a trusted absolute Git binary through
  `GitConfig`. Keep the store, its ancestors, and preparation parent inaccessible
  to workers for writes. Store directories must be private and caller-owned.
- Authorize each exact snapshot/slot binding, including repair-purpose access to
  rejected work. Bind attempt IDs, repository provenance, original inputs, gate
  evidence and retention in the existing kernel persistence layer.
- Start a consumer only after successful materialization and its own current
  authorization/runtime checks. Materialization alone is not consumption authority.

Linux `openat2`, `renameat2`, `/proc/self/fd`, process locks and a local filesystem
with atomic rename and working file/directory `fsync` are required. Unsupported
facilities fail explicitly. Preparation destinations must be on the same
filesystem as the store. The library API is exported only on Linux; callers on
other platforms must refuse this delivery capability.

Use a blocking thread when calling from an async executor. All Cargo validation
for this task uses the cooperative `/tmp/neige-1501-cargo.lock` and the shared
repository target directory.

## Public API

```rust,ignore
let store = ArtifactStore::open(&kernel_store, limits, GitConfig {
    binary: "/usr/bin/git".into(),
    timeout: Duration::from_secs(5),
})?;
let receipt = store.capture(CaptureRequest {
    key: capture_operation_id,
    source: QuiescentSource { root: &stopped_workspace, boundary_id: stop_proof_id },
    outputs: &[OutputSlot {
        name: "code".into(),
        paths: vec!["src".into(), "notes.txt".into()],
    }],
})?;
let snapshot = store.open_snapshot(&receipt.snapshot)?;
let prepared = store.materialize(&[SlotBinding {
    snapshot: receipt.snapshot,
    output: "code".into(),
    into: "inputs/b".into(),
}], &fresh_consumer_directory)?;
```

`Limits` has required `max_entries`, `max_file_bytes`, `max_total_bytes`,
`max_manifest_bytes`, `max_path_bytes`, and `max_depth`. These also constrain
snapshot reads and aggregate materializations, not just initial writes. Git index
stdout is bounded by the manifest byte ceiling and Git execution by the explicit
timeout. No environment variable configures library behavior.

`SnapshotId` is a checked lowercase SHA-256 `Digest`. `Snapshot` exposes read-only
`id()`, `manifest()`, and `missing_outputs()` getters. A `SnapshotManifest` records
`file-manifest-v1`, delivery schema `git-v1`, sorted complete candidate `entries`,
and sorted public `outputs`. Entries preserve relative paths, file SHA-256, byte
length and executable bit; directory entries also preserve empty directories.
`Materialized` returns the exact destination and prepared entry inventory.

`SlotBinding { snapshot, output, into }` is the minimal immutable filesystem
binding. Logical task/key, attempt and purpose belong in caller persistence, not
this library. A slot's declared paths retain their source-relative layout:
`["src", "notes.txt"]` bound into `inputs/b` creates `inputs/b/src/...` and
`inputs/b/notes.txt`. Bindings' `into` paths must be disjoint. One slot's paths may
not duplicate or overlap another slot's paths.

## Whole candidate versus output slots

All ordinary worktree files and directories are captured, including untracked
and ignored files. Only the root `.git` metadata is excluded. The `.git` marker
must be a directory or regular gitdir file; it is never copied or recursively
walked. Linked Git worktrees are supported. Git performs only a bounded read-only
index listing to refuse tracked symlinks, gitlinks and sparse trees, including
index-only entries with no working-tree marker.

The index subprocess uses a cleared, explicit environment: fixed `PATH` and
locale, system/global Git config disabled, prompts and optional locks disabled,
protocols denied and lazy fetching disabled. Command options override fsmonitor,
hooks and untracked caching. stdin/stderr are null. A deadline/output failure
kills and reaps the child. Git may parse local repository metadata for index
admission; capture and materialization never use filters, hooks, Git checkout or
Git's object representation for file bytes.

Nested `.git` and `.gitmodules` paths, symlinks, special files, source hardlinks,
non-UTF-8 names, path traversal, reserved metadata paths, absolute paths and
ambiguous path spellings are rejected. This conservative `.gitmodules` refusal
also applies if the file declares no active submodule. Rooted descriptor reads
refuse symlinks in every path component and crossing source mount points.
Timestamps, owner IDs, xattrs and setuid/setgid bits are not copied. Prepared
files are `0600`, or `0700` when executable; directories are `0700`.

A failed candidate can lack required outputs. Capture retains it and reports
`CaptureReceipt.missing_outputs` explicitly. `materialize` refuses an incomplete
slot. `materialize_candidate(id, fresh_destination)` prepares all retained
candidate files for authorized repair, including notes outside public outputs.
It does not recreate `.git` or install the original frozen input set. The kernel
owns those subsequent preparation decisions. There is no accepted/eligible bit.

## Content-addressed storage and crash recovery

The file-manifest store is smaller than implementing Git object transactions,
retention refs and filter-independent worktree reconstruction for this scope.
Each snapshot directory holds a canonical manifest and raw SHA-256 objects.
Identical candidate bytes, executable bits, directories and normalized slots
produce the same ID regardless of capture key, boundary token or source location.
Objects are deduplicated within a snapshot. Cross-snapshot deduplication and GC
are intentionally absent.

```text
FORMAT
.lock
staging/capture-<random>/ { request.json, snapshot/{manifest.json, objects/...} }
captures/<key-digest>/request.json
captures/<key-digest>/snapshot/       # only while publication is pending
snapshots/<manifest-digest>/ { manifest.json, objects/<file-digest>... }
staging/prepare-<random>/...          # unpublished consumer directory
```

Capture holds an exclusive process lock while it:

1. Streams bytes into private staging and checks all limits, then fsyncs objects,
   the canonical manifest, the request record and directory entries.
2. Atomically freezes the capture key by moving that complete request directory
   into `captures/`, fsyncing both parents **before** publishing the snapshot.
3. Verifies all staged objects, atomically moves the snapshot to its content ID
   without replacing any existing snapshot, then fsyncs both affected parents.
4. Reopens/verifies the snapshot and returns its receipt.

A replay with the same key, boundary and normalized slot declarations returns the
original snapshot with `replayed: true`, even if the source changed or disappeared.
Changing the boundary or slot declarations conflicts. The receipt and snapshot
contain no implicit "latest" reference. Different keys can retain the same content
ID. The key is globally scoped within this store; callers should use a unique
capture Operation identity. The source path is deliberately not replay identity:
a frozen request never re-reads any source location.

A failure before durable key freeze may be retried against the source because no
snapshot binding was accepted. After freeze, the capture's exact staged identity
survives and the same request resumes publication. Missing/corrupt frozen data
fails closed; it never falls back to the present source. A returned success means
publication/fsync completed. An I/O error after a rename is an uncertain response;
retry the same capture key to reconcile.

First creation persists all three control directories before committing `FORMAT`.
A nonempty directory without that marker is an incomplete or unknown store and
is refused without cleanup or adoption. A visible, valid marker with the complete
layout can resume after an uncertain initialization fsync: open repeats the file,
root and root-parent durability barriers. An initialized store missing `captures/`,
`staging/` or `snapshots/` fails with an integrity error; open never creates a new
empty replay ledger over lost metadata.

Every capture replay repeats the `captures/` and `staging/` parent fsyncs before
publication or acknowledgement, including when its snapshot is already present.
A failed barrier leaves the frozen key bound and returns an error. When the
canonical snapshot already exists, its exact manifest and every object must
verify, and its publication parent must sync, before the redundant staged copy
is discarded. Interrupted deletion of that duplicate can then resume without
requiring the partially deleted bytes to verify. Missing or corrupt canonical
content is never ignored; when canonical content is absent, the staged copy must
still verify completely before publication.

Store open serializes with writers and removes only store-issued unpublished
`capture-`/`prepare-` staging directories. Unexpected staging names fail explicitly.
Frozen captures and retained snapshots are never scanned for deletion. No global
GC or workspace lease controls retention. Later authorized Track cleanup must
coordinate caller references and running operations before deleting a store.

Materialization verifies manifests and objects, writes private staging, verifies
prepared bytes and executable metadata, fsyncs it, and atomically publishes the
whole directory using no-replace rename. Existing destinations (even empty ones
or symlinks) are refused. A failed preparation never exposes a partially populated
destination. A process crash after successful rename may leave a complete
prepared destination with a lost response; the caller's Operation must reconcile
that state and its frozen bindings. The library never overwrites an existing
consumer binding or guesses whether an existing directory is reusable.

## Validation

The crate tests real production APIs with temporary Git repositories and files.
They cover source mutation/deletion, raw binary bytes and ignored/untracked files,
metadata and slots, deterministic identities, concurrent/replayed requests,
interrupted capture publication and orphan recovery, path and link refusal,
tracked index-only gitlinks, missing/corrupt objects and resource ceilings. Test
fault checkpoints wrap the actual capture transaction, not a second store.

The parent #1501 integration owns independent full-diff reviews, shared gates,
provider stop proof, persistence bindings, ordinary/repair eligibility and the
A/B/C product experiment. This crate alone does not establish reliable recovery
or complete S2.

Validation recorded for this implementation (original identity/link mutations
below predate the persistence fixes):

- `cargo nextest run --locked --offline -p calm-task-artifacts --test-threads 8 --no-fail-fast`:
  32 passed, no skips, across library and two integration binaries. The eight new
  persistence regressions use the real capture/open paths: seven reproduced the
  reviewed failures before the fix; the canonical-corruption negative control
  and all original 24 tests already passed.
- `cargo clippy --locked --offline -p calm-task-artifacts --all-targets -- -D warnings`:
  passed. `cargo fmt -p calm-task-artifacts` ran.
- Production mutation removing the digest comparison from `verify_identity`:
  the complete red set was only
  `content_identity_rejects_same_length_corruption_before_materialization`.
- Production mutation removing only `RESOLVE_NO_SYMLINKS` from `open_beneath`:
  the complete red set was only
  `source_symlink_is_rejected_without_following_internal_or_external_targets`
  and `store_object_symlink_is_rejected_even_when_internal_and_byte_identical`.
- Each mutation ran all 24 tests with `--no-fail-fast`, compared actual versus
  predicted red names, restored the exact original production bytes, and passed
  all 24 tests again while retaining the same exclusive Cargo lock.

Commands used `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6`
and `CARGO_TARGET_DIR=/mnt/data2/kenji/neige-calm/target` under
`flock /tmp/neige-1501-cargo.lock`. No real Codex, workspace-wide suite, generated
API changes or dependency upgrades were involved.

The persistence regressions inject directory-fsync errors at the production I/O
boundary using a per-thread, `cfg(test)` hook, and interrupt the real duplicate
cleanup phase after removing an object. They cover both interrupted key parents,
replay after publication, missing control directories, incomplete initialization,
an uncertain `FORMAT` commit, and partial duplicate deletion with valid, corrupt
or absent canonical content. These are deterministic failure/order checks, not
physical power-loss experiments.

Five single-factor persistence mutations ran the full 32-test suite with
`--no-fail-fast`; each complete failure set matched its prediction, and restoring
the exact production bytes returned all 32 tests to green under the same lock:

| Mutation | Complete failing test set (`store::tests::` prefix) |
| --- | --- |
| Remove `captures/` fsync | `capture_replay_repairs_interrupted_key_sync_before_publication`, `capture_replay_requires_key_sync_even_after_publication` |
| Remove `staging/` fsync | Same two key-sync tests |
| Recreate missing initialized control directories | `open_rejects_initialized_store_missing_control_directory` |
| Require the redundant staged snapshot to verify again | `duplicate_cleanup_replays_after_partial_removal` |
| Delete the redundant copy before syncing canonical publication | `duplicate_cleanup_preserves_stage_until_published_sync_succeeds` |
