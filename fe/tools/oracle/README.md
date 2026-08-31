# Oracle validator

The validator enforces [the oracle schema](../../../docs/oracle/SCHEMA.md) across every tracked oracle YAML file. It checks field shapes, globally unique IDs, owner/layer consistency, source and test locations, and source anchors against TypeScript and CSS ASTs.

`anchor-baseline.json` is a shrinking account of known anchor debt. New, changed, duplicated, expired, or already-fixed rows fail validation.

`anchor-pending.json` temporarily holds anchors exposed by the stricter scanner. Its allowed ID set is frozen in the validator, rows may only be removed, and an ID cannot appear in both pending and baseline accounts.

`anchor-none.yaml` records identifiers that cannot form useful anchors. `anchor-unsupported.yaml` records source shapes the AST checker cannot inspect. Both are exact accounts, not open-ended exemptions.

The checker proves structural consistency, not that a cited line still justifies the contract. Semantic relevance remains a review responsibility.

Run the real-data test whenever oracle YAML or cited source files change:

```bash
npm exec -- vitest run tools/oracle/oracle.test.ts --project platform-independent
```
