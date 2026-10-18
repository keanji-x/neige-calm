# Source to surface motion, first adoption (#2809)

Review tier L2: shared cross-primitive geometry and reversible paint lifecycle.
The existing UI Motion spring, camera/depth and native playback remain the owners.
Menu (including the real AreaSelector) and nested HoverPreview adopt lift in this
batch. Dialog/Drawer, vendor Layer/Sheet/MobileNav and mobile edge navigation are
not migrated. Card/PTY boards and full-page content remain instant.

## Approved deletion design (428491)

Model B replaces the former paint-independent nested-source contract. A source is
its real visual DOM rectangle, including parent lift; reading and avoidance inputs
use the same viewport. Nested cards intentionally follow that paint, may change
side, and use the existing close path when no placement is legal. This is a
semantic change, not a passing implementation of the old stationary oracle.
Historical failed inverse/rounding and original-page evidence below stays historical.

Delete the inverse geometry function, descendant bounds port, injected source
reader and Hover's borrowed parent reader atomically. Keep a ref-only
`SurfaceSource`, `SurfaceBinding.readBinding`, current binding readiness/revision,
identity/suspension/freeze, one solver and per-part rebase. Semantic registration
still carries setParent/release and live ancestor proof. Candidate and post-bind
publication remain the only admission path; the actual wrapper supplies the target
frame independently of its painted card. Menu gains no placement policy.

The forward graph is external DOM → visual source/reading → sole placePreview →
CSS-committed reservation → independent actual wrapper → native card paint →
later visual sources. Only earlier interactive reservations are obstacles.
External reading/avoid must not depend on this slot or downstream slots. Intrinsic
size affects actual height only. Own visual ticks never reposition their publisher.

An optional visual invalidation belongs to the existing native playback, compares
every owned Animation.currentTime, and sends its terminal sample. Paused, settled,
reduced, cancelled, suspended, unmounted and stale playbacks do no recurring work.
Hover reuses ordered layout notices to schedule only later portals, never recursively
publishes on the notice stack, and submits no equal slot. Active forwarding may
cost O(n²) across a deep tree; this is not compositor-only motion.

Acceptance: same physical visual inputs → production placePreview → independent
empty CSS ruler; each committed edge within 1/64 CSS px; same-input/same-phase
revisits do not drift. Report deep trajectory separately as intentional following.
Cover three/five layers, fine CSS, narrow/wide, fractional scale/zoom/multiline,
live/terminal following, no-input settlement and 20-frame zero idle pump/DOM work,
interaction/permissions, source/layout/scroll/intrinsic and fixed/smooth side effects.
Finite Chromium cases and controlled-report ProductionApp checks are distinguished
from unmodified live data and untested combinations.

L2 remains required. Worker iterates focused tests and exclusive single-factor
production mutations with predicted complete red sets and byte/hash restoration.
The frozen kernel gate owns text, lint/build and complete requested target replay;
Planner owns independent formal review. No Report, rules, manifest, dependency,
business policy, service, broad suite or Tier1 changes are part of this revision.

## Native playback control repair

The b032 review found a real lifecycle gap: after native pause drained the visual
pump, native play and paused currentTime seek could change paint without notifying
later slots. Earlier live/terminal tests and mutations did not cover that path.
The production spring and real Hover browser counterexamples now pin it without
resize/scroll or a caller-supplied notification. Keep the prior original-page
artifacts; that page's natural-running success did not prove drained resume.

The smallest owner is the existing native playback. Its optional visual adapter
owns controls on each Animation it created, preserving object/effect identity,
ready/finished promises, native clock semantics and analytical trajectory sampling.
There is no global prototype patch, synthetic WAAPI event, scheduler/state layer,
second solver or business-prop change. Ordinary callbacks use the same exposed
Animation from Playback.animations or DOM getAnimations(). The public native
operations run first; failed operations retain native exceptions and do not notify.

```text
create / acquire → common native startTime → running → sample changed clocks
  play / reverse / resolved startTime / nonzero rate → arm → running frames
  pause → native ready → final changed sample → no work
  currentTime seek while paused → one changed sample → no work
  paused playbackRate / updatePlaybackRate with unchanged time → no work
  pending play/pause/rate → guarded native ready → sample/arm if needed
  finish / natural finish → deduplicated final sample → no work
  native cancel / owner cancel → revoke watcher, stop frame and ready authority
  retarget / rebind / suspend / reduce / unmount → owner cancel → fresh acquire
```

