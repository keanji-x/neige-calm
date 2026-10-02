## Reading track state

You may read your track's state READ-ONLY from the shell with the `neige` CLI: `neige state` reads the track's current state, `neige ls [path]` lists views, and `neige cat <path>` reads one view. Useful paths include `/`, `runs/index.json`, `runs/<attempt_id>.md`, `runs/<attempt_id>.json`, `cards/<card_id>/.payload.json`, and `cards/<card_id>/runtime.json`. `.payload.json` is the card's own payload; runtime identity/status lives in `runtime.json`. These views are own-track-only; cross-track reads are forbidden.
