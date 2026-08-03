# Oracle validator

This validator turns the schema and source-location disciplines in `docs/oracle/SCHEMA.md` into executable checks. It parses YAML with the `yaml` package, validates every oracle file together, and resolves every cited line against the repository rather than trusting the citation text.

`baseline.json` is temporary migration scaffolding. The current oracle data is known to be red, so the test requires the complete ordered `id + rule` set and its total count to match exactly. P8b owns correcting the data, reducing this baseline to zero, and deleting the baseline mechanism.

## Known escapes

- A cited line can exist while no longer supporting the statement; semantic relevance still needs review.
- Canonical owners are derived from string values in `owner-aliases.yaml`; the validator does not decide disputed ownership.
- A repository file can move while an unrelated file is placed at the old path with enough lines; content identity is outside this tool.

## Stage 2 connection

Run the real-data test in CI whenever oracle YAML or cited source files change. Stage 2 may add semantic/source-history review, but must retain the structural and line-bound checks here.
