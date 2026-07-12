# Today Launchpad — AI concierge (propose-then-confirm)

> Status: design **r3** (post two review rounds; converged). Issue: TBD (renumber file to `_NNN-...` on filing).
> Decision locked: **propose-then-confirm** (agent proposes; user confirms; existing REST creates). Scope: **launchpad only**.
> Reviews r1: `docs/_today-launchpad-design-review-{codex,subagent}.md`. Reviews r2 (confirm): `docs/_today-launchpad-design-review-{codex,subagent}-r2.md`. All four = *fix-then-ship, approach endorsed, no rethink*.

## 0. r1 → r2 — what the review changed (read this first)

Both channels endorsed the shape but killed four r1 assumptions. r2 folds in the fixes:

1. **The Today wave is NOT a purposeless harness.** It boots a real spec agent with `goal="Today"` and `cwd="/"`, which can write plan/report/wave-state before any concierge UI mounts (codex B1). → **Server-owned explicit creation** (no `WaveGoal`, safe cwd, concierge prompt injected *before* thread start, idempotent migration of existing Today waves).
2. **Tool visibility is role-only and discovery-only.** `visible_to_roles: &[CardRole]` can't distinguish concierge from normal spec (both are `CardRole::Spec`), and `tools/call` routes by name regardless of `tools/list` (both channels, BLOCKER). → **Handler-level fail-closed concierge gate** on both new tools, at `tools/list` *and* `tools/call`.
3. **"No role_gate change" was half-true and dangerously framed.** A resolved `AiSpec` is *unscoped* for ordinary events (`role_gate.rs:413-459`, `decision_gate.rs:101-105`), so the gate will NOT stop a forged target scope. → Safety comes from **deriving scope from the resolved `ToolCallIdentity`** (never args) + a **narrow additive `AiSpec` self-scope rule** for the proposal event.
4. **Concierge identity via System-cove ∧ title 'Today' is discovery, not identity** (races, rename, no uniqueness). → **Persisted server-minted launchpad marker** + a transactional get-or-create ensure endpoint. This single change collapses items 1, 2, and 4.

Plus: survey must be **DB-backed + minimized + redacted** (no runtime `git` shelling; no leaking absolute paths/titles); proposal is a **typed versioned event**, not chat JSON; POST gets a **dedup constraint**; prompt is **propose-only-when-resolved**, not "always propose."

### r2 → r3 — what the confirm round tightened (all impl-precision; no rethink)

The r2 confirm round (both channels: *fix-again-then-ship*) closed the r1 blockers in direction but caught six under-specified points. r3 folds them in:

1. **Marker is set by a dedicated server tx, never the `NewWave` body.** `create_wave` binds `Json<NewWave>` (`waves.rs:320`); a client-settable `purpose` would mint a rogue concierge or 500 on the unique index. Mint via a dedicated tx mirroring `cove_create_system_tx`. Enforce a **partial unique index** (`WHERE purpose='launchpad'`) mirroring `idx_coves_one_system` (`migrations/0009_coves_kind.sql:29-35`).
2. **Retire the client `ensureTodayWave` path.** The marker only stops ensure-vs-ensure; the live client still creates Today waves by title with `purpose=NULL` (no `waves(cove_id,title)` unique index), so duplicates persist unless that path is retired + the legacy row migrated deterministically.
3. **The `role_gate` arm is self-scope-ONLY.** `enforce_role` has no marker source in its signature (only role + wave→cove caches), so it *cannot* verify "is the launchpad card." The **who-boundary lives in the handler `is_concierge` gate**; the gate arm only enforces `enforce_card_self_scope` (which does reuse cleanly for `AiSpec`).
4. **`tools/list` needs a contextual augmentation step, not role hiding.** An unresolved daemon's `tools/list` returns the *union* of role-visible descriptors (`transport.rs:440-466`). Keep both tools `visible_to_roles: &[]` and add them only via a dedicated async step *after* a resolved+active+marked identity.
5. **`cwd` must be a real server-managed directory** (`is_dir`, created before harness start) — the spec-start path passes cwd unchecked to the provider (`spec_harness_start_adapter.rs:192-207`).
6. **The `LaunchpadProposal` event surface is large, and dedup needs real columns.** A new `Event` variant costs: Rust variant+serde+`topics()`+goldens, FE zod discriminated-union + `invalidationPolicies` + regenerated `generated-events.ts`, **plus a durable proposal-history endpoint** (a live WS event alone fails reload durability). Dedup needs dedicated **`issue_repo`+`issue_number` columns + partial unique index, server-normalized** — not `json_extract` over `workflow_input`.

