# Linked repair settlement acceptance briefing

On delivery of the existing R2 settlement notification, compute a repair-only
acceptance section in the same transaction as the current authority check.
Preserve ordinary R1 notices and all existing queue/replay relevance predicates.

Reuse the exact candidate input binding and delivery view for C2 snapshot,
publication, verification policy/outcome, authenticated R2 report identity and
full finding responses, and original receipt findings. Run the existing producer
verdict preflight on an in-memory accepted event (no append or receipt write).
This supplies the same candidate checks as verdict admission without creating a
second qualification algorithm. Current notification authority remains an
additional prerequisite. Existing decision reads distinguish a prior decision
from a fresh acceptance opportunity; invalid/stale evidence is unavailable.

Only acceptance-ready snapshots offer calm.task.verdict with the exact C2
producer attempt ID, accepted status, and a requirement for the Planner's own
message. The Planner must assess the supplied untrusted report evidence; the
snapshot is not durable authorization and the actual verdict revalidates.
Blocked, unavailable and already-decided snapshots offer no acceptance call.
No schema, persisted protocol, event, task kind or automatic verdict changes.

Acceptance checks exercise actual queued Planner turn input for ready and revoked
lineage/receipt cases, including exact identities and complete response evidence.
Failed/unsettled R2 and already-decided cases call the same production observation
reader in its transaction, independently of turn scheduling. Mutation-check the new authority
assertion exclusively in the writer worktree, restoring before parent reviews.
Run bounded candidate repair/settlement tests and required quick Rust gates.
Parent owns independent experiments, reviews and PR/merge; this writer commits
only the intended implementation, tests and this design.
