/**
 * The one breakpoint. Below it the rail collapses to an icon strip (§7.1).
 *
 * Media queries cannot read custom properties, so this cannot be a token. CSS
 * hand-writes `@media (width < 60rem)` and points back here in a comment; a node
 * audit asserts every `@media (width` literal in the repo equals this number.
 */
export const RAIL_COLLAPSE_REM = 60;
