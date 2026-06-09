# #573 Follow-up Implementation

## Files changed

- `web/src/cards/builtins/wave-report.tsx`
  - Lifted `selectedPath` into `WaveReportCardImpl`.
  - Reset selection on `waveId` change.
  - Gates edit pencil on `waveId !== null` and default/report selection only.
- `web/src/cards/builtins/wave-report-sidebar.tsx`
  - Made file selection controlled via `selectedPath` and `onSelectedPathChange`.
  - Kept `expandedDirs` and focus state local to the sidebar remount boundary.
- `web/src/app/router.tsx`
  - Re-throws non-contract create failures after logging.
- `web/src/pages/Wave.tsx`
  - Added modal create error state.
  - Closes schema/directory modal only after successful create.
  - Renders failed create errors inline with `role="alert"`.
- `web/src/calm.css`
  - Added schema modal error styling, including wide directory-modal inset style.
- `web/src/cards/builtins/wave-report-sidebar.test.tsx`
  - Added a controlled wrapper for standalone sidebar tests.
- `web/src/cards/builtins/wave-report.test.tsx`
  - Added edit-pencil visibility coverage for default selection and non-report file selection.
- `web/src/pages/Wave.test.tsx`
  - Added codex create 500 coverage: error visible and modal remains open.
- `web/src/cards/registry.test.tsx`
  - Updated router runtime-failure expectation from swallowed to surfaced rejection.

## Test deltas

- New wave-report card assertions:
  - edit pencil visible when `selectedPath === null`.
  - edit pencil hidden when `selectedPath === "cards/card_1/payload.json"`.
- New WavePage assertion:
  - rejected codex create with 500 shows `internal: shared codex app-server is not running`.
  - directory modal stays open after failure.
- Updated registry/router assertion:
  - rejected atomic create now rejects to caller and does not invalidate queries.

## Gates

- `RUSTC_WRAPPER= cargo check -p calm-server` passed.
  - Plain `cargo check -p calm-server` failed because `sccache` returned `Operation not permitted`.
- `cd web && PATH=$HOME/.nvm/versions/node/v24.4.1/bin:$PATH npm run typecheck` passed.
- `cd web && PATH=$HOME/.nvm/versions/node/v24.4.1/bin:$PATH npm test` passed.
- `cd web && PATH=$HOME/.nvm/versions/node/v24.4.1/bin:$PATH npm run lint` passed.

## Notes

- `docker-compose.yml` was already dirty in the worktree and was not touched.
