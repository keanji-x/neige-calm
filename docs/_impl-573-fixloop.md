# #573 Fix Loop Implementation

## Changed
- Added wave-file invalidation for `codex.hook`, `claude.hook`,
  `codex.job_requested`, `terminal.job_requested`, `task.completed`, and
  `task.failed`.
- Hook invalidation resolves `card_id` through cached wave detail and falls
  back to broad `['wave-files']`; current job/task payloads lack owner ids, so
  they use the same broad fallback.
- Reworked the wave report sidebar tree to `ul[role=tree]` /
  `li[role=treeitem]`, with `aria-expanded`, `aria-selected`, roving
  `tabIndex`, and Arrow/Enter keyboard handling.
- Keyed sidebar state by `waveId`, resetting expanded dirs, selected file, and
  focused item on wave navigation.
- Extended HTTP/MCP wave-file equivalence coverage for `cards/<id>` ls,
  `conversation.md`, `wave.json`, `index.md`, and seeded `runs/run-list.*`.
- Restored the dropped verdict-vs-run rationale comments in `wave_fs_view.rs`.

## Tests
- Added EventBridge coverage for all six wave-fs-derived event kinds and hook
  ownership fallback.
- Added sidebar tests for waveId reset, keyboard expand/select, and
  `aria-selected`.
- Extended `http_wave_file` equivalence assertions for the new paths.

## Verification
- `cargo check -p calm-server` passed with escalation after sandbox blocked
  `sccache`.
- `RUSTC_WRAPPER= cargo clippy --all-targets -- -D warnings` passed.
- `RUSTC_WRAPPER= cargo test -p calm-server` passed with escalation; sandboxed
  run failed only on socket handshakes with `Operation not permitted`.
- `npm run typecheck` passed.
- `npm run lint:js` passed.
- `npm test` and full `npm run lint` are blocked on local Node `v18.19.0`:
  Vitest/Rolldown requires `node:util.styleText`, and Stylelint uses JSON
  import attributes.

## Punted
- Out-of-scope review items stayed untouched.
- `npm run gen:api` not run; no `ToSchema` changes.
