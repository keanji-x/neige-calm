# #573 fix-loop — dual-review findings

Both channels (subagent + `codex review`) agree on MUST-FIX. Subagent also raised SHOULD-FIX items below. Full reviews in `docs/_pr576-review-subagent.md`. Branch is now rebased on `origin/main`.

## MUST-FIX (both channels)

1. **Invalidation gap.** `web/src/app/invalidationPolicies.ts:162-176` — six event policies are `noop()`'d but the wave-fs projection's `runs/*`, `cards/*/events.json`, `cards/*/conversation.md`, and runtime-projected `cards/*/payload.json` derive from them. With the sidebar open during a worker turn, the viewer silently goes stale.

   For each of these event kinds, ALSO invalidate `waveFilesKey(...)`:
   - `task.completed`, `task.failed`, `codex.job_requested`, `terminal.job_requested` — affect `runs/*`
   - `codex.hook`, `claude.hook` — affect `cards/<id>/{events.json,conversation.md}`

   If `wave_id` is on the event payload, use `waveFilesKey(ev.data.wave_id)`. Where the payload only has `card_id`, use the existing `findWaveOwningCard` helper (see how `card.created/updated/deleted` policies do it). If neither is available, broaden to a wave-id-less `['wave-files']` invalidation as a fallback. Keep each policy's existing payload-handler logic — just add `waveFilesKey(...)` to the `keys` array.

   Add eventBridge tests covering one event of each kind that invalidates the wave-files cache.

## SHOULD-FIX

2. **Sidebar state not reset on waveId change.** `web/src/cards/builtins/wave-report-sidebar.tsx:18-20` — when WaveContext switches waves (route nav), the cached `expandedDirs` + `selectedPath` from the previous wave leak into the new one. Reset both on `waveId` change (effect or `key={waveId}` pattern). Add a unit test that re-renders with a new `waveId` and asserts the tree collapses.

3. **ARIA tree pattern.** Sidebar tree is currently buttons + lists. Apply the WAI-ARIA tree pattern:
   - Root `<ul role="tree">`, child `<li role="treeitem">` with `aria-expanded` for dirs and `aria-selected` for the active file.
   - Arrow-key nav: `↓/↑` move focus, `→` expand dir or move into first child, `←` collapse dir or move to parent, `Enter` activate.
   - Roving `tabIndex={0}` on the focused item, `-1` on the rest.
   - Add 2 tests: keyboard expand + select; aria-selected reflects state.

4. **Equivalence test gaps.** `crates/calm-server/tests/http_wave_file.rs` MCP↔HTTP equivalence loop currently covers only a subset of paths. Extend it to also assert equivalence for:
   - `cards/<id>` (ls)
   - `cards/<id>/conversation.md` (cat)
   - `wave.json` (cat)
   - `index.md` (cat)
   - `runs/<key>.md` and `runs/<key>.json` (cat, only if your fixture seeds a run; if not, add a seeded run via `tests/support/wave_file.rs`)

5. **Restore lost "why" doc-comments.** During the wave_file.rs → wave_fs_view.rs extraction, two rationale blocks were dropped:
   - `is_spec_verdict_event` doc-comment (was at original `wave_file.rs:619-625` explaining dispatcher-spawn failure scope vs verdict scope).
   - The `Event::TaskCompleted` arm comment ("Wave-scoped verdicts are routed to verdict, not completed…") at original `wave_file.rs:501-507`.
   Re-add them at the equivalent locations in `wave_fs_view.rs` so future readers don't lose the rationale.

## Out of scope (defer to follow-up issue, NOT this PR)

- `WaveFsEntry.extra` `#[schema(ignore)]` typing surface for TS.
- Malformed-cookie / dev-autologin auth coverage.
- The 5 NICE-TO-HAVE items in `_pr576-review-subagent.md`.

## Gates

- `cargo check -p calm-server && cargo clippy --all-targets -- -D warnings && cargo test -p calm-server`
- `cd web && npm run typecheck && npm test && npm run lint`
- `npm run gen:api` (only if you changed any `ToSchema`; should not be needed)

Write `docs/_impl-573-fixloop.md` (≤60 lines): what was changed, test deltas, anything you punted.

No grep -r. No code beyond what's needed for the items above.
