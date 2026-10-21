# Interaction motion contract

Styles owns small CSS feedback tokens; UI owns the generic Motion physics and playback adapters. Domain state and focus remain synchronous in callers.

Under reduced motion, the base layer defaults undeclared CSS transitions to `none`.
This default has normal priority so component-owned transitions still complete at
the existing tiny duration, including vendor lifecycles that require `transitionend`.

## Physical motion

This is the default for new or changed interactive position, size and presence motion. Consumers reuse `SizeMotion`, `useSpringPresence` or the shared playback adapter and declare targets/geometry; they do not add local response frequencies, duration formulas, curves or spring solvers. The UI owner calibrates the shared response. Declared color-feedback, direct-manipulation and decorative-loop contracts remain intentional exceptions.

`spring.ts` uses Motion 14's `spring` solver and its analytical velocity and rest criteria. One response frequency (20/s), one damping ratio (0.72) and normalized mass define a lightly perceptible rebound. There are no per-component durations, reference distances, distance multipliers or duration bounds. Motion decides settlement.

The library trajectory is sampled once at 10ms rendering precision and played through owned browser-native effects. This is interpolation precision, not a feel setting. Native animation time samples the same model's position and velocity for a retarget. Cancellation discards native effects; identity guards reject stale completions. The adapter owns all effects directly, avoiding a vendor completion callback that writes discarded styles. Size still requires layout; this is not a compositor-only claim.

`SizeMotion` measures an intrinsic flow root that contains margins/floats. Mode changes animate real height with no text scaling, unmount or focus movement. Ordinary typing/updates and initial mount are immediate; late intrinsic updates retarget while moving. Settlement restores automatic height. Reduced motion and unmount release clipping and effects. Only a declared mode change reads old geometry in React's pre-mutation snapshot and new geometry after the commit. This narrow class lifecycle is required for a real before/after capture; ordinary child updates do not synchronously measure or keep a height cache. ResizeObserver retargets only an active transition. See the [React snapshot contract](https://react.dev/reference/react/Component#getsnapshotbeforeupdate). Children must not derive their height as a percentage of the animated host. Keep floating/portalled overlays outside transient clipping.

`useEdgePresence` supplies physical edge presence for mobile navigation and Tracks
page transitions. Dialog and ConfirmDialog use `useSurfacePresence` single lift with a
required explicit `origin` and `present=open`: dismissal cancels effects and
unmounts immediately. Drawer uses `useSurfacePresence` with the paired-only
`edge-reveal` geometry below. Compact pages/reduced motion settle directly;
native-child transition events never own the spring lifecycle.

`EdgePresenceInput.frame` is a required `SurfaceSource` for the unanimated layout
positioner; `part.element` is the exclusive paint node and the layout owner supplies
`part.readFrame`. Present-scoped ResizeObserver observes both actual nodes (once
when equal), refreshes on node or ref replacement, and releases old observations
and queued work. Observation identity includes each owner's document/window, so
same-node adoption rebinds subscriptions and cleanup uses the captured original
document/window. Reader or frame-validation errors revoke playback and completion
authority before propagating; completion validates the latest frame and both
owners' documents even before observer delivery. A null frame suspends paint
until a later layout commit restores it. Same-node declarations do not restart playback; completion reads the latest
refs and reader. Independent frame resize rebases viewport contribution and signed
velocity without requiring a paint resize, React render or window resize.

The edge browser regressions cover independent frame invalidation and replacement
of a ref declaration during close. History still uses its native deferred-close
path: the earlier Settings edge-click failure has unknown cause. Test-side native
input/close, dialog, focus and route traces improve future attribution; successful
replays do not explain that historical failure. This slice does not complete the
History/Sheet migration or close #3007.

## Press feedback

`DepthFeedback` is installed once by ProductionApp (including login/recovery). Its
UI-owned document event adapter resolves native buttons and explicit button roles,
including Astryx and portal controls, without DOM observation or business identities.
The entire button, including text and icons, moves. `depth.ts` defines one camera:
rest is zero, positive depth projects smaller and slightly lower, release returns
to zero. Text surfaces use the same projection at a restrained depth; `SizeMotion`
continues to expand real layout without scaling body text or inputs.

The initial press gives the shared spring a small inward velocity so even an
instant release has visible travel without delaying click. Every reversal samples
and inherits position and velocity; there is no dwell timer or component curve.
Additive transform keyframes preserve existing layout transforms and independent
translate/scale properties. The styles-owned Astryx theme neutralizes the published
Button hook's vendor resting/active transform so the spring is the only press source;
caller layout transforms still win through inline or component-layer styles.
Each control retains its input-source set independently of replaceable playback.
Only the last source release retargets to rest. Focus loss cancels that control's
keyboard sources; a still-valid pointer remains held. Pointer cancellation/pan/drag
removes pointer ownership without losing a held key. No inline styles are retained.
This adapter does not capture pointers, prevent defaults or change focus/click behavior. Pointer
departure/cancel, lost capture, key release and focus loss release their source.
Touch panning and drag discard pointer-only press effects immediately. Disabled, detached, reduced-motion and unmounted surfaces discard
owned effects. An animation-frame check exists only while effects are owned, to
catch disabled/removal changes without a document observer. Direct drag motion
remains caller-owned. Controls must use native disabled or aria-disabled semantics.

