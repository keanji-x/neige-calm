# Today Launchpad design review — confirm round r2 (Codex)

## Verdict

The r1 conceptual blockers are **closed in direction**, but r2 is **FIX-AGAIN, then ready-to-slice**, not yet ready-to-slice verbatim. The server-owned marker/no-seed bootstrap closes B1/M1; identity-derived scope plus a narrow typed-event gate closes B3; persisted folder repo identity closes M4; and a card-topic proposal event is consistent with the existing frontend transport. Two additions remain materially underspecified: concierge-only `tools/list` cannot be implemented by merely adding a predicate to the static registry because unresolved daemon discovery deliberately returns a role union, and “transactional dedup constraint” cannot reliably key normalized repo/issue data buried in JSON without defining indexed relational columns (or accepting a weaker in-transaction check). Also make the launchpad cwd a real server-managed directory and specify legacy Today adoption/localStorage reconciliation. None requires rethinking propose-then-confirm.

## Findings

### CLOSED-OK — BLOCKER — B1/M1: a marked, server-owned singleton can boot idle and race-free

**Evidence.** SQLite already uses exactly the required singleton pattern: `idx_coves_one_system` is a partial unique index on the marked value (`crates/calm-truth/migrations/0009_coves_kind.sql:29-35`). An analogous `waves.purpose` column plus `UNIQUE ... WHERE purpose = 'launchpad'` is feasible; if the singleton is intended per system cove rather than process-wide, index `(cove_id, purpose)` and state that invariant. Empty/absent goal produces an empty pending queue (`crates/calm-server/src/harness/mod.rs:227-232`), whereas normal wave creation currently derives the goal from title and embeds it in the spec-card payload (`crates/calm-server/src/routes/waves.rs:563-576`). Thus a dedicated transaction can mint marker + cards without a `WaveGoal`, then start the harness after commit.

**Fix.** Keep Slice A, but specify the exact partial unique index and implement ensure as insert-or-select/retry under that constraint, returning the winning row after a uniqueness conflict. Create all wave/card/terminal records in one transaction; start the harness only for the winner and report dormant/start-failed explicitly.

### NEW — MAJOR — “safe cwd” must be an existing server-managed directory, not merely a non-root string

**Evidence.** Wave POST validates only absolute-path shape and normalization (`crates/calm-server/src/routes/waves.rs:368-379`); system coves deliberately skip folder attachment while retaining cwd for the daemon (`crates/calm-server/src/routes/waves.rs:381-399`). The spec-start payload accepts cwd as an unchecked string (`crates/calm-server/src/operation/spec_harness_start_adapter.rs:192-207`) and passes it through to app-server thread start (`crates/calm-server/src/shared_codex_appserver.rs:695-715`, `:743-751`). There is no local existence/directory validation in that path, so a synthetic nonexistent cwd defers failure to the provider.

**Fix.** Define a stable server-owned launchpad directory, create it before ensure commits/starts (or use a known existing application data directory), canonicalize it, require `is_dir`, and fail ensure clearly if preparation fails. Do not attach it to `cove_folders`.

### RESIDUAL — MAJOR — legacy Today adoption/reset and cached terminal reconciliation need a deterministic policy

**Evidence.** The old UI caches only `calm.todayCardId`; when that card still resolves, it returns immediately without rediscovering the wave (`web/src/hooks/useTodayTerminal.ts:65-73`). Generic Today discovery is first-title-match and creation is non-atomic (`web/src/hooks/useTodayTerminal.ts:165-196`). Resetting only the adopted spec thread is compatible with retaining the terminal card, but replacing the terminal/card set can leave the cache pointing at the legacy terminal and bypass the new ensure result.

**Fix.** Define adoption deterministically (for example oldest eligible unmarked Today in the system cove), mark it inside the singleton transaction, preserve its terminal card/terminal when valid, and force-new-thread/reset only its spec runtime/transcript according to the stated product policy. Make the Today UI always call ensure and overwrite/remove the legacy localStorage key from the returned `terminal_card_id`; never let the cache bypass ensure. Concurrent adopters must converge through the unique marker and re-read the winner.