## 1. Goal (one line)

On the Today page, a conversational AI **concierge** where you paste a GitHub issue URL; the agent uses tool-use to inspect existing coves/repos, semantically resolves which **local repo** the issue belongs to, and **proposes** an issue-dev wave. You confirm → the existing `POST /api/waves` creates it.

## 2. North-star oracle trace (happy path)

1. Today page calls **`POST /api/today/launchpad/ensure`** → `{wave_id, spec_card_id, terminal_card_id, terminal_id}`. It mounts the concierge conversation on `spec_card_id`.
2. User pastes `https://github.com/keanji-x/neige-calm/issues/950`. FE sends it to the concierge spec card (`POST /api/cards/{id}/spec/input`).
3. Concierge turn calls **`calm.cove.survey`** → `[{cove_id, folder_ids, repo_identity, dedup_keys:[{repo, issue_number, status}]}]` (DB-backed; minimized; credential-redacted).
4. Agent parses URL → `repo=keanji-x/neige-calm, issue=950`. Exact-matches persisted `repo_identity` → cove **neige-calm**. Checks `dedup_keys` → no wave bound to #950.
5. Agent calls **`calm.launchpad.propose`** with `{title, workflow_id:"issue-development", workflow_input:{issue_url, repo, issue_number, merge_policy}, cove_id, folder_id, rationale}`. Handler derives the concierge's own card/wave/cove from `ToolCallIdentity`; writes a typed `LaunchpadProposal` event scoped to the concierge spec card (idempotency key = normalized issue identity).
6. FE (subscribed to the concierge card topic) renders the proposal as an **editable confirm card** (dedicated bridge — see Slice F). Nothing auto-submits.
7. User clicks **Create** → FE calls the existing `POST /api/waves` (`createWave`, `web/src/api/calm.ts:175`) with the (possibly edited) body.
8. `create_wave_with_spec_harness` (`crates/calm-server/src/routes/waves.rs:534`) creates the issue-dev wave in cove neige-calm + boots its bound harness. FE marks the proposal `accepted` and links to the new wave.

## 3. Invariants (backbone) — reframed per review

- **I1 — concierge power = one global read + one own-wave proposal write.** It is still a `CardRole::Spec` card, so it retains normal spec write-authority *over its own (system-cove) wave*; it gains no cross-cove *write*. Blast radius is bounded to the system-cove launchpad wave by cove-confinement — **state this explicitly**, because the concierge ingests attacker-controlled issue text.
- **I2 — confirm re-validates the AUTHZ invariants, not correctness.** `POST /api/waves` re-runs trusted-workflow resolution (`waves.rs:472-487`), input-schema validation (`:489-530`), absolute-cwd normalize (`:368-379`), cove existence (`:390-395`), folder ownership/`FolderConflict` (`:403-466`). It does **not** validate GitHub repo/issue semantics, issue dedup, or cwd existence. → add dedicated **`issue_repo` + `issue_number` columns** on `waves` (populated server-side after schema validation, repo normalized **by the server** — not trusted from the client) + a **partial unique index** over the active domain; define terminal/archived key-release + override semantics. Confirm card exposes every authoritative field; never auto-submit.
- **I3 — proposal-write safety is two-layered: handler = *who*, role_gate = *self-scope*.** The **handler `is_concierge` gate** (marker lookup from the resolved `ToolCallIdentity`) is the who-boundary — `enforce_role` has no marker source, so it cannot do this. The write then goes through `enforce_role_resolving_session` (requires an **active** concierge session — `decision_gate.rs:87`) with a new **self-scope-only** `AiSpec` arm for `Event::LaunchpadProposal` (mirrors `enforce_card_self_scope`, `role_gate.rs:413-456`). Handler builds `EventScope` from the resolved identity, never from tool args (a resolved `AiSpec` is otherwise unscoped).
- **I4 — normal waves unchanged.** Only the launchpad-marked concierge gets `calm.cove.survey`/`calm.launchpad.propose`. `tools/list` exposure is via a contextual augmentation step (descriptors are `visible_to_roles: &[]`); `tools/call` is fail-closed via the handler `is_concierge` gate. Every other wave's agent keeps its existing wave-scoped tool set + prompt. (Test: a normal spec, in `tools/list` **and** on a direct call by tool name, is denied.)

