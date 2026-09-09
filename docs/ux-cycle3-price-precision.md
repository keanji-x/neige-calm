# UX cycle 3: preserve quoted price precision

Status: independent source/test reviews and candidate GUI acceptance passed on
2026-09-09. Display precision remains bounded by the explicit eight-digit maximum.

Outcome: the quote, holdings source and newly configured portfolio table show
4.637 CNY for the same ETF. Seven units remain valued at 32.46 CNY. Initial
positions, trades, cash and section arrangement do not change in this cycle.

Market holdings rows preserve the provider's finite quote instead of rounding
unit prices to cents. Both complete valuation and priced-but-unconverted rows
follow this rule. Quantities, conversion rates, market values and totals retain
their existing rounding rules.

Saved layout columns gain optional `minDigits`: integers with
0 <= minDigits <= digits <= 8. Omitting it preserves the existing fixed decimal
contract (`minimumFractionDigits = maximumFractionDigits = digits`); explicit
minDigits makes digits the maximum and trims only trailing zeroes beyond that
minimum. Numeric, percent and share columns use the same generic formatter.
Text remains text. Null, non-numeric and invalid share values remain a dash.
The Rust validator, agent-visible JSON Schema and TypeScript schema agree.

Only the canonical starter portfolio's price columns opt in with digits 8 and
minDigits 2. Existing user Recipes and Reports are not rewritten. A user may
apply the same optional configuration to their current saved Report through the
normal chat/block-write path. Older kernels/readers reject the unknown optional
field rather than silently changing its semantics; templates without it remain
unchanged. No database migration or portfolio-specific renderer is introduced.

Acceptance: real market process tests compare quote/list/overlay at 4.637 in
complete and missing-FX cases; amount rounding remains cents. Generic schema
bounds and saved-block roundtrips cover absent, valid and invalid minDigits.
Browser tests render the actual starter template with source rows, and preserve
fixed-decimal output for an old template. The real schema generator/preflight
runs after contract changes; independent review and actual UI use follow the
fixed source snapshot.


First source-review checkpoint:

- Both real-process market regressions first failed with 4.64 versus 4.637,
  covering complete and unavailable-FX valuations. Both now pass, with 32.46
  CNY retained for seven units in the complete case.
- The actual starter template first failed its browser assertion at 4.64 CNY.
  It now displays 4.637 CNY and 200.00 USD; a saved fixed-two-decimal template
  still displays 4.64 CNY. Six relevant browser tests pass.
- Twelve focused Rust tests pass, including template instantiation, actual
  Assistant block edits, unchanged saved rows/value formatting, rejected invalid
  limits without document mutation, and the agent-visible kinds schema.
- Frontend lint/build and 50 related domain/Report tests pass. The production
  layout-schema function was exported and independently checked with Python's
  Draft7Validator across 96 minimum/maximum cases; all pass.
- Quick Rust preflight and final review artifacts are pending at this checkpoint.
  No existing user Recipe or Report and no running service has been changed.

Final acceptance of `e0fb6361a`:

- Both independent reviewers accepted all 17 changed files. Each independently
  ran the immutable Rust archive, 12/12 passed, including both real Market
  process paths and saved-template/Assistant block-write coverage. Their separate
  frontend, browser, schema and numeric-boundary checks also passed. The archive
  SHA256 is `ec6b9dd18259fc807d9fcc9bc2a452a25bce0a7bbfc05c9d059d2488745a5f7a`.
- Changing only the omitted-minimum fallback from `digits` to zero caused the
  one predicted legacy-format assertion to fail. Restoring the original source
  returned all 12 focused formatting tests to green.
- Final quick Rust gates, frontend lint/build, and the real `npm run gen:api`
  completed successfully. API generation ran 64 export tests and left no
  generated-file drift. Default-feature runtime files and the new frontend
  distribution were packaged together with hashes.
- The primary agent used the actual Recipes UI to create and save a new starter
  template, selected it in New track, and submitted a fictional seven-unit
  SH:510300 holding request. The initial Report body, summary, recipe identity
  and zero document revision were checked against the saved template, so an
  early model edit could not be accepted as the baseline. Both price columns
  retained `digits: 8, minDigits: 2`.
- Real quote, holdings and overlay results each contained 4.637 CNY, the browser
  showed `4.637 CNY`, and seven units remained valued at 32.46 CNY. The complete
  Report payload stayed unchanged through registration and browser reload.
  No transaction was created or brokerage order placed. This used an isolated
  candidate service and the default runtime, not a real Codex E2E suite.
- The saved Recipe page still displays layout fences as code; fixing that
  preview is a separate following UX cycle, not part of this precision change.
