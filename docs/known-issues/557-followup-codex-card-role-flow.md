# #557 followup: codex card create flow and role assignment

## TL;DR

UI `+codex` goes through `POST /api/waves/{wave_id}/codex-cards`.
That operation writes `CardRole::Plain`, not `Worker`.
Codex `Worker` cards are currently dispatcher-created from `calm.dispatch_request`.
So the observed no-wake is consistent with current role semantics, not a random UI payload miss.

## Q1. UI `+codex` goes through which API?

- `AddPanel` is a generic `+ Add` menu from registry entries: `web/src/shared/components/AddPanel.tsx:1-15`.
- `CodexEntry.addPanel.label` is `codex`: `web/src/cards/builtins/codex.tsx:499-505`.
- Router selection with a form calls `addCardWithValues`: `web/src/app/router.tsx:381-396`.
- `addCardWithValues` parses the entry schema then calls `createFromEntry`: `web/src/app/router.tsx:435-454`.
- For atomic entries, `createFromEntry` calls `entry.create.submit(...)`: `web/src/app/router.tsx:525-528`.
- `CodexEntry.create.submit` sends `{ cwd, prompt, theme }`: `web/src/cards/builtins/codex.tsx:453-461`.
- API wrapper posts that body to `/api/waves/${waveId}/codex-cards`: `web/src/api/calm.ts:293-298`.
- Backend route is exactly `POST /api/waves/{wave_id}/codex-cards`: `crates/calm-server/src/routes/codex_cards.rs:50-51`.

## Q2. What role does this API assign?

- Route normalizes the body and submits operation kind `codex-create`: `crates/calm-server/src/routes/codex_cards.rs:132-164`.
- `CodexCreateAdapter` handles the tx path and calls `card_with_codex_create_tx`: `crates/calm-server/src/operation/codex_adapter.rs:245-271`.
- The role argument there is hard-coded to `CardRole::Plain`: `crates/calm-server/src/operation/codex_adapter.rs:256-268`.
- DB helper documents the intended split: user-facing `POST /codex-cards` passes `Plain`, wave-create passes `Spec`, dispatcher passes `Worker`: `crates/calm-server/src/db/sqlite.rs:1284-1289`.
- Therefore the user-suspected point is real: `+codex` creates `Plain`.

## Q3. How are `role=Worker` cards created now?

- Semantics doc says `Worker` is a dispatcher-spawned worker card: `crates/calm-server/src/model.rs:32-33`.
- `calm.dispatch_request` is the MCP surface: descriptor says it requests dispatcher spawn of a worker card: `crates/calm-server/src/mcp_server/tools/emit.rs:70-75`.
- That tool is only callable by `Spec` or `Worker`: `crates/calm-server/src/mcp_server/tools/emit.rs:93-99`.
- For `kind: "codex"`, it emits `Event::CodexJobRequested`: `crates/calm-server/src/mcp_server/tools/emit.rs:114-131`.
- Dispatcher consumes that path and creates codex card with `CardRole::Worker`: `crates/calm-server/src/dispatcher.rs:1163-1187`.
- Dispatcher also creates terminal workers with `CardRole::Worker`: `crates/calm-server/src/dispatcher.rs:1493-1507`.
- Separate manual Claude route exists and writes `CardRole::Worker`: `crates/calm-server/src/routes/claude_cards.rs:1-7`, `crates/calm-server/src/operation/claude_adapter.rs:313-324`.
- I did not find a manual codex worker create API; manual codex route writes `Plain`.

## Q4. Is there an UI add-worker entry?

- Grep in `web/src` finds no `dispatch_request` UI caller.
- `+ Add` only exposes registry labels; codex and claude labels are plain `codex` / `claude`: `web/src/cards/builtins/codex.tsx:499-505`, `web/src/cards/builtins/codex.tsx:569-585`.
- UI `+codex` maps to `CodexEntry.create.submit`, not dispatcher: `web/src/cards/builtins/codex.tsx:453-461`.
- UI `+claude` maps to `/claude-cards`; backend names it "manual Claude worker card creation": `crates/calm-server/src/routes/claude_cards.rs:1-7`.
- There is no explicit "add codex worker" button or UI path. Codex workers are created by spec/worker agents through `calm.dispatch_request`.

## Q5. By design or bug?

- `event_warrants_spec_push` wakes spec for task events and user report edits, but for codex/claude stop hooks requires `CardRole::Worker`: `crates/calm-server/src/dispatcher.rs:125-132`.
- Blame for the `is_worker` line points to `2a6a7ce3 feat(#510): PR-add - spec harness skeleton + 3 ProviderAdapter impls + cutover groundwork`.
- `routes/codex_cards.rs` top comment describes atomic user-facing codex-card creation, not spec workflow membership: `crates/calm-server/src/routes/codex_cards.rs:1-24`.
- The stronger evidence is the DB helper comment explicitly assigning manual codex route to `Plain`: `crates/calm-server/src/db/sqlite.rs:1284-1289`.
- So current code says: `+codex` is an interactive/plain card; worker stop hooks are the spec-workflow signal.

## Conclusion

This looks by design for codex: `+codex` is not supposed to be a worker under the current role model.
The bug, if product expectation is "user-added codex in a spec wave should wake spec on stop", is not a missing body field in UI; it is a semantic mismatch between UX expectation and role/workflow policy.
To create a codex worker today, the spec (or an existing worker) must call `calm.dispatch_request` with `kind: "codex"`; the dispatcher then mints the `Worker` card.
If direct human-created codex workers are desired, the missing layer is an explicit UI/API path for "codex worker", not changing the existing `+codex` route silently.
