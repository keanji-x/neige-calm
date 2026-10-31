# Write amplification and frequency governance (#3411)

Status: investigation and staged implementation. Historical conversation and
closed-Track retention belong to #3412. This document specifies outcomes, not
permission to delete production history or deploy changes.

## Two dimensions, separate measurements

A logical user action can cause repeated payload copies, SQL mutations, history
commits and WAL page rewrites. A frequent observer can also acquire the writer
without adding any durable fact. Measure these separately:

| Dimension | Measurements | Do not substitute |
| --- | --- | --- |
| Content amplification | Logical input/output bytes, bytes bound to SQL, bytes newly retained by owner, manifest size | Row count alone |
| Frequency | Attempts, writer admissions, SQL mutations, commits, event and VCS counts | Timer cadence alone |
| Contention | Admission wait/hold duration, read-pool wait, timeout counts | Physical write rate |
| SQLite I/O | Published WAL frames/bytes, checkpoint work, allocated/free pages | Allocated WAL file length |
| Process I/O | Logical reads/writes and kernel storage counters | Cached reads as disk traffic |

Use matched workloads with the same installed conversations, task/provider
activity, input sizes and measurement duration. Include idle, one edited report,
a burst of tool results, repeated identical publication and actual changed
publication. Record maintenance and concurrent work separately. Never log
credentials or content to establish byte counts.

## Owner and write-path inventory

The source baseline for this design is `01f945adc70607d5baf43c5923f8227297a038fd`.
Production observations in #3411 were collected from an earlier build and are
not proof of the current code's measured throughput.

| Path / owning component | Content amplification | Frequency and current protections | Next action |
| --- | --- | --- | --- |
| Report persist, `calm-server/src/track_report/write.rs` | Current CRDT/projected payload, full `CardUpdated`, `TrackReportEdited` before/after, then VCS payload/Markdown objects | One accepted edit creates multiple representations; actual writer owns task/objective guards | Design revision references and sweep readers before changing wire/history contracts |
| Event-to-VCS, `calm-truth/src/track_vcs/commit.rs` and `delta.rs` | Manifest and commit metadata can be written for an event that declares no changed path | An event-name exclusion list duplicates only part of the projection policy | S1: use the existing exhaustive path projection as the sole decision for empty deltas |
| VCS object store, `track_vcs/store.rs` | Changed manifests store complete path maps; existing blobs may still be rendered/hashed/bound | Immutable inserts already skip existing rows; transcript preparation already reuses prepared blobs | Distinguish empty delta, unchanged projected tree and real change; do not claim CAS insert avoidance is solved here |
| Turn-boundary transcripts, `track_vcs/transcripts.rs` | Hook history renders both `events.json` and `conversation.md` | Preparation occurs outside writer, with current-input witness checks inside; not a per-token write | Preserve this refresh contract; investigate unchanged refresh separately |
| Overlay storage and publishers, `db/sqlite/overlay.rs`, routes and plugin callbacks | Full overlay payload plus `overlay.set`, sometimes downstream VCS work | Generic upsert updates timestamp even for equal payloads; activity and Planner-context publishers already have pre-read equality checks | S2: transaction-current unchanged outcome shared by callers; preserve permissions and audit/sync contracts |
| Activity / Planner-context projections | Rebuildable overlay and event payloads | Event wake plus 30-second reconciliation; caller-side equality does not replace transaction-current equality | Keep reconciliation for recovery; coalesce projection work at the owner and revalidate inside writer |
| Canonical harness and checkpoints | Full recovery/presentation checkpoint can repeat | Event-driven/deadline migration is owned by #3395; some paths remain staged | Consume #3395; do not fork its scheduler or weaken source/result qualification |
| Session activity/liveness and reaper | Observation timestamps and current state | Push observations and periodic probes are different facts; timestamps may be needed for expiry | Measure actual mutations and establish a valid observation freshness policy before suppressing writes |
| Native transcript / normalized worker flow | Native provider payload and normalized display can retain overlapping content | Tailers poll at 250 ms, but polling does not establish that a DB write occurs; items and cursor commit together | Identify canonical recovery/display roles per provider; preserve atomic capture/cursor and accepted inputs |
| Operations and leases | Payload, outputs, phase details and durable retry receipts | Claims/renewals/retries are owned by operation lifecycle; empty candidate reads must not grant authority | Measure by operation and phase, distinguish required lease writes from empty admission; coordinate existing fixes |
| Background retention / object GC | Reachability scans, temporary data and deletion batches | Hourly event/object passes, six-hour history pass; expensive scans also contribute read load | Retention policy in #3412; write-budget instrumentation/transaction sizing remains shared infrastructure |