## 4. Architecture: reuse vs new

**Reused (verified against code):** turn-reactive persistent harness (`harness/run_loop.rs`); user-turn `POST /api/cards/{id}/spec/input` (`routes/cards.rs:687`); the **card-ID-scoped** chat hooks `useSpecCurrentRun.ts`/`useSpecChatHistory.ts` + `SpecConversation.tsx` (do NOT assume Wave-page context — verified); session→actor resolution `enforce_role_resolving_session` (`decision_gate.rs:56`); create path `create_wave_with_spec_harness` (`waves.rs:534`); the editable-form→`createWave(workflow_id, workflow_input)` half already built in `web/src/shared/components/NewTaskForm.tsx:456-470`.

**New (six pieces):**
- **A.** Persisted **launchpad marker** + `POST /api/today/launchpad/ensure` (transactional get-or-create; returns the 4 ids; migrates existing Today waves).
- **B.** `calm.cove.survey` (DB-backed read) + **handler-level concierge gate**.
- **C.** `calm.launchpad.propose` (typed `LaunchpadProposal` event) + the additive `AiSpec` self-scope role-gate arm.
- **D.** Persist **normalized repo identity** per cove folder (resolved at attach/refresh, not at survey time).
- **E.** Concierge **prompt template** selected by the launchpad marker at harness start (`spec_harness_start_adapter.rs:209`).
- **F.** FE: Today conversation shell (mount `SpecConversation` on `spec_card_id`) + proposal→confirm-card bridge (prefill + `createWave`; not a straight `NewTaskForm` reuse — Slice F).

## 5. Slices + interface specs

### Slice A — launchpad marker + ensure endpoint
`waves.purpose='launchpad'` column, spec card stays `CardRole::Spec` (harness untouched — see Q-C). The marker is **set only by a dedicated server tx** (mirror `cove_create_system_tx`), **never** via the `Json<NewWave>` body of `create_wave` (`waves.rs:320`). Enforce a **partial unique index** `WHERE purpose='launchpad'` (mirror `idx_coves_one_system`, `migrations/0009_coves_kind.sql:29-35`); `ensure` = insert-or-select/retry under that constraint, returning the winner. `POST /api/today/launchpad/ensure` mints, in one tx: the marked wave (**no `WaveGoal`** → idle harness, `harness/mod.rs:227-232`), spec card, report card, terminal card — with a **real server-managed cwd** (a stable app-data dir, `mkdir -p` + canonicalize + `is_dir` **before** harness start; not `/`, not `cove_folders`-attached). Concierge prompt selected via the marker. Starts the harness only for the winner. **Retire the client `ensureTodayWave` path** (`useTodayTerminal.ts`) — else it keeps minting `purpose=NULL` Today waves; migrate the legacy row deterministically (adopt oldest eligible unmarked Today in the system cove, mark it in the singleton tx, preserve its terminal card, force-new/reset only its spec thread per stated transcript policy), and have the UI always call `ensure` + overwrite the legacy `localStorage calm.todayCardId`. Returns `{wave_id, spec_card_id, terminal_card_id, terminal_id}`; UI handles dormant/start-failed (201 ≠ live concierge).

