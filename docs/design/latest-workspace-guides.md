# Contextual onboarding on the current workspace

Base: `b09a46389` (main, 2026-10-17). Outcome: the optional JSON-driven first-use
guides and verified continuation/preview fixes work on the current application,
including its contextual work card. Keep current layouts and capabilities.

The previous candidate is an implementation reference, not a replacement for
current main. Port its scoped guide lifecycle and reusable Floating UI callouts;
adapt every anchor and description to the current controls. In particular, the
desktop work card now groups conversations, active work and related material,
so it must not teach the former Tasks / Tools / Conversations layout.

Existing projects remain opt-in; new empty workspaces can progress through
native Add Area, Template, Folder and request controls. Skip and replay remain
available. Closed work continues through the existing capability-fenced reopen
mutation. Saved-result previews refresh after report/task updates. Network
guidance explains restart and Planner Ready checks.

Review tier: L2 because the port includes first-use preference persistence and
scope fencing, as well as a substantial frontend integration diff. No backend
contract, authorization rule or database migration is changed.

Dependency ownership decision (tooling/architecture): carry forward the existing
Floating UI request #2735 for the same optional callouts. Approve only the
`@floating-ui/react` dependency and its real generated lock entries in
`fe/package.json` and `fe/package-lock.json`; keep all enforcement rules intact.
Preserve the original ownership trailers on the port commit and any eventual PR.

Acceptance: scoped progress tests, preference tests, catalogue validation,
mounted HTML refresh regressions, continuation success/failure tests, current
context-card tests and focused desktop/mobile browser checks pass. Verify the
new build in the real preview and replay the native first-use controls. Run
frontend lint/build and text gates, with Tier 1 integration coverage; use two
independent review worktrees. Preserve the user's main checkout and all local
uncommitted work.
