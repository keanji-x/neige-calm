# Managed ACP Planner backend

Tracking: #2343. Related: #2292, #1928.

## Outcome and scope

Add ACP v1 to the existing `provider` crate and run managed OpenCode Planner sessions through the existing Harness. Transport and pure event/configuration translation are reusable; OpenCode identity is a server registration using `opencode acp`.

The first delivery covers explicit text input, native text/tool output, declared model/effort selection, stop, reset, deletion and controlled restart. It does not connect to an independently running OpenCode server, import arbitrary native sessions, observe another client's activity, implement steer/edit/compaction, or add an interactive permissions workflow. Native permission requests use **never**: reply with the ACP cancelled outcome. Ordinary `neige_user_ask` remains the existing MCP flow. Image input is explicitly refused before dispatch.

Review tier: **L2**. Process authority, durable session identity and submission recovery change. Two independent review channels must check abstraction ownership, duplicate logic and application assumptions, and converge after any behavioral fix.

## Ownership

- `provider::acp` owns bounded stdio JSON-RPC, native protocol types, capability negotiation, session configuration parsing and direct `PlannerEvent` translation. It depends on no server, Axum or database package. ACP updates do not pass through Codex notifications.
- The server owns process configuration/environment, spawn/stop, MCP credential issuance/revocation, thread seals, native binding and durable admission. The shared process primitive matches an exact backend marker and PID start time; Claude retains its existing marker key.
- The existing Harness owns queueing, transcripts and conversation lifecycle. Server construction facts live in `harness::wiring`; historical Claude module paths are direct type re-exports, not parallel contracts.
- OpenCode remains a closed, typed agent identity. This release permits it only for resumable Planner sessions. That boundary is enforced by the new database constraint; it is not an ACP protocol special case. MCP author identity remains the existing Planner session identity.

## Admission and recovery

Before dispatch, persist the original input, native session and kernel correlation under the stable input key. A pipe write is dispatch evidence, not acceptance. A lost/ambiguous response leaves an unresolved durable receipt. A repeated exact key returns the original correlation without sending another prompt; changed input or binding is refused. Fresh input is fenced until explicit reset. An unknown outcome is never reported as success.

A settled session may load its original native ID after restart. Load/configuration replay is setup traffic; it must be drained before the fresh turn's translator is installed. Native IDs and current settings belong to the registered agent. The registration/cwd digest prevents silently switching the backing profile, and the backend refuses adopting a pre-existing native binding without managed ownership.

Process cleanup and boot credential revocation select only sessions registered in `acp_managed_sessions`. The `NEIGE_ACP_PLANNER` namespace and canonical data-dir instance marker exclude independent native clients and other Neige instances. Existing migrations are unchanged; additions are 0155 and 0156 on the initial base.

## Configuration

Pass `--acp-planner-config /absolute/path/acp-planner.json`:

```json
{
  "agents": [{
    "provider": "opencode",
    "command": "/absolute/path/opencode",
    "args": ["acp"],
    "env": {
      "HOME": "/private/opencode-profile",
      "XDG_CONFIG_HOME": "/private/opencode-profile/config",
      "XDG_DATA_HOME": "/private/opencode-profile/data",
      "XDG_CACHE_HOME": "/private/opencode-profile/cache"
    },
    "expected_agent_name": "OpenCode",
    "expected_agent_version": "1.18.34"
  }]
}
```

Use an operator-owned private profile and authenticate through the native CLI outside Neige. All launch fields are required, unknown keys are refused, and the agent's initialize identity/version must match before session operations. The child inherits no ambient environment. Kernel PATH and marker/credential keys cannot be overridden by registration. Only the MCP shim gets the per-session MCP credential.

Model configuration ids and opaque values come from declared ACP `configOptions`, using standardized `model` and `thought_level` categories. Model queries never ask Codex for an ACP card. The catalog is populated by the managed session's setup; before that it is explicitly unavailable and the registered agent's own settings are inherited. A null choice keeps current native session settings. Unknown choices are judged during fresh setup before prompt dispatch, without silently substituting another model or effort.

## Acceptance

Use deterministic stdio peers through the production transport and real boot/REST/Harness entry points. Check negotiation, bounded/malformed frames, correlated requests, cancellation, two turns, text/tool output, MCP authentication, never permissions without Ask creation, exact-key recovery, unknown-outcome fencing, same-session restart, lifecycle cleanup and unsupported-control refusal. Tests must wait for actual production decisions, not merely sleep before asserting zero writes.

Mutation-verify the small set of unique safety assertions in an exclusive recoverable worktree, predict the complete red set, restore exact production bytes and prove green. Run focused provider/server/compatibility tests, text gates, contract gates, quick Rust preflight and frontend/browser checks. Generate real wire/OpenAPI artifacts after schema changes. Real Codex E2E is prohibited on the shared host. Live OpenCode acceptance is separate from deterministic peer coverage and must not be claimed unless run.

## Risk and rollback

ACP v1 does not provide native exactly-once execution or authoritative admission recovery. Keeping an unresolved submission fenced trades automatic recovery for avoiding repeated operational commands. Native optional capabilities are not assumed.

Disable the ACP registration to stop new managed turns. Do not roll an upgraded database back into an older binary that cannot decode OpenCode identity or knows fewer migrations; use a pre-upgrade backup after quiescing this instance if binary rollback is necessary. Restoring source alone is not a database rollback.
