# #573 Phase 2 — frontend Report sidebar + viewer

Issue #573. Phase 1 backend already landed on this branch (HTTP routes `GET /api/waves/{id}/files/{ls,cat}` with `WaveFsEntry` / `WaveFsContent` DTOs visible in `web/src/api/generated.ts`). Design + impl notes in `docs/_explore-573-report-sidebar.md`. This phase is FRONTEND ONLY.

## Do

1. Add typed REST helpers in `web/src/api/calm.ts` next to the existing wave fetchers:
   - `listWaveFiles(waveId, path?): Promise<WaveFsEntry[]>` → `GET /api/waves/{id}/files/ls`
   - `catWaveFile(waveId, path): Promise<WaveFsContent>` → `GET /api/waves/{id}/files/cat`
   Use the generated `paths` types from `generated.ts`. Follow the existing `CalmApiError` pattern.

2. Add query keys + hooks in `web/src/api/queries.ts` (or matching module):
   - `waveFileListQueryKey(waveId, path)` → `['wave-files', waveId, 'ls', path]`
   - `waveFileContentQueryKey(waveId, path)` → `['wave-files', waveId, 'cat', path]`
   - `useWaveFileList(waveId, path, { enabled })`
   - `useWaveFileContent(waveId, path)`

3. New component `web/src/cards/builtins/wave-report-sidebar.tsx`:
   - Left tree: root `ls` on mount; child dirs lazy on expand. Track `expandedDirs: Set<string>` + `selectedPath: string | null` in component state.
   - Render entries as `kind + truncated id` for card dirs (per explore §4: small version, no Card.title field). For non-card entries use the filename verbatim.
   - Right viewer pane:
     - `content_type: 'text/markdown'` → `<ReactMarkdown remarkPlugins={[remarkGfm]}>` (same stack as `wave-report.tsx:214`).
     - `application/json` (or unknown) → reuse `CodePane` from `file-viewer-codemirror.tsx`; pass logical path so extension highlighting works.
   - States: empty root ("No files"), empty dir ("Empty"), no selection ("Select a file"), `CalmApiError` inline.

4. Wire into `wave-report.tsx`:
   - Import `WaveReportSidebar`; render it BESIDE the existing `ReadOnlyView` (sidebar left, current report sections right). Keep the existing H1-section model untouched — Phase 2 is additive.
   - Pass `waveId` (already available via `WaveContext`). Hide sidebar when `waveId === null` (unit tests render without wave context).

5. CSS in `web/src/calm.css` under `.wave-report-files-*` namespace:
   - Grid: sidebar `clamp(200px, 24%, 280px)` / viewer `minmax(0, 1fr)`.
   - Tree row: monospace, hover state, selected state, caret icon (▸/▾ reuse the existing approach in `wave-report.tsx:209`).
   - Don't break existing `.wave-report-*` rules.

6. Invalidate `['wave-files', waveId]` on `wave.updated`, `wave.report_edited`, card add/update/delete events in `web/src/app/invalidationPolicies.ts`. If a payload lacks `waveId`, broaden to `['wave-files']`.

## Tests

- Component test `wave-report-sidebar.test.tsx`: mock fetch, assert root render, expand cards/, click `cards/<id>/payload.json`, viewer shows JSON via `CodePane`. Cover empty-root + error paths.
- Markdown branch: click `report.md`, viewer shows rendered markdown (assert a heading element).
- Add 1 chromium spec `web/e2e/wave-report-sidebar-files.spec.ts`: open a wave → Report card shows sidebar → click a real file → viewer shows content.

## Gates before declaring done

- `cd web && npm run typecheck` (or `tsc`)
- `cd web && npm test` (vitest unit suites)
- `cd web && npm run lint` if it exists
- Do NOT run chromium-e2e here (CI handles it; per memory it has no codex CLI).

Write `docs/_impl-573-phase2.md` (≤80 lines): files changed, test results, screenshots-not-required.

Don't grep -r. Use generated types from `generated.ts`. Don't touch backend.
