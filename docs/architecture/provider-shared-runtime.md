## Outcome
Keep shared Codex HOME/config operations, launch environment/proxy resolution and wire-to-liveness projection in the provider crate. The server supervisor (`shared_codex_appserver::{heal_retry, daemon_state}`) owns retry timing, failure lanes and daemon typestate; calm-truth owns the `SharedDaemonState` persistence vocabulary. Server owns card identity/token authorization, persistence, takeover reconciliation and transaction/event effects. This first slice deliberately retains those kernel coordinators instead of abstracting their policy into provider.

## Acceptance
- Production server paths use provider implementations for HOME, launch environment/proxy resolution and wire-to-liveness projection.
- HOME seeding sanitization, config lock/0600 atomic writes, launch guard and toolset bump preserve behavior. Kernel MCP identity/env are required typed arguments, never hardcoded in provider.
- Server-owned retry timing, failure classification and generations retain their behavior. The calm-truth state decoder preserves the database vocabulary and rejects unknown values instead of silently treating them as idle.
- No reverse server dependency, migrations or API/schema drift. Existing production-entry integration tests remain registered.
- L2: authority/credential and config persistence boundaries. Two independent source reviews in isolated worktrees, focused provider/server tests, security mutation, text/contracts/quick Rust gates and CI.

Tracking issue: #2396.

## Scope and ownership

`provider::codex::shared` owns HOME I/O, launch environment/proxy resolution and wire-to-liveness projection. The server supervisor owns retry timing, failure lanes, status/readiness and live-connection types; calm-truth owns typed shared-daemon row decoding. The server adapter declares MCP key/environment/command and supplies settings from one snapshot. The existing server spawn, boot and replay paths consume those implementations.

The remaining supervisor is intentionally a kernel coordinator: it resolves card ownership, persists per-card tokens, validates partial durable process identities, fences deletion/replay and commits durable state. No such authority or transaction is delegated to provider in this slice. Thread release selection/serialization remains there too. Existing oversized coordinator/test files are unchanged extraction exceptions; all newly owned provider source files are below 800 lines.

## Checks

Use the registered `shared_codex_appserver` integration binary and `runtime_suite::codex_runtime_suite::shared_codex_home_case` plus server supervisor unit tests. Run provider package tests, text/contracts and quick Rust gates. Mutation verification removes only provider's production `env_clear()` and predicts the ambient-canary production-spawn regression to fail; restore and verify green in an exclusive worktree with its own target directory.
