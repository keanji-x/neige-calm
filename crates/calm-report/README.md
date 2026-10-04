# calm-report

Report document operations and H1 section selection, independently testable
without building the server. Shared payload and block types remain in
`calm-types`; transactions, authorization, event publication, and task
projection remain in their existing owners.

## Extraction design

Dependency direction: `calm-server -> calm-report -> calm-types`. This crate
must not depend on `calm-server`, `calm-truth`, Axum, or SQLx. The server
re-exports the original module paths so existing callers use the same concrete
types and implementation, without wrapper methods or duplicated logic.

Review tier: L1, behavior-preserving extraction with no change to persistence
formats, write authorization, or transaction ownership. Preserve every moved
production function and existing test, apart from imports and module layout.
Production document code stays together (under 800 lines); tests live in two
separate modules to keep each file under 800 lines.

Acceptance checks:

- All existing document and section unit tests run in this package.
- Cargo's dependency graph excludes server and database crates from this
  package's test build.
- Server report callers compile and related integrated paths pass when the
  base checkout compiles.
- The complete diff passes the repository text gates and Rust preflight.

Run unit tests on the shared host with:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked -p calm-report --lib --test-threads 8
```

Changes to the exported document contract also require the affected server
tests; package boundaries are compilation boundaries, not proof that callers
cannot regress. CI retains workspace coverage.

## Further server extraction candidates

- Codex app-server client: first move protocol-owned types and errors out of
  planner/server modules; keep planner lifecycle and recovery in their owner.
- Plugin manifest and schema validation: currently imports plan validation,
  forge credential policy, and template input validation. Establish those
  ownership boundaries before extracting the core.
- Plugin MCP transport: currently embeds forge caller metadata and uses server
  role types. Separate wire transport from host-specific policy first.
- Operation and harness: defer a whole-module move; both participate in
  transaction, runtime, recovery, and cross-component state contracts.

Measure compilation, linking, and execution separately before claiming a speed
gain. This extraction reduces the scope of report-core test builds; it does
not make server integration test builds independent of their dependencies.
