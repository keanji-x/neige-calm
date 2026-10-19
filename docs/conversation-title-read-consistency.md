# Conversation input authority and release

This repair is L2 because release crosses persistent input ownership. Formal
review belongs to the Planner; the declared gate owns required verification.

[The harness input owner](../crates/calm-server/src/harness/run_loop.rs) owns
pending text, attachments, ordering, ids and revisions. Transcript projections
describe dispatched input; confirmed rows retain history. Receipts prove initial
acceptance, not current text. Metadata projects these facts without repairing
input or inferring lifecycle policy.

Title metadata uses the repository's existing shared `SqliteDb`: the original
`begin_immediate_tx(repo.db())` acquires its single writer lease before any
candidate SELECT. Candidate batches and runtime projections read through that
same immediate transaction until commit. Other reads retain the read pool's
capacity. The start owner's transfer proof uses one SELECT through
`repo.sqlite_db().read_pool()`; it needs no writer admission. Read-pool role is
an application contract, not SQLite `query_only` enforcement.

[Typed commit continuations](../crates/calm-server/src/harness/run_loop/input_commit.rs)
retain the actual resolved issuance or accepted steer until its responsibility completes.
They hold outcome identities, not copies of pending, steered or restored input.
Closing ingress does not revoke accepted work's commit responsibility. The
snapshot writers take the continuation lock before collecting canonical input,
and complete the covered result only after a matched transaction commits. Result
incorporation holds that same lock, so an earlier snapshot cannot clear a later
result. This applies to ordinary, terminal and failed-current snapshots; the
original issuance writer keeps its own retired boundary. Err/false retains the
continuation and discoverable owner unless there is positive transfer proof.
Tick and ordered Release each attempt the same completion once, without
provider redelivery or sleeping inside the actor. Mutable commands are refused
while a result remains unresolved; reads, replayable observations and terminal
notifications continue. Retry snapshots the latest canonical state, including
terminal restoration. HTTP cancellation does not cancel the input actor.

An accepted steer's transfer proof comes from the start owner's existing typed
journal decoder: the same source, entry id and revision must appear in the
committed take. One SQLite SELECT binds that journal to the stamped, superseded
source and the card's current successor. Missing rows, active IGNORE writes,
unrelated retirement and compensating/failed operations do not prove transfer.
The read grants no write authority and does not restore the old source snapshot.
Issuance keeps its original commit boundary; no proof is inferred from a batch
key or provider success alone. The watchdog copies its armed deadline before
entering the timeout body, allowing its existing checkpoint to finish without
relocking a guard retained by the condition. Timeout and state policies are unchanged.

Queued edit/delete commit through the same owner guard before ACK and audit;
Err/false restores queue and debounce. Durable ingress already owns atomic
snapshot plus send-key binding and rollback. Replace already commits its queue,
rewind and binding transaction. Observations retain events and their old durable
watermark for replay on failure. Returning dispatched input to pending retains
`restored_projections` until snapshot plus provisional DELETE commit together;
confirmed history is excluded. Failed steers still awaiting terminal settlement
prevent release. A handle with no unfinished operation or restoration owes no
extra database access merely because it stops, even with durable pending input.

Closed-track [hibernation](../crates/calm-server/src/harness/run_loop/hibernation.rs)
owns a different prerequisite: unloading native context hands subsequent wake to
the durable snapshot. After shared input settlement, its idle queue owner must
commit the existing guarded checkpoint before closing observations or stopping;
only then may the lifecycle owner remove the handle and release the provider.
Err or a zero-row write keeps registry membership, provider resources and ingress
available for retry. Cancelled replies and queued observations are rechecked
under the existing observation gate before closure. Ordinary resource cleanup
still owes no checkpoint without an actual input responsibility.

The ordered Release waits behind accepted processing. Failed release keeps the
notification owner alive. A stopped receiver is joined before direct settlement;
cancellation retains the JoinHandle. Registry removal follows successful release
and checks instance identity. Preserving HTTP recovery reloads committed input
before resuming the original provider thread. Release has no retired full-write
fallback. Only the original issuance boundary retains its existing retired writer.

Scope A fixes the new closing/result-checkpoint windows, covered by the original
six and sibling [release tests](../crates/calm-server/tests/cases/planner_release_commit.rs).
[Start, harvest and compensation](../crates/calm-server/src/operation/planner_harness_start_adapter.rs)
keep their original order and guards. The KNOWN GAP in `stranded_user_messages` remains: harvest can race a provider-accepted
outcome that has not committed, and redeliver it. This repair preserves successful
restart when an accepted steer returns across that committed MOVE; it does not
close the duplicate-delivery window. No registry freeze, transaction fence or earlier predecessor stop
is introduced; failed-start streaming semantics are unchanged. A process crash
before the outcome checkpoint still loses the in-memory continuation and can
replay durable pending. This repair makes no exactly-once guarantee.

[Owner contracts](../crates/calm-server/src/harness/run_loop/conversation_title_tests/settlement.rs)
retain restoration/DELETE fault, cancellation, no-responsibility cleanup and
current-carrier fences. Existing install, missing-table, losing-start, give-back,
legacy harvest, second restart and late-thread tests keep their premises. No
schema, provider protocol, permanent caller/test manifest or CI invocation is
added. Adding a command requires its owner contract; adding a regression uses
the existing suite/filter. Test and mutation evidence belongs in the task report.
