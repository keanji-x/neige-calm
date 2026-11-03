# Subtrack creation and route vocabulary

## Live creation authority (#3509)

Review tier: L2, because creating a subtrack crosses an authority boundary.

A `spawn: "sub-track"` declaration may allocate a new child only while its parent
Track is an open Area Admin. The child operation calls
`area_admin::require_admin_tx` in the actual creation write transaction, after
the stale-context fence and the `child_track.continue` branch, before any child
Track, Card, or workspace is allocated. This shares the authority predicate with
independent `track_add` admission. Storage errors propagate; an authority refusal
is the existing permanent `Forbidden("not_admin")` operation failure. The
Scheduler settles the parent task and notifies its Planner, without retrying the
refused allocation or creating child resources.

Authority is live storage state, never a `FrozenParent` preparation fact. A
queued declaration, Pending operation recovered before creation, or preparation
snapshot cannot preserve a grant revoked before the creation transaction. A
same-key operation replay retains its terminal refusal. Once creation has
committed, recovery and workspace/bootstrap retries consume the committed child
and its frozen configuration without requiring a new grant. Revocation does not
delete or invalidate existing children.

`child_track.continue` assigns new work to an eligible retained child under its
existing binding and lifecycle checks; it allocates no Track and requires no new
Admin grant. Its stale-context fence still applies. Creation still enforces the
same Area, tree inventory and member budgets, depth, workspace ownership, and
idempotency contracts.

Admin authority is never inherited by the child. Subtracks are ineligible for an
Admin grant under the existing eligibility rule, so a newly created child cannot
create a nested child. Historical nested trees remain readable and retain their
budget/depth safeguards; synthetic historical fixtures test those mechanisms
without claiming nested creation is currently authorized. Legal creation tests
grant the specific parent explicitly, without making general boot fixtures
Admin by default.

## Route vocabulary migration

Review tier: L2, because the route spelling changes persisted task/report data.

Both features are useful only as distinct ownership contracts: a subtrack executes a
parent task with scheduler completion and tree admission; track_add starts an independent
recipe-based workstream. Share the mechanical Planner/Report/layout assembly and events,
not their authority, resource admission or lifecycle policies. One `initialize_track_tx`
entry takes a typed `TrackInitialization` parameter: Independent carries prepared Planner
configuration, Subtrack requires the delegated seed and inherited provider. No boolean mode
or optional-field combinations can silently erase those differences.
Planner provider lookup belongs to the harness profile, not the child operation adapter.

Rename the public child route to `sub-track`. Keep the within-track route out of scope. Released SQL
migrations, immutable event/operation evidence and request fingerprints remain byte-frozen.
A forward SQL migration registers the upgrade. A boot-time data migration updates task routes and uses ReportDoc
and writes its CRDT plus projection together before serving/recovery; read-side historical
report normalization remains explicit. Change only task block spawn, never arbitrary prose,
context strings or completed operation payloads. Preserve block IDs/revisions, root hashes,
parent/creator relationships and all unrelated report fields. Fence old frontend clients.

Acceptance: child and independent creation retain authorization, recipe, workspace and
completion behavior; new sub-track accepted and old write value refused; legacy reports
and task rows upgrade consistently and idempotently, migration failures prevent startup;
historical events/hashes remain unchanged. Exercise an online SQLite backup of service
4140 through the production migration entry point; verify integrity, foreign keys, report
CRDT/cache equality and absence of legacy child routes in active declarations. Only a
validated offline upgrade may touch the live database. No real Codex E2E on shared host.

Verification: focused report/types/migration tests, track_add and scheduler child tests;
critical migration assertions mutation-verified in exclusive worktree; text/contract/quick
Rust gates; frontend lint/build/test and focused browser coverage for affected UI; two
independent full-diff review channels after final fixes. Broad Rust suites belong to CI.

Implementation detail: repository startup depends on the report codec (calm-report), a
transport-free crate, to migrate authoritative documents in the storage transaction.
The explicit historical read codec is required for immutable events and restore sources;
it does not grant old spelling to the live write validator. Non-root task context hashing
uses its released route spelling, like the existing frozen root-hash field contract.

Track VCS HEAD remains the last event-anchored snapshot. The upgrade does not invent a
user edit or rewrite prior content-addressed objects; active reports use the migrated
CRDT/cache, and the next relevant report event snapshots canonical content. Structured report/event readers decode the released child route; raw VCS blobs stay unchanged. This is intentional audit preservation.

Legacy V1 CRDTs are an explicit exception to preserving stored terminal field spelling:
their context reader already projects the terminal-command upgrade in memory. Persisting
that same effective V1 projection is required to preserve frozen non-root references.
Existing V2 blocks receive only the child-route rename. Both source layouts have regression
coverage through the production context projection, rather than a copied hash implementation.
