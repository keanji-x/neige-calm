# Writing a gate

Declare named `gate` steps; the kernel checks them after worker report.

- Steps tolerate restart and rerun.
- Steps leave the checkout unchanged: `cargo fmt --check`, not `cargo fmt`; no lockfile refresh. Changes fail with `gate-target-mismatch`.
- Steps use `/bin/sh` with only `PATH`, `HOME`, `LANG`, `LC_ALL`, `TERM` and proxy settings. No `neige` works (no `NEIGE_MCP_SOCKET`/`NEIGE_MCP_TOKEN`). Direct kernel CLI steps fail; scripts are not inspected.
- Codex/claude tasks take no `gate.cwd`: gates run in the verified checkout; use `cd <subdir> && …` in the step. Terminal tasks keep `gate.cwd`.
- Check checkout files: e.g. `python3 -m unittest discover`, `test -s artifacts/result.json` (existence only). For other artifacts, the goal gives paths/semantic checks; the worker reports paths.
- {task_gate_guidance} Read-only tasks may run side by side.
- Unstarted `ready: true` task writes fail on static errors (`gate_required`, duplicate key, cycle, bad ref); edits to started or finished tasks are kept.
- `spawn:"sub-track"` requires `kind:"codex"`. `child_track:{provider,model,reasoning_effort}` overrides its Planner; omitted fields inherit. Provider changes reset omitted model/effort to defaults. After handback, `child_track:{"continue":"id"}` reuses the child with a new key and no overrides. Approval inherits; creation choices freeze. Invalid provider/model fails. A gate runs on child close in task `cwd`, else parent checkout.

`neige_task_ls.verification`: did a failed gate judge code? Fix the environment and `neige_task_regate`, or use a new task with a fixed command.