Paths above are relative to `crates/` where applicable. An observation timer is
not automatically a write defect. Never disable a recovery/lease transition
because it is frequent without tracing the authority and expiry contract.

## S1: stop event-only VCS commits

Outcome: a batch whose declared track-file delta is empty retains its authorized
events but creates no manifest, commit or HEAD update. A mixed batch still
projects all changes into one history commit. A hook-only event does not create
an initial tree; explicit snapshot/backfill and turn-boundary refresh retain
ownership of their existing projection work.

The VCS owner already has an exhaustive event match returning `PathDelta`.
Use emptiness of all its components (including removals, run keys, run cards,
run task keys and full-snapshot request), instead of maintaining another list
of event names. Preserve the special Track-deletion rule for the whole batch.

This removes downstream mutations and needless projection work. It does not
remove the event append transaction, introduce event coalescing, skip authority
checks or change retention. Unchanged-but-nonempty deltas remain out of scope:
they require comparing actual projected content and checking reader contracts.

Verification uses production event append, real VCS projection and subsequent
transcript refresh. Cover existing HEAD, absent HEAD, and a mixed hook/card-change
batch. Assert durable receipts, unchanged history/object counts for event-only
work, and that real content and delayed transcripts remain readable. Existing
VCS/transcript/GC suites check affected sibling paths.

Review tier: **L2** because the persisted history creation policy changes.
Two independent reviews must inspect the calling write pipelines, mixed-batch
semantics, snapshot ownership, recovery/read contracts, abstraction boundaries,
duplicate policy, hardcoded assumptions, external contracts and permanence cost.

## S2: unchanged publications

An unchanged overlay needs an explicit unchanged outcome in its storage owner;
callers must not emit a second event or historical version. Equality uses the
whole declared payload, not plugin-specific field stripping. A timestamp that
changes the payload is a genuine change under that contract; a plugin must
separately decide whether observation time belongs in published business state.

Inspect and reuse the existing typed transaction/event APIs. They currently
reject empty event batches, so silently returning an empty vector is not an
implementation. Define a no-change result that rolls back or completes without
mutation, while preserving result shape and required authorization. Cover REST,
plugin callbacks, kernel publishers and direct repository callers, including
concurrent publication and entity deletion. A read-only fast path alone is not
authoritative and must not bypass current permissions or resurrect deleted data.

After eliminating duplicate mutations, separately measure whether redundant
writer admission remains significant; only then add a safe owner-specific read
preflight with transactional revalidation. Do not promise zero admissions from
an equal-payload SQL predicate.

## S3: report content stored once per revision

Reuse the report owner's editing authority and existing immutable revision/object
storage. Events identify the edited object, before/after revision, author and
change metadata; consumers materialize content when needed. Keep current CRDT
semantics, task/objective guards and historical edit/diff behavior.

Before implementation, sweep websocket replay/live clients, REST/generated wire
schemas, frontend reducers, report-day changes/edits, report-read knowledge,
Planner tools, replay fixtures and retention/object GC. Publish the protocol and
migration decision. Every referenced version is a GC root for the complete life
of its readers/replay window, including during transactions and interrupted
cleanup. Do not shorten retention to conceal unresolved missing-content cases.

Avoid an extra revision database or a second report authority. Derived Markdown
and payloads can be generated or cached only after readers and cost are known;
compression and truncation are supplemental, not the primary duplication fix.

## S4: remaining frequency and matched-load acceptance

Complete the matrix with observed per-owner counts before attributing load.
Reuse existing writer admission instrumentation and add only bounded owner labels
where needed. Track source facts after commit, coalesce rebuildable projection
work, and schedule required observations by their owner deadlines. Preserve
accepted message order, unknown outcomes, restart, loss reconciliation and
carrier/execution fencing. Never debounce durable user intent.

The final acceptance compares both content and frequency for matched workloads:
no unnecessary mutation/history for unchanged state; correct changed-content
publication; no lost input or missing recovery fact; reduced measured write
amplification and frequency for the targeted owners. Required lease/protocol
writes are reported as such, not hidden in aggregate counters.

## Separation from historical cleanup

#3412 owns retention, closed-content compaction, pin lifetimes, sparse history
semantics and eventual file reclamation. A closed Track can still hold a valid
comparison baseline or cross-Track reference. Permanent idempotency receipts
must remain protected. New revision references introduced here must be included
in the cleanup design before either change ships.

No historical deletion, production VACUUM, provider process removal or deployment
is part of this write-path change. Performance numbers and review/test outcomes
belong in the corresponding PR evidence, not permanent source assertions.
