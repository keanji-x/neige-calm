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

This replays the accepted candidate `44d42e43d22b66a88f8b36cd4b0c50b335908582`
(the diff from `90aaa109cbadc166ff05b9e50d2c561ee31e7ca9`) onto kernel-fetched
startup main `f8d3c314f86eb1f8f933a20b688bc1537051b290`. The initial tree was
clean and matched that main; the kernel had not replayed the candidate.
All intervening main behavior is retained, including #2756 claimed
writes/compensation, #2772 queued editing and continuous Planner state, #2773
fresh-send visibility, #2779 storage-neutral ignored notifications, #2780 Progress
expansion, immutable first-input receipts/observation and query publication,
workspace execution/recovery, and #2782 shared depth/scroll geometry. Released migrations
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
startup main, candidate base and candidate versions for all twelve rejected files:
`harness/run_loop.rs`, `openapi.rs`, `routes/mod.rs`, the track-conversation test
root, generated OpenAPI, providers public/queries, and router provider, recovery,
send-feedback, new-composer fixtures and `public.tsx`. Conflict-free application
is also inspected for overlapping owners, including start interaction, summaries,
version response, domain decoding, store projection and mutation expectations.

Resolution keeps the main InspectInput dispatch and input-inspection modules
beside the archive-fenced durable command owner. Both archive and initial-input
routes, schemas and test modules are registered. Provider queries retain the
answered receipt callback, inspect operation, canceled-read fence and atomic
beforePublish/cache write; archive adds only its own mutation. Router resolution
retains submitted draft state, initial-input notices, queue/edit/submission and
Progress wiring. Creation fixtures retain the main receipt response and add
explicit archive metadata. The newly added first-message fixture also declares
`archivedAt: null`; no decoder defaults are introduced. Two main hotspot browser
callers omitted the existing required `hasPendingMessages` argument; the compile
failure is resolved by declaring false for those transcript-only fixtures, as in
their sibling callers, without changing assertions or runtime. No operation
claim/compensation implementation or generic card/list implementation is replaced.
Archive remains in its existing owners. Main still used REST 33 / Web 55 and
migration 0168, so 34 / 56 / 0169 are unoccupied. Real generation also updates the
flattened TrackConversationCreated schema and type, preserving its receipt.

The earlier production mutation evidence remains applicable to compact,
replacement, durable issuance and transactional track access: those owner files
and their admission logic are byte-identical to the accepted candidate. Repeating their original
red reproduction would provide no new evidence. The integrated tree reruns those
regressions alongside command/idempotency tests. Ignored notification tests live
in the library test target, so a separate focused library run covers their actual
scope and the upstream claimed-write transaction tests. Browser replays include
queue motion, fresh-send visibility and persistent Track view state in addition
to desktop, coarse-pointer and 390px archive/restore. The frozen kernel gate
replays its declared targets; this does not substitute for independent final L2
reviews or precise-head PR CI, which remain Planner-owned.


## Persisted start admission (PR #2790 final review)

An HTTP-owned CardStartFence is not the lifetime of a start operation. Submission
commits Pending before waiting for the operation drive mutex. Cancelling that
request drops its fence but leaves durable work. The old archive path saw an idle
predecessor and returned 204; a later production drive replaced its session and
native thread, and reset also cleared the transcript. Real HTTP reset and restart
regressions reproduce this schedule before the fix.

The start submission owner, CardStartFence, now exposes settled-start admission.
Archive holds that card fence and the existing operation drive mutex, then asks
this owner before writing metadata. It reuses the operation repository's boot
recovery inventory and the start wire decoder, rather than copying operation SQL,
phase lists or payload field spellings into the route. The existing first-send
CardConversation read intentionally prioritizes ThreadToPreserve over StartInFlight;
it cannot answer whether an existing conversation also has a pending reset, so
its return contract stays unchanged. Any same-card resumable
start, including a committed preparation or compensation, refuses archive with
409 and no metadata, card timestamp or event write. Other cards' starts do not
block it. The card fence prevents a new submission until archive commits; the
drive mutex serializes normal provider preparation, spawn and compensation.
Boot recovery runs before serving HTTP and uses the same durable inventory;
terminal/stuck operations cannot resume effects. Start has no AwaitingRetry
phase. Successful completion or compensation permits a later idle archive.
No generic driver or kernel policy depends on Assistant identity.

