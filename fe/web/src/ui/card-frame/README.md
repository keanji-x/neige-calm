# Card frame

`CardFrame` is the common visual shell for conversation and workspace cards.
It forwards native div props and ref without adding a wrapper. Callers own
layout, accessible role/name, keyboard/focus behavior and lifecycle; the frame
owns rounded clipping, surface, border and theme-aware shadow. A caller may
adapt its layout on compact viewports, including an edge-to-edge surface.
