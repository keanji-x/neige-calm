# Managed ACP Planner backend

Tracking: #2343. Related: #2292, #1928.

## Outcome and scope

Add ACP v1 to the existing `provider` crate and run managed OpenCode Planner sessions through the existing Harness. Transport and pure event/configuration translation are reusable; OpenCode identity is a server registration using `opencode acp`.

The first delivery covers explicit text input, native text/tool output, declared model/effort selection, stop, reset, deletion and controlled restart. It does not connect to an independently running OpenCode server, import arbitrary native sessions, observe another client's activity, or implement steer/edit/compaction. Native permission requests follow the card's permission mode (#2348, see Permissions below). Ordinary `neige_user_ask` remains the existing MCP flow. Image input is explicitly refused before dispatch.

Review tier: **L2**. Process authority, durable session identity and submission recovery change. Two independent review channels must check abstraction ownership, duplicate logic and application assumptions, and converge after any behavioral fix.

## Ownership

- `provider::acp` owns bounded stdio JSON-RPC, native protocol types, capability negotiation, session configuration parsing and direct `PlannerEvent` translation. It depends on no server, Axum or database package. ACP updates do not pass through Codex notifications.
- The server owns process configuration/environment, spawn/stop, MCP credential issuance/revocation, thread seals, native binding and durable admission. The shared process primitive matches an exact backend marker and PID start time; Claude retains its existing marker key.
- The existing Harness owns queueing, transcripts and conversation lifecycle. Server construction facts live in `harness::wiring`; historical Claude module paths are direct type re-exports, not parallel contracts.
- OpenCode remains a closed, typed agent identity. This release permits it only for resumable Planner sessions. That boundary is enforced by the new database constraint; it is not an ACP protocol special case. MCP author identity remains the existing Planner session identity.

## Admission and recovery

Before dispatch, persist the original input, native session and kernel correlation under the stable input key. A pipe write is dispatch evidence, not acceptance. A lost/ambiguous response leaves an unresolved durable receipt. A repeated exact key returns the original correlation without sending another prompt; changed input or binding is refused. Fresh input is fenced until explicit reset. An unknown outcome is never reported as success.

A settled session may load its original native ID after restart. Load/configuration replay is setup traffic; it must be drained before the fresh turn's translator is installed. Native IDs and current settings belong to the registered agent. The registration/cwd digest prevents silently switching the backing profile, and the backend refuses adopting a pre-existing native binding without managed ownership.

The durable receipt is the settlement authority: final outcome and ordered item frames are committed before the Harness completion projection. Recovery and operation replay use one checkpoint loader after reserving the runtime and stopping its predecessor. It reads the entire current snapshot, reconciles the receipt's exact queue claims, and retains later input. Generic harvest paths apply that same retirement rule before moving queue ownership. Text and tool identities occupy disjoint namespaces, and replay retains first-seen item order.

External Harness checkpoint persistence takes the same issuance lock as queue drain; shutdown, which already owns that lock, uses the internal writer. Managed ACP sessions own their driver task and join it before recovery or replacement can continue, even when credential/process cleanup fails. Joining borrows the stored handle, so cancellation of a shutdown cannot detach a remaining receipt writer. No timed wait is treated as quiescence evidence.

The kernel registry retains the predecessor's discoverable live slot throughout shutdown. Replacement reserves only after that shutdown completes and atomically checks instance identity; removal uses the same identity check. Cancellation therefore leaves a predecessor available for the next recovery attempt to join. Removing before shutdown and immediate replacement are restricted to fixture seams.

Receipt projection is reconciled by the storage owner in one transaction. If native item rows are missing or out of order, it replaces only the receipt-owned identities in their declared order, retaining user input and other turns. The ordinary append path shares the same insertion primitive. An exact, ordered projection is left untouched, preserving row identity on repeated recovery.

Process cleanup and boot credential revocation select only sessions registered in `acp_managed_sessions`. The `NEIGE_ACP_PLANNER` namespace and canonical data-dir instance marker exclude independent native clients and other Neige instances. Existing migrations are unchanged; additions are 0156 and 0157 after main's permission-mode migration.

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

Use an operator-owned private profile and authenticate through the native CLI outside Neige. All launch fields are required, unknown keys are refused, and the agent's initialize identity/version must match before session operations. The child inherits no ambient environment. Kernel PATH and marker/credential keys cannot be overridden by registration.

Operational launches explicitly receive their active Planner card's
`NEIGE_MCP_SOCKET` and `NEIGE_MCP_TOKEN`, using the same per-turn context as the
MCP shim (#2413). This enables the kernel-served `neige` CLI without inheriting
user or daemon credentials. Readiness launches receive neither. Managed ownership
and active-carrier checks precede token issuance; launch/negotiation failures
revoke it, as do setup failure, settlement and shutdown. The operator-registered
native binary is trusted to hold the same card credential already delivered in
its MCP descriptors; the advertised agent name/version remains a compatibility
check, not cryptographic authentication of the executable. CLI commands still
use the authoritative kernel parser and role checks; native permission requests
follow the card's permission mode.

Model configuration ids and opaque values come from declared ACP `configOptions`, using standardized `model` and `thought_level` categories. Model queries never ask Codex for an ACP card. The catalog is populated by the managed session's setup; before that it is explicitly unavailable and the registered agent's own settings are inherited. A null choice keeps current native session settings. Unknown choices are judged during fresh setup before prompt dispatch, without silently substituting another model or effort.

Native MCP consumers can build model-visible output from `content` alone (#2401).
ACP sessions therefore launch the stdio shim with the explicit
`--structured-content-as-text` presentation mode. For a successful, correlated
`tools/call` reply, it adds a JSON text block containing `structuredContent` when
that exact data is not already present as text. Original summaries, warnings,
images and the structured result remain intact. Other responses, tool errors,
unsolicited frames and default shim clients retain their existing wire format.
The adaptation belongs to the MCP transport; report and terminal handlers keep
their authoritative result contract. Authentication and native permissions are
unchanged.

## Permissions

`session/request_permission` follows the permission mode the harness resolved when it issued the
turn (#2348); `provider::acp::approvals` owns the mapping. Under `never` every request is answered
`cancelled` where it is read. Under `ask` each request is a `hold` ask: one question whose title is
the tool call's kind, title and files, and whose options are the agent's option names in its order.
The chosen option is answered `selected` with that option's id; a request withdrawn or never asked
is answered `cancelled`. The process is the held-request connection. The turn ends its requests by
a fence, under the lock every answer takes: a stop sends `session/cancel` and then `cancelled` for
every pending request, and the end of the driver's read (the prompt settled, the process exited, a
protocol error) answers every pending request `cancelled` before any teardown and before the harness
is told the connection is lost. After the fence an answer writes `cancelled` or nothing: OpenCode
1.18.35 still runs a command whose `selected` answer arrives after its turn ended. Requests during
session setup are always answered `cancelled`.

Under `ask` each turn's OpenCode process is launched with
`OPENCODE_PERMISSION={"bash":"ask","edit":"ask","webfetch":"ask"}`, set explicitly by the launch
(`AcpAgentConfig::permission_env`), never inherited. OpenCode merges it over every config file, so
bash, edits (also write and patch) and fetches ask; reads outside the workspace ask as before. It
has no flag or ACP method for this. Under `never` nothing is added: OpenCode runs with the operator
profile's own `permission` configuration, which with OpenCode's defaults runs bash, edits and
fetches unsandboxed without asking, as before. OpenCode's own `always` answer approves a command
prefix for the rest of the process, which is one turn here; it is not written to disk.

## Acceptance

Use deterministic stdio peers through the production transport and real boot/REST/Harness entry points. Check negotiation, bounded/malformed frames, correlated requests, cancellation, two turns, text/tool output, MCP authentication, never permissions without Ask creation, held permission answers, cancel and exit withdrawal, exact-key recovery, unknown-outcome fencing, same-session restart, lifecycle cleanup and unsupported-control refusal. Tests must wait for actual production decisions, not merely sleep before asserting zero writes.

Mutation-verify the small set of unique safety assertions in an exclusive recoverable worktree, predict the complete red set, restore exact production bytes and prove green. Run focused provider/server/compatibility tests, text gates, contract gates, quick Rust preflight and frontend/browser checks. Generate real wire/OpenAPI artifacts after schema changes. Real Codex E2E is prohibited on the shared host. Live OpenCode acceptance is separate from deterministic peer coverage and must not be claimed unless run.

## Risk and rollback

ACP v1 does not provide native exactly-once execution or authoritative admission recovery. Keeping an unresolved submission fenced trades automatic recovery for avoiding repeated operational commands. Native optional capabilities are not assumed.

Disable the ACP registration to stop new managed turns. Do not roll an upgraded database back into an older binary that cannot decode OpenCode identity or knows fewer migrations; use a pre-upgrade backup after quiescing this instance if binary rollback is necessary. Restoring source alone is not a database rollback.
