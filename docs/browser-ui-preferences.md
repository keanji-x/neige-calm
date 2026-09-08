# Browser UI preferences

Outcome: returning to a Track or reloading the browser restores its last open
existing conversation, each Area disclosure, and the manual sidebar width choice.
An explicit close/collapse remains closed/collapsed across navigation.

Persist only IDs and display flags under versioned browser-local keys, using the
existing injected storage boundary. Conversation contents, drafts, attachments,
and server state retain their existing ownership and are not persisted here.
Stored conversation IDs must resolve through the current route’s server-backed
rows before any transcript is opened. Unavailable or malformed storage must leave
the UI usable, with in-memory preferences for this application instance.

Acceptance: reproduce Track A → B → A and explicit close; preserve Area collapse
across Track changes and remount; restore from a fresh preference store; ignore
invalid values and unavailable storage; verify actual browser navigation/reload.

Final validation (2026-09-08, run from `fe/`):

- `npm ci --no-audit --no-fund`: installed the locked dependencies in this worktree.
- `npm run lint`: passed, including architecture, ownership and CSS gates.
- `npm run build`: passed.
- `npm test -- --maxWorkers=4`: 3,069 passed; one existing test skipped.
- `npm run test:browser -- --maxWorkers=2`: 405 passed.
- `FE_DEV_PORT=15180 FE_API_PROXY_TARGET=http://127.0.0.1:14041 npm run e2e -- e2e/browser-ui-preferences.spec.ts`:
  passed against an isolated native server with an in-memory database and the
  repository's `osc-probe-child` Codex stand-in; inspected the browser screenshot.
- Mutation check: disabling the production conversation restore read failed
  exactly the two predicted route-return and fresh-router restore tests.
  Restored the production file byte-for-byte; all 101 focused tests passed.

Two independent Agent reviews checked the complete rebased diff in separate
worktrees. Both reported no blocking or in-scope actionable findings. Review A
independently passed 115 related tests; Review B passed 205 focused tests and
28 additional Today/sidebar contract tests. Runtime verification also covered
actual browser navigation/reload and storage-failure tests.
The focus regression and the old navigation-as-close expectation found during
verification were corrected; the final checks above cover the corrected code.
Storage contains only display flags and conversation IDs. Missing/deleted IDs
cannot open a transcript unless they resolve through the current route's rows.

Rollback: revert this frontend change. No server migration or API change is
needed; the versioned preference keys can remain and will be ignored by the old
frontend. Browser choices are local to this origin, not synced across devices.
