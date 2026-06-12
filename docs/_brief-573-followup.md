# #573 follow-up — UX edit-button + create-codex-card investigation

User feedback on the PR #576 preview at http://192.168.5.20:4573/calm/:

1. **Edit pencil is always visible**, but most files in the sidebar (`.payload.json`, `conversation.md`, `events.json`, `wave.json`, etc.) are read-only projections. Only `report.md` (the default view) is editable. User suggests: hide the Edit button when the selected sidebar path is NOT `report.md` / null.

2. **Cannot create a new codex card.** Server log snapshot in `docs/_server-log-pr573.txt` — relevant lines:
   - `ERROR shared codex app-server start/takeover failed; continuing boot error=codex app-server: spawn shared codex app-server: Permission denied (os error 13)` (boot)
   - `WARN spec harness start submission failed; wave created but spec agent is inert error=internal: shared codex app-server is not running` (×2, when user created waves)
   - No `POST /api/cards` (or similar) attempt seen — looks like the UI never fired a "create codex card" request, OR fired one that returned before reaching server logs.

## Investigate (READ-ONLY, write a research doc — NO code edits)

For each issue, write 1 section to `docs/_research-573-followup.md` (≤120 lines total):

### Issue 1 — Edit button visibility

- Find where the Edit pencil is rendered for the report card. Start from `web/src/cards/builtins/wave-report.tsx:504` (the `canEdit && !editing` branch — added in phase 2 wiring).
- After phase 2, when a sidebar file is selected, what is shown in the right pane? Does the Edit button still make sense in that state?
- Propose ≤2 fix shapes. Recommend one. The simplest is to hide the button when `selectedPath` is set and != `report.md`. State needs to flow from `WaveReportSidebar` to the report card (or vice versa).

### Issue 2 — Cannot create codex card

- Find the frontend "add codex card" entry point (likely AddPanel / `codex.tsx` `create` / `addPanel` field in CardEntry). Trace what happens on click.
- Find the backend route that handles codex card creation (look for `POST /api/cards` or `POST /api/waves/:id/cards` or similar).
- The boot-time `codex app-server: Permission denied (os error 13)` — what does the codex card creation path do when the shared codex app-server is dead? Does it 500, 503, silently no-op, or refuse to surface the affordance in UI?
- Per memory `project_codex_npm_bin_path_change.md` / `project_codex_sandbox_blocks_uds_connect.md`, the docker server image's codex binary may be at a stale path or sandboxed. Note whether this is the same class of problem.
- Recommend the **smallest** investigation step the user can take to confirm root cause (e.g., specific docker-compose env var to check, a curl against `/api/cards` to reproduce, or a frontend devtools observation).

Constraints:
- DO NOT edit code, only research.
- File:line slices from the listed files only. No grep -r.
- ≤120 lines markdown total.
- Read `docs/_server-log-pr573.txt` if helpful.
