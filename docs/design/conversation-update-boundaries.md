# Conversation update boundaries

Tracking: [#2807](https://github.com/keanji-x/neige-calm/issues/2807).
Motion policy is a separate change, [#2808](https://github.com/keanji-x/neige-calm/issues/2808).
The parallel [#2809](https://github.com/keanji-x/neige-calm/issues/2809) owns
source-anchored surface geometry and reversible presence. This change adds no
anchor, source inference, navigation or presence mechanism. Preserve Chat's
existing entry markers, focus and reading contracts; consume any changed public
UI contract from that work rather than duplicating it. Recheck overlap before
each implementation freeze and merge.
Review tier: L2, because replacing the local state owner must preserve delivery,
draft adoption and same-tick command isolation. No transport or persistence format
changes are planned.

## Outcome and scope

Ordinary text edits in one conversation should update its editor without
rebuilding history, lists or the other editor. Changing a tail reply should reuse historical message
and activity bodies whose inputs did not change. A feature developer should be
able to identify the owner of each fact and subscribe to that fact without
registering a component in a global framework.

Keep React, Query, the existing domain functions and all loaded message DOM. Native
Find, cross-message selection, drafts, focus and reading intent retain their
contracts. This is not a claim that retained DOM layout becomes free or that a
browser DOM commit can run on another thread. Initial history preparation and
canonical grouping may still scan the loaded transcript. Measure computation,
React execution, DOM work and observed interaction latency separately.

## Existing owners and the concrete gaps

Paths below are relative to `fe/`.

| Responsibility | Owner | Change |
| --- | --- | --- |
| Network facts, Query keys, mutation admission and read order | `web/src/app/conversations/store.ts`, `outbox.ts`, `live-replies.ts`, existing core contracts | Continue consuming the same authoritative facts and domain functions. |
| Local drafts, composers, outboxes, edits, upload/queue/restart leases and open intent | `web/src/app/conversations/public.tsx` | Replace React state/ref pairs with one per-Provider owner and real scoped subscriptions. |
| Main/side association, selected surface and injected capabilities | `web/src/app/router/public.tsx` | Consume narrow snapshots; put high-frequency editor text in an actual subscribing leaf. |
| Canonical quiet-sync grammar and adjacent activity grouping/key assignment | `core/domain/conversation-quiet-sync.ts`, `conversation-groups.ts` | Keep these rules authoritative; reconcile their derived results without inventing a second grammar. |
| Message/activity rendering, expansion and committed group-key memory | `web/src/features/chat/thread/` | Reuse unchanged body inputs and avoid historical activity conversion/effects. |
| Reading, focus, content admission and motion primitives | Existing `web/src/ui/` owners | Preserve their declared contracts; do not add conversation fields to UI. |

The current Context value changes for every Registry state update. All its
consumers execute again, even when their card or field did not change. A selector
that still reads that broad changing Context does not solve this. Both pane hooks
also execute in one router parent: narrower notifications alone cannot prove
independent pane execution.

ChatThread currently recreates group wrappers and activity arrays whenever the
transcript array changes. Existing MessageEntry memoization does not protect
activity conversion, unstable reading callbacks or historical ToolCallGroup
layout effects. Stable React keys alone do not stabilize those inputs.

## One local state owner, two read modes

Create the Registry once per ConversationProvider. Context carries this stable
instance, not a reactive aggregate. Construction has no IO or subscriptions. The
instance replaces the old state/ref pairs; do not mirror old React state into a
second store in an effect. Query and view-local reading state are not imported
into it.

Separate stable commands/current imperative reads from render subscriptions.
Event and async commands may hold the owner and read its latest facts. Render
code uses narrow hooks backed by useSyncExternalStore. Do not make a current
getter silently stand for a reactive render dependency. An internal selector
bridge may be generic, but public owner hooks should make the intended fact and
identity explicit; no new package or public cross-application store is needed.

The owner publishes immutable snapshots. Each hook returns the same reference
until its actual selected fact changes. Notify only subscribers whose selected
value changed under Object.is. Selector functions must be pure and must not
allocate a new snapshot on every getSnapshot call. A private listener mechanism
may evaluate cheap selectors at publication; avoid duplicate handwritten lists
of affected feature names or parallel topic invalidation rules.

| Read scope | Snapshot and invariants |
| --- | --- |
| Card composer | Existing ComposerContent, with separate text/attachments/semantic-emptiness selectors where consumers need only one fact. Attachment URL/metadata changes are meaningful; refill equality is not a universal comparator. |
| Scope unsent text | New-conversation composer text, separate from a held draft's submitted text/immutable submitted input. |
| Scope draft slot | The existing held/adopted union, published as one fact. |
| Card remembered data | Canonical turns and summary independently selectable. A history-only write retains the unchanged summary; a summary-only write retains unchanged canonical turns. |
| Card delivery | Outbox and spent-row accounting from the same completed command. |
| Card edit/upload/leases | Select the existing edit, notice, upload or busy boolean; internal queue count 1→2 does not change the busy fact. |
| Remembered collection | Intentional collection or Track projection where a caller needs it; preserve unchanged rows/collection when only history changes. Use actual trackId, never parsed IDs. |
| Open intent | One nullable object carrying id and focus intent. Repeating an explicit open is a new intent even if its payload matches. |

Null/inactive identities use stable empty values and no active subscription.
Opaque card/scope IDs are not parsed by the subscriber. turnsOf currently allocates
the withConfirmedSends view: subscribe to its stable canonical/outbox/spent inputs
and derive through the existing function, or retain a bounded current result
keyed by those actual input references. Never write the displayed overlay back
into canonical memory.

## Atomic publication and lifetime

For each command, compute its complete immutable next root, publish it, then
notify. Every callback can read all affected fields in that final state. Pure
updater exceptions publish nothing. This needs a small owner-local commit
operation, not a rollback journal or an async transaction framework.

Preserve these existing command boundaries:

- beginSend synchronously fences another same-tick send and publishes its outbox,
  cleared notice and replaced-edit departure together.
- Outbox retirement and spent-row cleanup/accounting publish together.
- Enter/cancel/settle edit publishes its composer, edit and notice effects
  together. A late queued save still matches the exact held edit and cannot clear
  a newer composer.
- acceptDraft validates the scope/key and receipt before publishing accepted
  delivery plus adoption together. False/throwing acceptance changes nothing.
- A restart guard is acquired synchronously before its async operation. Queue
  writes retain their existing initial microtask boundary until a separately
  proven change is required.

Audit reads immediately after commands. In particular, use editOutbox's returned
written value when releasing settled initial-input reads; do not accidentally
depend on an old render closure or change source/lease ordering through a new
current getter. Read counters remain imperative and do not notify.

Subscription cleanup is exact and idempotent. Last unsubscribe removes listeners,
not conversation facts. Closing/switching keeps the existing retention policy.
No new delete/forget policy is introduced. A replacement Provider owns a new
instance; late completion against the old owner cannot notify the replacement.
Do not permanently dispose a useState-created instance in an effect cleanup:
StrictMode reuses it across setup/cleanup/setup. With no external resources,
ordinary hook unsubscription and reachability are sufficient.

getSnapshot, subscribe and render do not execute commands, acknowledge reads,
navigate, focus or advance committed group-key/cache memory. React's external
store bridge handles the write-before-subscription/commit gap.

## Router isolation with a bounded first step

Migrate every production Registry reader, its effects and relevant tests to narrow
snapshots plus stable commands. Search actual callers; do not keep a permanent
consumer inventory or an indefinite broad compatibility hook.

Move existing/new editor text subscription into app-owned composer and compact
dock leaves; cover both ChatComposer and ChatDock production paths. Their
parent continues to own attachment, command and status capabilities, while event
handlers read current command inputs where needed. Retain the feature Composer's
existing controlled-input contract and all current state/focus/send behavior.
Text edits that preserve semantic emptiness must not invoke either pane's
history/controller hook. Existing queued-edit availability and refused/dormant
refill notices depend on isComposerEmpty, so an empty↔nonempty transition is a real
status change that may notify those narrow boolean readers. The current-response
edit action uses the same fact and must update too. Do not freeze those controls
or notices to win an isolation assertion. Do not subscribe the parent to
a compound ComposerContent if it only needs attachments or that boolean.

Keep the coordinating main/side hooks and their adoption, source validity,
companion and lifetime policies initially. Query, selection and actual status
changes may still execute both cheap pane controllers. The claim is selective
notifications, isolated editor keystrokes and stable heavy descendants, not zero
shared-parent execution for every event. If measurement still identifies those
controllers as material, a later component split must declare its slot lifetime
and injection contract first; do not add portals or a second controller to claim
isolation without proving it.

## Incremental view boundary

Build a pure derived view from the one assembled transcript using the existing
quiet-sync fold, adjacent grouping and committed key assignment. Reconcile against
the previous committed view so unchanged entries, activity arrays and quiet-sync
bodies retain references. Keep position-sensitive index, exchange/gap/live/edit
metadata outside the reusable body. Prepends can change positions without
invalidating every unchanged body.

Equality follows actual immutable entry inputs and complete quiet-sync semantics:
membership, lifted entries, author and outcome. An ask/failure can change the fold
verdict without joining its body. Do not optimize by assuming only tail appends,
using timestamps as invalidation, or creating a history/live ordering merger.
Canonical grouping may remain O(N); the useful incremental guarantee is unchanged
body identity and avoided conversion/render/DOM effects, not O(delta) arbitrary
edits.

Keep at most the current committed derived view per reading view. Candidate
projection during render is pure. Adopt candidate cache and group-key memory only
after that render commits; an abandoned render cannot poison future identity or
keys. Existing historical byActivity key memory remains its current owner/policy,
not an excuse for another historical cache. Conversation/reading-owner changes
must not reuse another view's mutable memory.

Introduce a memoized feature-owned activity-run boundary. Its stable activities,
reading UI and callbacks keep historical tool conversion and ToolCallGroup
effects asleep. Make reading callbacks stable for their actual owner, suppress
true no-op UI writes and keep an owned stable untouched-group value. Preserve the
vendor's detail-state replay, direct-child data-nc-entry DOM shape and focus
restoration contract. Do not bypass meaningful live, error, expanded/detail,
image, edit or gap changes to win a render count.

## Delivery slices and acceptance

Design converges through two independent source-grounded reviews before runtime
implementation. Each behavioral slice is L2, with two fresh full reviews after
behavior fixes and required checks before squash merge.

1. **Subscriptions and editor isolation:** replace the local owner, migrate real
   readers and the text leaf in one coherent change. Prove no unrelated listener
   notification and no history/controller execution on a text edit preserving
   semantic emptiness; prove empty-boundary controls/notices remain reactive. Exercise
   multi-field atomicity, same-tick guards, empty/no-op reads, subscription gaps,
   StrictMode and provider replacement, then existing adoption/outbox/edit/upload
   and main/side/Today paths.
2. **Incremental transcript views:** use canonical grouping and committed bounded
   reconciliation; stabilize activity/reading inputs. Prove tail edits preserve
   historical bodies, while prepend/delete/reorder/detail/quiet-sync changes are
   correct; test discarded renders and existing key/focus/reading behavior.
3. **Actual interaction and profile:** same named normal production workloads
   before/after, desktop/narrow and normal/reduced motion, full loaded history and
   native Find/selection. Include typing, tail stream, switch, held/rejected/unknown
   send, queued stale/done/gone outcomes, close/return and first-input handoff.
   Report raw conditions, adverse samples and remaining main-thread layout. Use
   source-mapped profiling only for attribution; final latency uses normal builds.

Add stable red regressions at actual production entry points before fixes. Only
the few load-bearing isolation/publication/commit invariants need explicit
production mutations, with a complete predicted red set and whole observing
target runs. Reuse existing test discovery/CI jobs; no growing test/file roster,
new benchmark CI threshold or configuration pin is required. Run repository text,
frontend gates and relevant tier-1 integrated paths; CI owns broad suites.

## Extension exercise and maintenance cost

A new upload badge reads the card upload snapshot and receives existing attachment
commands; it knows no other card, registers nowhere and receives no text edits.
A new transcript form extends Chat's renderer and, only if necessary, its core
grammar. A new local fact adds its owner type/command, narrow hook and meaningful
tests. Adding a component/test does not change a subscriber registry, central
list, motion policy or Query invalidation scheme.

The new subscription mechanism remains private to app/conversations until a
second independent responsibility proves a reusable contract. Projection remains
pure domain/view derivation; browser memory and rendering stay in Chat. No provider
private layout, inferred protocol identifiers, new transport version or persisted
schema is introduced. Ordinary later changes pay only for the owner contract they
actually extend.
