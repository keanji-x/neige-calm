# Extract the plugin runtime

## Outcome and scope

Extract the protocol and execution machinery into `crates/plugin-runtime` without
changing plugin manifests, wire messages, REST/OpenAPI, database migrations,
authorization, or persisted events. `calm-server` remains the composition root.
The runtime must not depend on `calm-server`, `calm-truth`, SQLite, or Axum.

Move manifest/config/template validation, MCP stdio and HTTP clients, CLI-query
execution, plugin process primitives, secret-file reads, tool materialization,
permissions, token primitives, and result caching. Retain lifecycle orchestration,
registry ownership, host callbacks, built-in implementations, managed installation,
UI resource registry resolution, and HTTP error mapping in the server. Re-export
moved types at existing server paths so callers share the same implementation.

Shared forge environment policy and the reserved kernel overlay namespace belong
in the IO-free `calm-types` contract layer. Process-group signaling has one owning
implementation in the runtime; server process-identity callers delegate to it.
The server owns its kernel version, never the new runtime package version.
Server-dependent prompt golden and error-mapping tests remain in the server;
protocol and connector tests move with their implementation. Existing large
modules retain their structure for this extraction to keep behavior changes out
of the move; further file splitting is a separate concern.

## Acceptance and review

- The runtime builds and tests independently and has no server/database dependency.
- Existing manifests and protocol tests still pass from their new owner.
- Production REST/MCP plugin paths retain authorization and lifecycle behavior.
- Secret isolation, bounded child output, cancellation and descendant cleanup
  retain their existing tests; mutation-check the load-bearing credential fence.
- Package contracts, text gates, compile/lint and OpenAPI drift checks pass.
- Count actual server source reduction after the extraction, including adapters.

Review tier: L2, because this is a large cross-crate move of credential and process
isolation code. Two independent review channels check the complete diff, abstraction
boundaries, duplicate policy, and application assumptions before delivery.
