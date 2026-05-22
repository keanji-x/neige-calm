/**
 * Stylelint config — slice 1 of issue #142.
 *
 * Goal: keep inline colors (`oklch(...)`, `#hex`, `rgb(...)`, `hsl(...)`) out of
 * component selectors in `src/calm.css`. The only legitimate place to write a
 * color literal is inside the token-definition blocks at the top of the file
 * (`:root { ... }` and `[data-theme="dark"] { ... }`); everywhere else, code
 * should reach for the `--surface-*` / `--text-*` / `--accent` / `--warn`
 * tokens established by issue #137.
 *
 * Scope today:
 *   - Lint target is `src/**\/*.css` (today that's only `src/calm.css`).
 *   - Token-definition blocks at the top of `calm.css` are excused via a
 *     `stylelint-disable ... / stylelint-enable` pair around `:root` and the
 *     first `[data-theme="dark"]` block.
 *   - Existing component-level literals that #137 hasn't migrated yet carry
 *     `stylelint-disable-next-line` markers in `calm.css` itself. They're
 *     technical-debt markers; future #137 slices remove them as the
 *     selectors move onto `var(--surface-*)` / `var(--text-*)` tokens.
 *   - The gate therefore catches *new* violations introduced after this PR
 *     lands without forcing the rest of #137 to land in lockstep.
 *
 * Extending in future:
 *   - Add another property to `declaration-property-value-disallowed-list` if
 *     a new vector for raw colors shows up (e.g. `caret-color`, `fill`,
 *     `stroke` — none of those are exercised in calm.css today).
 *   - To narrow the gate (e.g. start enforcing the no-color rule on `.svg`
 *     fills), add the property key with the shared `RAW_COLOR_FNS` list.
 *
 * Allowed values pass implicitly (the regexes below don't match them):
 *   `currentColor`, `transparent`, `inherit`, `unset`, `var(--token)`, named
 *   colors, and any value that doesn't start with `oklch(`/`rgb(`/`rgba(`/
 *   `hsl(`/`hsla(`. Hex is caught separately by `color-no-hex`.
 */

// Shared regex list for the disallowed-list rule. Keyed off the leading
// function name so values like `linear-gradient(..., oklch(...))` (which
// *embed* a color) still pass — token-defining gradients in `:root`
// legitimately need that shape. Component selectors are expected to use
// `var(--token)` references, which don't start with these prefixes.
const RAW_COLOR_FNS = [
  /^oklch\(/i,
  /^rgb\(/i,
  /^rgba\(/i,
  /^hsl\(/i,
  /^hsla\(/i,
];

module.exports = {
  extends: ['stylelint-config-recommended'],
  rules: {
    // Catches `color: #f00`, `box-shadow: ... #abc`, etc. Anywhere a hex
    // literal sneaks in (including non-color properties like box-shadow),
    // this rule flags it. Token-definition blocks disable this inline.
    'color-no-hex': true,

    // Property-scoped ban on raw color functions for the properties that
    // most directly carry component color decisions. `background` is
    // intentionally included even though it's a shorthand: in calm.css the
    // shorthand is almost always used as background-color, and the few
    // gradient cases that legitimately need a literal sit in the
    // `:root`/`[data-theme="dark"]` blocks (and are wrapped by the block
    // disable above the token tables in `calm.css`).
    'declaration-property-value-disallowed-list': {
      color: RAW_COLOR_FNS,
      'background-color': RAW_COLOR_FNS,
      background: RAW_COLOR_FNS,
      'border-color': RAW_COLOR_FNS,
      'border-top-color': RAW_COLOR_FNS,
      'border-right-color': RAW_COLOR_FNS,
      'border-bottom-color': RAW_COLOR_FNS,
      'border-left-color': RAW_COLOR_FNS,
      'outline-color': RAW_COLOR_FNS,
    },

    // `stylelint-config-recommended` enables a small set of "must-fix"
    // syntax-error rules. The ones below fire on intentional patterns in
    // `calm.css` and are out of scope for #142 (which is purely about
    // banning inline color literals). They're disabled here rather than
    // littered across the file with one-off `stylelint-disable` comments;
    // each can be re-enabled in a dedicated follow-up.
    //
    // - `no-descending-specificity`: dark-theme overrides
    //   (`[data-theme="dark"] .foo:hover { ... }`) routinely follow lower-
    //   specificity component selectors above. That's intentional cascade
    //   ordering, not a mistake. Re-enable once #137 finishes tokenizing
    //   the dark overrides away.
    // - `property-no-deprecated`: trips on `.sr-only { clip: rect(...) }`,
    //   the standard visually-hidden recipe that AT still understands.
    //   Migrating to `clip-path` is a separate a11y cleanup.
    // - `declaration-property-value-keyword-no-deprecated`: trips on
    //   `word-break: break-word`, used inside `.term-line` / `.diff-line`
    //   to wrap long terminal output. The modern replacement
    //   (`overflow-wrap: anywhere`) has different fallback behavior; the
    //   swap is a deliberate follow-up, not slice-1 churn.
    'no-descending-specificity': null,
    'property-no-deprecated': null,
    'declaration-property-value-keyword-no-deprecated': null,
  },
};
