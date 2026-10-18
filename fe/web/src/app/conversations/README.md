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
snapshot, the Registry edit/start/accept/discard port, recovery-admitted transport,
typed create/inspect/refresh/derived-id ports, capability booleans and open/close/onGone
signals. Construction performs no IO or Hook initialization. Dispatched attempts
retain their original scope/key/text and settle through the Registry, including
unknown-result rereads and refused-before-dispatch restoration. Navigation
destinations remain chosen by the route through its injected signals.

`pane-lifecycle.ts` owns the mounted pane's draft retention cleanup, selected-target
resolution, requested-open consumption, scoped Escape interruption and one-shot
side-draft auto-send. Main and side panels resolve their existing view target here
before deriving parent/child ownership and presentation gates. Registry remains
the owner of cross-mount drafts and open requests. Adoption and
open requests wait until their row belongs to this pane, inline panes do not
consume a shared open request, and auto-send consumes its intent before delivery.
Escape belongs to the focused conversation region and yields to source panels,
composer menus and native overlays. Cleanup removes only undispatched drafts.

Pane still observes capabilities and holds layout/focus. Those remaining
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
real row directly for both panel ownership and rendering; there is no intermediate
closed drawer or second selection state. Normalizing the target
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

First-message creation uses this existing outbox with a distinct accepted-input
arm. It retains the original `{text, atMs}` submission and real immutable receipt;
it grants no ordinary input retry, edit, dismiss, queued caption or running status.
The mutation imports that acceptance and adopts its actual scope/key synchronously
before publishing the canonical list row. A recovery lookup follows the same
publication boundary, cancelling the list's older retryer before adoption and
publication; a listed but incomplete creation is not accepted input.

Current presence comes from the serialized server inspector through an ordinary
TanStack query. Its 1000 ms refresh applies only to an unresolved accepted input
in a visible pane before an error; events, reopen, focus, reconnect and Reload can recheck
an unresolved input after an unavailable/error result.
Every coherent response publishes its observation immediately and starts a fresh
history/run read pair behind that observation's boundary, including repeated
payloads. Periodic inspection pauses while either actual source query is fetching,
so slow reads can finish; explicit refreshes still use the existing query owner.
Queued evidence outranks stale projections after rebuffer. Exact segment provenance
deduplicates local words. Later actual history and run reads can display newer
canonical words while an older queued claim remains unresolved. Once the filtered display
admits an exact current source with an actual non-null provider turn, the accepted op
retains its required nullable `sourceWitness`. This permanent handoff prevents unloaded
pages from reclaiming the original display; it does not claim execution completed.
An unbound provisional row contributes positive version/body knowledge and deduplicates
while present, but can be withdrawn on failed start and rebuffered at the same revision.
It therefore never becomes a permanent witness: its retained input fact supplies the
plain copy across withdrawal and the next queue drain. Positive input facts still end
creation-specific observation; normal history/run owners continue the retry. Projected
evidence can also retire an unresolved original outside the loaded page after both fresh reads.
A changed queued body hides the original words immediately and releases input,
retaining nondrawing receipt bookkeeping until both cached sources catch up. An
unknown current owner remains unavailable with its known receipt; empty queue
and history reads cannot establish absence. An actual local PATCH/DELETE ACK
records its typed entry/revision/body before reconciliation starts. Deletion or
a changed stored body suppresses the original copy and stale canonical source
without treating freshly reread old unbound rows or loaded-page misses as deletion proof.
The existing accepted op keeps at most one actual source witness and the latest actual
ACK for its registry lifetime. Card/track deletion keeps the existing
query removal and pane cleanup; registry disposal releases the retained record.
No additional journal or list is kept. Source handoff or a locally displaced original
releases the exact creation-query lease, including its in-flight request; later event,
focus, reopen and manual reads belong to the existing history/run query owners. Actual
provider-bound historical input and an exact source carrying the ACK's stored
edited body stay readable without claiming new provider delivery.
A same-body edit retains original acceptance; a successful steer delivers the
input and only retires its departed queue copy. Actual newer queue revisions
retain their controls under the existing queue owner.
Stale queued text and controls use the same observation/read fence; a newer real
run page can report a restored entry. Empty reads and elapsed time prove nothing.
The existing query cache retains the last coherent fact only while creation observation
is needed, including across close/reopen. Handoff, terminal ACK or retirement removes its exact query; card,
track and database-scope deletion use the existing cache-removal channel.

The Registry exposes raw `rememberedTurnsOf` separately from display `turnsOf`.
Only raw source enters the store's history fallback and memory writes: a local
outbox projection must never feed itself back into canonical memory. Before content
admission the router selects one renderable outgoing text from the same final,
queue-filtered display and uses the existing pure `OutgoingMessage`; `ChatThread`
replaces it in the admission commit. This adds no second transcript or presentation
acknowledgement state, and grants no read receipt, source snapshot or history action.


