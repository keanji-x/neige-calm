# Today Launchpad design review (Codex)

## Verdict

**FIX-THEN-SHIP.** Propose-then-confirm is the right security boundary and reusing the existing Today wave is a reasonable minimal product shape, but the draft is not implementable safely as written. The existing Today spec harness is live and is automatically given `"Today"` as its first goal, the proposed System-cove-plus-title identity is neither a durable identity nor expressible through the current role-only kernel-tool visibility mechanism, and the proposal “artifact” is left unspecified even though that choice determines whether `AiSpecSession` can authorize the write and how the UI observes it. Keep the core approach, but add a server-owned concierge marker/identity, define a concrete card-scoped proposal representation and authorization path, avoid runtime git subprocesses, and expand the acceptance oracle around authorization, visibility, confirmation, deduplication, and singleton races.

## BLOCKER findings

### B1 — The Today harness is live, but it is not currently “running-but-purposeless”

**Claim.** The draft says the existing Today wave has a purposeless harness which can simply receive a new prompt and tools.

**Evidence.** `ensureTodayWave` calls the normal `createWave` with title `Today` (`web/src/hooks/useTodayTerminal.ts:173-196`). Normal creation always mints a Spec-role card (`crates/calm-server/src/routes/waves.rs:563-579`), derives its goal from the trimmed wave title (`crates/calm-server/src/routes/waves.rs:566-575`), and submits/waits for `spec-harness-start` with that goal (`crates/calm-server/src/routes/waves.rs:649-679`). The initial snapshot converts a non-empty goal into a queued `Observation::WaveGoal` (`crates/calm-server/src/harness/mod.rs:227-232`). Failure is explicitly tolerated, leaving the wave with an inert or possibly inert agent (`crates/calm-server/src/routes/waves.rs:679-718`). The Today cwd is `/`, not a harmless absent cwd (`web/src/hooks/useTodayTerminal.ts:177-190`).

**Why it matters.** On first bootstrap the ordinary spec prompt processes the meaningless task “Today”; it may use ordinary Spec tools and write plan/report/wave state before the concierge UI is ever mounted. Merely changing prompt selection later does not repair already-created threads or transcript/state, and changing the prompt without changing goal initialization still makes the concierge process “Today.” Running at `/` also gives the model/tooling an unnecessarily broad filesystem starting point. Existing Today terminal behavior does not consume the spec card, but the wave report/spec state can already have been clobbered by this initial turn.

**Recommended fix.** Make Today creation server-owned and explicit: create or migrate it with a durable concierge marker, no initial `WaveGoal`, a safe cwd, and concierge instructions before the harness/thread starts. Add an idempotent migration/reset for existing Today waves that clears/restarts the obsolete spec thread deliberately (with product-visible transcript policy). Do not infer concierge mode only after a generic harness has booted. Treat harness-start failure/dormancy in the Today UI, since creation returning 201 does not prove a live concierge.

### B2 — Current tool visibility cannot implement “concierge only, normal Spec hidden”

**Claim.** Extending `tool_visibility.rs` / `visible_to_roles` can expose the two new tools only to the Today concierge.

**Evidence.** A kernel tool descriptor exposes a static `visible_to_roles: &'static [CardRole]` (`crates/calm-server/src/mcp_server/registry.rs:253-266`), and registry listing filters only by role (`crates/calm-server/src/mcp_server/registry.rs:320-338`). Today and normal spec cards both have `CardRole::Spec` (`crates/calm-server/src/routes/waves.rs:567-579`). `plugin_scope_for_wave` concerns plugin ownership, not kernel `calm.*` tools; its own module says kernel tools do not route through it (`crates/calm-server/src/mcp_server/tool_visibility.rs:1-10`). Dispatch separately resolves identity before invoking a registered kernel handler (`crates/calm-server/src/mcp_server/transport.rs:585-628`), so hiding only from `tools/list` would not be an authorization boundary.

**Why it matters.** Adding `[CardRole::Spec]` exposes survey/propose to every normal spec agent. Hiding the descriptor from all roles and relying on prompt knowledge is security by obscurity unless the call handler performs the same predicate. Modifying plugin scope is the wrong choke point.

**Recommended fix.** Add a first-class server-owned concierge capability/card role (preferred), or extend registry discovery and dispatch authorization with a contextual capability predicate evaluated from the resolved card/session and persisted marker. Apply the identical rule to both `tools/list` and `tools/call`, with tests proving normal Spec denial even when it directly calls the known tool name. Do not use title as the capability.

### B3 — “Proposal artifact” is too undefined to support the no-role-gate-change claim

