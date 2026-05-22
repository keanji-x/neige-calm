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

// Raw spacing literals (slice 1 of #165). We disallow values that start with
// a digit, plus mid-value `Npx` literals on the shorthand properties. Token
// references (`var(--space-*)`), keywords (`auto`, `inherit`), `calc(...)`,
// `clamp(...)` etc. all pass implicitly — none of them match `^\d` or the
// `\sNpx` mid-value sniff.
//
// Negative literals: most margin* sites that historically used a negative
// pixel ("tunnel adjustment") opt out via a stylelint-disable-next-line
// marker in `calm.css`. The `^-?\d` form on margin longhands documents that
// new negatives need explicit suppression, not silent passage.
//
// Shorthand properties (`padding`, `margin`, `gap`) need a mid-value sniff
// because their values are space-separated and `^\d` only checks the first
// token — without `(\s\d+px)`, `padding: var(--space-0) 6px` would slip past
// even though the second value is a raw literal.
const SPACING_BAN_LONGHAND_NONNEG = [/^\d+/];
const SPACING_BAN_LONGHAND_NEGOK = [/^-?\d+/];
const SPACING_BAN_PADDING = [/^\d+/, /^-\d+/, /(\s\d+px)/];
const SPACING_BAN_MARGIN = [/^\d+/, /^-\d+/, /(\s-?\d+px)/];
const SPACING_BAN_GAP = [/^\d+/, /\s\d+px/];

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
    //
    // Padding/margin/gap entries (slice 1 of #165) gate raw spacing
    // literals on the same properties — they must read through the
    // `var(--space-*)` vocabulary established in :root. Negative margin
    // literals opt out via a stylelint-disable-next-line marker (audit
    // found 3 sites: `.sr-only`, `.surf-clock-colon`, `.cal-agenda`); the
    // 38px outlier on `.codex-card-head` is also opted out — it tracks the
    // absolutely-positioned X button's geometry, not the rhythm grid.
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
      // Z-index scale gate (slice 4 of #165). Bans bare integer (incl.
      // negative) z-index values in component selectors — they must go
      // through one of the `--z-*` tokens declared in `:root`. The regex
      // matches `^-?\d+$` so `var(--z-modal)`, `auto`, `inherit`, `unset`
      // all pass implicitly (none start with `-` or a digit). The token-
      // definition block at the top of `calm.css` is already wrapped in
      // the existing `stylelint-disable` pair, so the `--z-*: 0|2|4|…`
      // declarations in `:root` are excused without a new exception.
      'z-index': [/^-?\d+$/],
      // Slice 3 of #165 — leading + tracking scales. The regexes match the
      // bare literal forms the migration replaced; `var(--leading-*)` /
      // `var(--tracking-*)` references are excused because stylelint sees
      // the raw `var(...)` string at parse time and the regexes don't match
      // it. `inherit` is the one remaining bare keyword and is intentionally
      // excused — it's a cascade directive, not a literal value.
      'line-height': [/^\d+(\.\d+)?$/],
      'letter-spacing': [/^-?\d+(\.\d+)?(em|rem|px)$/],

      // Radius gate — slice 2 of #165. Ban raw numeric `border-radius`
      // literals so component selectors must read through one of the
      // `--radius-*` tokens (or the back-compat `--r` alias, which itself
      // points at `--radius-xl`). `/^\d+/` catches values that *start* with
      // a digit (`6px`, `999px`, `50%`); `/\s\d+px/` catches a stray
      // `Npx` in the second corner of a shorthand (e.g. the rare
      // `border-radius: var(--radius-md) 4px;` form). Values starting
      // with `var(`, `inherit`, `unset`, `currentColor`, etc. pass.
      'border-radius': [/^\d+/, /\s\d+px/],
      'border-top-left-radius': [/^\d+/],
      'border-top-right-radius': [/^\d+/],
      'border-bottom-left-radius': [/^\d+/],
      'border-bottom-right-radius': [/^\d+/],
      padding: SPACING_BAN_PADDING,
      'padding-top': SPACING_BAN_LONGHAND_NONNEG,
      'padding-right': SPACING_BAN_LONGHAND_NONNEG,
      'padding-bottom': SPACING_BAN_LONGHAND_NONNEG,
      'padding-left': SPACING_BAN_LONGHAND_NONNEG,
      'padding-inline': SPACING_BAN_LONGHAND_NONNEG,
      'padding-block': SPACING_BAN_LONGHAND_NONNEG,
      'padding-inline-start': SPACING_BAN_LONGHAND_NONNEG,
      'padding-inline-end': SPACING_BAN_LONGHAND_NONNEG,
      'padding-block-start': SPACING_BAN_LONGHAND_NONNEG,
      'padding-block-end': SPACING_BAN_LONGHAND_NONNEG,
      margin: SPACING_BAN_MARGIN,
      'margin-top': SPACING_BAN_LONGHAND_NEGOK,
      'margin-right': SPACING_BAN_LONGHAND_NEGOK,
      'margin-bottom': SPACING_BAN_LONGHAND_NEGOK,
      'margin-left': SPACING_BAN_LONGHAND_NEGOK,
      'margin-inline': SPACING_BAN_LONGHAND_NEGOK,
      'margin-block': SPACING_BAN_LONGHAND_NEGOK,
      'margin-inline-start': SPACING_BAN_LONGHAND_NEGOK,
      'margin-inline-end': SPACING_BAN_LONGHAND_NEGOK,
      'margin-block-start': SPACING_BAN_LONGHAND_NEGOK,
      'margin-block-end': SPACING_BAN_LONGHAND_NEGOK,
      gap: SPACING_BAN_GAP,
      'row-gap': SPACING_BAN_LONGHAND_NONNEG,
      'column-gap': SPACING_BAN_LONGHAND_NONNEG,
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
