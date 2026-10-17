# Navigation row regions

`navigationRowPresentation` accepts a required spec: `{ kind: 'caption', depth: 0 }`
or `{ kind: 'entry', depth: 0 | 1 }`. A frozen kind table selects the actual ListText
tone: captions use secondary metadata, entries use primary content. Hosts apply that
tone to their label, not merely the outer container. Domain membership, disclosure,
menus, state and callbacks remain with their existing hosts.

The CSS owner defines one status/disclosure slot, one label column and two trailing
action slots. Depth shifts only the label and selected background by the same
relationship. Indicators remain on the global leading axis. There are no host font,
tracking or arbitrary label-offset overrides.

`navigationRowClass` remains the geometry-only entry for brand and surrounding
chrome. Apply `content` to the navigation button and keep controls as siblings.
`leading`, `inner`, `outer`, `chromeOuter` and `inlineActions` share the same column
centres. `revealActions` reserves the second action slot on hover, focus, expanded
menu or touch; hosts retain visibility and hit-target behavior.

Production browser tests compare actual label Range bounds, complete typography,
nested selected-fill indentation and global status positions in narrow and wide
rails, retaining keyboard, menu, hover and touch coverage.