## Other motion

`readMotionTransition` consumes the styles-owned enter, exit, layout, disclosure, feedback and emphasis recipes for CSS/other consumers. Motion physics replaces the former size-specific distance formula. Keep simple color feedback, static controls, pointer-linear response, report emphasis and owner-defined activity/brand native loops on their existing contracts.

## Verification

Unit trajectory contracts verify proportional response and analytical velocity continuity without copying the solver. Production browser tests cover real long-message refill/focus, unscaled text, native interpolation, dynamic retargeting, queued finish, reversal, paired seam disposal and reduced motion. The jsdom-only Animation model supplies manual platform controls and a stable per-document timeline sample (0 for an active document, null without a browsing context). It does not advance a rendering clock, simulate physics or paint, or deliver frame/finish events. Paired DOM consumers share that document timeline; real browser tests remain authoritative for native time and promise lifecycles.

`CollapsePresence` retains one departing flow item until its shared spring settles.
It animates a clipped grid track and opacity, preserving text size and keyed children.
Consumers own collection order, revision handling, focus recovery and final removal;
reappearance reverses the same item, while reduced motion settles without waiting.

## Source lift and paint retention

`geometry.ts` and `useSurfacePresence` supply typed single/paired lift parts with
explicit final unanimated frames and stable anchor/center/paired-edge origins.
Menu and HoverPreview consume lift; Dialog and ConfirmDialog consume single lift;
Drawer consumes the paired-only edge reveal. Each part projects the
same viewport source into its local fractional positive scale/translation frame.
The shared progress and per-part fixed x/y compensation channels reuse
`springTrajectory`, native
`playTrajectory` and the target cancellation/identity owner. Origin/frame retargets
preserve each part's local motion contribution and analytical velocity through its
own old/new frame; primary compensation is never copied onto its peer. All parts
retain one progress, playback identity and explicit native clock, with no second
physics model. A newly declared peer joins the current shared progress/velocity
with zero compensation because it has no earlier paint contribution.

`edge-reveal` requires paired parts and a logical paired-edge origin. Its primary
final frame defines center C and logical edge O (including RTL). Using the same
camera scale s and sink, viewport displacement is D=(1-s)*(O-C)+(0,sink).
Both parts have a unit linear matrix: each divides that shared viewport D by its
own frame's positive scales. Body text and inputs never scale. Geometry owns one
displacement/analytical-derivative map for both keyframes and per-part rebasing;
old `liftKeyframe`/`rebaseLift` retain their existing lift contract. The snapshot
includes the actual map and primary center as well as every frame, so same-node
map changes cannot disappear in equality. Rebase preserves each local motion
contribution and derivative, while ordinary layout displacement remains visible.
No distance multiplier, extra clock, solver, polling or global observation is added.

The layout owner declares a final frame from an unanimated positioner. Source
snapshots freeze after detach/hidden/ref replacement until full exit; first missing
sources use the explicit center fallback. Semantic open, permitted paint and exit
completion are separate, with no business work delayed. Present-scoped observers
and coalesced geometry events dispose on release; reduced motion settles directly.
A dynamic null part ref or frame samples position/velocity, cancels the complete
playback and invalidates its target. The saved sample resumes after recovery even
when node/frame/open are unchanged; preparation nulls never claim exit. Layout
commits refresh observation so refs first acquired after preparation are observed.

Actual paint elements, declaration order, final frames, sample, target and playback
share one binding. Same-commit React keyed replacements transfer the current
position/analytical velocity after cancelling the whole old group. Retained
actual elements carry their channels through reorder/cardinality changes;
unmatched replacements inherit an absent old element's same slot; genuinely new
slots join shared progress with zero compensation. No old channel is duplicated
when its element moves to another slot. Source-anchor replacement still freezes
the cycle source; it does not freeze the paint node. Completion rechecks current
refs and connectivity, invalidates obsolete targets for recovery, and can release
retention only once for the current group. Settled open replacement stays at rest.
The [real React identity tests](surface-presence.browser.test.tsx) exercise these
transitions through the production hook and native effects.

See [the adoption design](../../../../../docs/source-surface-motion.md) for equations,
limits, maintenance cost and remaining consumers.

