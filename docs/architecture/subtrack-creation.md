# Subtrack creation and route vocabulary

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
