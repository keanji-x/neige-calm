# Issue #573 report sidebar implementation notes

## 1. Backend refactor
- Current MCP handlers are identity adapters: `wave_ls(ctx, identity, args)` and
  `wave_cat(ctx, identity, args)` gate Spec/Worker, parse `path`, resolve the
  bound wave, then dispatch the projection (`crates/calm-server/src/mcp_server/tools/wave_file.rs:84`,
  `:144`).
- Move the projection out of `mcp_server/tools/wave_file.rs` into a new
  crate-level module, recommended `crates/calm-server/src/wave_fs_view.rs`.
  HTTP should not import an MCP tool module.
- Target API:
  ```rust
  WaveFsView { repo: &dyn RouteRepo, write: &WriteContext }
  async fn ls(&self, wave: &Wave, path: Option<&str>) -> Result<Vec<WaveFsEntry>, WaveFsError>;
  async fn cat(&self, wave: &Wave, path: &str) -> Result<WaveFsContent, WaveFsError>;
  ```
- `WaveFsEntry` should serialize to the existing MCP entry objects. `WaveFsContent`
  should serialize as `{ content, content_type }`.
- Keep MCP argument/role/identity handling in `wave_file.rs`; keep HTTP path/session
  handling in `routes::waves`; both call `WaveFsView`.
- `load_report_for_wave` is already role-agnostic and says callers must enforce
  binding first (`mcp_server/tools/wave_report.rs:346`). To avoid constructing an
  MCP `AppContext` in HTTP, relax it to take `repo: &dyn RouteRepo` or move the
  report-card lookup into `WaveFsView`.
- `project_runtime_into_card_payload(repo, &mut card)` already takes a repo trait
  plus card (`runtime_lookup.rs:106`), so the view can project payloads without
  depending on `AppContext`.

## 2. HTTP endpoints
- Add routes to `routes::waves::router`, next to existing wave-scoped routes
  (`routes/waves.rs:67` and `:80`):
  - `GET /api/waves/{id}/files/ls?path=<logical_path>`
  - `GET /api/waves/{id}/files/cat?path=<logical_path>`
- No JSON request body. `path` is optional for `ls` and required for `cat`.
  Normalize with the same rules as MCP: omitted or `/` means root for `ls`;
  leading/trailing slashes are ignored.
- 200 body for `ls`: the MCP-aligned bare array of entries with `name`, `kind`,
  optional `size`, and optional `updated_at`.
- 200 body for `cat`: `{ "content": "...", "content_type": "text/markdown" }`
  or `{ "content": "{...}", "content_type": "application/json" }`.
- Resolve the `{id}` wave with `repo.wave_get`. Missing wave is 404. Logical path
  not present is 400. A `cards/<card_id>` path outside the wave remains 403.
- Auth: protected REST is wrapped by `auth::require_session` in `main.rs:132`;
  it inserts `Principal` into request extensions (`auth.rs:288`). Include a
  `_principal: Principal` extractor to make the dependency explicit.
- Wave ownership: current app is single-owner and existing protected wave routes
  do not check per-user membership. Do not add a fake ownership check; rely on
  session auth plus wave existence. Keep the internal card-in-wave check.

## 3. OpenAPI / generated.ts
- Yes, register both endpoints with `utoipa::path`; they are protected REST
  routes and should appear in `/api/openapi.json`.
- Add the path fns to `crates/calm-server/src/openapi.rs` near the other waves
  entries (`openapi.rs:58` through `:65`).
- Define `ToSchema` DTOs for `WaveFsEntry` and `WaveFsContent`. Per the OpenAPI
  drift rule, touching exposed schema docs or adding endpoints requires regen:
  run `npm run gen:api` from `web/`, which regenerates `src/api/openapi.json`
  and `src/api/generated.ts` (`web/package.json:22`).

## 4. Frontend - sidebar component
- Put the component beside the report card as
  `web/src/cards/builtins/wave-report-sidebar.tsx`, then import it from
  `wave-report.tsx`. That keeps the already-large report card file focused.
- Small version: render inside the report card body area, no `Card.title` or
  second card chrome.
- State model: `expandedDirs: Set<string>`, `selectedPath: string | null`, and
  per-dir lazy queries. Root `ls` loads on mount; child `ls` enables on expand.
- Add query keys in `web/src/api/queries.ts`:
  - `waveFiles(waveId) -> ['wave-files', waveId]`
  - `waveFileList(waveId, path) -> ['wave-files', waveId, 'ls', normalizedPath]`
  - `waveFileContent(waveId, path) -> ['wave-files', waveId, 'cat', normalizedPath]`
- Invalidation: invalidate `['wave-files', waveId]` on `wave.updated`,
  `wave.lifecycle_changed`, card add/update/delete, and `wave.report_edited`.
  Hook events can use `findWaveOwningCard`; run/task events lack wave id in the
  current WS payload, so either invalidate `['wave-files']` globally or defer.
- Empty/error states: root "No files", empty dir "Empty", no selection "Select
  a file"; inline retry for tree errors and pane-local error for `cat`. 401
  still flows through `CalmApiError` and global unauthorized handling.

## 5. Frontend - viewer pane
- Markdown: reuse the same `ReactMarkdown` plus `remarkGfm` stack as
  `ReadOnlyView` (`wave-report.tsx:214`). Keep `rehype-raw` out.
- JSON: reuse `CodePane` from `file-viewer-codemirror.tsx`. It is already
  read-only, handles `.json` via `loadLanguage`, and applies the existing
  CodeMirror light/dark themes. Do not create a separate JSON renderer unless
  the sidebar needs custom folding or copy controls later.
- For unknown `content_type`, fall back to `CodePane` with the logical path so
  extension-based highlighting still works.

## 6. CSS
- Reuse the `.wave-report-*` namespace, but add a specific sub-namespace such as
  `.wave-report-files-*` for tree/viewer layout. Avoid generic `.sidebar-*`.
- Width: fixed for v1, not resizable. Use a stable small column such as
  `clamp(200px, 24%, 280px)` with `minmax(0, 1fr)` for the viewer. Stack or hide
  the sidebar behind a toggle only if the card/container becomes too narrow.

## 7. Test surface
- Backend: add HTTP route tests that seed the same wave/card/run/report shapes
  as `tests/mcp_wave_file.rs`. Its helpers are private today, so either move a
  minimal fixture module to `tests/support/wave_file.rs` or add the equivalence
  cases in the same file.
- Equivalence assertion: for representative paths (`/`, `cards`, `runs`,
  `report.md`, `cards/index.json`, `runs/index.json`,
  `cards/<id>/payload.json`), compare HTTP JSON bodies to MCP `call_tool`
  results byte-for-byte after parsing JSON.
- Also cover HTTP 401 without session, 404 missing wave, 400 unknown path, and
  403 cross-wave card path.
- Frontend unit tests: sidebar root render, lazy dir expansion, selected file
  fetch, Markdown render, JSON `CodePane` path/text handoff, empty state, and
  `CalmApiError` display.
- Chromium E2E spec name: `web/e2e/wave-report-sidebar-files.spec.ts`.

## 8. Risk / open questions
The main risk is auth-model mismatch: MCP exposes this tree only to Spec/Worker
actors bound to one wave, while HTTP lets any authenticated web user request any
wave id. That matches the current single-owner REST model, but in a future
multi-user model the sensitive paths are `cards/*/payload.json`,
`cards/*/events.json`, `cards/*/conversation.md`, and `runs/*`; those should be
hidden or 403 unless the principal owns the wave or has explicit access.
