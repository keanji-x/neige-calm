# Track row actions

Review tier: L2 — new persisted personal presentation choices and manual read state.

Each workspace Track offers a three-dot menu for global pin (the existing server
`pinned_at` contract), pin within its Area, and mark unread. Area pins are personal
browser preferences, scoped to origin/user/database, and survive kernel restart.
They reorder only the owning Area, before the existing limit; they do not add
Tracks to Pinned or alter recency. Existing pin/delete shortcuts remain available.
Manual unread is a separate database-scoped flag alongside the existing monotonic
receipt. It works without activity and before the first-entry baseline, and is
cleared by the existing visible-view read acknowledgement. No API or frozen key
contract changes are needed; the preferences owner defines the new keys.

Acceptance: actions never navigate, toggles reverse their state, Area pins survive
reload and do not change other groups, manual unread survives reload and clears
on opening, and database/user scope switches cannot leak either choice. Verify
real row and sidebar entry points, preferences persistence, browser interaction,
frontend gates and text ratchet. Two independent review channels explicitly check
abstraction boundaries, duplicate logic and hardcoded application assumptions.
