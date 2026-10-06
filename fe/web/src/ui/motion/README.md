# Interaction motion

Styles owns duration and enter/exit/layout easing tokens. CSS interactions and drawer/seam keyframes consume these tokens directly. Motion animates intrinsic sizes where CSS cannot coordinate live content.

`SizeMotion` has no business knowledge: `motionKey` identifies a presentation mode change. It skips initial mount and ordinary content updates; a mode change animates height with `--motion-medium` and `--ease-layout`, without transforms on text. Changes to intrinsic content during the transition retarget from the painted height. Reversal interrupts the old animation. Unmount and reduced-motion changes stop it and release inline styles. Children stay mounted and interactive throughout.

Edit owns its state, draft and focus. It calls the edit action synchronously and passes the editing mode as the size trigger. No document View Transition, shared-element identity, or flying text is involved.
