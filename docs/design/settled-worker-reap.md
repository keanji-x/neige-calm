# Settled task worker reaping

Outcome: end the PTY session owned by a settled task attempt within one sweep
once 60 seconds have elapsed since `tasks.finished_at_ms`, on open or closed
tracks. Retain the existing IMMEDIATE claim, Codex interrupt and PTY reap.

Review tier: L2, because ending a process crosses session lifecycle authority.
The Planner arranges independent review channels.

Use `tasks`, not `current_tasks`: retries bind new cards and leave historical
attempts owning their old cards. Require Worker card role and an owning settled
attempt (`done`, `failed`, `canceled`) with a recorded finish timestamp. Exclude
any card with a dispatched, running or verifying attempt. Include terminals
with persisted exit evidence so resumable running sessions converge too.
Independently, a closed track authorizes ending running PTYs created at or
before `tracks.closed_at`, including task-less user-opened worker cards. Sessions
created after close survive that term. Both terms exclude in-flight attempts
and require Worker role; Planner sessions remain outside the set.

The 60-second grace allows the report response and provider turn to finish;
with the existing 30-second tick, normal cleanup is 60–90 seconds after finish.
Measure persisted wall-clock milliseconds, like the task writers, not terminal
creation or session activity. Missing finish timestamps fail closed for the
settled-worker term. Require
`ws.created_at_ms <= owner.finished_at_ms` so restarting a settled card does not
transfer the old attempt's cleanup authority to the new session. Recheck
this same predicate inside the claiming transaction, including a racing regate.

Source checks: `scheduler/git_delivery.rs::drive_git_delivery` operates on lease,
branch and persisted delivery, and `operation/workspace_lease/release.rs`
records delivery in the transaction settling the attempt. Gates and regates
use frozen candidates (`operation/task_verify_adapter/regate.rs`) and persisted
workspace/operation cwd (`task_verify_adapter/mod.rs`); no worker process is
needed. Terminal input's `terminal_interaction/target.rs::lifecycle_refusal`
refuses settled workers. Result receipts and accept/reject use persisted task,
report and delivery state (`mcp_server/tools/track_state.rs`); reviews are new
attempts. No post-settlement dependency on a live worker was found.

Acceptance: production sweep tests cover open/closed settled states, every
in-flight state, retry on another card, grace/missing timestamp, exited PTY,
user terminals on open tracks and Planner exclusion, closed-track teardown,
after-close survival, restart protection, claim recheck and isolated Codex
interruption (including a dead PTY, with a second sweep). Recorded terminal exits
on task-owned Worker cards follow the card, just as Terminal-card exits do: the
orphan arm retains the terminal row, including its 8 KiB raw `pty_output` tail
and exit facts for MCP reads by attempt ID. The reap retains the renderer entry
so a fresh FE/WS attach receives the final screen until the server restarts;
the ended entry cannot accept input or respawn a child. MCP reads of an ended
session return explicit
`source=persisted_exit` ANSI output and truncation/exit facts, without a live
observation ID, revision, connection or input capability. This does not
reconstruct the live RMUX model from legacy serialized output. Unrecorded orphan residue still converges through existing cleanup.
No new permanent
registry, migration, or CI check: ordinary later changes edit the owning code
and relevant tests; the existing focused replay runs once in this task's gate.

Implementation self-check: the sweeper owns lifecycle cleanup; adapters retain
no duplicate eligibility rules. Candidate enumeration and IMMEDIATE claim share
one SQL predicate; interrupt and reap retain their existing owners. Card role,
task status and task/card IDs are owned protocol facts, with no provider/plugin
identity added to generic policy. No external private session layout is inferred.
Existing test helpers mint PTYs through the production terminal-open operation;
the tests call the production sweep and claiming entry points. No released
migration changes are required. The terminal-read prompt change regenerates
`crates/calm-server/tests/goldens/mcp_tool_registry.json` (the tool-registry golden).

The deferred continuation feature #2520 conflicts with ending settled sessions:
it must explicitly resolve session lifecycle/authority before assuming a live
worker remains available. Starting/turn_pending reaping (N7) and the spawn-op
ownership proof (N6) remain follow-up work, outside this change.
