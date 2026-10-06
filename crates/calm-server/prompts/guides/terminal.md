# Driving a terminal

The `neige_terminal_*` descriptions define the switches.

1. Find exact tool names (`neige_terminal_show`, `open`, `read`, `control`, `input`) once and reuse them.
2. For a task's worker, use `neige_terminal_show` with the current `attempt_id` from `neige_task_ls`; read, control and input with that same id. Never open a substitute terminal. For a codex worker, wait for its task to settle before planning its successor.
3. Open your own terminal with `neige_terminal_open`, a stable `idempotency_key`, and `claim: true` when operating it.
4. Start Claude in that open: `program` `claude --settings "$NEIGE_CLAUDE_SETTINGS"` (`ccode` if requested; keep the proxy), `claim: true`, `wait_for: "text"`, `wait_text: ["trust this folder","❯"]`.
5. Send Claude a prompt: `submit` with `read: true`, `wait_for: "signal"`, `wait_text_absent: ["esc to interrupt"]`. Read the returned state: `stop` means finished; `permission_request` or a notification needs your input.
6. Shell command: `submit`, `read: true`, `wait_for: "change"`, `wait_ms: 15000`. For TUI readiness use `wait_for: "text"` and `wait_text` naming its screen. Before dispatching preview self-checks, wait for the server's ready text or read to confirm readiness and check its health. Output change alone is not readiness; if startup failed or exited, fix it first.
7. Inspect a draft before Enter. In Claude Code, `Ctrl+J` inserts a newline.
8. Fix an unsubmitted draft with one bounded `sequence`, `read: true`, `wait_for: "change"`; check readback, then submit.
9. Use `claim: true` on a scenario's first input and `release: true` on its last. After human takeover, do not reclaim. Release when done.
10. For Claude Code's `/rewind`, use the real menu and verify the restored conversation, never a new session, transcript edit or script.
