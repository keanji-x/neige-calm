# Today Launchpad — SUBAGENT design review (adversarial, code-grounded)

Reviews `docs/_today-launchpad-design.md`. Every finding is grounded in the
canonical trees (`crates/`, `web/`, `plugins/`); `.claude/worktrees/*` ignored.

## (a) Verdict

**fix-then-ship — 1 blocker, 3 major, 6 minor.**

The core shape is right and the two hardest-to-verify claims hold. **I2** (the
confirm step routes through the *same* `POST /api/waves` and it re-validates
trusted-workflow, input-schema, cwd, and FolderConflict) and **I3** (the
propose tool is an ordinary wave-scoped write by a Spec-roled card and needs
**no** `role_gate.rs` change) both **verify against code**. Propose-then-confirm
with the human as the semantic gate, reusing the already-existing Today wave's
spec harness, is genuinely the minimal safe shape — I endorse it.

The blocker is a *different* security invariant than I3: the design's **I4**
("only the concierge gets `calm.cove.survey`") is **not deliverable by the cited
mechanism**. `visible_to_roles` is keyed on `CardRole`, is discovery-only, and
`tools/call` routes by name regardless — so a *global cross-cove read* would be
reachable by every spec agent in the system. That, plus I1's "read-only"
overclaim and a few plumbing/oracle gaps, must be fixed in the doc before impl.
None require rethinking the approach.

A single cheaper primitive collapses the blocker + two majors at once: a
persisted **concierge marker** (the `CardRole::Concierge` / wave-flag that Q-C
defers). See "Recommended reframe" at the end.

---

## (b) Findings (severity-ranked)

### BLOCKER

#### B1 — "`calm.cove.survey` visible only to the concierge" is unimplementable as specified; the global cross-cove read leaks to every spec agent

- **Claim.** §5 Slice A: *"Visible **only to the concierge** (see B)."* §3 I4:
  *"Only the concierge (Today/system-cove wave) gets … `calm.cove.survey`."*
  §5 Slice B points the carve-out at *"the scope logic
  (`mcp_server/tool_visibility.rs` / `visible_to_roles`)."*
