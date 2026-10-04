# Driving a terminal

The `neige.terminal.*` tool descriptions define every switch. This is the order to use them in.

1. Find the exact tool names (`neige.terminal.resolve`, `open`, `observe`, `control`, `input`) once, then reuse them. Avoid broad, overlapping tool searches.
2. A task's worker terminal: call `neige.terminal.resolve` with the current `attempt_id` from `neige.plan.list`, then observe, control and input with that same `attempt_id`. Never open a substitute terminal for a task. For a codex task's worker, wait for its task to settle, then plan its successor.
3. Your own terminal: `neige.terminal.open` with a stable `request_id`, and `claim: true` when you will operate it.
4. Start Claude in that one open: `program` `claude --settings "$NEIGE_CLAUDE_SETTINGS"` (`ccode` when the user asks for it; keep the configured proxy), `claim: true`, `wait_for: "text"` with `wait_text` `["trust this folder","❯"]`.
5. Send Claude a prompt: `submit` with `observe: true`, `wait_for: "signal"` and `wait_text_absent: ["esc to interrupt"]`, then read the answer from the returned state. Signal `stop` means the turn ended; `permission_request` or a notification means Claude needs your input.
6. Run a shell command: `submit` with `observe: true` and `wait_for: "change"` (`wait_ms: 15000` to wait for an answer). To wait for a TUI's screen, use `wait_for: "text"` with `wait_text` naming the target state.
7. Inspect a typed draft in the readback before you send Enter. In Claude Code, `Ctrl+J` inserts a newline.
8. Fix an unsubmitted draft with one bounded `sequence` (`observe: true`, `wait_for: "change"`), check the readback, then submit.
9. Operate a scenario between `claim: true` on its first input and `release: true` on its last. After a human takes over, do not keep reclaiming. Release when you are done.
10. For Claude Code's `/rewind`, use the real menu and verify the restored conversation. Never substitute a new session, a transcript edit or a script.
