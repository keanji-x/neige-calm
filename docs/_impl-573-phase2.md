# #573 Phase 2 frontend implementation

## Files changed
- `web/src/api/calm.ts`
  - Added typed `listWaveFiles` and `catWaveFile` helpers using `generated.ts`
    `paths` response types.
- `web/src/api/queries.ts`
  - Added `waveFileListQueryKey`, `waveFileContentQueryKey`,
    `useWaveFileList`, and `useWaveFileContent`.
- `web/src/cards/builtins/wave-report-sidebar.tsx`
  - New file tree and viewer for the wave file projection.
  - Root lists on mount; child directories list lazily on expand.
  - Markdown renders through `ReactMarkdown`/`remarkGfm`; JSON/unknown content
    renders through `CodePane`.
- `web/src/cards/builtins/wave-report.tsx`
  - Report read mode now uses the file sidebar when `WaveContext` provides a
    `waveId`; isolated/no-context renders keep the old report-only view.
- `web/src/calm.css`
  - Added `.wave-report-files-*` layout, tree, state, markdown, and CodeMirror
    fill rules.
- `web/src/app/invalidationPolicies.ts`
  - Invalidates `['wave-files', waveId]` on `wave.updated`,
    `wave.report_edited`, and card add/update/delete events.
- Tests:
  - `web/src/cards/builtins/wave-report-sidebar.test.tsx`
  - `web/src/app/eventBridge.test.tsx`
  - `web/src/cards/builtins/wave-report.test.tsx`
  - `web/e2e/wave-report-sidebar-files.spec.ts`

## Test results
- `cd web && npm run typecheck`: passed.
- `cd web && npm test`: passed, 61 files / 813 tests.
- `cd web && npm run lint`: passed.
- Chromium e2e was not run here by request; CI should run
  `web/e2e/wave-report-sidebar-files.spec.ts`.

## Notes
- Local `/usr/bin/node` is `v18.19.0`, which cannot start the current
  Vitest/Rolldown stack. Gates were run with
  `/home/kenji/.nvm/versions/node/v24.4.1/bin` first on `PATH`.
- Screenshots not required.
