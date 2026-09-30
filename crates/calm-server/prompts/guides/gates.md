# Writing a gate

A gate is the machine check the kernel runs after a codex or claude task reports. Declare it on the task block as `gate` with named steps.

- Steps must be re-runnable: a gate can run more than once after a kernel restart.
- Steps must not change tracked files or leave new ones: `cargo fmt --check`, not `cargo fmt`; no lockfile refresh. The kernel samples the checkout before and after, and a changed tree ends the gate as `gate-target-mismatch`, listing the paths.
- Steps run under `/bin/sh` with an empty environment plus the inherited `PATH`, `HOME`, `LANG`, `LC_ALL`, `TERM` and the configured proxy settings. There is no `NEIGE_MCP_SOCKET` or `NEIGE_MCP_TOKEN`, so `neige cat`, `neige state` and task reporting do not work inside a gate. A step that calls the kernel CLI directly is refused when the task is declared; that check cannot see into scripts.
- Codex and claude tasks take no `gate.cwd`: the gate runs in the checkout of the attempt it verifies. For a subdirectory, write `cd <subdir> && …` inside the step. Terminal tasks keep `gate.cwd`.
- Check files in the checkout, e.g. `python3 -m unittest discover` or `test -s artifacts/result.json` (the latter proves the file exists, not that it is right). For other artifacts, name the paths and semantic checks in the worker's goal, and have the worker report the paths.
- On a track with `require_task_gates`, an ungated codex or claude task is written but not scheduled: the read shows a `gate_required` diagnostic until the block has a gate or a `no_gate_reason`. Terminal tasks are exempt.

When a gate fails, `calm.plan.list`'s `verification` says whether the code was judged. If it was not, fix the gate command or its environment cause before the next round.
