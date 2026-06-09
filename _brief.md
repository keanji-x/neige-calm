# Brief — #581 PR1+PR2 (merged): rename worker-request events + typed `Card.runtime`

Implement two cleanups in one PR. Branch `feat/581-pr1pr2-rename-card-runtime`. #577 already retired dispatcher worker-spawn; this PR closes #581 items 2 + 3 + parts of 5.

## Part A — Rename request events (item 2 + 5)

Rename in Rust + on the wire, keep durable event-bus hop and `calm.dispatch_request` API unchanged:

- `Event::CodexJobRequested` → `Event::CodexWorkerRequested` (`crates/calm-server/src/event.rs:545`).
- `Event::TerminalJobRequested` → `Event::TerminalWorkerRequested` (`crates/calm-server/src/event.rs:561`).
- Wire kind `codex.job_requested` → `codex.worker_requested`; `terminal.job_requested` → `terminal.worker_requested`. Update `serde(rename = ...)` (`crates/calm-server/src/event.rs:544`, `:560`) AND `kind_tag()` match arms (`crates/calm-server/src/event.rs:804`, `:805`).
- Backward compat: add `#[serde(alias = "codex.job_requested")]` / `#[serde(alias = "terminal.job_requested")]` on the new variants so old `events` rows deserialize. (For internal-only tagged enum this means using `serde(rename_all = ..., tag = ...)` is fine — confirm the deser path actually reads both. If serde aliasing on tagged variants is awkward, add an explicit `From<old_kind> for new_kind` in any kind-string filter that reads `events.kind` rows from SQL.)
- Dispatcher filter subscription strings (`crates/calm-server/src/dispatcher.rs:481-483`) and event match arms (`:982`, `:984`, `:1016`, `:1018`).
- MCP `calm.dispatch_request` emission paths must build the new variants (`crates/calm-server/src/mcp_server/tools/emit.rs:94`, `:115`, `:127`, `:146`). The MCP tool name `calm.dispatch_request` and its public arguments do not change.
- `wave_file::project_runs` request-side projection: match on the new variants and use the new kind strings (`crates/calm-server/src/mcp_server/tools/wave_file.rs:410-412`, `:464`, `:475`, `:480`, `:491`).
- Update stale comments saying dispatcher mints/spawns workers itself in `crates/calm-server/src/state.rs:297-299` and `:882-883`. Confirm `dispatcher.rs:7-11` already reflects reactor-only.
- Update tests pinning serde/wire strings: `crates/calm-server/src/event.rs:1682-1699`, `:1717-1743`; `crates/calm-server/src/dispatcher.rs:1222-1259`; `crates/calm-server/tests/dispatcher.rs:318-374`; `crates/calm-server/tests/mcp_emit_tools.rs:301-324`; `crates/calm-server/tests/mcp_wave_file.rs:883-887`.

Migration: add a tiny SQL migration to `crates/calm-server/migrations/` that updates existing rows: `UPDATE events SET kind = 'codex.worker_requested' WHERE kind = 'codex.job_requested';` and same for terminal. This lets dispatchers/projections that filter by `events.kind` keep working without runtime alias mapping.

## Part B — Add typed `Card.runtime` (item 3, additive only)

Goal: `Card` carries an optional `runtime` typed field; legacy payload keys remain (`payload.terminal_id`, `payload.claude_session_id`, `payload.codex_thread_id`, `payload.codex_source`, `payload.codex_thread_status`) so frontend readers don't move yet. PR3 (later) deletes them.

- Add a new public DTO `CardRuntimeView` near `crates/calm-server/src/model.rs:452`. Shape:
  - `runtime_id: String`
  - `kind: String` (mirror `RuntimeKind` serde repr)
  - `status: String` (mirror `RunStatus` serde repr)
  - `provider: Option<String>` (mirror `AgentProvider`)
  - `terminal_id: Option<String>`
  - `thread_id: Option<String>`
  - `session_id: Option<String>`
  - `source: Option<String>` (`"shared"` when shared-spec)
  - `thread_status: Option<String>` (`pending_thread_start` / `failed_to_spawn` / `started` — same three values the payload projector emits today)
  - Derive `Serialize, Deserialize, Clone, Debug, ToSchema, TS`. Apply `#[serde(default, skip_serializing_if = "Option::is_none")]` on every field of CardRuntimeView itself? — no, only on its Optional fields. Use `#[ts(optional)]` for tsrs.
- Add `pub runtime: Option<CardRuntimeView>` to `Card` (`crates/calm-server/src/model.rs:452`). Apply `#[serde(default, skip_serializing_if = "Option::is_none")]` and `#[sqlx(default)]` so `FromRow` keeps working. TS gets `runtime?: CardRuntimeView`.
- Extend `project_runtime_fields` (`crates/calm-server/src/runtime_lookup.rs:150`) to ALSO set `card.runtime = Some(view)` with the projected fields. Keep all existing payload-key writes unchanged.
- `project_runtime_into_card_payload` and `project_runtime_into_cards_payload` (`crates/calm-server/src/runtime_lookup.rs:106`, `:120`) get the same dual write for free since they call `project_runtime_fields`. Confirm `project_runtime_into_event_payload` (`:137`) path stays the same — it goes through `project_runtime_into_card_payload`.
- DO NOT remove any payload key. DO NOT change frontend yet.
- Tests: extend `crates/calm-server/tests/runtime_repo.rs:745-764`, `:921-937`, `:963-973` to assert BOTH the legacy payload keys AND `card.runtime.thread_id` / `.terminal_id` / `.source` / `.thread_status` are set as expected. Add an explicit assertion that `card.runtime.is_some()` after projection.
- Regenerate frontend bindings via `npm run gen:api` (from `web/`). Commit both `openapi.json` and `web/src/api/generated.ts` and `generated-events.ts`.

## Done conditions
- `cargo fmt --all --check` clean.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo test --workspace` green.
- `npm run gen:api` clean (no further drift after commit); `npm test -- --run`, `npm run typecheck`, `npm run build` green from `web/` under Node 20.20.2 (path: `/home/kenji/.nvm/versions/node/v20.20.2/bin/`).
- Migration runs against an empty DB without error (`cargo test -p calm-server -- migrations` or whatever pin exists).
- Write a single PR body draft to `_pr-body.md` in this worktree.
- Commit message: `feat(#581): rename *.job_requested → *.worker_requested + typed Card.runtime (additive)`. Body must say `Addresses #581 (items 2, 3, 5)` (per memory partial_fix_issue_closing — NOT Closes/Resolves).
- No Co-Authored-By trailer.

## Constraints
- No grep -r; trust the file:line slices above.
- Stay in this worktree; never edit primary repo.
- If any file:line slice is wrong (e.g. line moved by 1-2 lines), use ripgrep on a specific symbol name — do not fall back to grep -r.
- If you discover Part A's serde alias approach can't deserialize tagged-enum aliases on the wire, EITHER (a) keep the SQL migration as the canonical fix and skip serde aliases, OR (b) add a small per-row mapper in dispatcher filter / wave_file projection that maps the two old kind strings to new before dispatch. Pick the smaller diff and note it in the PR body.
