# Single-task isolated Codex execution (#1501)

## Outcome and scope

An explicitly opted-in Codex task runs in a fresh private workspace through the owned
runtime and dedicated controller, reports completion or failure to its original task,
and retains its execution record and files. A restart reconciles the existing attempt;
an uncertain turn-start acknowledgement does not authorize another turn.

This slice does not implement repository input delivery, downstream artifact bindings,
post-execution repair, machine verification, Finding propagation, or supervision policy.
The previous S1 startup-recovery/history contract remains intact for existing tasks.
The initial workspace preference is empty; support for committed repository inputs is
reserved for a separate bounded change if required by the user.

## Explicit selection

Use the existing authored context and its frozen source/constraint comparison rather
than adding a new Task kind or another task-state table. The planned reserved selection is:

```json
{"neige_execution":{"version":"isolated-codex-v1","workspace":"empty"}}
```

This field belongs in a task block's `context`. Absence preserves the current route.
Presence is parsed as a strict typed contract; malformed/unsupported values cannot silently
fall back. This first route rejects dependencies, gates, non-Codex kinds and child-track
spawns that would promise capabilities it cannot deliver. The prompt states that the
workspace is new and empty. No existing project files are implied or copied.

A distinct task-bound Operation kind preserves the selected backend across restart and
configuration changes. Existing in-flight Operations are never reinterpreted based only
on the server's current configuration. Configuration is typed and opt-in; missing helper,
provider or isolation support produces a clear unavailable/failed state, not legacy fallback.

## Lifecycle boundaries

- Reuse existing task allocation, report authoring, scheduler claims and Task state writers.
  Reuse existing per-card/session identity and native Worker report authorization.
- Prepare only kernel-owned attempt directories, including the controller's required real
  `.codex` directory, before provider activation. Record the exact workspace and launch
  intent in private Operation state. Runtime/controller private state and credentials remain
  outside model-readable workspace and output.
- Use the dedicated controller with required compare-and-save checkpoints and a one-use
  TurnAdmission. Its final send must use the current Task/Operation owner/source fence,
  including initial isolated attempts; only bounded control I/O may hold that writer.
- Prefer one task-bound parked Operation for the execution lifetime. Mark actual acknowledged
  startup through the existing canonical task-start path; retain the existing completion
  report path. Observe/stop/cleanup writes remain possible after withdrawal or Task terminal
  state under exact physical and Operation ownership; they do not grant a new start.
- Persist controller journals without allowing generic phase bookkeeping to overwrite newer
  checkpoints. Resume the same recorded endpoint after restart; uncertain IssuingTurn stays
  unresolved until reconciled or explicitly failed, never automatically resubmitted.
  If a kernel restart replaces the native MCP socket, this slice stops the owned runtime,
  records failure and retains files. It does not claim seamless continuation or silently
  accept a different socket inode; a stable relay/rebinding protocol is a follow-up.
- Reuse existing WorkerFlow recording and normalizers with an exact dedicated endpoint/session
  binding. Shared-client boot, UI attachment and reaper paths must not accidentally act on
  an isolated endpoint. No second recorder or scheduler is introduced.
- Completion requires an actual authorized Worker report. A finished turn without a report,
  setup failure, or lost/failed runtime is a failure with evidence; process exit alone is not
  semantic acceptance. Stop the owned boundary and retain its truthful outcome. Preserve
  workspace/logs; no automatic artifact publication or current-repository writeback occurs.

## Acceptance

1. A real report declaration selects the isolated route; malformed or unsupported requests
   are refused and ordinary Codex tasks retain their existing route.
2. The real scheduler/Operation path launches one fake-provider task, records its identity,
   displays running after acknowledgement, accepts its normal report and retains output.
3. Error, cancellation and restart cases preserve evidence and do not repeat an uncertain
   turn or allow stale owners to issue another start. Exact cleanup still converges.
4. Existing host/private-state, command-environment, native-MCP and endpoint isolation
   invariants remain enforced; no credentials appear in public responses or command context.
5. The user explicitly authorized a local real-Codex deployment for this slice. Use
   separate loopback ports, data/workspace/private roots and owned temporary processes.
   Run a small real task through the product entry point; record the dialogue, result and
   retained file, then stop the temporary service. Routine regression suites use fakes.
6. Two independent complete reviews converge and all applicable CI passes before squash.

Implementation may refine the parked-Operation integration using existing framework hooks;
any necessary change to these authority or persistence decisions is recorded here first.

## Operator entry point

The source-deployed pilot builds `calm-server`, `calm-worker-boundary` and
`neige-mcp-stdio-shim`. Start the server with
`--isolated-codex-config /absolute/path/isolated-codex.json`. The file is a strict JSON
object; all fields below are required. Use separate, absolute directories owned by the
kernel for workspaces, provider endpoints and runtime records.

```json
{
  "workspace_root": "/srv/pilot/workspaces",
  "private_root": "/srv/pilot/endpoints",
  "runtime_root": "/srv/pilot/runtime",
  "runtime_helper": "/srv/pilot/bin/calm-worker-boundary",
  "runtime_bwrap": "/usr/bin/bwrap",
  "sandbox_bwrap": "/opt/codex/codex-resources/bwrap",
  "codex_binary": "/opt/codex/bin/codex",
  "code_mode_host_binary": "/opt/codex/bin/codex-code-mode-host",
  "mcp_shim": "/srv/pilot/bin/neige-mcp-stdio-shim",
  "provider_config": "/home/operator/.codex/config.toml",
  "provider_auth": "/home/operator/.codex/auth.json",
  "provider_environment": {},
  "connect_timeout_ms": 10000,
  "request_timeout_ms": 30000,
  "task_timeout_ms": 180000
}
```

The provider imports the configured model and authentication through the existing bounded
private-home seeding policy. Explicit proxy/CA transport settings belong in
`provider_environment`; arbitrary process-environment inheritance is not supported.

Author a normal `kind: "codex"` task with the selection in its `context`, an explicit
`no_gate_reason`, `ready: true`, and the existing user-release fields. Start its Track
through the normal lifecycle action. A new Track is a draft even when its task is ready;
a separate empty Git directory can satisfy the existing Track workspace requirement while
the isolated task still receives a fresh empty workspace.

The selected backend must be configured; absence never silently selects the shared daemon.
Stop active isolated attempts before removing this configuration. Retain its roots and
private records for recovery and investigation. Distribution packaging, repository inputs,
and a user-facing backend chooser remain follow-ups to this source-deployed pilot.

## Parked-operation schema compatibility

The integrated fake run exposed migration 0042's database CHECK: every parked operation
requires the legacy process-group artifact column. The adapter hook alone cannot satisfy it.
Migration 0099 therefore rebuilds only this CHECK, preserving all operation columns, indexes
and the permanent keyed-row deletion fence. Legacy kinds still require their exact existing
artifacts. Only the isolated kind may park without them, with a versioned private receipt
bound to its operation, attempt and card. Application validation still verifies the full
physical handle; JSON presence is not a quiescence proof.

The migration keeps foreign-key enforcement on. It saves and temporarily clears the nullable
`worker_sessions.spawn_op_id` references inside the migration transaction, rebuilds the parent
table, and restores those exact references. Upgrade tests preserve complete operation/session
rows and the keyed-delete fence, reject malformed isolated receipts, and keep the legacy
parked-artifact requirement. No released migration is edited.
