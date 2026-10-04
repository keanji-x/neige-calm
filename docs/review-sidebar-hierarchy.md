# Sidebar hierarchy review (#2048)

Review tier: L1 — this changes desktop sidebar presentation and its browser regression checks without changing authority, persistence or resource membership.

Source/history review traced the regression to PR #2032 (`4590bce3f`, 2026-10-04 14:27 Asia/Shanghai). The old rail-edge section label was replaced by a shared header that reserves the same leading gutter as an Area. A real Chromium reproduction measured both labels at x=40 and the parent/child column assertion failed. The fix gives section-level hosts their original label column, retaining the shared disclosure/list implementation and existing Area/Track row geometry.

Abstraction boundaries remain intact: the shell owns group geometry and consumes the disclosure's `aria-expanded` state. It hides only the decorative section marker when expanded; collapse, hover and keyboard focus reveal it. Actions, focusable labels and membership remain unchanged. Duplicate logic is avoided by styling the shared section host once, without a second disclosure component. No Area name, template identity or application identity is special-cased.

A separate browser-contract check exercises measured hierarchy and every marker state against the actual DOM/CSS. Four focused Chromium checks and 50 sidebar/Area behavior tests passed. Fresh review of the complete diff found no unresolved actionable finding. The Daily Planner changes are excluded from this PR.
