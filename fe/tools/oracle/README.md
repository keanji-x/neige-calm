# Oracle validator

This validator turns the schema and source-location disciplines in `docs/oracle/SCHEMA.md` into executable checks. It parses YAML with the `yaml` package, validates every oracle file together, and resolves every cited line against the repository rather than trusting the citation text.

Real oracle data is required to pass directly with zero violations. There is no exception baseline or migration allowlist.
In addition to structural location checks, `source-anchor` requires at least one statement code identifier to occur inside the cited source ranges whenever such an identifier can be extracted and exists in a cited file. Statements with no extractable identifier are recorded in `docs/oracle/ANCHOR-NONE.md` for human differential review.

## Known escapes

- A cited range can contain a matching identifier while no longer supporting the statement; semantic relevance still needs review.
- Canonical owners are derived from string values in `owner-aliases.yaml`; the validator does not decide disputed ownership.
- A repository file can move while an unrelated file is placed at the old path with enough lines; content identity is outside this tool.

## Stage 2 connection

Run the real-data test in CI whenever oracle YAML or cited source files change. Stage 2 may add semantic/source-history review, but must retain the structural and line-bound checks here.

P8b must also move the mutation verification into a tracked, CI-runnable location; the current `scratchpad/mut-p8a.sh` evidence is intentionally gitignored and cannot be replayed by CI.
