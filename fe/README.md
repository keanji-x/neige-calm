# Neige Calm frontend

This is an independent npm project; it deliberately is not a root workspace member.

`make dev` builds this frontend and serves it through the Docker stack at
`http://localhost:<printed-port>/next/`. The legacy frontend remains available
under `/calm/` during the cutover.

For HMR, run `make fe-dev` from the repository root, then open
`http://localhost:5180/next/`. The `/next/` prefix matches the built mount path
and also applies to LAN previews.

`@astryxdesign/core` is pinned exactly. Astryx shipped 12 releases in 5.5 weeks, 67% with breaking changes and no codemod, so upgrades must be reviewed as dedicated work.

The module-state lint rule defaults both `new` and top-level calls to rejection. Its pure-factory exceptions are source-and-export-specific. To add one, document why its returned object graph is immutable, register that exact import/API in `tools/architecture/no-module-runtime-state.mjs`, and add a passing regression fixture plus a mutation that makes only that fixture fail. TypeScript `as const` is not runtime immutability; freeze static arrays as `Object.freeze([... ] as const)`.
