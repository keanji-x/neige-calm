## Outcome
Move shared Codex runtime foundations into the existing provider crate: shared HOME/config operations, retry/readiness/status/state contracts and live connection typestate. Server owns card identity/token authorization, persistence, takeover reconciliation and transaction/event effects. This first slice deliberately retains those kernel coordinators instead of abstracting their policy into provider.

## Acceptance
- Production server paths use provider implementations; existing public server paths remain thin adapters/reexports.
- HOME seeding sanitization, config lock/0600 atomic writes, launch guard and toolset bump preserve behavior. Kernel MCP identity/env are required typed arguments, never hardcoded in provider.
- Retry and state representations retain wire/database strings, retry timing and generations.
- No reverse server dependency, migrations or API/schema drift. Existing production-entry integration tests remain registered.
- L2: authority/credential and config persistence boundaries. Two independent source reviews in isolated worktrees, focused provider/server tests, security mutation, text/contracts/quick Rust gates and CI.

Tracking issue: #2396.

## Scope and ownership

`provider::codex::shared` owns HOME I/O, launch environment/proxy resolution, retry timing, status/readiness/live-connection types and wire-to-liveness projection. The server adapter declares MCP key/environment/command and supplies settings from one snapshot. The existing server spawn, boot and replay paths consume those implementations.

The remaining supervisor is intentionally a kernel coordinator: it resolves card ownership, persists per-card tokens, validates partial durable process identities, fences deletion/replay and commits durable state. No such authority or transaction is delegated to provider in this slice. Thread release selection/serialization remains there too. Existing oversized coordinator/test files are unchanged extraction exceptions; all newly owned provider source files are below 800 lines.

## Checks

Use the registered `shared_codex_appserver` integration binary and `runtime_suite::codex_runtime_suite::shared_codex_home_case` plus server supervisor unit tests. Run provider package tests, text/contracts and quick Rust gates. Mutation verification removes only provider's production `env_clear()` and predicts the ambient-canary production-spawn regression to fail; restore and verify green in an exclusive worktree with its own target directory.