- **Evidence.**
  - Kernel `calm.*` tool visibility is keyed **only on `CardRole`**:
    `descriptors_for_role(role)` filters `d.visible_to_roles.contains(&role)`
    (`crates/calm-server/src/mcp_server/registry.rs:327-333`). The concierge
    (Today wave's spec card) and every normal spec agent are **both
    `CardRole::Spec`** — there is no role that distinguishes them.
  - `visible_to_roles` is **discovery-only**: *"Wire-level `tools/call` still
    routes by name regardless — this only controls discovery"*
    (`registry.rs:260-266`). So even hiding it from `tools/list` does not stop a
    spec agent that knows the name from calling it.
  - The cited `tool_visibility.rs` governs **plugin** tools only and explicitly
    excludes kernel tools: *"kernel `calm.*` registry tools stay role-gated as
    before and **never route through here**"*
    (`crates/calm-server/src/mcp_server/tool_visibility.rs:1-18`). There is **no
    `visible_to_roles` symbol in that file** — the doc conflates two mechanisms,
    and neither delivers concierge-only scoping.
  - `tools/list` in the transport confirms role is the only key:
    `registry.descriptors_for_role(identity.role)`
    (`crates/calm-server/src/mcp_server/transport.rs:430, 477, 499`).
  - The tool it is told to "mirror," `calm.wave.state`, is **`visible_to_roles:
    &[]` (hidden from every list) AND identity-wave-scoped** via
    `resolve_wave_for_identity` (`crates/calm-server/src/mcp_server/tools/wave_state.rs:109, 118-119`).
    `calm.cove.survey` **inverts** that posture: it returns *all* coves, *all*
    folder filesystem paths + git remotes, *all* wave titles/issue numbers,
    system-wide.
- **Why it matters.** As written (Slice A `visible_to_roles: &[CardRole::Spec]`),
  every issue-dev spec agent — including one running in an untrusted repo the
  user just attached — could enumerate the entire cove/folder/wave graph. That
  is a direct break of the wave-scoping security posture the whole role-gate
  architecture defends. The design treats this as a solved detail; it is the
  single most security-critical piece and is unspecified.
- **Fix.** Authorize inside the handler, not via `visible_to_roles`. The handler
  receives `ToolCallIdentity { card_id, cove_id, wave_id, … }`
  (`registry.rs:101-110`), so it can resolve the concierge predicate
  (`cove.kind == System && wave.title == "Today"`, or a marker — see reframe)
  and **fail-closed** for any non-concierge caller before reading a single row.
  Optionally also thread wave context into the `tools/list` filter so it is
  hidden from normal agents, but the handler check is the real boundary because
  `tools/call` ignores visibility. Add the negative test (see m5).

---

### MAJOR

#### M1 — I1 "concierge is read-only over the kernel" overclaims; the Today spec card keeps full spec WRITE authority over its own wave

- **Claim.** §3 I1: *"concierge is read-only over the kernel. Its only new
  *power* is one global read."*
- **Evidence.** The concierge is a `CardRole::Spec` card. `enforce_role` lets a
  Spec actor emit **dispatch** (`CodexWorkerRequested`/`TerminalWorkerRequested`,
  `crates/calm-truth/src/role_gate.rs:246-253`), **plan** (`PlanUpdated`,
  `:246-253`), **review** (`ReviewRound`, `:357-364`), and **ratify.request**
  (`RatifyRequested`, `:357-364`) for its own wave. The default spec toolset
  (`calm.plan.*`, `calm.review.*`, `calm.task.verdict`, `calm.ratify.request`,
  dispatch) is registered for every Spec card
  (`crates/calm-server/src/mcp_server/tools/mod.rs:30-39`), and removing them
  from *only the concierge* hits the same role-keyed-visibility gap as B1 — and
  hidden tools remain wire-callable (`registry.rs:260-261`).
- **Why it matters.** The concierge ingests **attacker-controlled GitHub issue
  text** (the pasted URL's issue body) — a live prompt-injection surface. If
  steered, it retains real write power: it could write plans, dispatch workers,
  or open ratify requests. The saving grace is **cove confinement** — the
  worker/self-scope arm confines every AI actor to its home wave+cove
  (`role_gate.rs:389-459`, `enforce_card_self_scope` `:479-529`), and the Today
  wave lives in the isolated system cove with `cwd = "/"`
  (`web/src/hooks/useTodayTerminal.ts:186-195`). So the blast radius is bounded
  to the purposeless system-cove Today wave — but the doc must **say so** rather
  than assert "read-only," because "read-only" is a prompt-level aspiration, not
  an enforced property.
- **Fix.** Restate I1 as "concierge writes are confined to its own system-cove
  wave (kernel-enforced); read-only is a prompt convention, not a gate." Rely on
  cove-confinement as the true boundary. Optionally reduce the surface by keeping
  the Today wave's `cwd` non-actionable so a dispatched worker can do nothing
  useful.

#### M2 — Slice B places the concierge-prompt selection where the predicate data does not exist; the Today wave is unbound, and the running harness won't pick up a new prompt

- **Claim.** §5 Slice B: *"a concierge variant selected in
  `render_spec_developer_instructions` … when the predicate holds."*
- **Evidence.** `render_spec_developer_instructions(wave_id, workflow_descriptor,
  workflow_input)` has **no cove-kind or title parameter**
  (`crates/calm-server/src/operation/spec_harness_start_adapter.rs:209-213`).
  The Today wave is created with **no `workflow_id`**
  (`useTodayTerminal.ts:186-195`; corroborated by the FE survey), so
  `workflow_descriptor` is `None` and the function **returns the base spec
  prompt immediately** (`spec_harness_start_adapter.rs:218-220`). There is a
  `SPEC_SYSTEM_PROMPT_TEMPLATE` via `SeededCardRole::prompt_template()` but **no
  Concierge variant** (`crates/calm-server/src/spec_card.rs:426-448`). The Today
  harness was already booted at first Today-page load, so a new prompt only
  takes effect on a fresh thread (`force_new_thread` / reset — the harness is
  turn-reactive per `crates/calm-server/src/harness/run_loop.rs`).
- **Why it matters.** Without an injected concierge prompt the Today spec agent
  behaves like a generic spec agent (tries to plan/dispatch on goal `"Today"`) —
  the opposite of a concierge. Slice B is therefore load-bearing, but it is more
  than "select a variant in one function": you must (1) add a Concierge template,
  (2) compute `is_concierge` where cove-kind+title are known (the create/boot
  path), (3) thread it through `SpecHarnessStartOperationPayload`
  (`spec_harness_start_adapter.rs:191-207`) into the render fn, and (4) reset the
  already-running Today harness on rollout.
- **Fix.** Compute `is_concierge` at boot, thread it in, add the template,
  document the one-time reset of the existing Today wave. (A persisted marker —
  reframe — makes step 2 trivial and durable.)

#### M3 — No "agent-emitted actionable artifact" render infra exists; Slice C/D is more net-new FE than implied — but reuse `NewTaskForm` for the confirm/Create half

- **Claim.** §5 Slice D: *"Render each proposal artifact as an editable **confirm
  card**."* §4 lists the reused substrate but not a proposal renderer.
- **Evidence (FE survey, verified reads).**
  - The spec card's renderer is `Component: () => null`
    (`web/src/cards/builtins/spec.tsx:42-44`); SpecConversation renders agent
    output strictly **read-only** — entry kinds `user|agent|system|run|tool|…`
    render as `<details>/<pre>` with **no buttons**
    (`web/src/pages/SpecConversation.tsx:156-309`; `web/src/pages/specChatItems.ts:29-66`).
  - Overlays are status/progress/needs-input only, not clickable proposals
    (`web/src/cards/overlayRegistry.ts:40-58`).
  - **Ratify is a dead end**: `RatifyCardRequest`/`ratify.*` exist as backend
    wire types (`web/src/api/generated.ts:1530-1537`,
    `web/src/api/schemas.ts:663-675`) but there is **no UI renderer, no client
    fn**, and the events are explicit no-ops (*"no React Query cache consumes
    them yet"* — `web/src/app/invalidationPolicies.ts:251-255`).
  - **Directly reusable**: `NewTaskForm` is already an editable title/cwd/cove +
    raw-JSON `workflow_input` + merge-policy form whose submit calls
    `createWave({cove_id, title, cwd, attach_folder, theme, workflow_id,
    workflow_input})`, with an issue-dev variant binding
    `workflow_id: ISSUE_DEV_WORKFLOW_ID`
    (`web/src/shared/components/NewTaskForm.tsx:456-470`). `createWave`/
    `NewWaveBody` already carry `workflow_id` + `workflow_input`
    (`web/src/api/calm.ts:175-176`; `web/src/api/wire.ts:105-107`;
    `web/src/api/generated.ts:1436, 1447`).
- **Why it matters.** The doc's §4 "reuse vs new" undercounts Slice D: the
  *proposal-artifact renderer* (chat card that carries a Create button) has **no
  existing pattern** and is net-new glue. Conversely, the doc reinvents the
  *creation* half it could lift wholesale from `NewTaskForm`.
- **Fix.** Explicitly reuse `NewTaskForm` (or its form body) as the confirm card,
  pre-filled from the proposal; keep it user-initiated on Create → `createWave`.
  Scope Slice D as "new: subscribe-to-proposal + render one actionable card;
  reused: the entire create form."

---

### MINOR

#### m1 — Concierge liveness is a hard dependency for BOTH the turn and the write, and is currently unproven

- **Evidence.** Sending the concierge a turn hits `spec_harness_dormant` (409)
  when the harness is dormant (`web/src/pages/useSpecCurrentRun.ts:239-243`;
  route `send_spec_input` at `crates/calm-server/src/routes/cards.rs:687-716`).
  The propose *write* additionally requires the session to be
  `is_active_authority` = {Starting, Running, Idle, TurnPending}
  (`crates/calm-types/src/worker.rs:351-359`); a terminal session
  (Exited/Failed/Superseded) makes `enforce_role_resolving_session` return
  `SessionNotActive`/`SessionRowMissing`
  (`crates/calm-truth/src/decision_gate.rs:83-91`), i.e. a `Forbidden`. Because
  the Today wave's harness is "purposeless," it has likely **never attempted a
  write** — the AiSpecSession→AiSpec active-authority path is unexercised for it.
- **Fix.** Add a resume/resurrect story (the harness-start op already supports
  `force_new_thread`), and make the oracle assert the propose artifact *commits*
  (not just "agent called propose") so the authority path is covered.

#### m2 — The concierge predicate is not provably unique: the Today *wave* is not server-singleton

- **Evidence.** `ensureTodayWave` finds by `title === 'Today'` **client-side**
  and creates if missing (`useTodayTerminal.ts:173-197`). Migrations enforce
  one-spec/one-report **per wave** (`idx_cards_one_spec_per_wave`,
  `idx_cards_one_report_per_wave`) but there is **no UNIQUE index on
  `waves(cove_id, title)`**. Two cold-boot browsers can both miss and both
  `POST /api/waves`, yielding two "Today" waves → the predicate "System-cove ∧
  title 'Today'" matches >1 wave → >1 concierge. (The system *cove* is singleton
  via the idempotent upsert in `crates/calm-server/src/routes/coves.rs`, so only
  the wave races.)
- **Fix.** Server-side idempotent Today-wave upsert, or a persisted marker
  (reframe), which removes the title dependency entirely.

#### m3 — `git_remote` per folder is new kernel-side subprocess execution at tool-call time (not "mirroring the agent")

- **Evidence.** The issue-dev *agent* runs `git remote get-url origin` in the
  wave cwd, but it does so **inside its own sandbox/terminal** — that is what
  `plugins/git-forge/manifest.json`'s `issue-development` spec_instructions
  ("Repo cross-check") instruct. There is **no kernel-side bounded git-remote
  helper** to reuse; Slice A would shell `git` from the calm-server process. The
  kernel does already shell git elsewhere (`std::process::Command` +
  `GIT_COMMIT_SCRIPT` in `crates/calm-server/src/mcp_server/tools/emit.rs`), so
  it is not unprecedented — but the trust/perf domain differs.
- **Fix.** Specify: non-repo/non-zero exit → `git_remote: null`; guard
  `safe.directory`/dubious-ownership (folders the server didn't create); run off
  the async reactor (`spawn_blocking`); cache per folder (the doc's Q-A already
  suggests caching). Prefer `git -C <path> remote get-url origin`.

#### m4 — A new `Event` variant for the proposal triggers the full FE event checklist

- **Evidence.** Each event needs a zod schema
  (`web/src/api/schemas.ts`, e.g. `waveUpdatedSchema`) and an
  `invalidationPolicies` entry (`web/src/app/invalidationPolicies.ts`,
  `definePolicies` per `EventKind`) plus goldens. Modeling the proposal as a
  **card** (`CardAdded`) or **overlay** (`OverlaySet`) reuses existing
  `EventKind`s and avoids that surface — and both are ungated for `AiSpec` in
  `enforce_role` (they are not in the enumerated arms), preserving I3.
- **Fix.** Prefer a card/overlay-backed proposal artifact over a bespoke
  `Event::LaunchpadProposed`.

#### m5 — Acceptance oracle (§6) is missing the load-bearing negatives

The happy path + the one negative are good, but add:
- **(a) The I4 invariant, currently untested and (per B1) currently violated:**
  assert `calm.cove.survey` is **denied** when called by a normal (non-Today,
  non-system-cove) spec agent. This is the single most important missing test.
- **(b) Dedup** (trace step 4): a cove already has a wave bound to issue 950 →
  the concierge proposes **no** duplicate. The trace claims dedup; the oracle
  doesn't encode it.
- **(c) Strict input validation:** a proposal whose `workflow_input` carries an
  extra key is rejected — the `issue-development` schema is
  `additionalProperties: false`, required `[issue_url, repo, issue_number]`
  (`plugins/git-forge/manifest.json`), enforced by `validate_workflow_input_binding`
  (`crates/calm-server/src/routes/waves.rs:494-531`).
- **(d) Commit, not just call:** assert the proposal artifact is durably
  persisted (covers m1's authority path), and that the confirm→create lands the
  wave with the exact `workflow_input` in cove-A and boots the issue-dev harness.

#### m6 — FE plumbing gap: the Today page cannot currently reach the spec card

- **Evidence.** `SpecConversation` is fully props-driven off `specCardId` with
  **zero** router coupling — send-input is `sendSpecInput(cardId, text)` →
  `POST /api/cards/{id}/spec/input` (`SpecConversation.tsx:25-33, 368-483`;
  `useSpecCurrentRun.ts:237`). Good. But `useTodayTerminal` exposes only the
  **terminal** card id, never the Today wave/spec-card id
  (`useTodayTerminal.ts:41-53`). Mounting the concierge requires new plumbing to
  resolve the wave → `getWaveDetail` → pick the `type==='spec'` card, plus
  supplying `children` (report doc) + local `view` state (the component always
  renders the Report/Conversation toggle, `SpecConversation.tsx:575-594`).
- **Fix.** Extend `useTodayTerminal` (or add a sibling resolver) to surface the
  Today wave id + spec-card id; this is small but real and belongs in Slice D.

---

## (c) What the doc gets right (verified, not taken on faith)

- **I2 — creation path re-validates everything.** `create_wave`
  (`crates/calm-server/src/routes/waves.rs:317`) runs trusted-workflow resolve
  (`:344-360, resolve_trusted_workflow :476-487`), input-schema validation with
  `additionalProperties:false` + required fields (`:366, validate_workflow_input_binding
  :494-531`), absolute-cwd + normalize (`:368-379`), and the full folder-claim /
  `FolderConflict` 409 path (`:403-467`). A bad agent proposal is rejected
  exactly like a bad manual entry. The human confirm is a **true semantic gate**
  (the backend re-validates *structure*; only the human catches a
  structurally-valid-but-semantically-wrong cove/cwd). **I2 solid.**
- **I3 — propose is an ordinary Spec write, no `role_gate` change.** MCP writes
  resolve `AiSpecSession → AiSpec(card_id)` via `enforce_role_resolving_session`
  (`crates/calm-truth/src/decision_gate.rs:56-120`, after live session +
  active-authority + Spec-role checks), and `enforce_role` leaves `AiSpec`
  **unrestricted for any non-enumerated event** — section (3) gates only
  `AiCodex`/`AiClaude` workers (`crates/calm-truth/src/role_gate.rs:413-459`). A
  benign card/overlay-scoped proposal artifact passes untouched. **I3 verified**
  (subject to m1's liveness precondition).
- **Registration really is one line.** `register_default_tools` is a flat list
  of `mod::register_into(registry)` calls
  (`crates/calm-server/src/mcp_server/tools/mod.rs:30-39`) — adding a module +
  one line is accurate. The named repo methods exist:
  `coves_list_user_visible` / `cove_folders_by_cove` / `waves_by_cove`
  (`crates/calm-server/src/db/mod.rs:31-37`), and `coves_list_user_visible`
  already hides the system cove, so a concierge survey won't echo its own cove.
- **Reusing the Today wave doesn't clobber the terminal.**
  `create_wave_with_spec_harness` always mints a Spec card + ReportCard + boots a
  turn-reactive harness (`waves.rs:534-722`), even for the system cove (only
  folder-claims are exempted, `:381-398`). The terminal card is added *separately*
  by `useTodayTerminal`, so pointing SpecConversation at the spec card is purely
  additive. The doc's §4 substrate-reuse claims check out.
- **`createWave` carries the workflow fields** (`web/src/api/wire.ts:105-107`;
  `generated.ts:1436, 1447`), so the confirm→create path is viable end-to-end.
- **The doc's `file:line` citations are accurate** (spot-checked 317/476/494/534,
  `calm.ts:175`, `mod.rs:30`, `spec_harness_start_adapter.rs:209`).

## (d) Core-approach judgment

**Propose-then-confirm + reuse-the-Today-wave is the right minimal shape — keep
it.** The alternative (fully-autonomous `calm.wave.create`) is correctly deferred
in §8 because it needs a new cross-cove write posture. The human confirm is not
ceremony: it is the only check on *semantic* mis-resolution, since the backend
only re-validates structure. Reusing the existing singleton harness avoids new
harness/lifecycle infra. The design's weaknesses are all in *under-specified
security scoping* (B1, M1) and *undercounted plumbing* (M2, M3, m6), not in the
skeleton.

## (e) Recommended reframe — one primitive kills B1 + M1 + m2

Q-C defers a `CardRole::Concierge` (or wave flag) as "cleaner long-term." It is
actually the **cheapest correct** option and should be v1:

- A persisted concierge marker gives `calm.cove.survey`'s handler a **provable,
  race-free** authorization key (fixes **B1** without a title/cove lookup and
  without the non-unique-title race of **m2**).
- The same marker lets `render_spec_developer_instructions` select the concierge
  prompt from a first-class signal instead of threading cove-kind+title (**M2**).
- It also makes "which spec tools does this card get" answerable per-card if you
  later want to actually strip the write tools for real (partially addresses
  **M1**'s enforcement gap).

If a schema change is truly off the table for v1, the fallback is a **handler-
level fail-closed predicate** (`cove.kind==System && wave.title=="Today"`) — but
then B1's handler check is mandatory (not optional), m2's duplicate-Today-wave
race must be closed server-side, and I1/I4 must be reworded to match what is
actually enforced.
