# Writing a gate

Declare `gate` with named steps; the kernel checks it after worker report.

- Steps tolerate kernel restart and rerun.
- Steps must leave the checkout unchanged: `cargo fmt --check`, not `cargo fmt`; no lockfile refresh. Changes fail with `gate-target-mismatch`.
- Steps use `/bin/sh` with only `PATH`, `HOME`, `LANG`, `LC_ALL`, `TERM` and proxy settings. No `neige` command works there (no `NEIGE_MCP_SOCKET` or `NEIGE_MCP_TOKEN`). Direct kernel CLI steps are refused; scripts are not inspected.
- Codex/claude tasks take no `gate.cwd`: the gate runs in the verified attempt's checkout; for a subdirectory, write `cd <subdir> && …` in the step. Terminal tasks keep `gate.cwd`.
- Check checkout files, e.g. `python3 -m unittest discover` or `test -s artifacts/result.json` (existence only). Other artifacts: put their paths and semantic checks in the worker's goal; the worker reports the paths.
- A codex or claude task leaving the checkout unchanged declares `access: "read_only"` and no gate; read-only tasks may run side by side. Writing only ignored files (build output) is read-only. With `require_task_gates`, any other codex or claude task needs a gate or a `no_gate_reason`.
- An agent write to an unstarted `ready: true` task is refused for static errors (`gate_required`, duplicate key, cycle, bad ref); edits to started or finished tasks are kept.
- `spawn: "sub-track"` requires `kind: "codex"`. Optional `child_track: {provider, model, reasoning_effort}` overrides the child planner; other fields inherit. Changing provider resets omitted model/effort to its defaults. Approval always inherits. Choices freeze at creation; invalid provider/model fails the task. No gate needed; a declared gate runs on child closure in task `cwd`, else parent checkout.

`neige_task_ls.verification` says whether a failed gate judged code. Fix its environment then `neige_task_regate`, or declare a new task with a fixed command.
