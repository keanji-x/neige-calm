# Manual Assistant conversation archive

Related issue: #2640. Review tier: L2, durable state and input admission.

A person explicitly archives or restores an Assistant conversation. The default
list hides archived rows; an Archived view can read their history and restore
them. Planner and worker cards are ineligible. No timer archives conversations.

## Ownership and provider contract

Store archive time in a card-keyed table with cascading deletion. Conversation
summaries expose nullable archivedAt. Archive is an idempotent state assignment,
not a toggle. Emit CardUpdated transactionally after changing archive metadata
and the card timestamp, reusing the existing conversation invalidation path.
Only the REST user may change archive state, with track/card membership checked.

Archive does not delete, stop, reset or recreate provider sessions. This is
intentional: providers have different native resource and resume capabilities;
archive must not destroy context for a backend that cannot reload it. Existing
recovery continues to own native continuation and explicit provider refusals.
Provider hibernation is outside this change. Automatic recovery skips archived
Assistant cards; an explicit restore permits the existing lazy recovery path.

## Admission and concurrency

Archive and restore take CardStartFence, also held by message sends and session
starts. Live archive additionally holds the existing harness issuance mutex through
the metadata commit, reusing its confirmed-idle predicate. Issuance rechecks the
persisted archive flag before draining, including for non-durable observations. A live reply, compaction, or queued input refuses archive. Existing keyed
send receipts replay before admission; a new send or restart/reset while archived
is refused under the same fence. History remains readable. Restoring changes no
provider binding or thread identity and never silently starts a fresh session.

## Verification

REST tests exercise production routes, provider-parametrized persisted cards,
archive/restore idempotence, busy refusal, wrong track/role/actor, input admission,
and metadata persistence. UI tests cover default/archived lists, pending/error
states, read-only archived history, restoration and fresh query data. Browser
coverage includes desktop and narrow viewports. Run API generation, focused Rust
and frontend suites, repository text/contract/quick gates, and tier 1 E2E.
Mutation-verify the archived input fence against the complete archive test scope.
Two independent review channels check the final diff including abstraction
boundaries, duplicate logic, hardcoded assumptions, external facts and permanence.
