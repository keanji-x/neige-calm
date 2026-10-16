# Navigation row geometry

Public entry: `navigationRowClass` from `public.ts` returns a typed geometry
slot class. Its CSS Module stays colocated inside this UI owner. This UI primitive owns only geometry;
hosts own typography, surfaces, state, labels, actions and hit-target behavior.
It has no application, Track or Area knowledge.

Apply `row` to the positioning/inheritance root. `leading` anchors the leading
icon; `content` reserves that icon and one outer action; `twoActions` reserves
both trailing actions. `inner` and `outer` position sibling controls on the
same two centre lines. `labelStart` aligns text-only controls and the brand
with the content column. `chromeOuter` accommodates the larger control token
without changing its centre. `inlineActions` composes the same two slots in a
header. Decorative slots must not intercept navigation pointer events.

`revealActions` opts a root into reserving its second action slot on hover,
focus, an expanded descendant menu, or a no-hover pointer. Hosts continue
owning visibility and action behavior. Interactive siblings stay outside the
navigation button; no nested buttons or domain policy move into this module.

All positions derive here from `--control-h-sm`, `--control-h`,
`--navigation-label-inset` and `--space-2`. Consumers must not independently
recalculate rail padding or column offsets. Other Track variants keep their
own layout. Browser regressions compose production hosts, compare actual
text and glyph bounds, and change the tokens in both themes to catch drift.
