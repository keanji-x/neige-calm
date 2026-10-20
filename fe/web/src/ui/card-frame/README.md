# Card frame

`CardFrame` is the common visual shell for Terminal, File and other workspace
cards, conversation drawers, previews and modal card surfaces. It forwards native
div props and ref without adding a wrapper. Callers own layout, accessible
role/name, focus/keyboard behavior, motion and lifecycle; the frame owns radius,
background, border, theme-aware shadow and rounded clipping.

Raised cards all use the same material. `elevation="flat"` suppresses only the
shadow for persistent context panels; it keeps the shared radius, fill and clip.
A nested content host may suppress its outer elevation/clipping while a framed
child owns them, or use an edge-to-edge compact layout. Those adaptations belong
to the composition rather than another radius or shadow recipe.

Hover travel bridges and positioning/motion wrappers stay outside the clipped
paint node. Full document pages, menus and individual controls are not card
containers and keep their own presentation contracts.
