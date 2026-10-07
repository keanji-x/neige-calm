## Outcome

Expand the extracted runtime into one `plugin` crate containing protocol/execution, generic host lifecycle, and builtin Calendar/Gitforge domain implementations. Keep kernel authorization, transactional persistence, event publication and operation execution in server adapters.

## Boundaries and invariants

- No reverse dependency on calm-server, AppState or AppContext, and no SQLite/Axum dependency in plugin.
- Builtins supply declarations and domain behavior; generic host/transport code consumes registrations rather than special-casing plugin identities.
- Store/list/lifecycle ports retain typed records and original server error semantics; callback identity is resolved by the kernel, never request parameters.
- Calendar mutations retain a single atomic transaction for receipt+entry+events and for wake version/open-track/cursor checks+cursor+event.
- Gitforge publication retains candidate verification, idempotency payload/probes and the kernel operation executor.
- Preserve wire/OpenAPI, persisted values and byte-frozen migrations; preserve existing tests while moving ownership.

## Verification plan

Review tier L2: cross-crate authority/persistence/isolation boundaries and large move. Two independent read-only review worktrees must converge on the final head.

| Check | Place | Evidence / completion |
|---|---|---|
| Plugin library focused/domain tests | implementation | nextest passes on plugin |
| Actual server plugin/MCP/calendar/publish paths | implementation gate | selected nextest suite passes, nonempty selection |
| Critical credential and wake/lifecycle assertions | exclusive mutation worktree | predicted red set equals actual; restore and green |
| Text and source ratchets | local gate | local-ratchet-gates.sh passes |
| Package contracts | local gate | local-contract-gates.sh passes |
| Compile/lint/release/OpenAPI | local gate | local-rust-gates.sh --quick passes |
| Broad workspace/browser/stack coverage | CI | required exact-PR-head checks pass |
| Boundaries, duplicate policy, identities, callers and test parity | independent reviews | two L2 verdicts with no blockers |

Related to #2371/#2373 and the measurement roadmap #1983. No compile-speed claim.