`useSurfacePresence` returns a stable `SurfaceBinding` with `readBinding()` only.
`SurfaceSource` carries a ref only; its real DOM rectangle supplies the visual
source, including parent paint. Source bounds injection and descendant inverse
measurement are removed. The independent unanimated wrapper still supplies the
target frame. Source freeze, per-part compensation, native identity and suspension
remain unchanged. Rotation/reflection/perspective remain rejected.

Optional `onVisualChange` belongs to the current native playback. `native-visual.ts`
installs own controls on only the owner's Animation instances when first observed;
Playback.animations and DOM getAnimations() return those same native instances.
play/pause/reverse/finish/cancel, currentTime/startTime/playbackRate setters and
updatePlaybackRate delegate to their original public native operations before
invalidation. All nine entries use the actual receiver's membership in this
playback's complete animations group, including a control/setter borrowed from
another member. Getters are the original native getters. Native return values,
brand/conversion exceptions and ready/finished promise transitions are preserved;
controls are never bound to the instance from which they were read. Member cancel
synchronously revokes observation for the whole group.

The supported observation surface is these public instance entries and same-owner
borrowing. A legal foreign receiver still receives the native operation, but does
not notify or cancel this group; the adapter does not discover another owner.
Direct calls to Animation.prototype methods/accessors bypass the instance entries
and are outside control observation. Ordinary native terminal promise delivery
still works independently. No prototype is patched or play/seek event invented.
First observation requires fresh, extensible native instances with the public
prototype descriptors and no existing own controls. The entire group is preflighted;
definition failure rolls back all installed controls and propagates the original
error, without retaining a callback or marking installation complete. A failed
initial acquisition cancels its newly created effects. There is no silent fallback.
Each observed Animation retains nine own properties and their closures until GC,
including after disable/cancel; these dormant delegates retain native semantics
but cannot restore a cancelled watcher's authority.
The frame pump compares every part's existing currentTime, coalesces a control
batch, and continues only while a native clock is running at nonzero rate. A
paused seek publishes its new paint once then stops; a paused rate change alone
is silent. Native startTime follows WAAPI semantics (a resolved start can release
the hold and run). Pending operations use native ready with an identity guard,
not recurring polling. Terminal sample publication deduplicates the frame sample.

`Playback.setOnVisualChange` switches this optional observation without replacing
an effect, promise, clock, sample or binding. useSurfacePresence reconciles it on
each commit: none→callback samples current paint, callback→none stops immediately,
and callback replacement uses the latest callback. Default consumers install no
controls, ready handlers or frame work. Cancellation (including direct native
cancel), suspension, reduction, stale replacement and unmount permanently revoke
the watcher; captured controls and pending ready cannot restart it. A new playback
has fresh authority. Settled/reduced states have no idle loop. There is no second
progress clock, binding revision, solver, subscription registry or DOM write.

Geometry observation refreshes declarations every commit but rebinds observers
only when actual nodes, source ownership or presence change. Cleanup clears the
observation binding so effect replay/reconnection can acquire the same nodes.
Measurements never write a consumer's slot. The UI layout owner decides committed-frame readiness;
the shared hook retains its existing null preparation and suspension contract.

### Committed measurement revisions

The approved optional `onMeasurementChange(BindingView)` reports binding facts;
Menu may omit it. `SurfaceBinding.readBinding()` reads that same typed view.
A ready view carries a lifetime, logical revision and the actual declared parts
with their accepted frames. Preparation, null suspension and absent paint are
unavailable, even when the old binding still retains an analytical sample.
Declarations and the stable reader update before notification. Acquire, accepted
frame/node/order changes, suspension and same-frame recovery change the revision;
paint ticks and equivalent reduced-motion Binding allocations do not. Cleanup
invalidates the lifetime without notifying an unmounted owner, and effect replay
acquires a fresh lifetime. Readers reject replaced/disconnected/null declarations.
This channel owns neither caller admission nor a second solver or subscription.

Geometry callbacks also check the accepted measurement lifetime and current
observation record before queuing or running work. A captured delivery after
disconnect/unmount cannot reacquire a still-connected external paint part, notify
an unmounted caller, or act on a reconnected StrictMode lifetime.

The 428491 visual-source contract changes nested semantics: children follow their
parent paint, including possible side changes or existing placement rejection.
Each edge is checked within 1/64 viewport CSS px against the same physical visual
inputs through production placement and an independent empty CSS ruler. Exact
same-input/phase revisits and 20-frame zero idle work are required; deep trajectories
are separate diagnostics. Root progress alone omits downstream compensation and
therefore cannot identify equal physical inputs. The coordinate target keeps
that distinction explicit. See the [design/history](../../../../../docs/source-surface-motion.md)
for the old failures and the changed oracle. Finite Chromium evidence is not a
precision theorem or exhaustive browser/CSS coverage.
