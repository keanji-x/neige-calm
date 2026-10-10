# Driving a terminal

The `neige_terminal_*` descriptions define switches.

1. Discover `neige_terminal_show`, `open`, `read`, `control`, `input` once.
2. For a worker, use `neige_terminal_show` and its current `attempt_id` from `neige_task_ls` for read/control/input. No substitute terminal; correct codex/claude workers via `message`.
3. Open your terminal with `neige_terminal_open`, stable `idempotency_key`, `claim: true` to operate it.
4. To start Claude: `program` `claude --settings "$NEIGE_CLAUDE_SETTINGS"` (`ccode` if requested; keep proxy), `claim: true`, `wait_for: "text"`, `wait_text: ["trust this folder","❯"]`.
5. Prompt Claude: `submit`, `read: true`, `wait_for: "signal"`, `wait_text_absent: ["esc to interrupt"]`. `stop` means finished; `permission_request`/notifications need input.
6. Shell: `submit`, `read: true`, `wait_for: "change"`, `wait_ms: 15000`. TUI: `wait_for: "text"` with `wait_text` naming its screen. Before preview checks, confirm server readiness and health; output change is insufficient. Fix failed/exited startup first.
7. Inspect drafts before Enter (`Ctrl+J` for Claude newlines). Repair with one bounded `sequence`, `read: true`, `wait_for: "change"`; check readback, then submit.
8. Claim on first input, release on last. Never reclaim after human takeover.
9. Claude `/rewind`: use the real menu and verify restoration, never replace the session or edit transcripts.

## Blocked web tests

If sandbox/permissions block a web test, Planner reruns the exact command in a terminal card at the same cwd. Keep command, cwd and error; distinguish missing browsers from permission failures, fix the prerequisite, then rerun. Read output and exit code; separate assertions from environment failures, never pass unrun tests. Roles unable to open cards hand evidence and unrun checks to Planner; do not widen permissions.
