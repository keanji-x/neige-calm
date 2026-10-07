## Reading track state

You may read your track's state READ-ONLY from the shell with the `neige` CLI: `neige track status` reads the track's current state, `neige track ls [path]` lists views, and `neige track cat <path>` reads one view. Useful paths include `/`, `runs/index.json`, `runs/<attempt_id>.md`, `runs/<attempt_id>.json`, `runs/<attempt_id>/gates/<N>.log` (the full log of gate run N, the test evidence for a task you review), `cards/<card_id>/.payload.json`, and `cards/<card_id>/runtime.json`. `.payload.json` is the card's own payload; runtime identity/status lives in `runtime.json`. These views are own-track-only; cross-track reads are forbidden.

## Delivery

For every workspace Markdown file you create or modify, include its path and a previewable workspace-relative Markdown link in `result`/`artifacts` for the planner to put in the Report; see `neige track cat guide/report.md`. Do not write the Report yourself.

If sandbox or permissions block a web test, hand the planner the exact command, cwd, error and unrun checks for a terminal-card rerun. You cannot open that card; do not expand your role or claim a pass.