The regressions cancel real HTTP after Pending is stored. A second schedule drives
the same operation through real prepare, cancels at adapter interaction entry,
and invokes boot recovery after the refused PATCH. They check reset and restart,
unchanged pre-PATCH runtime/history on refusal, no archive event, operation convergence,
and successful later archive followed by a harmless production drive. Existing
create/first-send/replay, restart compensation and preserving-recovery scopes
remain the owners of those contracts. The new admission costs one existing
unfinished-operation inventory read per archive assignment, independent of the
number of providers or test cases. Future start phase changes update their owner
and its recovery contract, with no extra archive phase roster.

CI run 39049420963 attempt 1 on old head 8b3caf086 also exposed stale FE mutation
evidence: s2b-conversation-row-from-session-state now reddens four archive/restore
renderer cases. Those fixtures retain turn_pending without a kernel activity
verdict; the mutation adds working to their exact accessible row names in both
renderers. Their independent assertions stay intact. Only the existing mutation's
expected_red and rationale expand; selection and strict complete-red-set judgment
remain unchanged. The architecture whole-file focused recheck passed 75 tests;
its reported 5s CI timeout is not evidence for changing the timeout. New-head CI
and fresh independent L2 reviews remain required.

Local verification for this correction used the real api_suite archive scope:
`env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --test api_suite conversation_archive --no-tests fail --no-fail-fast --test-threads 8`.
The original code failed both Pending reset/restart cases after PATCH 204. The
fixed scope passed 21/21. Disabling only the start owner's settled rejection
predicted exactly those two reds across all 21 tests; the actual complete red
set matched, including an empty reset transcript and a replaced restart runtime.
Byte restoration passed 21/21. Prepared cancellation cases stayed green under
that mutation because their PendingThreadStart snapshot already refuses archive;
they additionally pin real recovery convergence. Previous, unchanged compact,
replace, durable-input and authority mutations were not repeated.

Focused library owner scopes selected 10 start-adapter, 3 start-read contract and
14 claim-transaction tests. Harness scopes selected 52 tests covering real start,
first-send/create ownership, boot/provider recovery, restart/compensation,
repoint/bootstrap and the start-fence invariant. The only initial owner failure
was the existing source invariant's count for the new read-only kind reference;
its existing entry now records that reference, and the focused recheck passed.
No test inventory, generic lifecycle policy or judge was added. Shutdown names
only the superseded predecessor after start completes; it neither mints a native
context nor clears transcript. Worker/PTY restart adapters are not Assistant
start entry points. Formal reviews and exact-head CI still own final acceptance.

The affected FE evidence was run with the existing runner:
`cd fe && npm run test:mutation:run -- --shard 81/104 --test-scope witness --report /tmp/archive-v6-fe-mutation.json`.
This selects one affected entry from the 104-entry manifest and runs its complete
three-file witness scope. Expected and actual red sets both contain exactly 12
test IDs; the unchanged judge reports no errors and the runner restores its
production target. A subsequent `npm test -- --maxWorkers=2 core/view/conversation-list.test.ts web/src/features/chat/list/public.test.tsx web/src/app/router/track-conversation.test.tsx`
passes all 200 tests. The first gate's text failures were corrected by using the
existing public frozen wire-key constant in the new test query and splitting
that SQL literal; neither ratchet baseline changed. That mechanical query change
passed the complete 21-test archive scope again and leaves the production
mutation and its load-bearing assertions unchanged.


## PR #2790 conflict replay on f8d3c314

This is an L2 integration candidate, not a new-head merge approval. The prior
approval names 44d42; fresh independent Codex reviews, exact-head CI and the
required new-head approval remain Planner-owned. No worker merge, rebase,
cherry-pick, commit or history rewrite was performed. The kernel gate owns the
attempt commit. The canonical architecture test retains only its accepted
30_000ms budget change; root, checker, fixtures and assertions are unchanged.