### Slice B — `calm.cove.survey` (read) + concierge gate
Read-only. **Gate — two sites, both keyed on a fail-closed `is_concierge(ToolCallIdentity)` (identity→card→wave→`purpose='launchpad'`):**
- `tools/list`: descriptors stay `visible_to_roles: &[]` (so no unresolved daemon sees them, `transport.rs:440-466`); add them via a **dedicated contextual augmentation step** run *only after* a resolved+active identity whose card→wave carries the marker. Add nothing for no-thread / unresolvable / cross-session / missing card / dormant / DB-error.
- `tools/call`: the handler independently re-checks `is_concierge` first and denies unknown/unmarked/dormant — this is the real boundary (`ToolCallIdentity` is resolved at `transport.rs:609-613`).

Data from `coves_list_user_visible()` + persisted repo identity (Slice D) + wave dedup keys. **Minimized output:** cove/folder IDs + normalized `repo_identity` + `dedup_keys` (repo, issue_number, status) + counts; **no** absolute paths or wave titles until a match is selected; credentials redacted; rows/bytes capped; deterministic sort. Prompt + test: never echo the inventory (prompt-injection).

### Slice C — `calm.launchpad.propose` (write) + gate rule + event plumbing
**Handler** (the *who* gate): `is_concierge` check first; then build `EventScope` from the resolved `ToolCallIdentity` (`card_id`/`wave_id`/`cove_id`, `registry.rs:102-109`) — never from args; write via `enforce_role_resolving_session`. **`role_gate.rs` arm (self-scope only):** for `Event::LaunchpadProposal`, accept only `AiSpec(card)` with cached `Spec` role and call `enforce_card_self_scope(card, scope, …)`; deny every other actor. (The gate cannot check the marker — no marker source in its signature; the handler already did.) Payload: stable `proposal_id`, `status` (`pending|accepted|dismissed|stale`), canonical `NewWave` fields, `rationale`, idempotency key = normalized issue identity.

**Full event surface (do not under-scope):** a new `Event` variant is not one file. It costs: Rust payload struct + `Event` variant/serde name/version (`calm-types/src/event.rs`), `topics()` → `card:<concierge_card_id>` mapping + replay/serde/topic **golden tests**; regenerated `web/src/api/generated-events.ts`; a hand-written **zod** discriminated-union arm (`web/src/api/schemas.ts`) + its type-equality test; an **`invalidationPolicies`** entry (`web/src/app/invalidationPolicies.ts`). Plus a **durable proposal-history read endpoint** seeded on mount — a live WS event alone fails reload durability; confirm/dismiss are idempotent status transitions on `proposal_id`.

### Slice D — persisted repo identity (no runtime git shell)
New nullable `repo_identity` (+ probe status/timestamp) column on `cove_folders` (migration; backfill best-effort). When a folder is created/refreshed, resolve+normalize `git remote get-url origin` → `owner/name` **once, BEFORE opening the SQLite write tx** (never shell git while holding the writer lock), then pass the value into both insert paths (`cove_folder_create_tx`, `cove.rs:186-225`; and the wave-attach caller `waves.rs:551-564`) so row + identity commit atomically. Invoke git **without a shell**, `--no-optional-locks`, strict timeout + sanitized env; store per-folder error/`null` for non-git/bare/inaccessible. Add an explicit refresh path for remote changes. Survey reads the persisted value. (Matching is deterministic-exact; the agent adds only semantic/ambiguity/rationale — the hybrid both reviewers pointed at.)

### Slice E — concierge prompt
New template selected in `render_spec_developer_instructions` (`spec_harness_start_adapter.rs:209`) when the wave carries the launchpad marker. Rules: survey first; exact `repo_identity` match, fuzzy fallback; dedup against `dedup_keys`; **propose only** after a syntactically valid issue URL with sufficient resolved fields — otherwise ask/clarify/explain (NOT "always propose"); on no local match, propose with editable/empty cwd. Never claim a wave was created.

