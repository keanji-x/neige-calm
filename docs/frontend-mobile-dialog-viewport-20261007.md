# Mobile modal viewport change request

Status: approved by the task orchestrator for the user-requested full mobile interaction review; tracked in [#2400](https://github.com/keanji-x/neige-calm/issues/2400).
Owner: `ui/dialog`; existing frozen props and focus/inert contracts remain unchanged.

Area creation/editing currently centres the modal inside the layout viewport, leaving its actions below an open mobile keyboard. The production browser reproduction sets visualViewport height to 480 and observes the panel bottom at 505.

The dialog now consumes the declared `ui/viewport` geometry contract. Its overlay fits the visible viewport including offsetTop, and the panel maximum height uses that viewport with the existing spacing token. This fixes the shared modal geometry rather than adding Area-specific selectors or duplicating keyboard logic in an app host. Desktop dimensions are unchanged when visual and layout viewports agree.

Acceptance: action buttons remain reachable at reduced height and nonzero offset; focus and the draft survive resize; cancelling restores the original navigation. Re-run existing dialog focus/inert and child-view contracts plus the integrated Area browser reproduction. Any future commit touching this frozen owner must carry the repository-required ownership change-request trailer referencing this decision.
