# Conversation assembly

`store.ts` is the shared read/operation assembly used by the Today and Track
conversation panes, including main and side panes. `contracts.ts` declares its
Scope, RouteIntent and Store interfaces. The router chooses a valid scope and
layout, then calls this one assembly; it contains no copy or re-export of it.

| Input or capability | Owner |
| --- | --- |
| Transport and recovery/unauthorized handling | app/providers and core/api |
| Scoped query facts and invalidation | QueryClient via app/providers/queries |
| Read numbering and live/history handoff | conversations/read-order and live-replies |
| Cross-mount input, drafts, send records, restart lease and confirmed memory | ConversationProvider |
| Outbox delivery and view projection | conversations/outbox and core/domain/conversation-outbox |
| Stop feedback and request lease | conversations/stop |
| Fresh-session restart strip | conversations/restart and core/domain/conversation-restart |
| Summary/name/state projection | core/domain/conversation-summary |
| Queue revision visibility | core/domain/conversation-outbox |

Initialization and cleanup retain the existing React hook order: tab registry,
transcript/read tracking, history and run observations, model catalog and
view-local state, live handoff, stop lease, restart strip, then outbox reconciliation. One
mounted view observes one card. Changing scope disables/rekeys its queries and
resets view-local state under the existing hooks. Closing/unmounting does not
clear the provider's drafts, inputs or dispatched sends; late results remain
owned by the original card. Query collection, stop cleanup and live retirement
stay with their declared owners.

Production contracts are exercised through `app/router/track-conversation`,
`today-conversation`, `planner-conversation`, `live-replies`, the side-pane browser
paths and `chat-performance` browser probe. Direct store consumers in tests
import the assembly here; query/route integration remains covered by the router.
The extraction preserves function bodies and contract types, with AST parity
checked against the source snapshot before moving them.

`draft-actions.ts` assembles first-message commands from a required draft
snapshot, the Registry edit/start/adopt/discard port, recovery-admitted transport,
typed create/refresh/derived-id ports, capability booleans and open/close/onGone
signals. Construction performs no IO or Hook initialization. Dispatched attempts
retain their original scope/key/text and settle through the Registry, including
unknown-result rereads and refused-before-dispatch restoration. Navigation
destinations remain chosen by the route through its injected signals.

`pane-lifecycle.ts` owns the mounted pane's draft retention cleanup, delayed
adoption, requested-open consumption, scoped Escape interruption and one-shot
side-draft auto-send. The router invokes each Hook at its original position;
Registry remains the owner of cross-mount drafts and open requests. Adoption and
open requests wait until their row belongs to this pane, inline panes do not
consume a shared open request, and auto-send consumes its intent before delivery.
Escape belongs to the focused conversation region and yields to source panels,
composer menus and native overlays. Cleanup removes only undispatched drafts.

Pane still observes capabilities and holds selection/layout/focus. Those remaining
responsibilities are tracked in #2083; loaded DOM and streaming display work remain
under #2235.

The router uses UI's generic `useContentAdmission` only for first mounting eligible
content. It keeps the selected shell/composer available while a rendering opportunity
precedes the expensive thread. Eligibility comes from this store's existing history,
remembered transcript and status facts; it is not another history readiness or send
authorization. Read acknowledgement additionally requires that presentation boundary.
Ordinary updates after mounting are not deferred, so visible actions keep their
existing transcript and command alignment. See the
[admission contract](../../ui/content-presentation/README.md), the Chat thread README
for feature integration and the production `conversation-switch` browser regression.

History derivation and confirmed-memory effects depend on the registry's stable
history/remember operations and relevant rows, rather than its whole context
value. Typing still updates the one retained composer; it does not invalidate
unchanged history. Outbox operations remain authoritative for dispatch, keyed
retry, queue visibility, refusal/unknown restoration and attachments. The app
keeps local Sending feedback separate from run/activity evidence of execution.

The outbox's `awaitingReconciliation` shares the existing input-blocking predicate;
the app declares waiting feedback from that fact and leaves read errors on their
existing retry surface. The sentence does not claim an active fetch or delivery
after an ambiguous replay, and successful POST handling still does not await reads.

Recovery methods return whether the local owner took the recovery intent, never
whether the server accepted it. Only a taken action requests composer focus.
The existing parking contract stops restoration when the user moves elsewhere.
Draft adoption probes current field/parking ownership through a UI-only handle
bound to that draft key. It never requests focus from cleanup, a capability read's
completion or a late result in another view. A selected adopted draft resolves its
real row directly; there is no intermediate closed drawer. Normalizing the target
is conditional on that draft still being selected, while Registry adoption and
the created row remain authoritative even after the user closes or switches.

A legal retry rekey reports its actual scope/key at adoption. The scope's callback
checks that identity against the current draft before probing its current UI
handle; an old attempt cannot hand focus to another draft. Initial composer submit
keeps the original void handoff into the held draft, including local refusals.
Only recovery commands consume the local-takeover boolean, so a private retry
cannot leave an extra restored copy in the next new-conversation composer.
Initial admission also passes its actual text to the held-draft owner: recovery
capture can refuse the POST without erasing the first sentence or inventing a send.