Controls are observed through owned instance entries for the entire group; batches
coalesce and the shared start clock remains explicit. Only running nonzero-rate
resolved clocks continue frames. A ready promise is a one-shot native dependency,
not a polling clock. Callback presence can change on the same binding: enabling
samples current paint; disabling stops work; replacement consumes the latest
callback. Default absent callbacks install no interception or work. A cancelled
watcher cannot be revived by old controls, callback toggles or stale ready/finish.
The target identity owner continues to guard completion and source/frame rebase.

Production caller sweep: surface-presence→playSurface→playTrajectory is the only
visual consumer; Hover forwards only to later slots and Menu omits visual work.
Scalar target (Dialog/Drawer/Collapse/Rotation), SizeMotion, DepthFeedback and card
MotionCell use playSpring/playPointSpring without callback and keep their existing
native control behavior. There are no other production callers of animations or
native playback setters. Tests also exercise direct DOM getAnimations controls.
New tests are automatically discovered in existing CI jobs (once per selected
browser project); ordinary variants add only owner code/tests, no registry or pin.
L2 formal review stays with Planner. Independent original-page rerun remains a
separate result. The completed 430916 page rerun covers ordinary pause/seek/play
at 390/1440 through ProductionApp with the original single controlled GET payload;
it does not cover the new same-group borrowed controls. Its old retained-click
AbortError was localized to the test global-hold hook's unhandled derived ready
chain (original-hook 3/3 errors, preview-only/no-hold each 3/3 clean), with no abort
in the network click phase. This is neither an unknown production error nor an
f8d3 full-page runtime comparison. Keep the independent raw materials unchanged.

### Group receiver ownership repair (430839)

The 78db review reproduced two native Animations from one playTrajectory: after
both pauses drain, a.play.call(b) runs b without a notice; a's own currentTime
setter called with b also changes paint silently. These are supported same-owner
entries, not foreign owners or prototype bypass. The native-controls whole target
pins both regressions against exact 78db production before the fix.

One membership check in the existing owner now handles all six methods and three
setters after successful native delegation. The actual receiver must belong to the
already-owned animations array. A member cancel immediately revokes the whole
watcher; a foreign cancel cannot revoke it. Native this, returns, errors, getters,
effects and promise lifecycle remain native, with no bind, global registry,
prototype patch, polling or caller notification policy.

The supported surface is exposed instance controls and borrowing within one
owner. Calling those controls with a legal foreign receiver still executes the
native operation but does not notify either owner via a discovered registry.
Direct external Animation.prototype method/setter calls bypass these own entries
and are outside control observation; independent native finished delivery remains.
This boundary does not excuse the supported same-group regression.

First installation requires fresh extensible native instances, public prototype
method/accessor descriptors and no preexisting own controls. Preflight covers the
whole group before any definition. Definition errors roll back every installed
property and propagate unchanged; callback and installed state commit only on
success, so a clean retry works. Initial acquisition failure cancels its effects.
No broad monkeypatch compatibility or silent fallback is promised. Observation
costs nine own properties/closures per Animation until GC; disabling or cancelling
leaves dormant native delegates, no recurring work, and no restored old authority.
Defaults still install no control or ready observer and schedule no visual frames.

Only Motion's receiver/installation owner changes; the Hover forward DAG, physical
visual-input ruler oracle, admission proofs, source freezing and per-part rebase
remain intact. Paired ownership is verified with the production native group,
without manufacturing a second Hover part. Tests are discovered by the existing
browser project; no permanent test-name lists, CI steps or manifests are added.

## Contract and coordinates

`ui/motion/geometry.ts` declares required single/paired parts, final frames and
lift origins: ref-only visual source with explicit unavailable-center policy, surface
center, or logical paired edge. Each part's layout owner supplies its final,
unanimated fractional border box. `readSurfaceFrame` reads an unanimated
positioner, rejecting rotation, perspective, reflection and nonpositive scales.
It never infers the final frame from an owned animated surface or rounded offsets.
Frames and source points use the same document's layout viewport CSS pixels,
without page scroll or device-pixel conversion. Native effects reject foreign
documents before acquiring effects; a failed paired start cancels acquired effects.