**Claim.** I3 asserts that an `AiSpec`/`AiSpecSession` writing an artifact in its own wave is already authorized and needs no `role_gate.rs` change.

**Evidence.** MCP Spec identity is session-keyed (`crates/calm-server/src/mcp_server/registry.rs:56-67`). The synchronous role gate explicitly rejects unresolved `AiSpecSession` actors (`crates/calm-truth/src/role_gate.rs:407-411`). A resolved `AiSpec` is not generally constrained to its home wave/card: the explicit self-scope enforcement applies to `AiCodex`/`AiClaude`, not `AiSpec` (`crates/calm-truth/src/role_gate.rs:389-466`); only particular event families receive special Spec checks (`crates/calm-truth/src/role_gate.rs:176-267`, `346-387`). Existing durable choices are materially different events such as `CardUpdated` and `OverlaySet` (`crates/calm-types/src/event.rs:402-444`, `567`), and the draft has not selected one or shown its write path/session resolution.

**Why it matters.** “Own wave” is not a policy currently established for generic AiSpec events. Depending on implementation, the proposal may fail because the session actor is unresolved, may require mutating a card payload, or may be accepted without enforcing that the target is the concierge's own card/wave. Thus I3 is unproven, and a new event can accidentally become a cross-wave write primitive.

**Recommended fix.** Specify the artifact now. Prefer a typed, append-only proposal event scoped to the concierge Spec card, with the handler deriving card/wave/cove from resolved `ToolCallIdentity` (never accepting target IDs from arguments). Resolve the active session to its bound Spec card before the truth write, and add a narrow role-gate rule enforcing exact home card/wave/cove. If that requires a role-gate change, make it; “no change” is not an invariant worth preserving. Test forged target IDs and direct calls from normal Spec/Worker sessions.

## MAJOR findings

### M1 — System-cove AND title `Today` is discovery logic, not a safe identity

**Claim.** The predicate is safe and unique because users cannot see the System cove.

**Evidence.** The frontend finds the first matching title (`web/src/hooks/useTodayTerminal.ts:165-176`) and creates when absent (`web/src/hooks/useTodayTerminal.ts:186-196`). There is no uniqueness constraint or atomic get-or-create shown here; two clients can both observe absence and create. The title is ordinary wave data used as the harness goal (`crates/calm-server/src/routes/waves.rs:563-574`). System coves also bypass all folder ownership checks (`crates/calm-server/src/routes/waves.rs:381-399`), so “System cove” is a broad internal category, not a concierge capability.

**Why it matters.** Rename/delete, legacy duplicates, or bootstrap races can remove concierge privileges from the intended wave or grant them to the wrong internal wave. Frontend localStorage caches only the terminal card and can bypass wave rediscovery (`web/src/hooks/useTodayTerminal.ts:65-73`), making identity drift harder to reconcile.

**Recommended fix.** Persist a server-minted purpose/role or stable singleton key and enforce uniqueness transactionally. Provide one backend `ensure Today launchpad` endpoint returning wave, spec card, and terminal identifiers. Treat title as display text only and migrate/deduplicate existing rows deterministically.

### M2 — Confirmation revalidates important creation invariants, but dedup and path existence are not among them

**Claim.** A wrong proposal cannot bypass creation invariants and human confirmation is a true gate.

**Evidence.** The POST resolves only running trusted workflows (`crates/calm-server/src/routes/waves.rs:344-360`, `472-487`), validates workflow input against the descriptor schema (`crates/calm-server/src/routes/waves.rs:363-366`, `489-530`), requires/normalizes an absolute cwd (`crates/calm-server/src/routes/waves.rs:368-379`), checks cove existence (`crates/calm-server/src/routes/waves.rs:390-395`), and enforces claimed-folder ownership/overlap with structured `FolderConflict` responses (`crates/calm-server/src/routes/waves.rs:403-466`). Creation is invoked only by the frontend after confirmation in the proposed flow, so the authority boundary is sound in principle. However, the shown route does not validate GitHub repository/issue semantics, enforce issue deduplication, or establish that cwd exists/is a directory before handing it to harness startup.

**Why it matters.** The doc overstates “re-validates everything.” A user can confirm a duplicate issue wave or semantically mismatched issue/cove, and malformed-but-schema-valid agent data may still create a wave. Confirmation is authorization, not correctness validation.

**Recommended fix.** Reword I2 narrowly. Put any required dedup invariant in POST as a transactional database-backed constraint/check keyed by normalized repo plus issue number (with a defined override policy), not solely in the agent survey. Validate attach/existence semantics consistently. The confirm card must clearly expose every authoritative field and never auto-submit.

