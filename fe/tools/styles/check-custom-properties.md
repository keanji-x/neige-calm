# CSS custom property existence gate

`npm run lint:css` runs `check-custom-properties.mjs`. It checks declaration
values in the CSS imported from `web/src/styles/entry.css` and recursively
found CSS Modules. Imported vendor CSS contributes definitions only. CSS
quoted strings, including escaped quotes and backslashes, are literal text;
`var()` inside them is ignored. A reference with an explicit fallback is
accepted, while nested fallback references are checked independently.

## Silent passes and limits

- TSX/JS inline styles, interpolated template strings, `setProperty` calls and
  CodeMirror theme objects are outside coverage, for definitions and uses.
  Their undefined references silently pass; browser coverage must check them.
  Conversely, a CSS reference whose only definition is in JS/TSX is reported.
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
prove the supported CSS boundary; they do not prove omitted JS/TSX styles.

Ordinary new CSS Modules require no registration. New supported syntax needs a
checker change and an independent fixture in `fixtures/custom-properties`, with
its case in `check-custom-properties.test.ts`. The production scan runs once
per `lint:css` invocation; this test target also scans the production tree and
runs isolated CLI probes. No runtime token allowlist is maintained here.
