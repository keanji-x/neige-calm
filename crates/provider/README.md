# provider

One package for agent provider contracts, Codex and Claude wire protocols, and
worker liveness/exit implementations. It has no application server, Axum, or
database dependency, including in unit-test builds.

- Shared `InputItem`, `TurnModelSelection`, and `events::PlannerEvent` preserve
  the existing request and transcript shapes.
- `codex` owns the WebSocket JSON-RPC client and native error classification.
- `claude` owns stream-json decoding/encoding and pure event translation.
  `translate::ToolNames` requires the registered MCP server key and visible
  tool names; it does not hardcode the kernel's application identity.
- `worker` implements the authoritative `calm-exec::WorkerProvider` contract,
  also re-exported here. Its generic execution vocabulary stays in its owner.

The server owns authorization, credentials, process lifecycle, thread seals,
recovery, and database settlement. Cross-layer provider conformance tests live
under `calm-server/tests/provider_conformance.rs` and use the real truth harness.

Run protocol and worker unit tests on the shared host with:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked -p provider --lib --test-threads 8
```

Wire tests use in-process Unix-socket WebSocket peers and redacted stream
fixtures; no real Codex/Claude binary is needed. The socket handshakes require
an execution environment that permits local sockets.

The `fixtures` feature gates the existing account fixture constructor; the
server forwards its fixture feature explicitly. The server owns `codex-e2e`
for cross-layer conformance and E2E tests; real Codex E2E remains disabled locally.

See [the extraction design](../../docs/architecture/provider-crate.md).