### M3 — Global survey deliberately widens read scope and exposes sensitive local metadata

**Claim.** Survey is a bounded harmless global read.

**Evidence.** Existing MCP identity is designed around a per-card token and card binding (`crates/calm-server/src/mcp_server/mod.rs:7-21`). The proposed response includes every user-visible cove's absolute folder paths, git remotes, wave titles, workflow IDs, and issue bindings. System-cove filtering only protects system rows in the frontend lookup context (`web/src/hooks/useTodayTerminal.ts:33-38`); it does not make user repo names, paths, private remote owners, issue numbers, or work titles non-sensitive.

**Why it matters.** Prompt-injected issue text can induce the concierge to repeat inventory into chat/transcripts or proposal rationale. Persistent harness history then stores a cross-cove index in one wave. This is an intentional break from wave-scoped reads and should be threat-modeled as such.

**Recommended fix.** Minimize output: return stable cove/folder IDs and normalized repo identity, not absolute paths until a match is selected; omit unrelated wave titles and return only dedup keys/counts. Redact credentials from remotes unconditionally. Cap rows/bytes, sort deterministically, exclude archived/sensitive categories as policy dictates, and instruct plus test against echoing the inventory. Document transcript retention and local-user trust assumptions.

### M4 — Shelling out to git per folder at tool-call time is avoidable and risky

**Claim.** `git remote get-url origin` per folder is a bounded way to obtain repo identity.

**Evidence.** The Today wave is currently rooted at `/` (`web/src/hooks/useTodayTerminal.ts:177-190`), while the proposed survey spans paths from all coves. No existing bounded subprocess/caching contract is identified in the design. The route's folder claims validate namespace overlap, not that a claimed path remains a git repository (`crates/calm-server/src/routes/waves.rs:403-466`).

**Why it matters.** N subprocesses add latency and resource amplification on every model call. A repository-local git configuration can define external commands or rewrite behavior; inherited environment and credential-bearing remote strings require care. Deleted, inaccessible, non-git, bare, multi-remote, worktree, SSH, and nested-repo folders are normal cases, not exceptional failures.

**Recommended fix.** Resolve and normalize repo identity when a folder is attached/refreshed, persist a sanitized value, and survey the database. If runtime probing remains, invoke git without a shell, use `--no-optional-locks`, a strict timeout/concurrency cap/output cap, sanitized environment/config, and per-folder error values rather than failing the survey. Normalize HTTPS/SSH/scp syntax, `.git`, case, and credential redaction; define non-git behavior.

### M5 — `SpecConversation` input/history mostly works by card ID, but mounting is not plug-and-play UI composition

**Claim.** Pointing `SpecConversation` at Today's spec card should reuse chat as-is.

**Evidence.** Its public contract needs only `specCardId`, controlled `view`, `onViewChange`, and report children (`web/src/pages/SpecConversation.tsx:25-33`); sending calls the card-scoped API through `run.submit` (`web/src/pages/SpecConversation.tsx:457-481`, `web/src/pages/useSpecCurrentRun.ts:221-251`). Run state and history subscribe to `card:<id>` (`web/src/pages/useSpecCurrentRun.ts:152-184`, `web/src/pages/useSpecChatHistory.ts:342-370`), so they do not require Wave-page context. But the current mount owns report/conversation mode and report content in `WaveReportPage` (`web/src/pages/WaveReportPage.tsx:259-347`), while `useTodayTerminal` returns only terminal card/terminal IDs, not wave/spec-card IDs (`web/src/hooks/useTodayTerminal.ts:41-49`, `97-115`).

**Why it matters.** Input does “just work” once the correct spec card ID is available, but the Today page needs a reliable bootstrap result, view ownership, layout/CSS integration, dormant/reset handling, and proposal rendering. Re-fetching wave detail separately creates races and duplicate bootstrap logic.

**Recommended fix.** Have the server-side ensure API return `{wave_id, spec_card_id, terminal_card_id, terminal_id}` and create a small Today-specific conversation shell around the reusable card-scoped hooks. Extract a lower-level conversation component if report-mode assumptions make styling awkward. Test remount, stale localStorage, dormant harness/reset, card replacement, and simultaneous terminal/chat use.

### M6 — There is event/overlay/card infrastructure, but no existing actionable proposal channel to reuse unchanged

**Claim.** A card payload or typed event can straightforwardly produce the clickable confirm card.

