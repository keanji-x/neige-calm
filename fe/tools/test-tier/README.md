# Test-tier gate

`check-test-tier.mjs` validates the complete oracle schema, tracked fixture manifests, and Vitest
project partition on every JavaScript lint run. It fails closed for unknown migration/tier values and
malformed authoritative locations. Browser-tier migrated tests may be owned by either Vitest Browser
or the Playwright `testDir`; jsdom-tier migrated tests are owned by the Vitest `web-dom` project.

The tier/project agreement is intentionally enforced only after an entry changes to `migrated`.
At the start of phase 2 the oracle contains 1119 `pending`, 8 `skipped`, and zero `migrated` entries,
so this agreement check has no teeth until entries are flipped to `migrated`; schema and partition
checks remain active throughout.

When migrating an oracle contract, change `migration: pending` to `migration: migrated` and point
`authoritative_test` at its new `fe/` test in the same change. All 1127 repository entries currently
point `authoritative_test` at legacy `web/` tests, so changing only `migration` necessarily fails the
tier checker. All other oracle fields remain frozen.

For the retained manual mutation proofs, run `tools/test-tier/p0b-tier-mutations.sh <case>` from
`fe` (run it without an argument to list the available cases).
