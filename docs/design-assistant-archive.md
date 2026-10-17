# Manual Assistant conversation archive

Related issue: #2640. Review tier: L2, durable state and input admission.

A person explicitly archives or restores an Assistant conversation. The default
list has independent Active and Archived collapsible groups on desktop and
mobile. Active starts expanded; Archived starts collapsed, shows its count and
is omitted when empty. Archived history remains readable and restorable. Planner and worker cards are ineligible. No timer archives conversations.

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

## Current-main integration and F1

This adapts PR #2642 (`fbf300ec7`) and the saved candidate `889bc704b` onto
the kernel-fetched startup main `90aaa109cbadc166ff05b9e50d2c561ee31e7ca9`.
The old candidate base `b09a46389` is not this delivery baseline. All ten intervening
main commits are retained, including #2756 claimed writes/compensation, #2772
queued editing and continuous Planner state, #2773 fresh-send visibility, #2779
storage-neutral ignored notifications, and #2780 Progress expansion. Released migrations
0166–0168 remain byte-frozen. Migration 0169 adds archive metadata; REST 34 and
Web 56 require its nullable `archivedAt` summary field. The production generator
owns wire/OpenAPI output. Experimental databases created by the unpublished PR's
conflicting migration 0166 are not supported upgrade inputs: recreate those
experimental databases explicitly; never reinterpret a released migration.

F1 is a cancelled HTTP request whose compact or replacement command already left
the channel. Its request-owned CardStartFence disappears, so channel emptiness is
not proof that the command stopped. A deterministic real-route reproduction parks
the queue owner after dequeue and before issuance, drops the HTTP request,
commits archive, then releases the command. The original compact calls the
provider and the original replacement accepts new input after successful archive.
The same reproduction also demonstrates durable ordinary input acceptance.

Compact, replacement and durable acceptance now revalidate archive state while
holding issuance, before provider preparation, queue staging, transcript removal
or key binding. Archive holds that same lock through its transaction. Either the
command wins and archive observes busy work, or archive wins and execution
refuses. Durable command staging, persistence and rollback have one owner shared
with the fixture ingress. Existing receipt replay remains a historical read.
Ordinary observations may remain buffered while archived; turn issuance refuses
to drain them until restore. Queue edits/steering require existing pending entries,
which already prohibit archive; they cannot accept a new durable send.

The new PATCH consumes authenticated Principal separately from its human Actor
criterion. Track access resolves the actual Area before lifecycle effects. The
commit re-reads Track and uses organization_access's transactional Collaborate
policy, including current registration/membership and System deployment-owner
rules. Both archive and restore, including idempotent requests, revalidate there.
Older routes' authorization debt is outside this adaptation.

The frontend preserves deriveConversationList, ContextItem/ContextList, explicit
Planner question indicators and the current card status/menu geometry. Archive
callbacks enter shared listProps, both conversation panes and the track-bound
mobile owner. History retains its four views, date groups and close-before-open
handoff. Surfaces without archive actions or archived rows retain their existing
presentation. Failed writes retain the row, show one error and allow retry;
archive metadata comes only from authoritative summaries, with no decoder default.

Verification ownership: the worker runs focused regressions, production-generator
execution and formatter/lint fixes. The kernel gate owns text/contract/quick,
frontend lint/build, and the declared focused replays. CI owns broad suites;
formal L2 reviews are arranged by the Planner. No permanent test roster is added:
ordinary future tests edit their existing owner scope only. A migration updates
its migration file and the existing head-schema inventory once. Versions update
the server/frontend constants and existing compatibility assertions once.

Required inherited ownership trailers (preserve verbatim in delivery and PR):

```text
OWNERSHIP-CHANGE: fe/core/api/generated/openapi.json — approved manual conversation archive contract and real generator output (#2640)
OWNERSHIP-CHANGE: fe/core/api/generated/wire.ts — approved required Assistant archive metadata and real generator output (#2640)
```


## Startup-main verification scope

The source replay used `git apply --reject` followed by `git merge-file` with
startup main, candidate base and candidate versions for the three rejected FE
files. Resolution keeps both the queued-edit operation and archive mutation,
both domain regression cases, and the current router queue/edit/submission and
Progress state wiring. No operation claim/compensation implementation or generic
card/list implementation is replaced. Archive remains in its existing owners.

The four previously verified production mutations remain applicable: compact,
replacement, durable issuance and transactional track access, plus their real-route
regressions, are byte-identical to the saved candidate. Repeating their original
red reproduction would provide no new evidence. The integrated tree reruns those
regressions alongside command/idempotency tests. Ignored notification tests live
in the library test target, so a separate focused library run covers their actual
scope and the upstream claimed-write transaction tests. Browser replays include
queue motion, fresh-send visibility and persistent Track view state in addition
to desktop, coarse-pointer and 390px archive/restore. The frozen kernel gate
replays its declared targets; this does not substitute for independent final L2
reviews or precise-head PR CI, which remain Planner-owned.
