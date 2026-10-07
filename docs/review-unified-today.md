# Unified Today review

Review tier: L2, because removing the retired frontend implementation produces a large diff. This change does not alter backend APIs, authorization, storage, or released migrations.

## Source and ownership review

Reviewed the full diff and the actual homepage → daily Track resolution → TrackRouteBody → TrackPage call path in an isolated checkout. The legacy URL redirects to this same route, including a historical `day`; no singleton renderer remains. Calendar date selection remains local to the agenda, while the report and history stay bound to the resolved daily Track.

- Abstraction boundaries: the app injects the existing calendar adapter and handles navigation; Calendar owns scheduling; Track owns its document/sidebar/conversation geometry. The generic sidebar slots contain no Daily/template identity checks. Backend singleton APIs and data remain untouched.
- Duplicate logic: removed the old Today route and feature, including the parallel report, conversation, activity and compact implementations. Kept the existing Calendar adapter, Track report reader, conversation lifecycle, and report evidence components as the authoritative owners.
- Hardcoded application assumptions: the shared Track page consumes optional composition slots rather than inspecting Track ids, titles or templates. The app owns which route supplies those slots. Retired directory ownership is an orchestrator-approved change recorded in the commit trailer.

Retired only tests and mutations for the removed singleton UI. Shared scheduling, Track, read/write failure, navigation, receipts, activity and conversation tests remain. The latest session-recovery coverage was moved to the real daily homepage. The shared router remains above the preferred source-file size, but this change removes its obsolete route instead of adding an unrelated split.

## Independent functional and contract audit

Used production-router browser tests, source/ownership/dependency gates, a single-factor production mutation in an exclusive verification checkout, and a separate native backend serving the production bundle. These checks exercise the declared owner contracts without copying production policy into a fixture.

- Abstraction boundaries: frontend dependency/ownership gates pass; the native instance uses the existing daily/calendar/report APIs, with no singleton request introduced by the unified entry. The original Tier 1 daily read case passes against the isolated native instance.
- Duplicate logic: both legacy URL tests resolve to the sole homepage and reject singleton API calls. Removing the homepage's calendar injection fails exactly the two predicted browser regressions; restoring the exact production bytes returns both to green.
- Hardcoded application assumptions: ordinary Track board geometry and sticky panels pass alongside daily-page tests. Compact home remains Today; starting an Area composer requires an explicit selection. The mobile header always publishes its declared title/actions slots and follows their populated content, avoiding a URL-derived second menu on the homepage. Session recovery uses the shared production drawer on the homepage and retains its transcript/draft.

Browser inspection found and fixed the shared header offset leaking into the sidebar conversation host. The final native page and browser geometry tests retain Calendar above the conversation and keep the Send button in the viewport at 768px and 1000px heights. Calendar plugin unavailability continues to use the existing adapter's feedback and date surface.

The final focused browser selection passes 76 tests. The real Playwright Areas-read recovery case passes against the native backend. Docker Tier 1 setup was attempted but could not allocate a network from the host's exhausted default IPv4 pools; the native run exercises the same Tier 1 case, not a successful Docker run. No real Codex E2E was run, and the native test backend is loopback-only with independent temporary data and `/bin/false` as its Codex binary.

A final compact-menu audit reproduced two Track menus on the homepage. Reused the existing slot-driven header correction already present in the primary checkout, preserving that unrelated checkout without editing it; added a real homepage regression that opens Conversations from the one usable menu.

No unresolved blocking finding remains in these source and executable audit channels.
