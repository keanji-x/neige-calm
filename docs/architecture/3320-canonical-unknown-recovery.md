# Canonical ACP Unknown recovery (#3320)

Review tier: L2, because recovery changes execution authority and persistent carrier identity.

## Outcome

An authenticated Human can explicitly abandon an unresolved ACP execution and start
an empty native session on the same canonical card, retaining readable history,
reports and completed task results. No uncertain prompt is resent. Unknown stays
Unknown. A separately persisted genuine normal-driver-join witness permits Keep
settlement after restart without fabricating END.

## Ownership and contracts

- ACP owns exact receipt/normal-join evidence, native issuance exclusion, and
  pre-C4 absence. Provider execution owns positive settlement evidence.
- Canonical lifecycle owns Human admission, start/dispatch/track/native/MCP
  exclusion, rechecks, and synchronous registry publication after commit.
- Truth owns distinct abandonment eligibility and atomic replacement. Share
  replacement writes with ordinary idle replacement; preserve its strict gates.
- HTTP/UI consume typed recovery declarations. Historical edit/rewind capability
  stays false. Explicit abandonment has a distinct action and endpoint.

## Recovery boundaries

Capture exact current worker, submission, attempt and revision. Under retained
exclusion prove old execution quiescent and revoke old card/session credentials.
Recheck current Human source, resource access, worker/binding/registration,
checkpoint, ingress and execution proof in the writer transaction. Fail closed.
Retire the old worker without changing its native identifiers, Unknown/Keep,
claims, checkpoint or transcript. Atomically compare/repoint card and planner root
(assistant replacement leaves root unchanged), preserving provider/model/workspace
contracts and the old accepted watermark. The fresh worker is dormant, with no
native/thread identity and no submission replay. An abandonment receipt makes
same-key response-loss replay explicit; a changed request conflicts.

Human pending inputs are transferred exactly with queue positions, timestamps,
envelope fields and next_queue_position, validated by the existing claim owner
without creating a claim. Their original idempotency receipts stay unchanged.
System observations and ExplicitSuccessor intents are worker-bound and are refused;
transferring them needs a separate Dispatcher/retry-owner contract. Open asks,
rewind and incompatible lifecycle state are refused. Never silently discard work. Post-prefix
events remain unaccepted rather than being skipped with a global maximum.

Persist normal-join evidence only after the exact retained task successfully joins
with CleanUnknown, independently of Keep. It binds the exact receipt, canonical
proof and registration. Process absence cannot create this witness. Witnesses
support Keep continuation only after current ownership and cleanup rechecks.

## Verification

First reproduce cold Unknown via the real HTTP creation/ACP fixture and assert the
explicit recovery endpoint succeeds (red before implementation). Preserve existing
Held/no-resend tests. Add tests for genuine witness across restart before Keep,
HTTP abandonment (planner and assistant), source/ownership/registration races,
unproven quiescence, pending-input refusal, immutable old evidence, history,
credentials, unchanged watermark, replay and fresh native identity. Mutation-check
load-bearing CAS/revocation/no-resend assertions in an exclusive recoverable tree.
Run focused Rust tests, text/contract gates, quick Rust gates and real generated API
artifacts. Run frontend lint/build/tests, browser preview and relevant E2E. Two
independent snapshot reviews must converge; CI owns broad suites.

## Risks and rollback

External stop/revocation can precede a refused replacement; the old carrier remains
held and history retained. No SQL transaction awaits provider I/O. Cancellation
cannot detach the recovery owner during stop/commit/publication. New migrations
are additive; old released migrations are byte-frozen. Disabling the explicit
action leaves the original safety fences in place.

## Integration contract checks

Canonical `Unknown` is also the delivery fact while a live ACP prompt awaits its
response. The run declaration reads the exact original submission's journal row:
`Sending` and `Completed` keep ordinary delivery feedback instead of presenting
abandonment. An `Unknown` receipt is never filtered out; a missing receipt still
produces Held, because abandonment requires positive journal evidence. Following
execution loss, the ACP execution completion owner calls
`acp_submission_recover_turn`, which atomically refines `Sending` to `Unknown`
without demoting a `Completed` winner. The read declaration supplies no settlement
authority and does not change the checkpoint or journal.

Assistant cold Unknown now declares `abandon_session` while historical replacement
remains unsupported. Its boot regression must retain the no-resend assertion and
check the explicit recovery action both before and after reopening. API revision
40 and web compatibility 63 cover this declaration and the abandonment endpoint;
the server and frontend compatibility constants must agree, and the real API
generator owns both OpenAPI and TypeScript artifacts.

Evidence preservation applies to retirement and replacement: abandonment updates
the old worker to `superseded`, preserving its checkpoint, receipts and native
identifiers; it does not delete either worker. Normal-join witnesses belong to the
exact worker/submission, and abandonment replay receipts belong to their old and
new workers. Their incoming `ON DELETE CASCADE` edges only remove metadata after
an explicit worker deletion, matching ACP submission ownership. They cannot delete
a worker or fire during retirement. Replay is retained while both workers exist;
these tables are not an audit archive independent of worker lifetime. The schema
inventory must enumerate these three new edges explicitly, retaining its rejection
of unregistered cascades and all worker-deleting triggers. Future changes to these
ownership edges must update the migration and that inventory, rather than add a
broad cascade exemption.

## Accepted Codex completion branch

The additional incident comment distinguishes Accepted/exact-native-turn from ACP
Unknown. Admit held Codex control restoration only for the canonical owner's exact
Accepted Original Initial, same deployment owner/provider/profile and native turn.
The configured deployment daemon's exact protocol client owns source/namespace
checks; no rollout scanner supplies authority. Native recovery reads the exact
terminal with source/activity and Human admission rechecks, imports only its final
items via the presentation owner, and retains a past canonical END association
without synthesizing live control. Ordinary END CAS closes the original once and
retains pending input identities/order. Native idle remains required before new
issuance; the persisted carrier need not already be idle before reconciliation.
Unknown/no-turn/mismatched native evidence stays held. #3321 remains separate.

## Retry boundary

ACP currently refuses ExplicitSuccessor creation even for fully frozen invocation
settings (`conversation/retry.rs::retry_availability_tx`, unsupported ACP binding).
The HTTP regression verifies this refusal preserves Unknown and does not prevent
explicit fresh recovery. The abandonment predicate also refuses existing retry
intents; no fabricated ACP retry-intent row is used to pretend that unsupported
acceptance path exists. Pending System observations are exercised through their
real event and acceptance owners.
