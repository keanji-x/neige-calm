# Typography scope

`typographyScopeClassName` is a class-only adapter for an explicit inherited font-family scope.
Compose it on the same element that overrides a family, or on its descendant.
There is no component, extra DOM, runtime measurement, or application identity.

The theme generator writes the CSS from the same role function as the root tokens.
Only family-dependent font shorthands, consumed family projections, and the existing
font aliases are recomputed. Spacing, colors and unrelated type dimensions inherit.
