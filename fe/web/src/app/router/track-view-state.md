# Track view state

The router owns a tab-local snapshot per Track. A plain selection of another
Track resumes its report, desktop-panel and grid scroll offsets, plus its
card/file/mobile-panel destination. Conversation selection continues to use the
existing UiPreferencesProvider, including explicit close and reload behavior.
Leaving still unmounts the route and releases its card hosts and subscriptions.
SessionGate unmounts the router tree on sign-out; snapshots do not survive it or
a page reload and are never written to browser storage or the server.

Navigation owns the URL and the `ncResumeTrackView` history intent. A plain
cross-Track selection sets that intent and copies only the destination's
card/file/panel fields. The new navigation supplies its own `from`; panel/file
history-push markers are not copied. An explicit card or block target takes
priority, including an explicit link to the same previously selected card.
Closing a resumed file therefore replaces the current entry rather than
popping into the Track just left.

The keyed Track body waits for detail before restoring its DOM scrollports.
Report/panel positions are restored in layout, with a final pass after the
board's two-frame selected-card reveal. Only resume navigation overrides that
reveal. Scroll listeners and pending animation frames are disposed with the
body. Conversation content, failed-send recovery and draft identity remain
owned by ConversationProvider; conversation selection remains owned by UiPreferencesProvider.

Acceptance is covered through production entry points:

- `track-conversation.test.tsx`: open drawer survives A → B → A, without leaking
  into B; explicit close survives the same trip.
- `track-view-navigation.test.tsx`: mobile-panel and file destinations resume;
  new return sources and close/history semantics remain intact; explicit
  same-card and anchor targets are distinguished from resume.
- `track-view-state.browser.test.tsx`: real router, shell, report and BoardHost
  retain independent report/grid offsets through actual sidebar clicks; an
  explicit same-card link still reveals the card.
- The scroll assertion was mutation-verified by replacing the production
  restored vertical offset with zero: exactly the predicted browser test failed,
  and passed again after exact restoration.

The new Context has one exact-path architecture exception with positive and
sibling-negative fixtures. No feature API, global stylesheet, backend contract
or persisted schema changes are required.