An accepted display copy carries its receipt's real queue entry identity, while
`inputSource` remains absent. The existing accounted-identity fold therefore
retires a departed queue ghost whenever that accepted fallback already draws the
words, including when a run update precedes a write ACK or stored history. This
does not manufacture canonical source, queue status or provider delivery.


Initial-input inspection describes current source certainty, not permission to
send. A declared accepted creation and healthy ordinary history/run paths keep
the composer usable while inspection is unavailable. The existing stalled,
failed-send, in-flight and ordinary reconciliation gates still own admission.


An actual PATCH ACK owns the revised readable body without altering the original
submission or receipt. The queue fold overlays its stored text/revision only on
an existing same-entry cached row; a real run omission never appends a queue row.
Until an actual same-entry raw source carries that acknowledged body, the existing
plain accepted display keeps it readable, with no queued caption or inputSource.
The one latest actual provider-bound point remains monotonic across later versions.
Only an exact current version owns current display; an older point cannot suppress
newer words, even when their body repeats earlier text. Page misses never erase
this positive history. Every
actual local queue-write ACK ends original creation inspection, including edits
that restore the original text. DELETE reports the removed revision and fences
that revision and all older ones; only a strictly newer real queue entry restores
its controls. The initial queue fold accounts for its hidden entries before ordinary
local tombstones filter rows. Ordinary deletion/steer keeps the existing departing
row spring unless an actual displayed carrier already accounts for that identity.


The accepted outbox record retains one nullable `inputFact { kind, rev, text }` from an
actual same-entry served queue, a validated queued inspection, an identity-bearing
CAS stale response, or a raw source carrying the declared paired entry id/revision.
The source provenance records a version without claiming queue presence. The latest actual local PATCH/DELETE ACK stays separately in
`localWrite`; a read is never converted into an acknowledgement. One core
`currentInitialInput` selector chooses the highest reported revision, with ACKs
winning ties (including a DELETE's unchanged removed revision). The immutable receipt
provides the actual initial `entryRev` and submitted body before later facts exist.
Observations below that revision or with a different immutable receipt are rejected. Missing queue
rows and unloaded history pages never erase a positive current body.

The existing queue-sync owner receives explicit `displayed` and `reported` sources.
Ordinary sends keep their existing displayed-queue synchronization; accepted inputs
consume only real reported facts. ACK overlays cannot report themselves as reads.
The same fact importer drives the selected queue projection and the existing registry
sync effect. Current source ownership always requires the selected actual version. An older
incoming fact may be retained as proof without reclaiming display, and repeated
same-version/body facts are unchanged. Source matching,
unbound-source suppression, cached-row overlays and plain readable copies all use
that one current version. A provider-bound raw point can witness only its declared entry revision,
regardless of equal words. The current wire pair remains required nullable. The FE
decoder explicitly normalizes only a missing pair on the released legacy
`{observation,event_id}` origin shape to unknown/null; a present malformed or incomplete
pair still rejects the whole history page. No old `queue_entry_id` fallback or zero
revision is invented. No queue caption or provider delivery is inferred from it.

A first positive queue fact also ends the creation-only observation lease, leaving
normal run/history/event owners responsible for later revisions. A queued inspection
starts its declared normal source pair before releasing the exact creation lease;
those independent normal reads continue. Unresolved no-source inspection retains
its existing event/focus/manual recovery and interval policy; unavailable/error
responses pause periodic retries. A source read failure after a projected observation
still leaves genuine reconciliation polling active. The record retains at most one
observed input fact, one latest actual ACK and one source witness for the existing registry
lifetime; no controller, journal, timer or unbounded list is added.


Ordinary input receipts identify an entry but do not report its queue revision.
Their exact provenance fast path therefore requires an actually known queue revision
and an eligible raw source revision. With an unknown revision, the existing bounded
text/high-water reconciliation remains authoritative; different known identities
cannot consume one another, and known older sources cannot bypass their version
fence merely because words repeat.


The core shares one initial-input placement helper between live display and Registry
projection. Without actual prior bound evidence, a missing creation prompt follows
leading real user/system input and precedes response content. After an actual older
bound version, the later current fallback retains ordinary tail placement, including
when that older bound point moves off page. Canonical source order stays authoritative;
ordinary queued/steer tail behavior and the existing #1923 gap remain unchanged. Role
comes from positive bound source structure and the same bounded witness, never text,
wall-clock ordering, renderer ID parsing or an inferred new provider turn.

Queue display accounting passes required paired entry references in separate
`displayed` and `suppressed` arrays. Core derives displayed references only from the
final visible presentation, after waiting copies move into the queue. A bare accepted
copy can account at its actual current revision without gaining inputSource; an
ordinary copy needs an actually known revision. Unknown versions emit no reference.
Suppression carries the initial-input owner's actual authoritative version, not the
older cached row's version. The app threads this contract. PendingQueue asks the one
core comparison about the effective saved departing revision; an older historical
carrier cannot remove a newer ghost, while live rows always keep their controls.