### RESIDUAL — BLOCKER — B2 is closed for `tools/call`, but `tools/list` needs an explicit contextual discovery design

**Evidence.** `ToolCallIdentity` already contains card, optional wave, and cove (`crates/calm-server/src/mcp_server/registry.rs:96-109`), and kernel dispatch resolves it before invoking the handler (`crates/calm-server/src/mcp_server/transport.rs:609-613`), so a handler can query wave marker and deny unknown/unmarked callers. Discovery is different: descriptors are statically filtered by role (`crates/calm-server/src/mcp_server/registry.rs:320-338`). Resolved thread and card-bound branches do have identity/card context (`crates/calm-server/src/mcp_server/transport.rs:420-438`, `:469-507`), but unresolved daemon `tools/list` deliberately returns the union of role-visible descriptors (`:440-466`). If the new descriptors are Spec-visible, every unresolved daemon sees their names; if hidden with `&[]`, current helpers can never add them.

**Fix.** Keep both descriptors hidden from static role discovery (`visible_to_roles: &[]`). Add a dedicated async contextual augmentation step only after a successfully resolved, active identity whose card→wave row has the launchpad marker; add the two descriptors there. For no thread, unresolvable thread, cross-session, missing wave/card, dormant session, or DB error, add nothing. Independently call the same fail-closed predicate at the beginning of each handler after dispatch identity resolution. Test every transport branch, not just a normal resolved thread.

### CLOSED-OK — BLOCKER — B3: a narrow `AiSpec` self-scope arm fits the truth gate

**Evidence.** Session resolution reads the live session, rejects inactive/cardless state, verifies the resolved card is Spec-roled, then converts it to `AiSpec(card)` before `enforce_role` (`crates/calm-truth/src/decision_gate.rs:77-119`). `enforce_card_self_scope` is a reusable card/wave/cove comparison already called for Worker and ReportCard actors (`crates/calm-truth/src/role_gate.rs:413-456`). The existing special families are independent `if matches!` arms (`role_gate.rs:176-267`, `:346-371`), so a new event-specific arm does not conflict if placed before the generic actor section and made exhaustive for actor kinds.

**Fix.** For `Event::LaunchpadProposal`, accept only `AiSpec(card)` with cached `Spec` role, verify that card is the marked launchpad card (a DB-derived/cache-backed predicate, not payload data), and call `enforce_card_self_scope(card, scope, ...)`; deny every other actor. Continue writing through `enforce_role_resolving_session`. Add unknown card, inactive session, wrong Spec card, and forged card/wave/cove tests.

### CLOSED-OK — MAJOR — handler scope can be derived entirely from trusted identity

**Evidence.** The resolved MCP identity directly carries `card_id`, `wave_id`, and `cove_id` (`crates/calm-server/src/mcp_server/registry.rs:102-109`), and can produce the session-shaped actor used by the decision gate (`:112-127`). Dispatch supplies that identity to the handler (`crates/calm-server/src/mcp_server/transport.rs:609-613`). Therefore the proposal args need contain no target event scope.

**Fix.** Require `wave_id: Some`, construct `EventScope` exclusively from identity, then cross-check card→wave→cove/marker from the transaction before append. Do not deserialize target scope IDs from arguments; proposal payload `cove_id`/`folder_id` are proposed NewWave fields, not the event's authority scope.

### NEW — MAJOR — the typed event has a wider concrete implementation surface than Slice C/F states

