# Navigation row regions

`navigationRowPresentation` accepts a required presentation: `{ kind: 'caption', depth: 0 }`
or `{ kind: 'entry', depth: 0 | 1 }`. A frozen kind table selects the actual ListText
tone: captions use secondary metadata, entries use primary content. Hosts apply that
tone to their label, not merely the outer container. All interactive Level 1 and Level 2
entries share one leading icon/guide axis and one label spine; nested (`depth: 1`) child
entries indent their highlight pill clear of the vertical tree guide while keeping labels
on that shared spine.

The CSS owner defines one leading icon/guide slot, the shared label start, and one- or
two-slot trailing gutters (`oneAction`, `twoActions`).

`navigationRowClass` remains the geometry-only entry for brand and surrounding
chrome. Apply `content` to the navigation button and keep controls as siblings.
`leading`, `inner`, `outer`, `chromeOuter` and `inlineActions` share the same column
centres. `oneAction` reserves room for a resting trailing status indicator; `revealActions`
reserves the two trailing action slots on hover, focus, expanded menu or touch; hosts
retain visibility and hit-target behavior.

Production browser tests compare actual label Range bounds, complete typography,
selected-fill alignment and status positions in narrow and wide rails, retaining
keyboard, menu, hover and touch coverage.
