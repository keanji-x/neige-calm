# Interaction motion contract

## Ownership and entry points

- `styles` owns duration and easing tokens, shared by both themes.
- `ui/motion/transition.ts` exports `MotionIntent`, `MotionTransition` and `readMotionTransition(surface, intent)`. It converts the surface's CSS tokens into Motion options. Enter uses medium/enter, exit uses snappy/exit, and layout uses medium/layout. Millisecond and second duration overrides are supported; absent or invalid tokens throw rather than silently adopting library defaults.
- `ui/motion/size.tsx` exports `SizeMotion({ motionKey, children })`, an interruptible intrinsic-height primitive. The caller supplies a stable string or boolean presentation mode. No domain identifiers, field selectors, or backend state enter the primitive.

CSS consumers use the same token pairs directly; hover/color changes remain CSS transitions. Motion coordinates live layout where CSS alone cannot do so. Decorative repeating SVG animation remains owned by brand components. New consumers extend this owner rather than introducing per-feature animation runners.

## SizeMotion behavior

Initial mount and ordinary content updates are immediate. A mode change animates height with the layout recipe. The measuring wrapper establishes a flow root, so child margins and floats contribute to the measured height. Children remain mounted, unscaled and interactive throughout; the primitive adds no role, label, keyboard handler, or focus movement.

Intrinsic size changes during travel retarget from the painted height. A reversed mode change interrupts the previous animation. Generation ownership rejects stale frame writes. Finish, unmount, and a change to reduced motion release inline height and clipping. Reduced motion also bypasses new transitions. After settlement, height is automatic again.

Children must size intrinsically; do not derive their height as a percentage of the animated host. Keep floating/portalled overlays outside transient clipping. Feature owners update domain state and focus synchronously, independently of motion completion. The caller retains DOM identity and supplies accessibility semantics.

## First consumers and validation

Chat edit calls its action immediately, changes the composer's mode key, and marks the original message in place. Drawer and seam use matching enter/exit token pairs. These consumers do not impose chat policy on the primitive.

`transition.test.ts` pins token conversion, local overrides and explicit rejection. `size.browser.test.tsx` exercises generic panels with margins, floats, live controls, dynamic content, StrictMode disposal and reduced motion. Chat edit tests additionally cover immediate refill/focus and rapid cancellation/re-entry through the production action. Future primitives should bring equivalent standalone lifecycle coverage before migrating more surfaces.
