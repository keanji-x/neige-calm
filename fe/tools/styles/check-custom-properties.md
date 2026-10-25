# CSS custom property existence gate

`npm run lint:css` runs `check-custom-properties.mjs`. It checks declaration
values in the CSS imported from `web/src/styles/entry.css` and recursively
found CSS Modules. Definitions also come from production `.ts`/`.tsx` under
`web/src` and `core`: any string literal or template without interpolation whose
entire value is `--<ident>` counts, including quoted object property names and
`setProperty` arguments. Imported vendor CSS contributes definitions only. CSS
quoted strings, including escaped quotes and backslashes, are literal text;
`var()` inside them is ignored. A reference with an explicit fallback is
accepted, while nested fallback references are checked independently.

## Silent passes and limits

- TS/TSX uses are never checked, including inline styles and CodeMirror theme
  objects. Their undefined references silently pass; browser coverage must check
  them. A string containing only `var(--x)` does not define `--x`.
- Any exact-name TS/TSX literal counts as a definition, even unrelated text or
  code that never runs. This deliberately permits CSS uses of the same name
  without proving that a style is actually assigned. Comments, partial-name
  strings and interpolated templates do not contribute definitions. JS/JSX
  definitions and uses are outside coverage.
- Plain CSS outside the entry import graph and CSS Modules is not collected.
  Test source paths are excluded from recursive discovery.
- This checks global name existence, not cascade, selector scope, runtime
  execution, cycles or value types. An unrelated declaration can satisfy a use.
- Dynamic names, concatenated names and CSS-escaped identifiers are outside
  the lightweight static reference syntax. This is not a complete CSS validator.

The independent fixtures cover both quote forms, escaped quotes, escaped
backslashes, comments, string-adjacent undefined references, direct fallbacks
and nested fallback violations. Each negative fixture contains one violation;
the test runs the real CLI with each to assert exit status 1. These fixtures
prove the supported CSS boundary. Independent source fixtures cover property
names, `setProperty`, double-quoted literals, static templates and TS files.
Source fixtures are stored as `.txt` assets and parsed with their logical TS/TSX
file names, so the tools build does not compile fixture snippets.
Single-violation CLI probes confirm that use-only strings, comments, partial
literals and interpolated templates cannot satisfy a CSS use; excluded test source
definitions cannot satisfy one either. These do not prove runtime TS/TSX styles.

Ordinary new CSS Modules require no registration. New supported syntax needs a
checker change and an independent fixture in `fixtures/custom-properties`, with
its case in `check-custom-properties.test.ts`. The production scan runs once
per `lint:css` invocation; this test target also scans the production tree and
runs isolated CLI probes. No runtime token allowlist is maintained here.