The current production archive route, start fence, compact, replace, durable
command and transactional access files match 44d42 byte-for-byte. The full boot
unfinished inventory method and recover_on_boot entry point also match the
candidate's base. The saved `/tmp/archive-v6-mutation-results.json` was read:
its complete 21-test scope produced exactly the two predicted Pending reset and
restart failures, restored production bytes and returned green. The saved FE
mutation report was read as well: its expected and actual 12-ID sets agree.
No admission boundary was changed by this replay, so those mutations are retained
as prior evidence rather than reported as newly executed. Current real-route
coverage reruns the guards on the new combination. The archive receipt regression
additionally checks immutable create receipt replay and read-only initial-input
observation against main's new receipt contract, with history unchanged.

Worker verification on this combination:

- `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 npm run gen:api`
  from `fe/`: exit 0; 135 binding tests and the real OpenAPI emitter.
- `npm run typecheck` from `fe/`: first red on the new first-message fixture's
  required archive field and the two upstream hotspot callers' missing required
  prop; the corrected tree passes. No required type or decoder was weakened.
- The gate's seven-file FE focus plus the two new first-input files: the seven
  declared files passed 445 tests; the first-input files initially had 38 schema
  failures from their missing required archive metadata. After explicitly fixing
  their single producer, rerunning both files passes 40/40. Assertions, production
  adoption, query publication and queue revision behavior remain intact.
- `npm run test:browser -- --maxWorkers=2` with assistant-provider,
  context-card.coarse, first-message-continuity, first-message-geometry and
  thread-hotspots browser files: 5 files, 64/64 passed in real Chromium.
- The gate's `api_suite domain_api_suite::track_conversations` focus with
  `--no-tests fail --no-fail-fast --test-threads 8`: 60/60 passed, including
  archive/start/recovery, the additional receipt assertions and simulated
  managed-provider lifecycle tests. No real Provider E2E ran.

- `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run
  --locked -p calm-server --test planner_harness_suite -E
  'test(planner_compact::) | test(planner_replace::) | test(planner_input_idempotency::)
  | test(planner_restart::) | test(planner_restart_compensation::)
  | test(planner_harness_boot_recovery::) | test(planner_start_fence_invariant::)'
  --no-tests fail --no-fail-fast --test-threads 8`: 91/91 passed. This is one
  selection across the command, recovery and start-fence owners, not a broad suite.
- `cargo fmt --all -- --check`: passed after module ordering corrections.

Logs are `/tmp/archive-v8-harness.log`, `/tmp/archive-v8-generator.log`, `/tmp/archive-v8-typecheck.log`,
`/tmp/archive-v8-typecheck-fixed.log`, `/tmp/archive-v8-fe.log`,
`/tmp/archive-v8-first-input-fixed.log`, `/tmp/archive-v8-browser.log` and
`/tmp/archive-v8-api.log`. The kernel gate separately verifies its frozen commit;
its actual verdict and command/recovery replay results accompany the worker
handoff. The only formatting corrections reorder the two combined Rust module
registrations. Generated wire trailing spaces are the real generator's format,
not hand edits. Released migration files have no diff, and no reject or conflict
marker remains.


The first frozen gate passed text, contract, quick (including OpenAPI drift),
60 API tests and 12 migration/version tests, then had one intermittent failure
among 52 command tests: `planner_replace::a_recorded_revert_survives_a_restart`
timed out waiting for the restarted queued turn. It had already passed in the
91-test worker scope. The exact complete 52-test command reran unchanged and
passed 52/52 (`/tmp/archive-v8-harness-replay.log`). The test and queue fixture
have no diff from startup main. Source inspection shows a pre-existing readiness
window: the fixture waits on refused_issuances, but that counter is incremented
before restoring the runtime state and persist_issuance_outcome; the test then
immediately reloads the snapshot. This is evidence of an unsynchronized test
schedule, not proof of the exact failed snapshot (the original log did not
capture it). No cause is attributed to machine load, and no assertions, timeouts
or production recovery behavior were changed to mask the failure. It remains an
explicit intermittent-test caveat for the Planner, with the subsequent complete
gate verdict reported separately. The first gate log is
`/home/kenji/.local/share/neige-next/data/gate-logs/d59ad94cb70a426f8ac5ea16755ac974:sync-2790-merge-conflicts-v8-r1.log`.
