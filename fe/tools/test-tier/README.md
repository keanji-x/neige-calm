# Test-tier gate

`check-test-tier.mjs` validates the complete oracle schema, tracked fixture manifests, and Vitest
project partition on every JavaScript lint run. It fails closed for unknown migration/tier values and
malformed authoritative locations. Browser-tier migrated tests may be owned by either Vitest Browser
or the Playwright `testDir`.

The tier/project agreement is intentionally enforced only after an entry changes to `migrated`.
At the start of phase 2 the oracle contains 1119 `pending`, 8 `skipped`, and zero `migrated` entries,
so this agreement check has no teeth until entries are flipped to `migrated`; schema and partition
checks remain active throughout.

For the retained manual mutation proofs, run `tools/test-tier/p0b-tier-mutations.sh <case>` from
`fe` (run it without an argument to list the available cases).
