# Oracle validator

This validator turns the schema and source-location disciplines in `docs/oracle/SCHEMA.md` into executable checks. It parses YAML with the `yaml` package, validates every oracle file together, and resolves every cited line against the repository rather than trusting the citation text.

Real oracle data is required to pass directly with zero violations. There is no exception baseline or migration allowlist. `source-anchor` uses the TypeScript scanner and PostCSS AST to exclude comments, requires complete identifier boundaries, and treats `.class`, `#id`, and `[attribute]` selectors as their own forms rather than stripping prefixes.

The machine check is deliberately not presented as proof of all line numbers. `ANCHOR-NONE.md` explicitly records three human-review classes: 219 statements with no extractable identifier, 98 where extracted identifiers do not occur in the cited code files (a strong suspicious signal, never silently waived), and 115 where identifiers occur in the files but not the semantic implementation ranges. Unsupported AST formats are also listed there.

Discriminating-power rerun (deterministic LCG seed `99720260803`): among 63 randomly ordered real entries, replacing `source` with the first cited file at `:1-3` was caught in 49 cases, **49/63 = 77.8%**. The remaining 14 demonstrate that lexical anchoring improves detection but does not machine-guarantee semantic line accuracy.

## Known escapes

- A cited range can contain a matching identifier while no longer supporting the statement; semantic relevance still needs review.
- Canonical owners are derived from string values in `owner-aliases.yaml`; the validator does not decide disputed ownership.
- A repository file can move while an unrelated file is placed at the old path with enough lines; content identity is outside this tool.

## Stage 2 connection

Run the real-data test in CI whenever oracle YAML or cited source files change. Stage 2 may add semantic/source-history review, but must retain the structural and line-bound checks here.

P8b must also move the mutation verification into a tracked, CI-runnable location; the current `scratchpad/mut-p8b1.sh` evidence is intentionally gitignored and cannot be replayed by CI.
