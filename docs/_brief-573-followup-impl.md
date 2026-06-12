# #573 follow-up impl — Edit-gate + surface card-create errors

Two fixes; both informed by `docs/_research-573-followup.md`.

## Fix 1 — Edit pencil should hide when a non-report file is selected

`web/src/cards/builtins/wave-report.tsx:486` currently gates Edit only on `waveId !== null`. After phase 2, picking `.payload.json`/`conversation.md`/etc replaces the right pane with a read-only projection, but the header pencil still opens the report editor — confusing.

Do:
- Lift `selectedPath` into `WaveReportCardImpl`. Pass `selectedPath` + `onSelectedPathChange` props into `WaveReportSidebar` so the sidebar still drives the value but the report card sees it.
- New `canEdit` becomes `waveId !== null && (selectedPath === null || selectedPath === 'report.md')`.
- Reset `selectedPath` to `null` on `waveId` change (already handled by the `key={waveId}` remount on the sidebar; for the lifted state at the parent, use an effect or include waveId in the parent's existing reset path).
- Make `WaveReportSidebar` accept the controlled `selectedPath` + setter; remove its internal `useState` for `selectedPath` (keep `expandedDirs` local).
- Update `wave-report-sidebar.test.tsx`: it currently mounts the sidebar standalone, so add a controlled-mode wrapper to keep tests passing.
- Add 2 wave-report tests: Edit pencil visible when `selectedPath === null`; Edit pencil hidden when `selectedPath === 'cards/<id>/.payload.json'`.

## Fix 2 — Surface card-create errors instead of swallowing

`web/src/app/router.tsx:535` catches non-contract create failures and `console.warn`s; `web/src/pages/Wave.tsx:102` closes the modal in `finally`. Result: a 500 from `POST /api/waves/{id}/codex-cards` (dead shared codex daemon) looks like a silent no-op to the user.

Do:
- In `web/src/app/router.tsx:525-540` area, surface the error: re-throw the `CalmApiError` (or expose it via the existing modal error channel) rather than swallowing with `console.warn`. Look at how `WavePage.submitModal` currently has an `error` channel for the schema modal — if absent, plumb one in.
- Confirm the schema modal in `web/src/pages/Wave.tsx:90-110` (or its modal component) renders an inline error string when the submit promise rejects.
- For `CalmApiError` with `status >= 500`, show the existing message (already formatted server-side, e.g. `"internal: shared codex app-server is not running"`). For 4xx, show the err.message.
- Keep the modal OPEN on failure so the user sees the error (only close on success).
- Add a test in `Wave.test.tsx` (or matching) that mocks `createCodexCard` rejecting with a 500 and asserts the error string is visible + modal stays open.

## Out of scope (NOT this round)

- Docker compose default path fix (handled separately).
- Backend behavior of `codex_adapter.rs` (already returns clean 500).

## Gates

- `cargo check -p calm-server` (should be a noop — no backend changes)
- `cd web && PATH=$HOME/.nvm/versions/node/v24.4.1/bin:$PATH npm run typecheck`
- `cd web && PATH=$HOME/.nvm/versions/node/v24.4.1/bin:$PATH npm test`
- `cd web && PATH=$HOME/.nvm/versions/node/v24.4.1/bin:$PATH npm run lint`

Write `docs/_impl-573-followup.md` (≤60 lines): files changed, test deltas.

No grep -r. file:line slices from this brief + the research doc only.
