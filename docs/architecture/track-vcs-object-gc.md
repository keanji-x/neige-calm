# Track VCS object GC (#2792)

Object GC marks immutable tree contents outside SQLite's writer, then deletes
old orphan candidates in separately validated transactions. Track history pruning
continues to own which commits remain; object GC roots reachability on HEAD refs,
not every retained commit row.

## Observation and authority

One recursive pooled SELECT obtains the reachable tree closure and object
presence/kind/length metadata. Recursion carries `(parent_hash, tree_hash)` so a
commit's parent and tree require one lookup. Equal pairs emit the same tree and
have the same next parent, so merging them with UNION preserves the tree closure
and terminates on finite cycles. Tree hashes are deduplicated before joining
object metadata, avoiding one object lookup per commit that shares a tree. It is a statement snapshot, with no deferred
transaction or later lock upgrade. Tree payloads are fetched in batches of 32;
each fetch releases its connection before JSON parsing. Parsed tree hashes and
the union of tree/blob references live only for this call. Full payloads are not
retained. A tree is marked observed only after complete successful parsing.

The owning store hashes schema and manifest entries in `hash_tree_manifest`,
then uses `INSERT OR IGNORE`; blob insertion hashes kind and bytes and also uses
`INSERT OR IGNORE`. Production writers do not update object bytes or kind.
Reinserting an identical content address therefore cannot change its reference
set. This is the existing store contract, not a new cache-coherence protocol.

The candidate scan first reads hash-only keyset pages of 256 through the
covering primary-key index, and excludes the observed reference union before
loading age metadata. Only remaining hashes are looked up by primary key with
the fixed cutoff. This avoids reading large live objects' records just for the
age column after their payload. Actual old orphan candidates accumulate across
index pages into independent deletion batches of 256, with one final partial
batch at EOF. A sparse population must not pay for a full ancestry proof once
per live-object index page. The buffer holds at most 511 candidates before a
flush. A scan with no candidates returns zero
without acquiring the writer. It can safely miss concurrently created garbage;
it cannot delete anything.

For each candidate batch, `BEGIN IMMEDIATE` recomputes the current complete tree
closure and checks existence/kind even for cached trees. HEAD equality alone is
insufficient: pruning can remove an ancestor without moving HEAD, and restoring
that ancestor can make an old tree reachable again. Every currently reachable
tree must be covered by a complete observation before deletion.

A new reachable delta of at most 32 trees and 1 MiB may be read/parsed inside the
final transaction. Both limits are checked from metadata before fetching bytes.
This keeps ordinary concurrent commits from starving a long external scan. A
larger delta releases the transaction and is observed externally, reusing this
call's prior observations. After three unsuccessful validation attempts the call
returns the deletions actually committed so far; a later sweep retries remaining
garbage. There is no full-payload fallback under the writer or new scheduler.

After extending observations, candidate hashes are filtered again against both
tree and blob references. The DELETE addresses only those hashes and rechecks
`created_at` against the call's fixed one-hour grace cutoff. The writer is
released between candidate batches, and normal calls continue until the object scan is
exhausted. Concurrently detached trees already observed can conservatively retain
some garbage until the next call. The keyset cursor advances even when newly
observed references or a refreshed age make a candidate batch delete zero rows;
that batch is consumed rather than requeued.

## Failure and concurrency semantics

A missing, wrong-kind or malformed reachable tree fails closed with its object
hash. This deliberately tightens the old inner join that silently omitted a
missing tree. The final metadata check also catches a cached tree removed by
another sweep and later referenced again.

Every batch owns its own reachability proof and transaction. A prior batch's
validation cannot authorize a later deletion. If a later batch fails, earlier
committed deletions remain committed; the warning includes their actual count.
The public result remains an error, so callers must not infer a whole-call
rollback. Repeated oversized concurrent changes can instead return safe partial
progress without an error. Both the hourly caller and admin GC use this entry.

The limits bound payload work and affected row counts, not wall-clock time.
Closure metadata still costs O(reachable commits); deleting a large individual
object also has storage costs. No strict millisecond guarantee is claimed.

## Validation

Task-local test pauses instrument the real sweep's parse and pre-validation
phases. The regression requires an unrelated writer to commit while parsing is
paused, then releases and drains both tasks before asserting. Safety tests cover
reactivated old objects, restored ancestors with unchanged HEAD, missing and
wrong-kind trees, concurrent sweeps, age refresh, multiple batches, small
continuous commits, both delta limits and bounded safe deferral. A real-entry regression also
restores a missing ancestor into a finite cycle with duplicate `(parent, tree)`
states and verifies all known reachable trees/blobs survive.

The initial optimized synthetic probe matched 65,633 commits, 108 HEADs and
18,846 trees containing about 419 MiB of valid manifests. The old implementation
took 2.85–2.96 seconds and delayed an unrelated writer for 2.83–2.94 seconds while
deleting zero objects. These shared-host synthetic measurements establish the
structural bottleneck; they are not fixed production latency. Performance
acceptance must repeat the same real-entry probe after this change.
