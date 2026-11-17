# Conversation approval verification

Review tier L2: execution authority and persisted conversation identity.
See [the design](design-conversation-permissions.md). Runtime evidence and
revision comparisons belong in ignored artifacts and Report/PR.

Focused Rust tests must select registered binaries explicitly, unset
`NEIGE_CODEX_BIN`, clear `RUSTC_WRAPPER`, cap build jobs at six, and use
`--no-tests fail`. Exercise real projector/registry, MCP, authenticated HTTP and
native provider paths. Valid conversation fixtures supply `permission_mode`
explicitly; the production reader must keep rejecting a missing choice.
Historical migration fixtures and deliberate malformed-input cases remain
distinct from current output contracts.

The CI closure covers calendar grant durability/revocation/CAS, shared Assistant
tools, malformed/cross-Track/retired-session reports, read-only persisted terminal
exits, actual native thread identities, version floors and activity high-water
preservation. Phase2 recovery must retain restored authority and queue facts,
consume the restored queue once, and count delivery after shutdown as well as
at first observation. Budget tests exercise every real role/profile branch; VCS
tests retain both current-head diff delivery and absence of an Assistant
transcript-refresh write.

The kernel gate owns final verification against its frozen target:

- CI text gates, Rust quick checks and full contract suites.
- Affected Rust owner namespaces in the five registered CI failure binaries.
- Frontend lint/build and full units with `--maxWorkers=2`.
- The five ModelPill browser consumer files, including the complete 390px
  provider file with its original alignment threshold, truncation and menus.
- Mutation infrastructure fixtures.
- The complete three-case `track-conversation-create.spec.ts` against a private
  production backend, explicit fake provider and execution supervisor. Preserve
  exact request bodies, 201/idempotency, first-turn/history and archive/restore
  assertions. Keep DB, HOME, ports and process cleanup isolated.

The two refreshed activity mutations run serially against production sources
with their original expected failure sets and complete witness files. Compare
the entire actual red set, reverse each patch, verify byte-identical restoration
and run the same complete files green. They may run after the gate on its clean
commit; exact source restoration preserves the gate verdict. No test-selection
CLI or additional product mechanism is needed.

Use the real generator whenever wire/schema outputs change and include every
updated artifact. CI owns broad Rust suites; do not run workspace Rust tests,
full Rust gates or real Codex E2E on the shared host. Repeat checks only when
their inputs or covered behavior change, apart from the intended gate replay.
The Planner arranges final independent L2 review.