**Evidence.** Existing UI subscribes to card topics for harness phase/items (`web/src/pages/useSpecCurrentRun.ts:152-163`, `web/src/pages/useSpecChatHistory.ts:342-364`). `WaveReportPage` separately derives generic event-line entries and runtime state (`web/src/pages/WaveReportPage.tsx:275-276`), while `SpecConversation` renders transcript entries, not arbitrary wave events (`web/src/pages/SpecConversation.tsx:374-447`). Existing durable kernel event variants include generic card updates and overlays (`crates/calm-types/src/event.rs:402-444`, `567`), but no proposal/survey-card event or renderer exists.

**Why it matters.** Reusing the event bus transport is sensible; claiming an existing UI-action artifact abstraction exists is not supported. Encoding proposals as chat JSON is brittle, mutating the Spec card payload risks colliding with its harness schema, and overlays have replacement/version/lifetime semantics that may not fit an append-only proposal history.

**Recommended fix.** Reuse event delivery, not an ill-fitting payload. Define a typed versioned proposal event/table with stable proposal ID, timestamps/status (`pending|accepted|dismissed|stale`), canonical NewWave fields, and rationale. Subscribe on the Today spec card topic, fetch durable history on mount, and make confirm idempotent. Keep rendering separate from markdown/tool-call text.

## MINOR findings

### m1 — The prompt rule “always end by proposing” is unsafe for ambiguous or conversational turns

**Claim.** Every concierge turn should end with `calm.launchpad.propose`.

**Evidence.** The UI sends arbitrary trimmed text to the persistent spec harness (`web/src/pages/SpecConversation.tsx:457-470`), not only validated GitHub issue URLs.

**Why it matters.** Greetings, corrections, ambiguity, unsupported hosts, and follow-up questions would create misleading durable proposals. Multiple tool calls can also create duplicate cards.

**Recommended fix.** Propose only after a syntactically supported issue URL and sufficient resolved fields; otherwise ask a question or explain the error. Give proposal calls an idempotency key derived from normalized issue identity plus conversation turn.

### m2 — The acceptance oracle misses the load-bearing negative and migration cases

**Claim.** Section 6 tests the core invariants.

**Evidence.** It checks one happy path and one no-match path, but the implementation has separate discovery/dispatch visibility (`crates/calm-server/src/mcp_server/tool_visibility.rs:1-10`), tolerated harness-start failure (`crates/calm-server/src/routes/waves.rs:679-718`), session-keyed Spec identity (`crates/calm-server/src/mcp_server/registry.rs:56-67`), and a frontend bootstrap that can race (`web/src/hooks/useTodayTerminal.ts:173-196`).

**Why it matters.** The test could pass while normal Spec agents retain the global tool, direct calls bypass hiding, proposal writes cross scope, duplicate Today/issue waves are created, or a pre-existing Today thread remains generic.

**Recommended fix.** Add tests for: normal Spec/Worker tools-list absence and direct-call denial; concierge direct-call success; forged target/card/wave/cove denial; zero POST before an explicit click; edit-then-confirm revalidation; cancel/dismiss; double-click/idempotent retry; stale proposal after folder/workflow changes; trusted workflow stopped; invalid schema and FolderConflict; duplicate issue race; concurrent Today bootstrap; existing generic-Today migration; renamed/deleted Today; non-git/private/credential-bearing remotes; survey bounds/redaction; dormant/start-failed/reset behavior; reload durability and event replay. Assert creation via persisted rows and active bound harness, not merely that a start operation was submitted.

### m3 — A dedicated launchpad service endpoint is a safer alternative only if it preserves confirmation

**Claim.** Reusing the Today wave is the minimal shape.

**Evidence.** The reusable conversation hooks are genuinely card-ID scoped (`web/src/pages/SpecConversation.tsx:368-375`, `web/src/pages/useSpecCurrentRun.ts:152-184`), so a second chat substrate is unnecessary. Conversely, Today bootstrap is currently frontend-orchestrated and terminal-centric (`web/src/hooks/useTodayTerminal.ts:86-137`).

**Why it matters.** Replacing the whole approach with a stateless REST matcher would lose semantic/tool-use value, but forcing identity, inventory, proposal state, and terminal bootstrap through title conventions creates avoidable coupling.

**Recommended fix.** Keep propose-then-confirm and the existing persistent harness, but expose a dedicated server-side Today-launchpad ensure/read API and explicit concierge capability. If the sole supported input remains a GitHub issue URL and matching is exact remote identity, first consider a deterministic backend resolver plus editable form; invoke AI only for ambiguity/rationale. That is simpler, faster, and reduces global data exposure while preserving the same human confirmation gate.