For final frame A(u)=t+diag(sx,sy)u, sx/sy>0, local source is (a-t)/scale.
The viewport lift is V(x)=a+s(x-a)+(0,sink)+b. The shared camera owns s and sink:
depth=8(1-progress), s=800/(800+depth), sink=.07depth. Each part paints A^-1 V A,
using a fixed zero local transform origin and its own matrix translation. Paired
parts share the viewport source, progress and explicit native timeline start. Each
part owns fixed x/y compensation for its own old/new frame; a primary frame rebase
must never apply its compensation to an unchanged peer.
No consumer supplies depth, a response curve, duration or arbitrary travel distance.

Edge reveal is reserved for a later real mobile consumer: its owner resolves a
logical container edge to a physical edge, and reveals through the actual distance
to that edge without text scaling. This batch adds no unused edge driver or registry.

## Source ownership and continuity

Menu's trigger has a stable nonpressed shell separate from its placement box.
HoverPreview owns its inline trigger span and an unanimated portal positioner.
The first valid layout/open samples the stable host; no focus or pointer lookup
supplies a missing source. Unavailable first source fixes the explicit center
fallback for that cycle. Detach, hidden/empty box or ref replacement freezes the
last valid source; reversal retains it. Only a fully exited cycle releases it.

Native animation time samples progress and analytical velocity on retarget.
Origin changes never directly change CSS transform-origin. At constant frame,
with delta=a_old-a_new, compensation becomes b_new=b_old+(1-s)delta and
b'_new=b'_old-s'delta. Frame changes preserve the motion contribution in local
space: scale the old viewport contribution/derivative by newScale/oldScale,
then subtract the new uncompensated contribution/derivative. Normal layout and
scroll displacement remains visible. The shared progress and per-part fixed x/y
channels all use
`springTrajectory`, one `playTrajectory` and the shared target identity owner;
there is no additional solver. Compensation springs back to zero.

ResizeObserver and present-scoped resize, scroll and visualViewport events coalesce
measurement to one requested frame. There is no idle animation loop, global DOM
observer or source registry. Cleanup cancels pending measurement, observers,
listeners and native effects. Reduced motion settles immediately; only the current
playback may complete an exit. Missing preparation refs/frames wait for layout
without claiming exit. A dynamic
null ref/frame samples position and analytical velocity before cancelling the whole
playback and invalidating its target. Recovery resumes that sample even for the
same node, frame and open value; only the current playback may complete once.

## Paint surface identity and state ownership

Source-anchor lifetime and paint-surface lifetime are separate. Anchor replacement
freezes the last source point for the cycle; replacing a paint node binds the
current motion to that new node. Legal React keyed replacement needs no intervening
null commit and never starts a second entry solely because its element changed.
The [surface owner](../fe/web/src/ui/motion/surface-presence.ts) stores one binding:
ordered distinct actual elements with their final frames and shared origin, the
matching progress/per-part sample, target and owned playback. Equality includes
actual elements, order, document and geometry. A detached element suspends the
group even if its ref and declared frame remain nonnull.

Before any retarget, sample native progress, every part's compensation and their
analytical velocities, then cancel the whole old group and its completion authority.
Map that sample into the new binding before acquiring one shared playback:

- A retained actual element keeps its own channels when declarations reorder or
  change between single and paired. Rebase through that element's old/new frames.
- An unmatched replacement inherits the old contribution of its declaration slot
  only when that old element is absent from the entire new group. This covers
  same-count keyed primary, peer and simultaneous replacements.
- A new slot, or a slot whose old element moved elsewhere, starts with zero
  compensation and the current shared progress/velocity. It cannot duplicate a
  retained element's channels. Removed parts lose all effects and authority.

These rules preserve local motion contribution/derivative, including nonzero
compensation; ordinary layout displacement remains visible. Settled open replacement
stays settled without entry replay. Preparation with no valid frame waits; suspended
bindings retain the old snapshot/sample and invalidate their target until recovery.
All branches continue to use the existing solver and explicit document clock.

Only a current binding whose entire current surface group reaches its target can
complete exit, at most once per cycle. Completion rechecks refs and connectivity;
an obsolete queued finish or disconnected node cannot release retention. Rejected
completion invalidates the target for later recovery. Reduced/disabled motion
settles the current valid group immediately; missing refs/frames wait even then.
Present release, unmount, invalid/cross-document geometry and paired acquisition
failure cancel owned effects. Observer node sets are checked after each layout commit and rebound only for
actual node/source ownership changes, including replacements and preparation recovery.

