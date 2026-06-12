# #573 Phase 1 — backend wave-fs HTTP endpoints

Issue: #573. Design + impl notes in `docs/_explore-573-report-sidebar.md` — follow it. This phase is BACKEND ONLY (no frontend touch).

## Do

1. Extract `wave_ls` / `wave_cat` projection logic from `crates/calm-server/src/mcp_server/tools/wave_file.rs:84,144` into a new module `crates/calm-server/src/wave_fs_view.rs` with pure async fns. Existing MCP handlers must delegate (no behavior change for MCP callers; existing `tests/mcp_wave_file.rs` must still pass).

2. Add HTTP routes in `crates/calm-server/src/routes/waves.rs`:
   - `GET /api/waves/{id}/files/ls?path=<logical_path>` (path optional)
   - `GET /api/waves/{id}/files/cat?path=<logical_path>` (path required)
   Mount next to existing wave-scoped routes (file:67 / :80 area). Reuse the `WaveFsView`.

3. Define DTOs `WaveFsEntry { name, kind, size?, updated_at? }` and `WaveFsContent { content, content_type }` with `utoipa::ToSchema`. Register both endpoints in `crates/calm-server/src/openapi.rs:58-65`.

4. Auth: use existing `auth::require_session` + `Principal` extractor pattern. Single-owner — do NOT add wave-ownership check; leave a `// TODO(#573 multi-user): ownership check` comment at the spot the explore doc identifies.

5. Errors: missing wave → 404; unknown path → 400 (preserve MCP message); cards/<id> outside wave → 403; missing session → 401 (auto from middleware).

## Tests

Add `crates/calm-server/tests/http_wave_file.rs`. Seed one wave with one card + one report; assert HTTP `/files/ls` and `/files/cat` for representative paths (`/`, `cards`, `runs`, `cards/index.json`, `report.md`, `cards/<id>/.payload.json`) match the equivalent MCP `tools/call` output byte-for-byte after JSON parse. Add 401 / 404 / 400 / 403 negative cases. Reuse fixture style from `tests/mcp_wave_file.rs` — if its helpers are private, lift the minimum to `tests/support/wave_file.rs`.

## Gates before declaring done

- `cargo check -p calm-server`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test -p calm-server --tests` (single-threaded if dispatcher flakes)
- `cd web && npm run gen:api` then commit both `openapi.json` and `generated.ts`. No other web changes this phase.

## Out of scope (do NOT touch)

- `web/src/cards/builtins/wave-report.tsx` (Phase 2)
- Any new React component, CSS
- `calm.wave.ls`/`calm.wave.cat` behavior or descriptors

Write a brief PR-style summary to `docs/_impl-573-phase1.md` (≤80 lines) with: files changed, test results, follow-ups.

Don't grep -r. Use file:line slices from the explore doc.