**Evidence.** `Event` is a serde- and ts-rs-generated discriminated union (`crates/calm-types/src/event.rs:388-402`), replay reconstructs variants by kind (`:1310-1323`), and every variant needs an explicit topic mapping (`:1326-1404`). The frontend also maintains a hand-written exhaustive Zod discriminated union (`web/src/api/schemas.ts:820-930`) whose inferred type is checked against generated `Event` (`web/src/api/schemas.test.ts:212-213`), plus an exhaustive `invalidationPolicies` map keyed by `EventKind` (`web/src/app/invalidationPolicies.ts:119-185`). Generated bindings land in `web/src/api/generated-events.ts` as documented at `crates/calm-types/src/event.rs:390-401`. Event/topic serde goldens/tests live in the same Rust event module (for example `crates/calm-types/src/event.rs:2041-2094`, `:2567-2628`).

**Fix.** Expand Slice C/F estimates and acceptance criteria to include: Rust payload + Event variant/serde name/version, `topics()` card mapping, replay/serde/topic golden tests, regenerated `generated-events.ts`, Zod runtime schema + schema tests, and an explicit no-op/direct-consumer invalidation policy. Also define durable history retrieval/status transitions; a live WS event alone does not satisfy reload durability.

### CLOSED-OK — MINOR — concierge card-topic subscription matches existing frontend practice

**Evidence.** Current-run and chat-history hooks both add `card:<cardId>` and filter payload card IDs (`web/src/pages/useSpecCurrentRun.ts:152-184`; `web/src/pages/useSpecChatHistory.ts:342-370`). Rust topic routing already uses the same grammar (`crates/calm-types/src/event.rs:1326-1335`).

**Fix.** Map `LaunchpadProposal` to `card:<concierge_card_id>` (and optionally its wave topic if the payload carries wave_id), add a Today-specific listener with the same addTopic/on/filter pattern, and seed it from a durable proposals/history endpoint on mount to close the event-before-subscribe race.

### CLOSED-OK — MAJOR — M4: persisted repo identity has clean attach points, with one transaction-boundary caveat

**Evidence.** Direct folder creation normalizes then calls the repository insert at `crates/calm-server/src/routes/cove_folders.rs:119-159`. Wave attach calls `cove_folder_create_tx` inside the same transaction as wave creation (`crates/calm-server/src/routes/waves.rs:551-564`); that helper owns the single SQL insert (`crates/calm-truth/src/db/sqlite/cove.rs:186-225`). The table is simple and migration-friendly (`crates/calm-truth/migrations/0015_cove_folders.sql:20-27`).

**Fix.** Add nullable `repo_identity` (and preferably probe status/error timestamp) by migration and model/read-query updates. Probe/normalize once before opening the write transaction, then pass the result into both repository insert APIs so the row and identity commit atomically; do not run git while holding SQLite's write transaction. Add an explicit refresh path for remote changes and backfill existing rows best-effort.

### NEW — MAJOR — I2's dedup “constraint” needs relational keys or must be described as a weaker transactional check

**Evidence.** `workflow_input` is nullable JSON serialized as TEXT (`crates/calm-truth/migrations/0061_waves_workflow_input.sql:1-8`; `crates/calm-truth/src/db/sqlite/wave.rs:49-59`). The current create route validates it before entering the write transaction (`crates/calm-server/src/routes/waves.rs:344-366`), and `wave_create_tx` then mechanically inserts it (`crates/calm-server/src/routes/waves.rs:551-564`). SQLite JSON extraction exists elsewhere in migrations, but no current wave issue identity column/index provides normalized, typed uniqueness; indexing expressions over optional schema-varying JSON also leaves normalization, malformed/legacy values, lifecycle scope, and override semantics unresolved.

**Fix.** Preferred: add nullable dedicated `issue_repo` and integer `issue_number` columns populated server-side after trusted workflow/schema validation, normalize repo centrally, and add a partial unique index for the intended active/non-overridden domain. Define whether terminal/archived waves release the key and how override is represented without making the index ineffective. If schema churn is rejected, perform `SELECT` + insert in the same immediate/write transaction and map the conflict, but call it a transactional check—not a DB constraint—and acknowledge that correctness depends on SQLite writer serialization. Do not trust client-supplied normalized `repo` merely because it passed workflow JSON schema.
