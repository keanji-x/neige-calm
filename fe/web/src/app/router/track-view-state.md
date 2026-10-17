# Track view state

The router owns a tab-local snapshot per Track. A plain selection of another
Track resumes its report, desktop-panel and grid scroll offsets, plus its
card/file/mobile-panel destination and Progress disclosure. Progress starts folded;
manual expansion or collapse resumes independently for each Track. Conversation selection continues to use the
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

The keyed Track body renders matching detail and actions immediately. Report
positions restore in layout after admitted complete report content commits; pending
geometry is not saved. Reader wheel/touch/scroll keys or a reading gesture suppress
deferred automatic restoration; header actions do not. Panel readiness is independent,
with a final board pass after the board's two-frame selected-card reveal. Only resume navigation overrides that
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

## Report preparation

The same signed-in router scope owns a report-feature preparation cache, separate
from the small UI snapshot map. Its weak keys are immutable report card objects
already held by the query cache. The report feature reuses pure syntax/outline/
reference data for that exact input; replacing the report card invalidates it.
No DOM, navigation callback, file port, subscription or timer is retained. The
cache neither keeps evicted query cards alive nor promises a memory cap while
queries remain reachable. Leaving the authenticated/recovery scope drops it.

TrackRouteBody composes generic UI admission and systems preparation leases through
the report typed adapter. Production injects per-job native Worker execution of
the canonical algorithm. Completed results bind to the exact input; changing or
leaving a source aborts the active job. ReportDocument receives that cloned model
and its own report identity atomically. Standalone renderers retain the canonical
synchronous contract. File admission and live authority remain current inputs.

Only an actually committed same-visit report can remain visible during replacement.
Its series revision, previews and task declarations remain coherent until swap.
New-source receipts wait for current complete content; unchanged-source overlay
updates retain existing behavior. Empty success commits through the same boundary.
Hash changes plus an explicit report-anchor activation token is served once after current content; an
ordinary report refresh does not replay a served anchor. Today uses this same path.