### Slice F — frontend
Today conversation shell around the card-scoped hooks (`useSpecCurrentRun`/`useSpecChatHistory`) pointed at `spec_card_id` (owns view mode + layout; handles dormant/reset). **Proposal render is a dedicated bridge, not a straight `NewTaskForm` reuse:** the proposal arrives as a top-level `LaunchpadProposal` event (not a harness item, so it bypasses the `useSpecChatHistory` pipeline) — subscribe a Today listener to `card:<spec_card_id>` + seed from the durable history endpoint on mount (close the event-before-subscribe race). `NewTaskForm` has **no prefill props** (state starts empty), so either add prefill or build a small confirm-card component; **Create** → existing `createWave(workflow_id, workflow_input)` (`calm.ts:175`); idempotent confirm; on success flip proposal → `accepted` + link the new wave. Today terminal → **Q-E** (tab vs secondary).

## 6. Acceptance oracle (expanded per review)

Seed cove-A (`neige-calm` @ `/tmp/a`, remote `keanji-x/neige-calm`) + cove-B (`other` @ `/tmp/b`). Core: concierge turn with issue URL for A → (1) called `calm.cove.survey`; (2) `calm.launchpad.propose` names cove-A, `folder_id` of `/tmp/a`, `issue_number=950`; (3) simulated confirm → `POST /api/waves` → wave with that `workflow_input` lands in **cove-A** with an **active bound harness** (assert persisted rows + live harness, not "operation submitted").

**Must-add negatives/edges:** normal Spec/Worker → tool absent from `tools/list` **and** direct-call denied; concierge direct-call succeeds; forged target card/wave/cove in propose args → denied; **zero** POST before an explicit click; edit-then-confirm revalidates; cancel/dismiss; double-click idempotent; stale proposal after folder/workflow change; trusted workflow stopped; invalid schema + `additionalProperties:false`; `FolderConflict`; duplicate-issue race (dedup constraint fires); concurrent `ensure` bootstrap (single concierge); existing generic-Today migration; renamed/deleted Today; non-git/private/credential-bearing remote (redaction); survey bounds/caps; dormant/start-failed/reset (turn AND propose both fail closed on inactive session). **r3 adds:** `tools/list` shows the two tools to a resolved-marked concierge but to **nothing else** (unresolved daemon, normal resolved spec, cross-session) — cover every `transport.rs` branch; `ensure` is idempotent under concurrent callers (single winner via the partial unique index); the retired client path no longer mints `purpose=NULL` Today waves; launchpad `cwd` exists and `is_dir` before harness start; dedup fires on the server-normalized `(issue_repo, issue_number)` index even when the client sends a differently-cased/`.git`-suffixed repo.

## 7. Open design decisions (need your call)

- **Q-C — launchpad identity representation. → RESOLVED to (A)** by the confirm round. `waves.purpose='launchpad'` marker (server-tx set + partial unique index, Slice A), spec card stays `Spec`, harness untouched. (B) new `CardRole::Concierge` was the cleaner-at-gate-time alternative but threads through harness/spec_card/role_gate — rejected as heavier for v1. Note: the *who*-gate lives in the tool handler regardless (the role cache carries role, not the marker), so (A) costs nothing extra there. 
- **Q-A — match strategy.** With persisted `repo_identity` (Slice D), exact remote match is deterministic; the agent handles ambiguity/no-match/rationale. Recommend this hybrid (keeps your AI-driven intent where it adds value, removes runtime git risk).
- **Q-E — Today terminal vs conversation.** Recommend conversation-forward, terminal → tab. Pure UX.
- **Q-B — proposal representation.** Recommend a typed `LaunchpadProposal` **event** (reuse the event bus transport) over card-payload/overlay (which carry harness-schema / replacement-version semantics). Confirmed no existing actionable-artifact renderer to reuse — the render is net-new.

## 8. Out of scope (later)

- Fully-autonomous create (agent calls a `wave.create` tool) — needs a cross-cove *write* posture. Revisit once semantic matching proves reliable.
- Daily-rolling "today wave" + auto-generated digest report.
- `GET /api/workflows` discovery (issue-dev id still hardcoded client-side).
