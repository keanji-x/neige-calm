# PR #576 review (subagent channel)

Branch: `feat/573-report-sidebar` at /mnt/data2/kenji/neige-calm/.claude/worktrees/573-report-sidebar/
Base: `origin/main` @ `c82b8040` (PR merged off-base — see MUST-FIX-1)

Scope reviewed (PR commits b7f733eb + 968a20c9 only — i.e. `git diff HEAD~2 HEAD`):
- Backend: `crates/calm-server/src/{wave_fs_view.rs,routes/waves.rs,mcp_server/tools/wave_file.rs,openapi.rs}`, `tests/{http_wave_file.rs,support/wave_file.rs,support/mod.rs}`
- Frontend: `web/src/{cards/builtins/{wave-report-sidebar.tsx,wave-report-sidebar.test.tsx,wave-report.tsx,wave-report.test.tsx},api/{calm.ts,queries.ts,generated.ts,openapi.json},app/{invalidationPolicies.ts,eventBridge.test.tsx},calm.css}`, `web/e2e/wave-report-sidebar-files.spec.ts`
- Verified the non-PR diffs surfaced by `git diff origin/main..HEAD` are stale-base artifacts (this branch sits at `c82b8040`; main has since merged #560/#561/#563/#564/#566/#567/#568/#571). The squash merge GitHub computes from the PR's own commits will NOT revert those — but reviewing against `origin/main..HEAD` would be misleading.

---

## MUST-FIX

### MUST-1. Rebase before merge to surface real conflicts
The branch is 8 commits behind `origin/main`. Squash merge based on the PR's own diff is safe — `git diff HEAD~2 HEAD` only touches the wave-files surface. But two cross-cutting risks justify a rebase first:

1. PR #561 deleted `boot_harnesses` (`crates/calm-server/src/lib.rs`); this branch still publishes the new `pub mod wave_fs_view;` against the old layout. A rebase will replay cleanly but should be run+tested first. (The boot_order_tests fixture is the bellwether — those will recompile against the new shape.)
2. PR #571 added `--font-nav-*` typography tokens and a `.cove-block` wrapper. The new `.wave-report-files*` block is purely additive at line 2742+; no collisions. But the user-visible verification should be done after rebase so the file viewer sits next to the new cove-tinted sidebar, not the pre-#571 sidebar.

Action: rebase onto current `origin/main`, re-run `cargo test -p calm-server`, `npm test`, `make` in worktree, re-run E2E. The "Preview needs worktree binary" memory applies — this PR ships backend changes.

### MUST-2. `invalidationPolicies.ts` — `task.completed`/`task.failed`/`codex.hook`/`claude.hook`/`codex.job_requested`/`terminal.job_requested` should invalidate `waveFilesKey`
File: `web/src/app/invalidationPolicies.ts:162-175`

The wave-fs projection surfaces `runs/index.json`, `runs/<key>.{md,json}`, `cards/<id>/conversation.md`, and `cards/<id>/events.json` — all derived from these six event kinds. After this PR all six remain `noop()`. If the user keeps the sidebar open on `cards/<id>/conversation.md` (the most natural reading flow during a long worker turn) **none of the streaming hook events trigger a refetch**. The viewer will silently show pre-turn content until the user manually re-selects or until a `card.updated` event arrives via runtime projection.

Recommended fix: add `keys: (ev) => [waveFilesKey(ev.data.wave_id)]` (or scoped to the specific path family) for these six event kinds, OR document the gap explicitly in the `noop()` reason so the next reviewer doesn't think it's intentional.

---

## SHOULD-FIX

### SHOULD-1. `wave_fs_view.rs` lost two load-bearing comments during extraction
File: `crates/calm-server/src/wave_fs_view.rs`

The original `mcp_server/tools/wave_file.rs` carried doc comments explaining:

- Why `is_spec_verdict_event` excludes `KernelDispatcher` (dispatcher spawn-failure path emits Wave-scoped `TaskFailed` as `ActorId::KernelDispatcher` while preserving wave scope; those failures are run failures, not verdicts).
- Why the worker-self-report `TaskCompleted` falls through `record_latest` (dispatcher retry after spawn failure → keep the latest).

Both comments are dropped in the new module (lines 487-503 and 605-607). These are the kind of "why" comments that prevent a future Rust dev from "simplifying" the predicate to `matches!(scope, EventScope::Wave { .. })`. Re-add them.

### SHOULD-2. `WaveFsEntry::extra` is invisible to the OpenAPI schema and TS client
File: `crates/calm-server/src/wave_fs_view.rs:309-323`, `web/src/api/generated.ts`

`#[schema(ignore)]` on `extra` means the generated TS `WaveFsEntry` only types `{ name, kind, size?, updated_at? }`. The `runs/` listing flattens 7 additional fields (`idempotency_key`, `status`, `run_kind`, `verdict`, `requested_at`, `finished_at`, `worker_card_id`) but the frontend can't see them via TS without `as any`. Sidebar today doesn't render those — so this is latent, not broken — but the sidebar's `entryLabel` already special-cases the `cards/` parent to combine `kind` + `id`; the natural follow-on for `runs/` would be to show the status badge from `extra.status`, which TS will reject. Either:

- promote those run-listing fields to a typed variant (`WaveFsRunEntry` with explicit fields), or
- drop `#[schema(ignore)]` and document the polymorphism with `additionalProperties: true`.

### SHOULD-3. `WaveReportSidebar` does not reset `selectedPath` / `expandedDirs` when `waveId` prop changes
File: `web/src/cards/builtins/wave-report-sidebar.tsx:19-20`

Empirically each wave gets its own report card id, so the parent re-mounts and state is fresh. But the prop accepts a string and there is no `key` discriminator at the call site (`wave-report.tsx:544`). If a future refactor reuses the card slot (Issue #480 / card-slot lifecycle was already shifting these mounts), a stale path from wave A will be requested against wave B — the server will 400/403/404 (handled), but it's a confusing UX dead end. Add:

```tsx
useEffect(() => {
  setSelectedPath(null);
  setExpandedDirs(new Set());
}, [waveId]);
```

### SHOULD-4. Tree widget has minimal accessibility
File: `web/src/cards/builtins/wave-report-sidebar.tsx:39-60, 154-174`

The tree uses a `<div aria-label="Wave files">` wrapping `<button aria-expanded>` rows. Missing:
- `role="tree"` on the container
- `role="treeitem"` on each row, with `aria-level={depth+1}`
- `role="group"` on the recursive child wrapper
- `aria-selected={selectedPath === path}` on file rows
- Keyboard navigation: ArrowDown/Up to move focus, ArrowRight/Left to expand/collapse dirs, Enter to select files. Tab currently moves through every row (each is a `<button>`), which is acceptable but not idiomatic tree behavior.

For an internal tool the current shape is workable but the issue's design doc cites the wave-report card as "primary surface for inspecting wave state" — worth meeting the WAI-ARIA tree pattern.

### SHOULD-5. `tests/http_wave_file.rs` MCP-vs-HTTP equivalence is shallow
File: `crates/calm-server/tests/http_wave_file.rs:55-90`

The "match MCP" loop only covers 3 ls paths (`/`, `cards`, `runs`) and 4 cat paths. It misses:
- `cards/<id>` (per-card directory ls — exercises the 4-entry listing)
- `cards/<id>/meta.json`, `events.json`, `conversation.md`
- `runs/<key>.md`, `runs/<key>.json`
- `wave.json`, `index.md`

A worker card is materialized so most of these would resolve. The `conversation.md` path in particular is what the sidebar will hit most often; not asserting MCP-byte parity here leaves the door open for a future MCP-only refactor to drift from HTTP. Either widen the path matrix or use a path generator + smoke-loop pattern so the asymmetry can't grow.

### SHOULD-6. Missing test for the auth bypass surface
File: `crates/calm-server/tests/http_wave_file.rs`

`missing_session_returns_401` is present but the cookie path is tested only as a positive. Worth adding:
- A request with a *malformed* session cookie (header present but unsigned/expired).
- A request that hits the route during the dev-autologin path (config `dev_autologin=true`) — to make sure that path still routes through Principal and not raw.

These guard against future regressions where `Principal: Principal` could be replaced with an `Option<Principal>` extractor in a refactor — the compiler wouldn't catch it.

---

## NICE-TO-HAVE

### NTH-1. `WaveFsCatQuery::path` could be a required `String`
File: `crates/calm-server/src/routes/waves.rs:88-91, 130-135`

`Option<String>` + manual `ok_or_else(... "missing path")` mirrors the MCP error message exactly. Either keep the parity comment ("matches MCP error text") or make the type `String` and let axum's Query extractor handle the absence.

### NTH-2. `listWaveFiles` accepts `'/' ` as a non-empty path
File: `web/src/api/calm.ts:208-216`

`if (path != null && path.length > 0)` — `'/'.length === 1`, so calling `listWaveFiles(id, '/')` will send `?path=%2F`. Server's `normalize_path` happens to handle it (`'/' → ''`), so this works, but the comment in `useWaveFileList` callers all pass `''`. Either trim `'/'` here or document the contract.

### NTH-3. `cards_updated_at` always called but the result discarded when no cards
File: `crates/calm-server/src/wave_fs_view.rs:53-55`

The `cards/` branch of `ls` computes `cards_updated_at(...)` and writes it onto the synthetic `index.json` entry only. For empty waves this falls back to `wave.updated_at` — same value as the parent. Cheap, but the loop materializes `cards` twice (once at the top, then inside `cards_for_wave` via the entry filler) for the same ls call. Could short-circuit by caching `cards` on the view. Skip if perf isn't measured to matter.

### NTH-4. `parseCardKinds` runs synchronously inside `useMemo` over potentially-large JSON
File: `web/src/cards/builtins/wave-report-sidebar.tsx:285-303`

The `cards/index.json` payload is fetched once on first expand of `cards/`. For a wave with thousands of cards it's a 100kB+ parse on the render thread. Internal scale isn't there yet, but worth a `useTransition` or background task when the wave card-count grows.

### NTH-5. TODO format works but consider linking the spec issue
File: `crates/calm-server/src/routes/waves.rs:124, 159`

`// TODO(#573 multi-user): ownership check` — discoverable by `rg 'TODO\(#573'`. Future-grep-safe. Marginal: add a sentence about the boundary ("once multi-user lands, scope to principal.wave_set").

---

## Cross-cutting / style

- The `runs/index.json` and `cards/index.json` files share a "virtual file" pattern not encoded anywhere — they are listing entries (kind=file) that don't exist on disk. The dispatcher-side comment about RESERVED_RUN_KEYS catches the `runs/index` collision, but `cards/index` has no similar guard. Today no card-id can be the literal string "index" (card_ids are prefixed), so this is fine — note in the module header that the safety is provided by the card-id minting scheme, not by an explicit check.
- `WaveFsView::new` takes `&'a dyn RouteRepo` + `&'a WriteContext`. Both are `Clone` (cheap arc-wraps), and the view is a transient handle. The lifetime gymnastics buys nothing at the boundary — taking owned `Arc<dyn RouteRepo>` + `WriteContext` would simplify call sites slightly. Skip if it'd cascade.

---

## What's solid

- The HTTP/MCP code path split is clean — MCP keeps role gates, HTTP keeps Principal+wave-lookup, and both funnel through the same `WaveFsView`. The 1:1 JSON byte-equivalence claim survives the inspection: `WaveFsEntry`/`WaveFsContent` serialize identically whether routed via MCP `serde_json::to_value` or axum `Json`.
- The 5-test http_wave_file.rs matrix correctly covers the four error shapes: 401 (no session), 403 (cross-wave card), 404 (missing wave), 400 (unknown path with the exact MCP error string).
- `support/wave_file.rs` is well-isolated under the test-support module (`tests/support/mod.rs:1`) — nothing leaks publicly.
- Frontend lazy expansion via `useWaveFileList(..., { enabled: expandedDirs.has(...) })` keeps the cards index quiet until the user opens `cards/`. React Query keying is correct.
- The fallback story (sidebar collapses to `ReadOnlyView` when `waveId === null`) is preserved at `wave-report.tsx:543-548`.
- The new CSS namespace `.wave-report-files-*` is disjoint from existing `.wave-report-card`/`.wave-report-edit-button`/`.wave-report-empty` selectors — no collision.
- E2E spec drives the real click→viewer path through a fresh cove/wave, not a fixture stub.
