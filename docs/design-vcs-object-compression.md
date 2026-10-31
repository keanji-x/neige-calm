# Lossless VCS object storage (#3412, first slice)

Outcome: preserve every commit, manifest entry, object hash and decoded byte while
reducing stored object payloads. This is L2: the persistent representation and GC
reader contract change. History retention, missing rewind anchors, events and
VACUUM remain outside this change.

## Format and ownership

Truth owns one raw/zlib codec used by all VCS readers, writers and the object
sweeper. Migration 0205 adds an explicit encoding discriminator and the decoded
size for compressed rows; existing arbitrary raw bytes never need sniffing.
Hashes and manifest byte lengths continue to describe decoded content. Unknown
encodings, corrupt streams, wrong lengths and compressed identity mismatches fail
closed. Decoding is bounded to 64 MiB; larger legacy raw objects remain readable
and are not compressed. GC budgets decoded sizes before parsing under its writer.

New writes use the codec. The operator converter uses the same codec, prepares
and roundtrip-checks outside the writer, then conditionally updates one unchanged
raw row per short transaction. Existing GC revision triggers invalidate prior
observations. Keyset progress, byte/count limits and yields allow interruption
and replay without deleting data or advancing history. A local maintenance
executable previews by default, applies only with an explicit flag, and requires
the new schema to have been installed by the updated server; it does not migrate,
VACUUM or deploy. Offline use is recommended to avoid independent writer queues.

## Verification and operation

Focused tests cover mixed raw/compressed reads through cat/diff/log and report
reads, stable hashes, corrupt/truncated/trailing data, bounded decoding, GC roots
and decoded-size budgets, interrupted conversion, unchanged-row revalidation and
idempotent resume. Mutation checks pin corruption rejection and GC budgets.
Two independent review channels inspect a frozen target in separate worktrees.
Required text/contract/quick Rust gates run once on the final target; CI owns broad
suites. No real Codex E2E or production changes are part of implementation.

Deployment requires an updated-reader release and database backup. Old binaries
refuse migration 0205 through the existing future-migration guard; rollback must
restore the matching backup. Preview and a bounded trial on a disposable database
copy precede production conversion. File compaction needs a separate reviewed
backup/space/recovery plan; logical savings alone do not shrink the main file.

## Operator commands (only after deploying matching readers)

Build with `cargo build --locked -p calm-truth --bin vcs-object-compress`.
Prefer stopping the updated service for conversion: a separate operator process
cannot share the server's in-process writer admission. Do not point an old
server binary at a converted database. The tool never installs schema or starts
providers. Its output contains only cursors, counters and byte totals.

```sh
# Read-only preview; replace the example path with the verified database URL.
vcs-object-compress sqlite:///absolute/path/calm.db --pages 2
# Bounded trial on a verified disposable backup copy first.
vcs-object-compress sqlite:///absolute/path/copy.db --apply --pages 2
# Resume at the last emitted `after` cursor; omit --pages to complete the pass.
vcs-object-compress sqlite:///absolute/path/copy.db --apply --after HASH
```

Keep the matching pre-upgrade backup until verification and the later file
compaction are complete. A lost progress line is harmless: restarting without
`--after` skips objects already encoded. `changed_since_read` is a reason to
repeat from the beginning after concurrent activity settles. Preview savings
count candidate payloads; applied savings count only acknowledged row updates.
