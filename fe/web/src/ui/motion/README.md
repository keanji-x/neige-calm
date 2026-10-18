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

`useSpringPresence` gives a declared single surface or pair one progress trajectory; the owner supplies its geometric paint mapping. Dialog and the paired Drawer/seam use `presenceDepthFrame`: restrained depth and opacity, receding into the page on exit and returning to rest on entry. Dialog retains immediate dismissal. They keep live content and velocity through reversal. Drawer geometry places both local transform origins at the same physical card/seam edge midpoint (mirrored for RTL), including stacked cards. Only the current playback may finish dismissal. Compact pages/reduced motion settle directly. Native-child transition events do not own the spring lifecycle.

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

Unit trajectory contracts verify proportional response and analytical velocity continuity without copying the solver. Production browser tests cover real long-message refill/focus, unscaled text, native interpolation, dynamic retargeting, queued finish, reversal, paired seam disposal and reduced motion. The jsdom-only native Animation stub supplies platform controls without simulating physics or paint; actual browser behavior remains authoritative.


`CollapsePresence` retains one departing flow item until its shared spring settles.
It animates a clipped grid track and opacity, preserving text size and keyed children.
Consumers own collection order, revision handling, focus recovery and final removal;
reappearance reverses the same item, while reduced motion settles without waiting.
