# Unified provider crate

## Outcome

Rename `calm-provider` to `provider` and extract the Codex protocol client and
Claude stream protocol/translation into that one package. Provider unit tests
must build without `calm-server`, Axum, SQLx, or the truth-layer test harness.
Future crates use responsibility names without the `calm-` prefix; this change
renames only the provider package.

## Ownership

- `provider::worker`: existing worker liveness/exit implementations. The
  authoritative generic `WorkerProvider` contract stays in `calm-exec` and is
  re-exported, rather than duplicated or made dependent on wire transports.
- `provider` shared contracts: input items, turn model selection, and normalized
  Planner events. Preserve their existing serialization and event meanings.
- `provider::codex`: WebSocket JSON-RPC client, native errors, notification
  parsing, wire types, and tool-name spelling. Native refusal/transport errors
  map exhaustively to the existing kernel error variants at server boundaries.
- `provider::claude`: stream-json encoding/decoding and pure event translation.
  Tool-name metadata takes the required MCP server key and declared visible
  names from the kernel; the provider package does not infer application policy.
- Server: process environment, spawn/stop, credential issuance, authorization,
  thread seals, recovery, persistence, and transaction settlement.

Move cross-layer provider conformance tests to the server integration surface;
their actual truth-layer harness remains unchanged. Move the redacted Claude
wire fixtures to the provider package and update all remaining fixture readers.
Retain server module paths as direct re-exports, without wrapper clients or
parallel implementations. Update every package reference and feature edge.

## Review and acceptance

Review tier: L1, a behavior-preserving extraction and package rename. The large
textual diff is relocation; protocol policy, process ownership, authorization,
and persistence boundaries do not change. Raise the tier if implementation
requires changing any of those boundaries.

- Preserve all moved tests; compare production/test source after normalizing
  documented imports, native error names, module layout, and MCP metadata input.
- Verify the provider normal/dev dependency graph excludes server, HTTP, and
  database crates. Exercise both backends through their existing wire fixtures.
- Verify shared contracts, refusal/transport classification, metadata-driven
  tool translation, and server callers. Mutation-check the small set of critical
  wire/security assertions in an exclusive recoverable worktree.
- Run scoped provider/server tests, cross-layer conformance, text gates, and
  Rust preflight. Keep real Codex E2E disabled on the shared host; broad tests
  remain in CI.
- Review abstraction ownership, duplicate logic, and application assumptions
  in an isolated checkout. No migration, wire schema, or generated API change
  is intended; verify OpenAPI drift.

Revert the refactor to restore the old package layout; no data conversion is
required. No performance multiplier is claimed without comparable measurements.
