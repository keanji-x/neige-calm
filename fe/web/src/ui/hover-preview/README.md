# Hover preview

The primitive owns hover/leave/travel timers, nested portal dismissal and keyboard
focus handoff. Hosts inject content, navigation and optional reading/avoid surfaces;
no application identity or admission policy belongs here.

Astryx `useLayer` owns native popover presentation and safe hosting (including
BottomSheetSwitcher). Its zero-size custom-position wrapper surrounds the slot;
`LayerDepthProvider` and `useLayerDismissal` preserve the shared Escape/IME stack.
The Layer render marker is portalled to the nearest native `dialog` root, or the
trigger document's body outside a native dialog. These are public platform hosts,
not vendor selectors. This keeps independent viewport slots outside ancestor paint
and CSS zoom while retaining native dialog accessibility/top-layer scope. Layer
still owns popover presentation, invoker wiring and safe host handling; the UI
does not duplicate its unsafe-content host policy.
Native presentation follows `present`, including retained exit paint. Shared
Escape participation reads current post-bind admission at event time, so pending,
suspended and revoked paint cannot consume Escape. Trigger-local pending Escape
still cancels preparation; editor consumption and IME flags remain authoritative.
Layer lazy mounting may commit open intent before the slot: observation starts
only when the actual slot exists. The slot/actual/card owners below retain all
measurement, visual forwarding, intrinsic sizing and interaction permissions.

`placePreview` is the only placement/collision owner. Its result is a fixed
reservation slot, independent of natural content height. Neige-owned portal DOM
order defines priority: only earlier interactive slots participate. Ancestors
precede descendants; retained reopen keeps its position, while unmount/remount
appends a new position. Parents never consume descendant or later sibling slots.

The tree is slot → unanimated actual wrapper → one paint card → scroll viewport
→ intrinsic flow root. Fractional intrinsic size plus header and borders determines
actual height, capped by the slot. Above content bottom-aligns in its slot; below
and side content top-align. The same children mount once. Intrinsic observation
continues after clamp and catches delayed content, image and font layout without
feeding natural height back into placement. Shared source lift replaces the former
CSS appear effect.
Semantic visibility removes interaction, collision/escape discovery and navigation
participation immediately; inert/aria-hidden exit paint may remain until shared
presence completes. The outer positioner also becomes inert immediately so its
retained box cannot intercept a pointer intended for the underlying control.
ArrowDown during retained exit schedules focus for the interactive open commit.
Nested previews use a primitive-owned typed interaction port across portals.
Each child registers only at its nearest declared parent's stable trigger endpoint
by its Neige-owned id; registration releases on unmount. Every parent close
synchronously revokes direct children, recursively clears pending intent and
rejects retained-content activation. Reopening a parent never reopens an old child.
The port carries semantic ownership and live ancestor proof only. Source, reading
and avoidance bounds use their real visual DOM rectangles, including parent lift.
Nested slots intentionally follow that paint; side changes and legal placement
rejection use the existing close path. Target frames come from the independent
actual wrapper, never the painted card. No borrowed inverse endpoint remains.
It adds no global registry, DOM broadcast or context-rule exception.
Navigation still focuses the connected root trigger before
calling the destination. Ancestor scrolling closes a child preview; scrolling a
child portal body does not close its parent. Hover delays are unchanged.

The stable trigger span, final positioner frame and paint retention consume the
[shared motion contract](../motion/README.md). [Browser tests](preview.browser.test.tsx)
cover actual nested portal geometry, one native effect per card, focus/navigation timing and scrolling. Menu primitive
coverage stays in [UI](../menu/public.browser.test.tsx); the real AreaSelector
composition belongs to [app](../../app/shell/area-selector-motion.browser.test.tsx).

Layout reads external inputs, computes a slot and submits only exact changes.
Observers bind actual nodes/owners rather than placement or callback identity.
Already queued input callbacks reject inactive ownership before reading/submitting.
An owned slot commit notifies only later portals; their fixed reservations form a
DAG. Content observation changes only actual height. A newly mounted actual wrapper
prepares until its committed slot width and natural size agree, then owns valid
frames throughout retargeting. There is no recurring pending-placement/null-frame
protocol, numerical threshold, iteration cap or timing-based convergence.

The report feature excludes all preview-owned report descendants from background
reading areas, including retained paint. It supplies no animation algorithm.
Ordinary content/tests require no manifest or test-name list changes. The frozen
gate replays the existing focused targets once; CI owns broader suites.

Preparation and retention use one capability: the actual DOM node whose legal
slot and intrinsic size have both committed and whose motion binding is readable
at post-bind publication. Open
intent mounts one measurable content tree but does not acquire retention. Until
that capability commits, the actual wrapper clips its paint and its positioner
is inert; no default frame is exposed. The layout wrappers disable CSS transition properties so the global
reduced-motion duration cannot animate slot geometry or preparation exposure.
Content keeps its own styling and motion contracts.

Every close uses the same synchronous semantic revocation and cancels open/leave
and focus intent. With no committed paint node, closed means absent immediately:
content effects, resource listeners/timers and geometry observation are released,
without awaiting motion completion. With committed paint, close keeps that same
node inert for the current shared exit; only its current onExited releases the
capability. Retained reversal preserves it. A rejected placement after preparation
exits the last valid paint; unavailable layout/source reads do not reacquire a
source or invent a frame. Shared null-frame preparation/suspension remains intact.
ArrowDown hands focus over only after the interactive frame commits. First paint
admission notifies later reservations even when slot geometry is unchanged, through
the same ordered layout event; an ordinary geometry input can finish preparation
without an unrelated render.

