# Writing a gate

A gate is the machine check the kernel runs after a codex or claude task reports; the worker runs its steps first. Declare it on the task block as `gate` with named steps.

- Steps must be re-runnable: a kernel restart can rerun a gate.
- Steps must not change tracked files or leave new ones: `cargo fmt --check`, not `cargo fmt`; no lockfile refresh. A changed checkout ends the gate as `gate-target-mismatch`, listing the paths.
- Steps run under `/bin/sh` with an empty environment plus the inherited `PATH`, `HOME`, `LANG`, `LC_ALL`, `TERM` and the configured proxy settings. With no `NEIGE_MCP_SOCKET` or `NEIGE_MCP_TOKEN`, no `neige` command, task reporting included, works inside a gate. A step calling the kernel CLI directly is refused at declaration; the check cannot see into scripts.
- Codex and claude tasks take no `gate.cwd`: the gate runs in the checkout of the attempt it verifies. For a subdirectory, write `cd <subdir> && …` inside the step. Terminal tasks keep `gate.cwd`.
- Check files in the checkout, e.g. `python3 -m unittest discover` or `test -s artifacts/result.json` (this proves only that the file exists). For other artifacts, name the paths and semantic checks in the worker's goal, and have the worker report the paths.
- A codex or claude task not expected to change the checkout declares `access: "read_only"` and no gate; it may run beside other read-only tasks. Writing only ignored files (build output) still counts as read-only. On a track with `require_task_gates`, writing any other `ready: true` codex or claude task with neither a gate nor a `no_gate_reason` is refused (`gate_required`).
- A `spawn: "sub-track"` task needs no gate; one it declares runs in the parent checkout once the child track closes.

If a gate fails, `neige_task_ls`'s `verification` says whether the code was judged. If not, fix its environment then `neige_task_regate`, or fix its command in a new task.
