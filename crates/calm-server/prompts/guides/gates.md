# Writing a gate

A gate is the kernel's check after a codex or claude task reports; the worker runs its steps first. Declare it as the task block's `gate` with named steps.

- Steps must be re-runnable: a kernel restart can rerun them.
- Steps must not change tracked files or leave new ones: `cargo fmt --check`, not `cargo fmt`; no lockfile refresh. A changed checkout fails the gate (`gate-target-mismatch`, listing the paths).
- Steps run under `/bin/sh` in an empty environment plus inherited `PATH`, `HOME`, `LANG`, `LC_ALL`, `TERM` and proxy settings. No `neige` command works there (no `NEIGE_MCP_SOCKET` or `NEIGE_MCP_TOKEN`). A step calling the kernel CLI directly is refused at declaration (scripts are not inspected).
- Codex and claude tasks take no `gate.cwd`: the gate runs in the verified attempt's checkout; for a subdirectory, write `cd <subdir> && …` in the step. Terminal tasks keep `gate.cwd`.
- Check checkout files, e.g. `python3 -m unittest discover` or `test -s artifacts/result.json` (existence only). Other artifacts: put their paths and semantic checks in the worker's goal; the worker reports the paths.
- A codex or claude task not expected to change the checkout declares `access: "read_only"` and no gate; read-only tasks may run side by side. Writing only ignored files (build output) is read-only. With `require_task_gates`, any other codex or claude task needs a gate or a `no_gate_reason`.
- An agent write adding or changing a `ready: true`, unstarted task is refused if the block alone is wrong (`gate_required`, duplicate key, cycle, bad ref); edits to started or finished tasks are kept.
- A `spawn: "sub-track"` task needs no gate; one it declares runs once the child track closes, in the task's `cwd`, else the parent checkout.

If a gate fails, `neige_task_ls`'s `verification` says whether the code was judged. If not, fix its environment then `neige_task_regate`, or fix its command in a new task.
