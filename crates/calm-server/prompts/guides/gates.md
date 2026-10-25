# Writing a gate

Declare `gate` with named steps; the kernel checks it after worker report.

- Steps tolerate kernel restart and rerun.
- Steps must leave the checkout unchanged: `cargo fmt --check`, not `cargo fmt`; no lockfile refresh. Changes fail with `gate-target-mismatch`.
- Steps use `/bin/sh` with only `PATH`, `HOME`, `LANG`, `LC_ALL`, `TERM` and proxy settings. No `neige` command works there (no `NEIGE_MCP_SOCKET` or `NEIGE_MCP_TOKEN`). Direct kernel CLI steps are refused; scripts are not inspected.
- Codex/claude tasks take no `gate.cwd`: the gate runs in the verified attempt's checkout; for a subdirectory, write `cd <subdir> && …` in the step. Terminal tasks keep `gate.cwd`.
- Check checkout files, e.g. `python3 -m unittest discover` or `test -s artifacts/result.json` (existence only). Other artifacts: put their paths and semantic checks in the worker's goal; the worker reports the paths.
- {task_gate_guidance} Read-only tasks may run side by side.
- An agent write to an unstarted `ready: true` task is refused for static errors (`gate_required`, duplicate key, cycle, bad ref); edits to started or finished tasks are kept.
- `spawn:"sub-track"` requires `kind:"codex"`. `child_track:{provider,model,reasoning_effort}` overrides its Planner; omitted fields inherit. Provider changes reset omitted model/effort to defaults. After handback, `child_track:{"continue":"id"}` reuses the child with a new key and no overrides. Approval inherits; creation choices freeze. Invalid provider/model fails. A gate runs on child close in task `cwd`, else parent checkout.

`neige_task_ls.verification` says whether a failed gate judged code. Fix its environment then `neige_task_regate`, or declare a new task with a fixed command.
