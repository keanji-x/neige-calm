# #557 vs #569 approval-gap cross-check

## TL;DR

`f8128bc6a5e235266dbb1afb201bc045a876a2de` does directly fix the observed MCP write-tool hang path: kernel MCP tools now advertise approval annotations in `tools/list`.
The fix is not "approval_policy=never auto-answers permission_request"; it prevents Codex from asking for approval for role-gated calm write tools.
`[mcp_servers.calm]` is daemon-home config in the shared Codex path, with per-thread/per-card env selecting the calling card; legacy comments still describe per-card seeding.
I found no open GitHub follow-up specifically for the #569 approval hang; there is a related closed #570 for per-card token follow-ups and a commit-note renderer follow-up.

## Q1. What changed in `crates/calm-server/src/`?

`f8128bc6` changed these calm-server source files: `mcp_server/registry.rs`, `mcp_server/transport.rs`, `mcp_server/tools/emit.rs`, `mcp_server/tools/wave_file.rs`, `mcp_server/tools/wave_report.rs`, `mcp_server/tools/wave_state.rs`.

Core fix, one sentence: MCP `tools/list` now includes per-tool `annotations`, so Codex's MCP approval classifier no longer treats calm write tools as approval-required by default.

Key anchors:
- `f8128bc6:crates/calm-server/src/mcp_server/registry.rs:186` adds `ToolDescriptor.annotations`.
- `f8128bc6:crates/calm-server/src/mcp_server/registry.rs:195` adds `read_only_annotations()`.
- `f8128bc6:crates/calm-server/src/mcp_server/registry.rs:199` adds `role_gated_write_annotations()` with `readOnlyHint=false`, `destructiveHint=false`, `openWorldHint=false`.
- `f8128bc6:crates/calm-server/src/mcp_server/transport.rs:377` serializes `annotations` into `tools/list`.
- `f8128bc6:crates/calm-server/src/mcp_server/tools/wave_state.rs:201` marks `calm.update_wave_state` with `role_gated_write_annotations()`.

## Q2. Does this directly correspond to the observed permission_request hang?

Yes, for the observed shape: shared Codex daemon, spec card, `approval_policy="never"`, workspace-write, MCP write tool.

Evidence:
- The commit message says the pre-fix `tools/list` emitted only `{name, description, inputSchema}`, missing `annotations`; Codex 0.13x defaulted absent annotations to "approval required", and `approval_policy="never"` did not auto-approve in workspace-write.
- The code fix matches that diagnosis: `transport.rs:389` inserts `annotations` when present, and write tools such as `calm.update_wave_state` opt into the non-approval hints at `wave_state.rs:201`.
- The regression test added in the same commit is exact enough: `f8128bc6:crates/calm-server/tests/codex_e2e_mcp_double_call.rs:5` boots real `SharedCodexAppServer` + real MCP server + real shim, and `:327` forces two `calm.update_wave_state` calls under `approval_policy="never"` (`:315`).
- The test asserts every `mcpToolCall` completes at `:403` and at least two starts happened at `:407`; the commit message records pre-fix `started=1 completed=0`, post-fix pass.

Important nuance: this is not proof that `permission_request` can now be surfaced/answered. It is proof the calm role-gated tools no longer cause Codex to enter that approval wait path.

Optional upstream file note: the requested `external/codex/codex-rs/hooks/src/events/permission_request.rs` path is absent in this worktree, so I did not use it as evidence.

## Q3. Where is `[mcp_servers.calm]`, and which cards can call MCP?

Shared daemon path: `[mcp_servers.calm]` is written into the shared daemon `CODEX_HOME`, not into each card's own home. `shared_codex_home.rs:18` says the shared layout is `<data_dir>/codex-home`, and `shared_codex_home.rs:84`/`:132`/`:136` write the daemon-level MCP block.

Boot path: `state.rs:707` seeds the shared `CODEX_HOME`; `state.rs:744` calls `ensure_daemon_mcp_config(...)` after spawning the kernel MCP server, and `state.rs:767` constructs `SharedCodexAppServer` with that same shared home.

Spec card shared path: `operation/spec_harness_start_adapter.rs:359` sends per-thread `shell_environment_policy.set` with `NEIGE_MCP_SOCKET` and `NEIGE_MCP_TOKEN`; `:367` starts the spec thread with `approval_policy="never"` and that config.

Worker shared path: `dispatcher.rs:1298` prepares worker spawn env; `:1307` folds worker `NEIGE_MCP_TOKEN` and `NEIGE_MCP_SOCKET`; `:1335` starts the worker via shared daemon; `:2124` sets `CODEX_HOME` to `shared_codex_appserver.status_snapshot().codex_home`.

Who can call: spec and worker cards can get MCP identity; plain cards do not. `spec_card.rs:338` says token/socket are threaded only for Spec/Worker, and `db/sqlite.rs:1380` mints MCP tokens only for `CardRole::Spec | CardRole::Worker`. Role gates still restrict tools: e.g. `calm.wave.state` allows Spec/Worker at `wave_state.rs:127`, while `calm.update_wave_state` is described as spec-only at `wave_state.rs:168` and enforced at `wave_state.rs:208`.

So the answer to "MCP only spec card?" is no: both spec and worker Codex instances can call MCP, but not the same write tools.

## Q4. Follow-up bug or unresolved?

From `gh issue list --state open --search "#569"`: no open issue matched.

`gh issue view 569` shows #569 is closed and originally filed as UI rendering (`mcpToolCall` empty bracket box), not as the approval hang; `f8128bc6` expanded the fix to include the approval gap and says `Closes #569`.

`gh issue view 570` is related but not the same hang: it covered per-card MCP token race/debug/reusable-thread follow-ups from #555/#567, and it is closed.

The only unresolved item I found in the `f8128bc6` PR/commit description is renderer coverage for other Codex v2 `ThreadItem` variants (`dynamicToolCall`, `commandExecution`, `hookPrompt`, etc.). That is a UI placeholder follow-up, not the MCP approval hang.

## 我的下一步

Bring the branch up to a main that contains `f8128bc6`, then rerun the dev-container spec-card repro using `calm.update_wave_state` and `calm.report.write`.
Expected result after the fix: `mcpToolCall` gets both started and completed rows, no `hook.codex.permission_request`, and spec runtime exits `turn_running`.
If it still hangs, capture `tools/list` from the shim path first; absence of `annotations` would mean the running binary is not actually on the fixed code.