The [real React/browser target](../fe/web/src/ui/motion/surface-presence.browser.test.tsx)
covers keyed single/paired primary/peer/both replacements at equal and changed
frames through entry, closing and reversal, plus preparation, settled replacement,
nonzero channels, order/cardinality changes, observer rebinding and stale completion.
No caller compensation, key restriction or additional registry is required.

## Primitive ownership

`open` declares the requested target, `present` permits preparation/retained paint,
and `onExited` releases only paint retention. Menu interaction remains immediate;
Hover interaction uses the post-bind admission protocol below. Menu selection restores trigger focus
before calling onSelect. Departing menu positioners and lists immediately become
inert/aria-hidden and reject activation, while roving and centered placement retain
their existing owners.
The caller's menu class still owns visual styling; the outer positioner carries
placement while the inner list carries the only lift/opacity effect.

HoverPreview retains `placePreview` as its only collision/slot owner. The approved
structural revision separates fixed reservations from natural height. Its own
portal DOM order defines a total priority: only earlier interactive reservations
are inputs. Parents do not consume descendants/later siblings; retained slots are
excluded, retained reopen preserves order, and remount appends. No global registry,
private DOM convention or business identity defines priority.

Slot coordinates/size depend on viewport, visual source, reading areas and earlier
slots. The existing source protection, viewport gap, side preference/narrow widths
and legal compact fallback remain. Intrinsic flow never changes its slot.
The tree is slot → unanimated actual wrapper → unique paint card → scrolling
viewport → intrinsic flow root. Fractional intrinsic height plus header/borders
sets actual=min(natural,slot); above content bottom-aligns, below/side top-align.
There is no constrained-height plus integer scroll-difference reconstruction and
no duplicate content tree or resource fetch. Intrinsic ResizeObserver detects
clamped growth, shrink, images and font reflow even when the outer frame is fixed.

The nearest explicit preview owner passes only semantic admission proof. Sources,
reading and avoidance bounds are read directly from their real visual DOM. The
ref-only source host isolates press feedback but may move with its parent's lift.
No inverse, source bounds injection or descendant geometry port remains. Target
frames still come from independent actual wrappers, never the painted cards.

External input → visual source → stable-order slot → exact equality/no submission
→ committed actual frame → native paint → later visual sources is the forward
path. Each new actual wrapper prepares until committed slot width and intrinsic
size agree; later frames stay valid for rebase. State updaters remain pure.
Callbacks are effect events; observers bind actual node/owner sets. Both slot
commits and current native-playback visual invalidations notify only later portals.
The latter compares Animation.currentTime and sends the terminal sample; cancellation
owns its pending frame. Paused, settled and reduced states have no recurring pump.
No notification publishes recursively or schedules its own slot. Natural size and
later collision inputs have no upstream edge; external reading/avoid must be
independent of this slot and later slots. Active forwarding is worst-case O(n²).
There is no second solver, global registry, permanent idle loop, transform-removal
transaction, containment workaround, epsilon or iteration cap.

Hover preparation and retained exit share one committed-paint capability: the
actual node with an agreed legal slot, intrinsic size and an accepted, readable
motion binding at post-bind publication. Open intent alone never grants retention. Preparation clips the measurable
actual wrapper and keeps its positioner inert until its first post-bind admission. Layout
wrappers have no CSS transition properties, including under the global
reduced-motion duration.
Closing before that commit synchronously removes the subtree and its effects,
listeners, observers and timers, through the same close path used by keyboard,
outside, parent revocation and navigation. No motion completion is expected for
unprepared content. A committed node instead retains its inert exit paint until
shared current-identity onExited releases the capability. Reopen reverses that
node; stale completion cannot release it. ArrowDown waits for the interactive
frame commit before focus handoff. Later placement rejection exits the last valid
paint. Unavailable source or binding preserves the existing shared suspension
and frozen-source rules, without another source acquisition or completion path.
First admission notifies later reservations even when geometry is unchanged, using
the existing ordered layout event. Layout commits and ordinary geometry inputs
share the same preparation acquisition path. The motion null-frame contract is
unchanged; there is no independent timeout, progress, solver, registry or hidden
retained preparation tree.

Background report reading excludes continuous preview ownership, including retained
report descendants; this is feature reading policy without a feature animation algorithm.

