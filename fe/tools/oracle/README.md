# Oracle validator

This validator turns the schema and source-location disciplines in `docs/oracle/SCHEMA.md` into executable checks. It parses YAML with the `yaml` package, validates every oracle file together, and resolves every cited line against the repository rather than trusting the citation text.

Real oracle data is required to match the checked accounts exactly. `source-anchor` uses the TypeScript scanner and PostCSS AST to exclude comments, requires complete identifier boundaries, and treats `.class`, `#id`, and `[attribute]` selectors as their own forms rather than stripping prefixes.

`anchor-baseline.json` records exactly 218 known `source-anchor` debts as `(id, subtype)`. It is a shrinking baseline, not an exemption: the rule still runs and classifies every item; any new violation, changed subtype, duplicate row, malformed row, or fixed violation left in the baseline fails validation. The baseline contains no other rule. `anchor-none.yaml` is the separate identifier-granularity configuration for ordinary words misidentified by extraction and is currently empty.

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

P8b must also move the mutation verification into a tracked, CI-runnable location; the current `scratchpad/mut-p8b1.sh` evidence is intentionally gitignored and cannot be replayed by CI.
