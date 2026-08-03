# Oracle validator

This validator turns the schema and source-location disciplines in `docs/oracle/SCHEMA.md` into executable checks. It parses YAML with the `yaml` package, validates every oracle file together, and resolves every cited line against the repository rather than trusting the citation text.

`baseline.json` is temporary migration scaffolding. The current oracle data is known to be red, so the test requires the complete ordered `id + rule` set and its total count to match exactly. P8b owns correcting the data, reducing this baseline to zero, and deleting the baseline mechanism.

After accepting the location separators present in the corpus, the exact baseline is 66 violations:
54 `source-location` (50 missing line numbers, 3 out-of-range references, 1 malformed prose reference),
4 `authoritative-test-location` (all missing line numbers), 6 `id-kind-prefix`, and 2 `skipped-owner`.

## Known escapes

- A cited line can exist while no longer supporting the statement; semantic relevance still needs review.
- Canonical owners are derived from string values in `owner-aliases.yaml`; the validator does not decide disputed ownership.
- A repository file can move while an unrelated file is placed at the old path with enough lines; content identity is outside this tool.

## Stage 2 connection

Run the real-data test in CI whenever oracle YAML or cited source files change. Stage 2 may add semantic/source-history review, but must retain the structural and line-bound checks here.

P8b must also move the mutation verification into a tracked, CI-runnable location; the current `scratchpad/mut-p8a.sh` evidence is intentionally gitignored and cannot be replayed by CI.