The old CSS appear animation is removed. Hover/leave/travel timers,
ancestor-scroll dismissal, child scrolling, nested portal navigation and focus
handoff stay synchronous with semantic visibility. Exiting positioners leave hit
testing immediately while their cards may continue painting. HoverPreview owns
an explicit nested interaction port across portals. Each child locates only its
nearest declared card owner and registers at that owner's stable trigger endpoint
by its Neige-owned DOM id. The endpoint exists before that owner can mount a
portal; retained cards keep the declaration for newly mounted descendants.
Every close revokes the owner's direct children synchronously and recursively.
Descendants combine local open with ancestor interaction, clear pending
open/leave intent when revoked, and stay closed when their parent reopens.
Registration is instance-scoped and released on child unmount; no global registry,
context allowlist exception or DOM query broadcast is added. Semantic child policy
remains in HoverPreview. Descendant exit paint
is inert/aria-hidden and excluded from collision/escape discovery immediately;
activation capture also rejects synthetic clicks on retained content. ArrowDown
uses semantic visibility to defer focus until the interactive open commit, rather
than using the retained card ref as evidence of interactive content.

## Verification and maintenance

Focused unit/browser targets live under motion, menu and hover-preview. New geometry
and surface-presence browser targets exercise production helpers/native playback.
The [Menu browser target](../fe/web/src/ui/menu/public.browser.test.tsx) covers only
UI interactions; the [app composition target](../fe/web/src/app/shell/area-selector-motion.browser.test.tsx)
uses the real AreaSelector with its domain data and styles. Geometry and reversal load-bearing
assertions, per-part rebases, suspension recovery and nested interaction receive
production single-factor mutation verification. Browser oracles include actual
elementFromPoint/underlying pointer clicks, local matrices/rects and analytical
velocity continuity; native paint still uses the existing 10ms interpolation grid.
Gate owns text, lint/build and focused replay; CI owns broader suites. Independent L2 review and
original-page evidence are arranged by Planner; this revision does not run Tier1.

Ordinary new menus change only their caller/content. New tests require no test-name
list or manifest edits. Geometry layout changes belong to each primitive; camera
calibration belongs only to motion. No permanent sweep or repeated CI check is added.

### Structural regression evidence

The app browser target mounts the real Router/track report with the original
390×844 long parent/child blocks and a populated real track-detail query cache.
The 69fb implementation reproduces `Maximum update depth exceeded`; mounting only
an isolated ReportDocument did not reproduce it and is not the red evidence.
The fixture transport supplies read-only track projection data, not a backend
write. The original live-page script additionally replaces only the report payload
of one controlled GET; it does not establish unmodified live seeded data coverage.

T1 asserts parent slot independence and a stationary exact fixed point with no
attribute commits. T2 pauses native paint and checks direct visual input → placement → CSS ruler,
including same-complete-input revisits. The binding target retains scroll, scale,
null/replacement and reduced readiness. The old paint-independent assertions are
withdrawn under 428491, not declared passing. T3 uses a delayed real file
resource, candidate width changes, clamped intrinsic growth/shrink, a native image
load and a minimal native font resource. It checks actual-frame retarget continuity
and one content/resource tree. T4 uses three real report levels at 390 for keyboard,
ancestor/child scroll, outside/timer/navigation, inert/escape/hit-test, reopen and
normal/reduced/runtime preference changes. T5 checks retained report ownership
against background reading inputs. T6 checks production placement replay and
ordered sibling/retained admission. These model-specific assertions are not
claimed as old-API red tests; the original crash is the explicit before/after test.

Ordinary new variants/tests edit their owner file only; no rules, dependency,
manifest or permanent test-name registry changes are required. Focused checks run
during implementation/mutation and replay once on the gate's frozen target.
Dialog/Drawer/vendor/mobile remain outside this adoption, and RTL AreaSelector's
pre-existing physical-translation defect remains separate.

## Approved admission protocol (420738)

Review tier remains L2: the measurement contract and recursive interaction
permission cross primitive ownership boundaries. The minimal approved extension
is the optional motion measurement callback plus its existing stable reader,
with typed unavailable/ready lifetime/revision/actual-part facts. No subscriber,
registry, general scheduler, caller compensation or second readiness policy is
added. Menu ignores the optional callback; public business props stay unchanged.

