# Oracle validator

This validator turns the schema and source-location disciplines in `docs/oracle/SCHEMA.md` into executable checks. It parses YAML with the `yaml` package, validates every oracle file together, and resolves every cited line against the repository rather than trusting the citation text.

Real oracle data is required to match the checked accounts exactly. `source-anchor` uses the TypeScript scanner and PostCSS AST to exclude comments, requires complete identifier boundaries, and treats `.class`, `#id`, and `[attribute]` selectors as their own forms rather than stripping prefixes.

`anchor-baseline.json` records exactly 218 known `source-anchor` debts as `(id, subtype)`. It is a shrinking baseline, not an exemption: the rule still runs and classifies every item; any new violation, changed subtype, duplicate row, malformed row, or fixed violation left in the baseline fails validation. The baseline contains no other rule. `anchor-none.yaml` is the separate identifier-granularity configuration for ordinary words misidentified by extraction and is currently empty.

`anchor-pending.json` is **not** a second baseline and grants no exemption. It is a deliberately temporary holding list for the 38 anchors that the #1148 scanner fix exposed as **never having anchored anything** — 23 whose statement identifiers occur nowhere in the cited file's real code, and 15 whose only hits are another case title, unrelated setup, or a helper definition. Each row carries the tracking issue (`#1170`) and a note saying which of the two it is; the list exists to be emptied, and when it is empty both the file and its validator branch are deleted. Four rules keep it from turning into a baseline, and every branch of every rule has its own single-violation fixture, so deleting that branch alone from `validator.ts` reds exactly its own case:

1. **Exact match, both directions.** A failure in neither account is reported as `unbaselined`; a row whose entry no longer fails that way is `stale pending`.
2. **Frozen id set — shrink-only for real.** The 38 admissible ids are enumerated as `ANCHOR_PENDING_IDS` in `validator.ts`, in source, not in the JSON. A row whose id is not in that set is rejected. Rows may therefore only be deleted; admitting any id — including trading a fixed row for a brand-new regression at an unchanged row count — costs a deliberate edit to the source file. `ANCHOR_PENDING_MAXIMUM` is kept as a row-count cap, but it is only a cap: on its own it would permit exactly that swap, so the frozen id set is the load-bearing rule.
3. **Row shape.** Subtype, issue reference (`#\d+`) and non-empty note are each checked separately, and an id may appear at most once.
4. **No double accounting.** An id present in both accounts is an error, so no debt can be counted twice or hidden behind the other list.

The 12 baseline rows whose `subtype` changed from `range-miss` to `not-in-file` in the scanner fix are not a loosening: the scanner stopped mis-reading comments as code, so those statements' identifiers turned out to occur nowhere in the cited files at all. The row count is unchanged at 218 and no id was added.

Unsupported AST locations are an explicit 36-entry / 41-location account in `docs/oracle/anchor-unsupported.yaml`, including entries whose statements have no extractable identifier. For mixed references the listed locations are not checked, while supported locations still run through the anchor rule. Any location added, removed, or changed fails validation.

Discriminating-power rerun, replacing every `source` with its first cited file at `:1-3`:

- Full-corpus view: **685/1127 = 60.8%** caught: 664 changes are caught by `source-anchor` as `unbaselined *`, and 21 by unsupported-account changes. The 218 baseline debts are separately reported and never counted in the numerator; among the 909 non-baseline entries, the same result is **685/909 = 75.4%**.
- Deterministic sample view (LCG seed `99720260803`, 63 entries): **36/63 = 57.1%** caught. The sample contains 13 baseline debts, separately reported and excluded from the numerator; among its 50 non-baseline entries, **36/50 = 72.0%** were caught.

These figures measure lexical discrimination, not semantic proof of every line number. The 909 non-baseline entries and 909 entries with extractable identifiers are different sets that happen to have the same size. Likewise, the 218 baseline debts (all of which have extractable identifiers) and the 218 statements with no extractable identifier are different sets. Identifierless statements are not exceptions or baseline rows and automatically enter anchor checking if their statement later gains a code-form identifier; their unsupported source locations are still accounted for now.

## Known escapes

- A cited range can contain a matching identifier while no longer supporting the statement; semantic relevance still needs review.
- Canonical owners are derived from string values in `owner-aliases.yaml`; the validator does not decide disputed ownership.
- A repository file can move while an unrelated file is placed at the old path with enough lines; content identity is outside this tool.
- `source-anchor` results are keyed by id, so duplicate ids retain only one anchor result; the earlier `id-unique` rule still reports the duplicate.

## Stage 2 connection

Run the real-data test in CI whenever oracle YAML or cited source files change. Stage 2 pays down the 218-item baseline through the batches in `docs/oracle/FOLLOWUPS.md`; each correction must remove its baseline row in the same change.

The mutation verification is tracked at `scratchpad/mut-p8b1.sh`; wiring it into a normal CI target remains follow-up work.