[Preparation browser regressions](preparation.browser.test.tsx) cover normal and
reduced first placement rejection from a real long inline source, cleanup and legal
reopen, unavailable declared measurement during keyboard/outside/parent/same-commit
reversal/unmount, StrictMode, first-frame exposure, stale exit, runtime reduced
changes and loss of a legal slot after preparation. No production DOM is rewritten
to manufacture the long-source rejection. Ordinary added tests edit their target
only; no new registry, rule, manifest or CI check is needed.

### Post-bind admission

Open intent, a committed candidate, historical paint capability and published
admission have separate responsibilities. The typed paint union is empty,
admitted, suspended or retained. A candidate alone cannot retain paint. A parent
port says pending, admitted or revoked: pending preserves explicit child requests;
revoked recursively cancels requests, focus and timers. Later admission never
revives a cancelled child request.

Motion consumes the candidate with open driven by request, and present driven by
request or historical capability. Its optional measurement notice only schedules
a local commit. The publisher runs after motion's binding commit, checks the
current actual/paint nodes, cycle, ancestor permission and binding lifetime/revision,
and first publishes the readable port. One DOM gate then writes clip, inert,
aria-hidden and active/Escape markers; focus, direct-child notice and ordered
later-slot notice follow. Static render defaults initialize safe permissions before child autofocus; React render does
not copy stale public attributes onto new nodes. Event guards consume the same
publication. Listener callbacks schedule local work and never publish recursively.
Equal binding/slot values generate no admission publication. Current native paint
invalidations use the existing ordered layout event to schedule only later portals;
they never schedule their own publisher or publish recursively. The current native
playback owns the complete native animation group, its finite clock-driven pump
and terminal notification. Its public instance controls observe the actual receiver,
including borrowing between members; legal foreign receivers do not notify this
owner. Direct external prototype calls bypass this observation contract. Hover
adds no control wrapper, foreign-owner discovery or paired-part test surrogate.
Paused,
settled, reduced, cancelled, suspended and unmounted states have no idle pump.
Active forwarding may cost O(n²); it is not compositor-only.

Preparing/suspended trees have no collision, global Escape, bridge, outside-press
exemption, focus or activation capability. Synthetic clicks are guarded as well as
native inert. Local trigger Escape cancels preparation; document Escape belongs
to the legal top layer. Temporary measurement loss preserves the request/sample;
focus loss caused by inert suspension does not start a business-close timer.
Leaving the actual trigger still cancels a preparing hover request through the
existing leave delay.
Close revokes the port and old descendant generation synchronously before keeping
only historically legal inert exit paint. Completion cannot release a reopened
request. Static owner/slot labels describe ownership rather than admission.

An admitted parent notice carries its publication token and a live permission
reader. Descendant ports and event guards validate that proof through the ancestor
chain, so a cached admitted notice cannot lend authority while an ancestor binding
is unavailable before its pending notice commits. This is a read fence, not recursive
publication or an additional readiness source.

Each child publication records its admitted ancestor token. Updating a child
parent port schedules a post-bind commit; receipt alone cannot renew the child
publication. A proof reader is bound to its exact publication, so replacement,
close/reopen and ancestor revision permanently invalidate older proofs. Equal
commits preserve the publication object and keep current proofs stable.

Forced focus loss from an unavailable ancestor is also pending, not local leave:
the ancestor proof gates the nested trigger blur/leave path. The real trigger
can still cancel its own preparation while its ancestor remains admitted.

Hover-open and leave/travel callbacks capture the request generation. Close
clears the open timer synchronously as well as leave/focus intent; a captured old
callback cannot create or close a newer request after re-admission. These remain
the existing interaction delays, never a layout recovery or convergence clock.

Static safe DOM defaults initialize newly created nodes; they never derive
permission from a previous render. The post-bind gate is the only dynamic writer.
The child layout-focus regression requires this initialization before child layout
work, while subsequent equal render defaults preserve the published gate state.

An open descendant with an unavailable parent proof declares no motion frame.
Shared suspension therefore precedes source sampling, preserving the existing
source-freeze policy instead of freezing a source merely because its ancestor
port is temporarily pending. A closed historical exit keeps its legal frame,
so ancestor revocation does not strand retained paint.

The [coordinate browser target](coordinates.browser.test.tsx) checks each slot
against the same physical visual inputs through the sole placePreview and an
independent empty fixed CSS ruler, with ≤1/64 CSS px per committed edge. It never
writes a production slot or suppresses a transform. Same complete input/phase
revisits are exact; a second held sample at every phase and 20 idle frames require
zero drift, pump, attributes and notices. Deep trajectory is reported as intended
following. Root phase alone omits child compensation and is not an equal-input
oracle. Three/five layers, fine CSS/gaps, 390/1440 and fractional scale/zoom/multiline
are finite coverage, not exhaustive. External reading/avoid must not depend on
this slot or downstream geometry; arbitrary external CSS-only animations without
owner invalidation are not globally discovered. Normal viewport fixed portals and
existing unsupported transforms retain their prior scope.
The [design/history](../../../../../docs/source-surface-motion.md) preserves old
inverse failures and distinguishes controlled-report ProductionApp evidence from
unmodified live data.

Native parent controls need no resize/scroll to resume following. The shared
playback owner invalidates later slots after drained pause→play and after a seek
while still paused. Hover retains the same source→placePreview→slot path; its own
paint notice never repositions itself. Paused/settled 20-frame zero pump/DOM checks
and terminal source checks cover this path in preview.browser.test.tsx. This does
not discover arbitrary externally created CSS animations.