Hover owns request and committed layout candidates. Motion accepts those committed
declarations and exposes its current reader before reporting a logical edge.
Hover's post-bind publisher validates candidate/node/cycle/ancestor and that
current binding token. It makes its public parent port readable, applies the single
DOM permission gate, then focuses and notifies direct children and later slots.
New nodes start clipped/inert/hidden in the DOM mutation phase, before child
autofocus or layout effects, without active/Escape markers. Pending keeps
explicit child requests without granting permission; revoked cancels them
recursively. Suspension retains request and analytical sample; close revokes public
identity first and preserves only a historical legal exit capability.

One external input can advance a finite requested tree through local candidate,
binding and publication commits. Parent and earlier-slot dependencies form the
existing forward DAG. Exact slot equality, binding revision deduplication and
scheduled (nonrecursive) listeners also prevent self-excitation. No timeout,
iteration cap or recurring idle measurement is used to force convergence.

The admission browser target explicitly injects null only at the measurement
boundary and requests descendants through public activate callbacks; this is not
a claim that pointer input can reach an inert descendant. It checks immediate
port reads inside publication/focus, global versus local Escape, synthetic guards,
three-level single-input recovery, motion-only same-frame recovery and idle frames.
The measurement target checks callback/read ordering, reduced-value deduplication,
paired null/replacement and development StrictMode lifetimes. Existing Router
T1–T6 and real long-inline rejection oracles remain in their owning targets.
Ordinary new cases edit owner tests only; no permanent name list or extra CI step
is added. Worker focused checks iterate locally; the frozen gate replays once and
CI owns broad suites. Independent real-page review remains a separate result.

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

## Visual coordinate oracle (428491)

The per-edge 1/64 CSS px budget now compares a slot to an empty fixed CSS ruler
fed the same physical visual source, reading/avoid, earlier slots and viewport
through the only production placePreview. The oracle never writes production
slots or suppresses transforms. Deep trajectories are intentional motion, reported
separately. Three/five levels, fine CSS, gap boundaries, 390/1440, fractional positive
scale/zoom and multiline sources form a finite matrix, not exhaustive coverage.

Root progress alone is not the complete physical input: each child retains its
own native progress and compensation. In the first migrated 390 scale .75 test,
revisiting root phase 0 changed the third source y from 836.585449 to 836.562195;
the slot moved 2/64 while each CSS-ruler error was 0. The raw failure is retained
at /tmp/visual-phase-input.log. It is not same-input drift. The current test keys
revisits by complete physical inputs and phase, requires exact repeated outputs,
and checks a second held physical sample is identical at every phase. Fixed-phase
20-frame pump, DOM attributes and notices are exactly zero. No budget was widened.

## Historical direct-edge budget (ask426999)

This section records the superseded inverse semantics and incomplete handoff. It
is not the current oracle or a promise to repair inverse precision. 428491 retains
the numerical budget but changes its physical reference as described above.


The user approved **at most 1/64 viewport CSS px for each direct parent-child
edge, with no accumulation over repeated measurements/time**. This replaces
ask425277's withdrawn full-depth total absolute/span budget. It does not remove
lift, change source freezing or relax admission, identity, clocks, analytical
velocity, null recovery, focus, inert, Escape or retained-exit semantics. A deep
slot's error against a root-anchored unanimated tree is recorded separately; it
can include finite contributions from earlier edges. It is not asserted to stay
within 1/64 over every depth, nor assigned a new depth-times-budget constant.

Each direct-edge reference must use its **current actual parent frame**, current
content, authored CSS, scroll and viewport. The coordinate browser target reads
production `unpaintBounds` for the source and reading surface, then synchronously
suppresses only that parent's native transform with an important CSS declaration.
Direct unpainted DOM rectangles supply the independent source/reading reference.
The test checks that the parent's rectangle equals its unchanged unanimated actual
wrapper frame. It restores the declaration before yielding or asking production
to remeasure. It does not cancel/seek/detach effects to obtain the reference,
clone children, reconstruct CSS layout, compare inverse algorithms or subtract
two root-relative cumulative errors. Native phase control is a separate sampling
precondition: one paused, nonpending effect and a production-assigned clock per card.
This temporary CSS read is test instrumentation, not a production measurement API
or a proposed scroll/fixed-safe runtime transaction.

The same real obstacles and viewport plus direct DOM inputs go through the sole
`placePreview`. An empty fixed DOM ruler commits its output through actual CSS;
its rectangle is compared to the actual child slot. No production slot is written
to obtain this edge reference: serializing/restoring a fractional inline placement
can itself lose precision and move the parent. Source, reading and committed-slot
errors and their phase spans are reported per edge. A separate single-tree native
reduced-motion direct-DOM forward replay retains the root-relative totals, without
using the reduced-motion inverse as truth.

