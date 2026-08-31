# Test-tier gate

`check-test-tier.mjs` validates the complete oracle schema, tracked fixture manifests, and Vitest
project partition on every JavaScript lint run. It fails closed for unknown migration/tier values and
malformed authoritative locations. A tier states the minimum capability a test needs; the owning
project may provide more capability, but never less:

| `test_tier` | Allowed project |
| --- | --- |
| `browser` | `browser`, `browser-coarse`, or Playwright |
| `jsdom` | `web-dom`, `browser`, or `browser-coarse` |
| `static` / `none` | Any project |

`browser-coarse` is a second Chromium project whose Playwright context is created with
`hasTouch` + `isMobile`, so its page reports `pointer: coarse` from first paint. It exists because
CDP touch emulation cannot be turned back off — disabling it leaves the page at `pointer: none`,
where neither branch of a pointer-split stylesheet matches, and Vitest browser mode reuses one page
per project. It is therefore a capability *variant* of `browser`, not a tier of its own: no
`test_tier` value requires it, and it is accepted wherever `browser` is. Its files are named
`*.coarse.browser.test.{ts,tsx}`, which the `browser` project must exclude — that suffix is also a
`*.browser.test.*`, so without the exclude the partition check reports the file in two projects.
Both projects run from `npm run test:browser`, which is what the "every configured project is
reachable from a test script" case in `vitest-projects.test.ts` binds.

The tier/project agreement is enforced after an entry changes to `migrated`.
Schema and partition checks remain active for pending and skipped entries.

When migrating an oracle contract, change `migration: pending` to `migration: migrated` and point
`authoritative_test` at its current `fe/` test in the same change. All other oracle fields remain frozen.

For the retained manual mutation proofs, run `tools/test-tier/p0b-tier-mutations.sh <case>` from
`fe` (run it without an argument to list the available cases).
