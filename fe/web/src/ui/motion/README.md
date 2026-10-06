# Interaction motion contract

Styles owns small CSS feedback tokens; UI owns the generic Motion physics and playback adapters. Domain state and focus remain synchronous in callers.

## Physical motion

This is the default for new or changed interactive position, size and presence motion. Consumers reuse `SizeMotion`, `useSpringPresence` or the shared playback adapter and declare targets/geometry; they do not add local response frequencies, duration formulas, curves or spring solvers. The UI owner calibrates the shared response. Declared color-feedback, direct-manipulation and decorative-loop contracts remain intentional exceptions.

`spring.ts` uses Motion 14's `spring` solver and its analytical velocity and rest criteria. One response frequency (20/s) derives critical damping with normalized mass. There are no per-component durations, reference distances, distance multipliers or duration bounds. Motion decides settlement.

The library trajectory is sampled once at 10ms rendering precision and played through owned browser-native effects. This is interpolation precision, not a feel setting. Native animation time samples the same model's position and velocity for a retarget. Cancellation discards native effects; identity guards reject stale completions. The adapter owns all effects directly, avoiding a vendor completion callback that writes discarded styles. Size still requires layout; this is not a compositor-only claim.

`SizeMotion` measures an intrinsic flow root that contains margins/floats. Mode changes animate real height with no text scaling, unmount or focus movement. Ordinary typing/updates and initial mount are immediate; late intrinsic updates retarget while moving. Settlement restores automatic height. Reduced motion and unmount release clipping and effects. Children must not derive their height as a percentage of the animated host. Keep floating/portalled overlays outside transient clipping.

`useSpringPresence` gives paired surfaces one progress trajectory. Drawer and seam map it to opacity and a spacing-token lift. They keep live content and velocity through reversal. Only the current playback may finish dismissal. Compact pages/reduced motion settle directly. Native-child transition events do not own the spring lifecycle.

## Other motion

`readMotionTransition` consumes the styles-owned enter, exit, layout, disclosure, feedback and emphasis recipes for CSS/other consumers. Motion physics replaces the former size-specific distance formula. Keep simple color feedback, static controls, pointer-linear response, report emphasis and owner-defined activity/brand native loops on their existing contracts.

## Verification

Unit trajectory contracts verify proportional response and analytical velocity continuity without copying the solver. Production browser tests cover real long-message refill/focus, unscaled text, native interpolation, dynamic retargeting, queued finish, reversal, paired seam disposal and reduced motion. The jsdom-only native Animation stub supplies platform controls without simulating physics or paint; actual browser behavior remains authoritative.