Three rounds of 21 distinct native phases plus repeated 0/40/80 visits compare
same-input/same-phase results to the **first** result for that phase. Fixed-phase
20-frame zero DOM attribute writes, zero layout publications and one exact rectangle
remain strict. Real CSS/scroll/layout updates have no source cache or dead zone;
updates below, at and above the paint budget must still be consumed. Tests exercise
production entry points; test-only transform suppression is outside the idle and
scroll side-effect observations. Finite Chromium coverage is not a cross-browser
precision theorem. Focused component/composition and controlled-GET ProductionApp
checks are distinct from unmodified live data and a full Cartesian product.

No runtime rounding, new representation, nominal origin, second solver, registry,
containment or compensation is required merely because a deep **total** exceeds
the withdrawn budget. A genuine direct-edge or temporal counterexample still
requires a production reproduction and owner fix, never a widened budget.

### Verification status of the edge-oracle handoff

Planner message `336e4b8d85f5494594dc5c6c210394a4` stopped this attempt for a
new structural-fix task. The first current-parent oracle version passed the three
original coordinate cases (63 samples each). The expanded 24-case target adds
21 distinct phases and more revisits, but its run was interrupted for handoff;
it has no completion verdict. No new critical production mutation, complete
focused replay, ProductionApp edge replay or passing gate is claimed. Production
runtime remains byte-for-byte unchanged. Passing a finite edge budget alone is
not sufficient acceptance for the user's latest structural-fix request.

### Historical total-budget failures (426093, 426741, 426953)

These remain historical evidence, not current promises or new edge failures.
The original three-level gaps 30/34 have direct unanimated source tops 244/350.
Parent native phase 80 gives inverse source 243.99999233748423, child slot
243.984375, then grandchild slot 349.96875: total absolute/span 2/64.
Historical raw logs remain `/tmp/bounded-geometry/result.json`,
`/tmp/coordinate-stability/result.json` and
`/tmp/local-coordinate-stability/result.json`; no files there are overwritten.

The rejected nominal-origin plus 1/1024 local-rounding candidate passed that
example but failed a five-level authored `translateY(-1/2048px)` independent DOM
replay at 3/64 total. It replaced real `B[k+1]=CSScommit(B[k]+u[k])` with a nominal
sum and erased legitimate authored commit differences. Reduced/normal inverse
self-comparison incorrectly passed. That candidate was restored byte-for-byte.

The distinct actual-origin/local-only 1/16384 experiment changed only
`unpaintBounds`, leaving Hover and frame equality unchanged. It passed the two
original tests and twelve independent five-level fractional/scale combinations,
but authored `translateY(-3/131072px)` produced total per-layer absolute errors
`[0,1/64,2/64,2/64,1/64]` and spans `[0,1/64,1/64,1/64,1/64]`.
A 1/65536 cell reintroduced the original three-level total 2/64 failure.
All experiments were restored; they are not formal mutation certificates.
Representation error `h/2` does not bound independent DOM inverse uncertainty,
and CSS commit discontinuities prevent inferring a universal total bound from
those finite successes. These failed methods are not a proof every numerical
method is impossible. Their old total-budget rejections do not certify or reject
the new current-parent direct-edge contract by themselves.


## Upstream native-host integration

The PR2854 replay onto `2729707925abec8fefbd62e8cebaad12924c3cae`
keeps upstream Astryx Layer hosting and shared dismissal. The Layer wrapper owns
native presentation and remains shown for retained paint; current post-bind
admission controls whether shared Escape may dismiss this preview. No additional
solver, native Animation interception, vendor patch or geometry fallback is added.
The reservation slot remains the ordering/placement authority inside the wrapper.
Layer's render marker lives at the nearest public native dialog root, or document
body, so nested CSS zoom does not rescale viewport placement. The unchanged
390px/0.75 zoom CSS-ruler oracle exposed the nearer-host conflict; no inverse
layout, ruler tolerance change or vendor modification resolves it.
Report hosts use upstream `data-nc-link-reading` surfaces (report and chat), while
excluding preview-owned content via `data-nc-preview-owner` during preparation,
admission and retention. Public source props and required fixture fields remain
explicit. Dialog/Drawer adoption and independent Collapse/RTL issues remain outside
this first slice.
