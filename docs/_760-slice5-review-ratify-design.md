# #760 slice ⑤ — Dual-reviewer primitive + convergence + ratify gate: sub-slicing design (⑤-a..d) — **v4 · CONVERGED (4 dual-channel rounds, 2026-06-22)**

> Sub-design under the converged #760 design (`docs/_760-issue-dev-workflow-design.md`, §3-C group C,
> §3-D group D, §1 oracle rows 4/5/6/10-dual/11/12/17, §6 invariants). Grounded at HEAD `72f10bee`
> (slices ①②③-a..d ④-a/④-b ⑥ ⑦ all merged). `file:line` cites prefer the named symbol on drift
> (the repo auto-syncs and advances HEAD). This doc lands in-repo with ⑤-a's PR.
>
> **v4 folds round-3 dual-channel review (channel B codex: CONVERGED 0/0; channel A subagent: 0 blocker
> / 1 should-fix / 2 nits).** Blocker trajectory across all rounds: **2 → 1 → 0.** Round-3 fold: the
> `review.round` idempotent-duplicate **no-op cannot return an empty event batch**
> (`write_with_actor_events` rejects it, `sqlite.rs:6227`), so it is realized via a **pre-read fast-path
> + early `Ok(no-op)`** (the `plan.rs:966-982` precedent), with the in-tx write closure mapping a racing
> duplicate to `Ok(no-op)`/reject — atomicity preserved without committing a row (⑤-b 2b / F1 / §0). Plus
> two cite nits (`write_with_actor_events:6210/:6227`; `AiSpec→SpecAgent :111`). No architectural change;
> see the §7 round-3 ledger. A confirmatory round-4 re-review of this fold is the last step.
>
> **v3 folds round-2 dual-channel review (channel A: 0 blocker / 1 should-fix / 3 nits — all round-1
> folds verified correct, 0 regressions; channel B: 1 blocker / 1 should-fix).** Both channels confirmed
> the B2 role-gate arms, the B3 head-match, and the A1–A4 cite folds are SOUND; the residual items
> drill the B1 idempotency core to ground. Round-2 resolutions folded:
> - **B1 round-2 (BLOCKER — out-of-order duplicate):** the v2 guard queried only the *latest* round for
>   S, so a *delayed* stale re-drive of `(S,n=1)` after `(S,n=2)` exists would fall through and append
>   as the new latest event, regressing the §4d fence. **Fix:** the guard enforces **strict monotonic
>   `n` per subject INSIDE the write tx** (query `max(n)` over ALL prior rounds for S; idempotent
>   same-`(S,n)`+hash ⇒ no-op; `n ≤ max(n)` different payload ⇒ reject; only `max(n)+1` appends) and the
>   cap query keys on **`max(n)`** (not max-event-id). Channel A's atomicity point + channel B's
>   out-of-order point are ONE fix: the dedup read + emit share the write tx under `BEGIN IMMEDIATE`
>   (which serializes concurrent spec turns — codex-confirmed). F1 + ⑤-b 2b + ⑤-d 4d updated.
> - **B3 round-2 (SHOULD-FIX — idem-key backward-compat):** changing `gh.pr.merge:{repo}:{pr}` →
>   `…:{expected_head_sha}` would orphan a pre-⑤ merge op. **Fix:** the workflow is greenfield (no live
>   merge ops), so the change is safe — stated in ⑤-c 3c.
> - **Round-2 nits folded:** `write_with_actor_events` (no `_typed` suffix); the role-gate arms are
>   NOVEL (the kernel-only arms allow `User`; these reject it) — wording tightened in ⑤-b 2e so the
>   implementer doesn't copy `:275` verbatim; the User `/ratify` route derives a raw `User` actor, not a
>   spec-input enqueue.
>
> **v2 folded round-1 dual-channel review (channel A subagent: 0 blocker / 4 should-fix / 5 nits;
> channel B codex: 2 blockers / 1 should-fix).** Both channels confirmed the architecture, the 7 fork
> resolutions, FSM soundness of row 17, and completeness are CORRECT; the rounds hardened three real
> substrate gaps + corrected cites. Round-1 resolutions folded here:
> - **B1 (BLOCKER — `review.round` idempotency):** the events table has NO dedupe key
>   (`0004_events.sql:23-32`; the idempotency unique index is only on `operations`
>   `0042_operations_parked.sql:96`), so a plain spec-authored `review.round` event could be
>   DUPLICATED on a crash/turn-re-drive. **Fix (F1 hardened):** `calm.review.round` carries a
>   deterministic `idempotency_key` `review.round:{wave}:{phase}:{slice}:{pr|design}:{n}`; the
>   cap-query semantics are **latest-by-event-id per subject** (so a re-emit is latest-wins, never
>   double-counted toward `cap`); the handler does a **pre-emit dedup guard** (query the latest round
>   for subject S — same `(S,n)` + same payload hash ⇒ no-op-return; same key + different hash ⇒
>   reject). This stays a spec-authored EVENT (not a forge operation — codex's operations-index
>   alternative was considered and rejected: operations are spawn/park-shaped, a synchronous
>   emit-an-event doesn't fit the `ProviderAdapter` trait — the "dead machinery" the forge-op grounding
>   found).
> - **B2 (BLOCKER — event-level authority hole):** `enforce_role` ends with `Ok(())`
>   (`crates/calm-truth/src/role_gate.rs:417`), so a BRAND-NEW event variant is **ungated by default** —
>   a worker card or plugin could forge a `review.round{converged:true}` or, worst, a
>   `ratify.resolved{Grant}` that self-approves the human gate. **Fix (⑤-b):** add explicit
>   `enforce_role` arms — `ReviewRound` + `RatifyRequested` allowed ONLY for `AiSpec` with cached
>   `Spec` role (reject Plugin/AiCodex/AiClaude/sessions/plain-User); `RatifyResolved` allowed ONLY for
>   `ActorId::User` (reject spec/workers/plugins/kernel) — mirroring the kernel-only pattern for
>   `task.dispatched`/`task.gate_result` (`role_gate.rs:275`/`:312`). Plus negative tests for a
>   plugin-forged `review.round` and a non-User `ratify.resolved{Grant}`.
> - **B3 (SHOULD-FIX — head-match is detect-after-the-fact):** the R4-4 invariant "merge head ==
>   latest converged revision for S" was only E2E-DETECTABLE, not PREVENTED. **Fix (⑤-c):** the
>   `gh.pr.merge` verb gains an `expected_head_sha` arg (sourced from the latest converged
>   `review.round.head_sha`) lowered to **`gh pr merge … --match-head-commit <sha>`** (the local
>   `gh` supports it) — a real RUNTIME head-match guard at the forge layer (still plugin argv, no
>   kernel coupling). `expected_head_sha` joins the merge idempotency key so retry can't collapse
>   distinct reviewed heads. `review.round.head_sha` is sourced from the reviewed
>   `forge.pr.diff.read.head_sha`.
> - **A1 (SHOULD-FIX — false safety claim):** `Observation::is_hard_fire` is a **`matches!` macro**
>   (`crates/calm-types/src/observation.rs:115`), NOT a compile-forced `match` — a new variant
>   defaults to `false` (non-hard-fire) with NO compiler error. Only `to_turn_text`
>   (`observation.rs:138`) is compile-forced. **Fix:** corrected throughout; the ⑤-b impl brief MUST
>   hand-add `ReviewRound`/`RatifyRequested`/`RatifyResolved` to the `is_hard_fire` `matches!` list or
>   all three silently won't wake the spec.
> - **A2/A3/A4 + nits (cite corrections):** `Observation`/`is_hard_fire`/`to_turn_text` live in
>   `crates/calm-types/src/observation.rs` (calm-server's is a re-export shim); the **Spec** system
>   prompt is rendered at `operation/spec_harness_start_adapter.rs:395` (`SeededCardRole::Spec`,
>   `developer_instructions`) — `codex_adapter.rs:1119` is the **Worker** path; `TaskKind` is
>   `crates/calm-truth/src/model.rs:320`. All re-anchored below.

## 0. Scope, ground truth, fork resolutions

⑤ aggregates **C** (dual-reviewer + convergence, rows 5/6/11/12/17 + dual-channel-half of 10) and
**D** (ratify gate, row 4). It splits into ⑤-a..d. Deps ③ ④ ⑦ ⑥ ① are merged.

### Confirmed ground truth (grounded at `72f10bee`; round-1 verified)

- **Event spine.** `SYNC_EVENT_VERSION = 10` (`crates/calm-types/src/event.rs:341`); `WEB_COMPAT_VERSION
  = 11` (`web/src/api/version.ts:79`). `Event` enum (`pub enum Event` `event.rs:366`,
  `#[serde(tag="ev",content="data")]` `:364`) has **42 variants**; `ALL_KIND_TAGS:[&str;42]`
  + `kind_tag_list_matches_enum` exhaustive-match + `goldens_cover_every_event_variant` asserts
  `files==61` (`crates/calm-server/tests/event_serde_goldens.rs:882/975/932`; 61 golden files on
  disk). The event-add recipe is ③'s — variant + `#[serde(rename)]` + `kind_tag`/`metadata`/`topics`
  arms + golden + ts-rs + `wave_vcs.rs` no-op arm + ONE batched version bump per release.
- **Forge/subject types ③ added** (the model to copy): `ForgePrMerged{wave_id, subject:
  ForgeMergeSubject, head_sha: String, merge_sha: String}` (`event.rs:777`); `ForgeMergeSubject{phase,
  slice_id, pr_number: u64}` (`event.rs:346`) — the **LOGICAL subject key** (R4-4). `ForgePrDiffRead`
  carries `{wave_id, pr_number, base_sha, head_sha, artifact_path}` (`event.rs:795`) — the source of
  the reviewed `head_sha`.
- **Lifecycle FSM + authority are READY — NO FSM CHANGE NEEDED** (`crates/calm-types/src/wave_lifecycle.rs`,
  `validate_transition` `:182`). All edges + authorities the ratify path needs exist:
  `reviewing→working` (spec-only, `:284`), `working→blocked` (spec-only, `:282`), `blocked→working`
  (User+spec `(true,true)`, `:290`), `reviewing→failed` (spec-only, `:286`), `reviewing→done`
  (spec-only, `:285`). **NO `reviewing→blocked` edge** (round-3 C4 — grep clean), so ASK-HUMAN is the
  two-edge `reviewing→working` THEN `working→blocked`. `ActorId::Plugin → ActorKind::Other` rejected
  for all transitions (`:116`,`:202`); `AiSpec → SpecAgent` (`:111`); Kernel/KernelDispatcher treated
  as SpecAgent for lifecycle. So ratify MUST be **spec-authored** (B5), not plugin-authored.
- **EVENT-LEVEL authority gate (`crates/calm-truth/src/role_gate.rs`, `enforce_role` `:143`).** Only
  named variants are special-cased: wave-write `:178`, dispatch/plan `:231`, `task.dispatched`
  kernel-only `:275`, `task.gate_result` kernel-only `:312`; everything else **falls through to
  `Ok(())` `:417`** (User/Kernel/KernelDispatcher/Plugin pass; AI workers are card-scoped `:430`).
  **⇒ a new event is ungated by default** — ⑤-b MUST add arms for the 3 new events (B2).
- **Spec tool surface.** Tools register via `register_default_tools` (`mcp_server/tools/mod.rs:29`)
  → per-module `register_into` (e.g. `plan.rs:77`); role-gated by `require_role`/`require_role_any`
  (`mcp_server/registry.rs:160`). **Precedent for a spec-only verdict tool** = `calm.task.verdict`
  (`mcp_server/tools/wave_state.rs:163`): `require_role(Spec)` `:198`, REQUIRES a non-empty
  `idempotency_key` `:201`, builds `TaskCompleted/Failed` `:218`, commits via
  `CardDecisionSink::commit_spec_verdict` (`decision_sink.rs:283`) which applies the optional
  lifecycle via `apply_requested_transition_in_tx` (`crates/calm-server/src/wave_lifecycle.rs:47`,
  validates the edge first). The actor-scoped batch writer is `write_with_actor_events`
  (`crates/calm-truth/src/db/sqlite.rs:6210`, via `begin_immediate_tx`); it persists every returned
  event in order and **rejects an empty batch as an error** (`:6227`; cf. `plan.rs:978` no-op
  precedent — load-bearing for the `review.round` no-op realization, ⑤-b 2b). **`calm.ratify.*` +
  `calm.review.round` don't exist yet.**
- **User-authored input** reaches the kernel ONLY via REST `POST /api/cards/{id}/spec/input`
  (`routes/cards.rs:652`, actor=`User`, enqueues `Observation::UserMessage` + emits
  `HarnessUserMessageEnqueued` `:734`, `#[utoipa::path]`-documented). **Users cannot call MCP tools.**
  So the human ratify verdict (`grant`/`deny`) MUST be a **REST route**, not an MCP tool.
- **§2.5-C observation plumbing (slice ⑦) — 6 stages, grounded:** (1) dispatcher subscription filter
  kinds vec `dispatcher.rs:655`; (2) `event_warrants_spec_push_with_role` arms `dispatcher.rs:70`
  (default `_ => false` `:100`); (3) boot-replay kinds array `harness/mod.rs:99`; (4) `Observation`
  enum (`crates/calm-types/src/observation.rs:18`; calm-server's `harness/observation.rs` is a 7-line
  `pub use` shim) + `is_hard_fire` (`:114`, a **`matches!` macro at `:115` — NOT compile-forced**;
  a missing variant silently defaults to `false`) + `to_turn_text` (`:138`, a **wildcard-free
  `match` — compile-forced**); (5) `harness_observation_from_event` `dispatcher.rs:1152`; (6) live
  arm in `handle_envelope`. A new spec-facing event not added to ALL of these (and to `is_hard_fire`
  by HAND) is silently dropped / non-hard-fire and not recovered on boot.
- **task.dispatched is kernel-claimed.** `claim_task` (`scheduler.rs:612`) emits `Event::TaskDispatched`
  authored by `KernelDispatcher` (`scheduler.rs:690`); `role_gate.rs:275` makes it kernel-only.
  `TaskKind` = `Codex|Claude|Terminal` (`crates/calm-truth/src/model.rs:320`) — **no Review kind**;
  tasks table (`0041_tasks.sql`; `context_json` column exists) has no reviewer-role/channel column but
  carries free-form `context_json`. Verdicts recorded via worker-emitted `task.completed`/`task.failed`
  (`emit.rs:138/194`) — **no built-in correlation of two reviewers into one round.**
- **Workflow descriptor (④-a/b) — STORAGE landed, CONSUMPTION inert (the load-bearing gap ⑤-a
  closes).** `WorkflowDescriptor{id, plan_template: Vec<PlanTaskInput>, gates: Vec<GateInput>,
  spec_instructions: String, card_kinds}` (`plugin_host/manifest.rs:217`), validated at manifest-parse;
  `Wave.workflow_id: Option<String>` (`crates/calm-types/src/model.rs:403`, migration
  `0059_waves_workflow_id.sql`) binds a wave at create time. **BUT `spec_instructions` has NO runtime
  consumer** (grep clean outside manifest-validate/tests) — the **Spec** prompt is rendered at
  `operation/spec_harness_start_adapter.rs:395` (`crate::spec_card::render_system_prompt(
  SeededCardRole::Spec.prompt_template(), wave_id)` → `developer_instructions` `:415`), a STATIC role
  template + `{wave_id}` substitution (`spec_card.rs:315`) that never reads the bound workflow.
  (`codex_adapter.rs:1119` is the **Worker** prompt — `SeededCardRole::Worker` — NOT the spec.) The
  current `plugins/git-forge` `issue-development` descriptor's `plan_template` has only 3 tasks and its
  `spec_instructions` deliberately omit review choreography — a placeholder for ⑤-c.
- **`neige.kv.*` plugin store** (`plugin_host/callbacks.rs:198-201`; table `plugin_kv` in
  `0002_plugins.sql`): per-`plugin_id` durable KV, quota-gated, queryable in tests via the repo trait,
  **NOT via REST, and it emits no events**. It is the PLUGIN's store; the codex spec calls `calm.*`
  MCP tools, not `neige.*`. (⇒ not usable as the spec's review-round store — F1.)
- **Forge-action operation infra (⑥/③)** is fully built (`operation/forge_action_adapter.rs`,
  post-park release, bounded `ForgeEventSpec` extractor, `complete_forge_op_with_result`,
  probe-recovery; subject required-at-validate for `forge.pr.merged` `:1068`, kernel-injected from the
  frozen payload `:1258`). The git-forge plugin lowers `gh.pr.merge` to
  `gh pr merge PR --repo … --squash --delete-branch` (`plugins/git-forge/main.rs:390`) and extracts
  merge `head_sha` from `headRefOid` (`:406`). FU2 handled (read-only diff, result-file-first
  recovery); FU4 lease/worktree teardown rides merge-op + lease compensation.
- **E2E harness.** ③ landed a **Rust** integration test `crates/calm-server/tests/forge_workflow_e2e.rs`
  (`git_forge_happy_path_persists_ordered_workflow_events` `:187`; crash-recovery
  `git_forge_merge_crash_recovers_once_via_probe` `:406`) with helpers `event_rows` `:1400` /
  `wait_for_event_count` `:1422` (SQL over the events table). This — not a bash case — is where ⑤'s
  assertions land. CI lacks Codex terminal bytes (project memory), so CONVERGENCE-FAILURE branches are
  driven by **scripted verdicts / event injection**, asserting on persisted events.

### Fork resolutions (the decisions this sub-design makes — round-1 reviewed)

- **F1 — `review.round` durability: the EVENT SPINE itself (hardened with idempotency, B1), NOT
  `neige.kv.*` and NOT a forge operation.** The parent offered "a forge/review OPERATION (or, minimally,
  the plugin KV store)"; grounding shows both are wrong-shaped (KV is plugin-scoped + emits no events;
  a forge-op is irreversible-side-effect machinery for an idempotent record). **Resolution:** a
  spec-authored MCP tool **`calm.review.round`** emits the typed `review.round` event in one atomic tx
  (precedent `calm.task.verdict`). The **events table IS the durable store** (durable + queryable +
  §2.5-C-recoverable); "latest round for subject S" = the `review.round` event for S with the
  **MAX `n`** (the round number is the authority, NOT event-insertion order — round-2 B). **Idempotency
  (B1, round-2-hardened):** the tool carries a deterministic `idempotency_key`
  `review.round:{wave}:{phase}:{slice}:{pr|design}:{n}` (in the EVENT PAYLOAD so it survives
  replay/query) + a payload hash; the handler enforces **strict monotonic `n` per subject INSIDE the
  write tx** (read-your-write under the repo's `BEGIN IMMEDIATE`, which serializes concurrent spec turns
  — round-2 A atomicity + round-2 B confirmation): query `max(n)` over ALL prior `review.round` for S;
  if `(S,n)` already exists with the same payload hash ⇒ no-op-return (crash re-drive); if `n ≤ max(n)`
  with a different payload ⇒ reject (a stale/out-of-order re-drive — e.g. a delayed `n=1` after `n=2` —
  must NOT append and regress the fence); only `n == max(n)+1` appends a new round. The cap query keys
  on `max(n)`, so a duplicate can never double-count toward `cap`. The same `max(n)` helper gives the
  spec its read path (reconstruct the next `n`/converged after a restart). (The idempotent-duplicate
  *no-op* uses a pre-read fast-path since `write_with_actor_events` rejects an empty batch — ⑤-b 2b,
  round-3 A.)
- **F2 — ratify authority/routing (two gates: tool/route entry + event-level role_gate, B2).**
  `calm.ratify.request` = spec MCP tool (`require_role(Spec)`; AiSpec actor; emits `ratify.requested` +
  flips `working→blocked`). `ratify.grant`/`deny` = **User REST route** `POST /api/cards/{id}/ratify`
  (actor=User; grant flips `blocked→working` + emits `ratify.resolved{Grant}`; deny records
  `{Deny}`, stays blocked). PLUS event-level `enforce_role` arms (B2): `ReviewRound`/`RatifyRequested`
  spec-only, `RatifyResolved` User-only — closing the "forge a grant to self-approve the human gate"
  hole. FSM unchanged.
- **F3 — dual-reviewer task tagging: distinct task KEYS + `context_json:{channel, reviewer_role}`; NO
  new `TaskKind`, NO schema change.** Two review tasks with distinct keys → distinct `task.id` →
  distinct payload hash (no idem collision), both kernel-claimed (`task.dispatched`×2). Both report
  `task.completed`; the spec correlates by subject and records BOTH verdicts in `review.round.channels[]`
  (a `channels.len() ≥ 2` validator makes "both verdicts recorded" a checkable primitive). No kernel
  descriptor validation of review-count (moat). *(Fork point: reviewers may want a descriptor-level
  "≥2 disjoint reviewers per phase" validator; default NO, asserted by E2E.)*
- **F4 — merge-fence enforcement = POLICY + VERIFICATION (+ a runtime head-match guard, B3).** "No
  merge for S while its latest `review.round` is `converged:false`" is enforced by (a)
  `spec_instructions` (the spec is instructed not to merge while unconverged) and (b) the E2E temporal
  subject-keyed assertion. The kernel does NOT block a merge op on review state (moat; the spec is the
  trusted driver, as it already is for `gh.pr.merge`). **NEW (B3):** the *head-match* half of R4-4 IS
  runtime-guarded — `gh.pr.merge` lowers with `--match-head-commit <expected_head_sha>` so an
  unintended-head merge FAILS at the forge layer, not just in the E2E. A full kernel hard-gate on the
  *converged* bit remains a deliberate non-goal (§6-Q5).
- **F5 — FU2 (gh.pr.diff recovery): DROP** (already handled; reviewer-task re-issues on failure).
- **F6 — FU4 (teardown fence): owned by ⑤-d** (E2E test + a small in-flight-forge-op guard).
- **F7 — CONVERGENCE-FAILURE E2E: scripted verdicts** (CI lacks Codex bytes); assert on persisted
  events.

---

## 1. ⑤-a — Workflow→spec consumption (substrate; flips no row)

**The connective tissue ④ declared but left inert.** Without it, ⑤-c's dual-review plan template +
convergence/ratify policy never reach the spec.

**Scope.** When a wave has `workflow_id = Some(id)`, resolve the bound `WorkflowDescriptor` and:
1. **Inject `spec_instructions` into the SPEC role's system prompt** — at the Spec-spawn site
   `operation/spec_harness_start_adapter.rs:395` (where `developer_instructions` is built from
   `render_system_prompt(SeededCardRole::Spec.prompt_template(), wave_id)`): append the bound
   workflow's `spec_instructions` (after `{wave_id}` substitution). Worker prompts
   (`codex_adapter.rs:1119`) are unchanged.
2. **Surface the `plan_template` to the spec.** Lean: carry the template tasks inline in the rendered
   instructions (the spec already authors its plan via `calm.plan.upsert`). *(Fork: a
   `calm.workflow.template` read tool if inline bloats the prompt.)*
3. **No-binding precedence:** `workflow_id = None` renders exactly as today (backward compatible).

**Files touched.** `operation/spec_harness_start_adapter.rs` (resolve `wave.workflow_id` → descriptor
at spec-spawn; pass `spec_instructions` into the prompt build), `spec_card.rs` (`render_system_prompt`
gains an optional workflow-instructions parameter, or a sibling that appends them), a small repo/manifest
accessor to fetch the bound `WorkflowDescriptor` by id.

**New events/tools.** None.

**Acceptance (substrate — FLIPS no row).** Direct test: a spec spawn for a wave bound to a test
workflow renders a `developer_instructions` that CONTAINS the descriptor's `spec_instructions` (+ the
template tasks); a `workflow_id=None` wave renders the unchanged static template. *Unblocks: rows
5/6/11/12/17 — the workflow can now drive the spec.*

**Size.** **M**. **Deps.** ④-a.

> **Scope honesty.** ⑤-a closes a gap arguably belonging to ④ ("scheduler/spec plumbing to read the
> descriptor"); grounding confirms it did NOT land. ⑤ owns it (first real consumer); it may be
> re-homed to a ④ follow-up but must land BEFORE ⑤-c.

---

## 2. ⑤-b — review.round + ratify.* events + spec/User tools + role-gate + §2.5-C wiring (flips row 4)

**The mechanism layer:** three new events, two spec tools, the User ratify route, event-level
authority, and observation plumbing.

**2a. Three NEW event variants** in `crates/calm-types/src/event.rs` (real enum; the
`crates/calm-server/src/event.rs` `pub use` shim is NOT edited):

| variant | kind | payload (besides wave_id) | scope |
|---|---|---|---|
| `ReviewRound` | `review.round` | `subject: ReviewSubject`, `head_sha: Option<String>` (reviewed revision; None for design), `n: u32`, `cap: u32`, `converged: bool`, `channels: Vec<ChannelVerdict>` (`{role, verdict}`), `root_cause: Option<String>`, `idempotency_key: String` | Wave |
| `RatifyRequested` | `ratify.requested` | `reason: String` | Wave |
| `RatifyResolved` | `ratify.resolved` | `decision: RatifyDecision` (`Grant`/`Deny`) | Wave |

**2a-i. `ReviewSubject` (round-1 Q6, lean b):** a distinct `ReviewSubject{phase, slice_id, pr_number:
Option<u64>}` (NOT reusing `ForgeMergeSubject`, whose `pr_number` is required for merge — keeping ③'s
required-subject-at-validate gate intact). The E2E correlates a per-PR `review.round` (`pr_number:
Some(N)`) to a `forge.pr.merged` (`ForgeMergeSubject.pr_number == N`) by `{phase, slice_id, pr_number}`.

**Per-variant recipe** (mirror `ForgePrMerged`): variant + `#[serde(rename)]` → arms in `kind_tag`
(`event.rs:1177`), `metadata` (`:972`, wave entity), `topics` (`:1267`, **emit `wave:<id>` from the
payload `wave_id`** per C6) → `event_serde_goldens.rs` `kind_tag_list_matches_enum` arm (`:975`) +
`ALL_KIND_TAGS` 42→45 (`:882`) + 3 `tests/goldens/events/*.json` + `goldens_cover_every_event_variant`
count 61→64 (`:932`) → web `schemas.ts` zod + `wireEventSchema` union (`:754`) +
`web/src/app/invalidationPolicies.ts` entry (it's a `{[K in EventKind]:…}` map — compile-forces a new
entry) + `generated-events.ts` (ts-rs auto; payload structs `ReviewSubject`/`ChannelVerdict`/
`RatifyDecision` need `#[ts(export)]`) → NO `from_kind_and_payload` arm (generic serde `:1248`) →
`wave_vcs.rs` no-op arm + skip-commit set (mirror `ForgePrMerged`) → ONE batched `SYNC_EVENT_VERSION
10→11` + history line + `web/src/api/version.ts` `WEB_COMPAT_VERSION 11→12`.

**2b. `calm.review.round` spec tool** (new `mcp_server/tools/review.rs`, `register_into` from
`tools/mod.rs:29`; `require_role(Spec)`). Args `{subject:{phase,slice_id,pr_number?}, head_sha?, n,
cap, converged, channels:[{role,verdict}], root_cause?}`. Handler: validate (`n ≤ cap`;
`channels.len() ≥ 2`; `converged ⇒ all channels approving`); compute `idempotency_key`
`review.round:{wave}:{phase}:{slice}:{pr|design}:{n}`; **strict-monotonic-`n` dedup guard**: query
`max(n)` over ALL prior `review.round` for S; three branches —
- **idempotent duplicate** (`(S,n)` already present with the same payload hash) ⇒ `Ok(no-op)`;
- **stale/out-of-order** (`n ≤ max(n)` with a different payload) ⇒ reject;
- **new round** (`n == max(n)+1`) ⇒ emit `Event::ReviewRound`.

**No-op realization (round-3 A).** `write_with_actor_events` REJECTS an empty event batch as a hard
error (`crates/calm-truth/src/db/sqlite.rs:6227`; documented at `mcp_server/tools/plan.rs:978`), so the
no-op branch CANNOT emit zero events from inside the write closure. Follow the `plan.rs:966-982`
precedent: detect the idempotent-duplicate via a **pre-read fast-path** and early-`Ok(no-op)` BEFORE
opening the closure (safe — an exact `(S,n)`+hash duplicate is idempotent regardless of races); the
**write closure (under `BEGIN IMMEDIATE`, which serializes concurrent spec turns —
`sqlite.rs:begin_immediate_tx`) re-reads `max(n)` in-tx** and, on a race, returns the monotonicity
`Err` (`n ≤ max(n)`) → rollback, which the tool layer maps (racing-identical-duplicate → `Ok(no-op)`;
genuine conflict → reject). Only the `max(n)+1` branch commits a row. *(`calm.task.verdict` is NOT an
in-tx-dedup precedent — it always emits, dedup is downstream-by-key; the dedup mechanism here is NEW,
modeled on `plan.rs`'s pre-read no-op, and reviewed at impl.)* No lifecycle flip. *(The spec computes
`converged` from the two task verdicts as POLICY; the kernel records it — same trust model as
spec-authored `calm.task.verdict`, §6-Q5.)*

**2c. `calm.ratify.request` spec tool** (in `review.rs`). Args `{reason}`. Handler `require_role(Spec)`,
emit `Event::RatifyRequested{reason}` + `apply_requested_transition_in_tx(working→blocked)`. (On the
row-17 cap path the spec FIRST does `reviewing→working` via its normal lifecycle arg, THEN calls
`ratify.request` for `working→blocked` — the two-edge path is the spec's orchestration.)

**2d. User ratify verdict REST route** `POST /api/cards/{id}/ratify` (`routes/cards.rs`; reuse
`send_spec_input`'s card-resolution guard — card exists + Spec-roled codex — but the actor is the raw
authenticated `User` (round-2 nit: `actor.as_str()=="user"`, NOT a spec-input text enqueue). Body
`{decision:"grant"|"deny", message?}`, actor=`User`. Grant:
emit `RatifyResolved{Grant}` + `apply_requested_transition_in_tx(blocked→working, User)`. Deny: emit
`RatifyResolved{Deny}` (stay blocked; spec decides `→failed`). `#[utoipa::path]`-documented (OpenAPI
regen gate).

**2e. Event-level authority (`enforce_role`, B2)** in `crates/calm-truth/src/role_gate.rs` (new arms
BEFORE the `Ok(())` `:417` tail (a structural sibling of the kernel-only arms `:275`/`:312`, but these
are NOVEL shapes — the kernel-only arms ALLOW `User`, whereas these must REJECT plain `User` for the
spec-only events and reject everything-but-`User` for `RatifyResolved`; do NOT copy `:275` verbatim):
`ReviewRound` + `RatifyRequested` → allow only `AiSpec(card)` whose cached role is `Spec` (the MCP
tool authors as `AiSpecSession`, resolved to `AiSpec(card)` by `enforce_role_resolving_session` BEFORE
`enforce_role` — round-2 B confirmed; reject Plugin/AiCodex/AiClaude/plain-User/Kernel); `RatifyResolved`
→ allow only `ActorId::User` (reject all card/plugin/kernel/spec actors). Negative
tests: plugin-forged `review.round` rejected; non-User `ratify.resolved{Grant}` rejected.

**2f. §2.5-C wiring** for all three events (all **hard-fire**): (1) dispatcher filter kinds vec
`dispatcher.rs:655` (+ test mirror); (2) `event_warrants_spec_push_with_role` arms `dispatcher.rs:70`
→ `true`; (3) boot-replay kinds array `harness/mod.rs:99`; (4) `Observation::{ReviewRound,
RatifyRequested, RatifyResolved}` in `crates/calm-types/src/observation.rs` + **HAND-ADD all three to
the `is_hard_fire` `matches!`** (`:115` — NOT compile-forced, A1) + `to_turn_text` arms (`:138` —
compile-forced); (5) `harness_observation_from_event` arm `dispatcher.rs:1152`; (6) live arm in
`handle_envelope`.

**Acceptance (FLIPS row 4; substrate for 5/6/10-dual/11/12/17).** **Row 4 → ✅** (ratify request parks
`working→blocked` + emits `ratify.requested`; User `POST /ratify` grant resumes `blocked→working` +
emits `ratify.resolved`). Each of the 3 events round-trips (golden+serde); `calm.review.round` rejects
`<2` channels / `n>cap` / a duplicate `(S,n)`; **role-gate negative tests** (plugin-forged
`review.round`, non-User `ratify.resolved{Grant}` both rejected); each event traverses §2.5-C (persist
→ `replay_harness_events_since` → recovered into `snapshot.pending_queue` → a turn issues; AND a live
emit pushes through `observe_harness_under_lock`).

**Size.** **L**. **Deps.** ③ (`ForgeMergeSubject`/event recipe), ⑦ (§2.5-C), ④-a.

---

## 3. ⑤-c — issue-development dual-review plan template + convergence policy (descriptor data; flips no row)

**The policy layer (plugin data + prose).** Fill the `plugins/git-forge` `issue-development`
`WorkflowDescriptor` so the workflow drives the dual-review + convergence + ratify protocol (⑤-a
consumes it; ⑤-b provides the tools).

**3a. plan_template — ≥2 disjoint review tasks per phase + design-before-impl deps:**
- **Design phase (row 6):** `review-design-a` (`kind:codex`, `context_json:{channel:"a",
  reviewer_role:"design-correctness"}`), `review-design-b` (`{channel:"b",
  reviewer_role:"design-failure-path"}`), both `depends_on:[]` (or a design-discovery task). The
  **impl** task `depends_on:["review-design-a","review-design-b"]` (oracle invariant 2; kernel DAG
  enforces via `compute_ready`).
- **Per-PR phase (row 10):** `review-pr-a`/`review-pr-b` (disjoint roles) `depends_on:` the
  diff-read/open-PR task; both read one PR's diff (③'s `gh.pr.diff` artifact).

**3b. spec_instructions — the coded convergence strategy (group C policy as prose):**
- After BOTH channels of a phase complete, call `calm.review.round{subject, head_sha?, n, cap,
  converged, channels:[both verdicts], root_cause?}`; `converged = all channels approving`;
  `head_sha` = the reviewed `forge.pr.diff.read.head_sha` (B3 — so the merge head-match is meaningful).
- Increment `n` per subject from the last observed `review.round` for S (+1); `cap` is a fixed policy
  constant (e.g. 8).
- **Always re-review:** every fix re-dispatches BOTH channels before the next `review.round` (row 12).
- **Merge fence (F4):** call `gh.pr.merge` for S ONLY when the latest `review.round` for S is
  `converged:true`, passing `expected_head_sha` = that round's `head_sha` (B3 → `--match-head-commit`).
- **Cap-exhausted (row 17):** at `n==cap` non-approving → no merge; **GIVE-UP**
  (`calm.plan.upsert{lifecycle:failed}` = `reviewing→failed`, terminal) OR **ASK-HUMAN**
  (`reviewing→working` then `calm.ratify.request{reason:cap_exhausted}` → `working→blocked`; on
  `ratify.resolved{grant}` resume `blocked→working→reviewing`, may converge+merge).
- Systemic-root-cause: record `root_cause` each round (a repeated facet drives a class fix).

**3c. `gh.pr.merge` verb extension (B3) — touches `plugins/git-forge/main.rs:390`:** add an
`expected_head_sha` input; lower to `gh pr merge … --squash --delete-branch --match-head-commit
<expected_head_sha>`; include `expected_head_sha` in the merge semantic idempotency key
(`gh.pr.merge:{repo}:{pr}:{expected_head_sha}` — vs ③'s current `gh.pr.merge:{repo}:{pr}`
`plugins/git-forge/main.rs:401`) so a retry can't collapse distinct reviewed heads. **Backward-compat
(round-2 B):** the issue-dev workflow is GREENFIELD — no production wave drives `gh.pr.merge` yet (all
slices unmerged), so there are no pre-⑤ parked/succeeded merge ops the new key shape could orphan; the
change is safe. (If a live workflow ever predated this, a legacy-`{repo}:{pr}`-key fallback would be
needed — not required here.)

**3d. `pr` card kind** — deferred (⑤ consumes PR identity via `ForgePrMerged`/`ReviewSubject`).

**Acceptance (substrate — FLIPS no row).** Direct test: the `issue-development` descriptor VALIDATES
with the new template; `plan_template` contains ≥2 review tasks with disjoint `context_json.reviewer_role`
per phase, every impl task `depends_on` both design-review keys (assert the DAG shape); the
`gh.pr.merge` lowering includes `--match-head-commit` when `expected_head_sha` is set; `spec_instructions`
within bounds + control-char-clean. *Unblocks rows 5/6/11/12 behavior, asserted in ⑤-d.*

**Size.** **M**. **Deps.** ⑤-a (consumption), ⑤-b (tools), ③ (`gh.pr.merge` verb), ④-a.

---

## 4. ⑤-d — E2E: convergence + CONVERGENCE-FAILURE branches + cap enforcement + FU4 (flips rows 5,6,10-dual,11,12,17)

**The verification layer.** Extends `crates/calm-server/tests/forge_workflow_e2e.rs`. **Scripted
verdicts** drive branches deterministically; assertions over persisted events (`event_rows` `:1400` /
`wait_for_event_count` `:1422`).

**4a. CONVERGE — `dual_review_converges_then_merges`:** design dual-review (`review.round(phase:design,
converged:true)`, two channels) BEFORE the first impl `task.dispatched` (invariant 2); per-PR
dual-review converges; merge fires with `head_sha == latest converged round's head_sha` (invariant
5b); full CONVERGE backbone present.

**4b. GIVE-UP — `cap_exhausted_give_up_fails_terminal`:** per-PR `review.round(n==cap, converged:false)`
→ GIVE-UP `reviewing→failed`; assert **whole-run** merge-tail absent for S (terminal); `failed` present.

**4c. ASK-HUMAN — `cap_exhausted_ask_human_pauses_then_resumes`:** `review.round(n==cap,
converged:false)` → assert two-edge `reviewing→working` BEFORE `working→blocked` (invariant 9; assert
NO direct `reviewing→blocked`); `ratify.requested` present; merge-tail absent WHILE the cap-hit round
is latest-unconverged for S. Then `POST /ratify {grant}` → `ratify.resolved{Grant}` + `blocked→working`;
a NEW `review.round(S, converged:true)`; assert the run re-enters CONVERGE and merges with the converged
revision's `head_sha`. **Do NOT assert whole-run merge absence here** (SF-2).

**4d. Cap-enforcement helper (temporal, subject-keyed, R4-4):** group `review.round` by LOGICAL subject
`{phase,slice_id,pr_number}`; for each, take the round with **MAX `n`** for S (round-2 B — the
authoritative round, not max-event-id; strict-monotonic-`n` at write guarantees they coincide): if it
is `converged:false`, assert no `forge.pr.merged`/`forge.issue.closed`/`done` for S later *unless*
`ratify.resolved{grant}` + a later `review.round(S, converged:true)` intervene; and any merge for S
carries that converged round's `head_sha` (both `head_sha`s derive from gh's `headRefOid`, so the
equality is meaningful — B3). A `phase:design` round (no `pr_number`) is a distinct subject.

**4e. Crash-recovery — `review_round_recovers_into_pending_queue`:** persist a `review.round`/`ratify.*`
then boot before the spec processes it; assert recovery into `snapshot.pending_queue` via
`replay_harness_events_since` + `harness_observation_from_event` (idempotent under the push watermark;
+ the review.round `idempotency_key` dedup — no duplicate round on re-drive, B1).

**4f. FU4 teardown fence:** after merge+close, assert `workspace.released{lease_id}` persisted, no
dangling worktree/branch, boot does NOT re-reclaim a released lease; + the guard — a wave/cove teardown
while a parked worker forge-op is in flight is fenced (409 / lease held across the op).

**Acceptance (FLIPS rows 5,6,10-dual,11,12,17).** All branches pass; **row 5** (dual-reviewer part),
**row 6** (design dual-review before impl dispatch), **dual-channel half of row 10**, **rows 11/12**
(round≤cap monotone + re-review), **row 17** (temporal subject-keyed cap-enforcement: GIVE-UP whole-run
absence, ASK-HUMAN two-edge + absence-until-grant). Stable ×3.

**Size.** **L**. **Deps.** ⑤-a, ⑤-b, ⑤-c, ③ (forge backbone), ⑥/⑦.

---

## 5. Dependency chain & flip-owner

`⑤-a (workflow→spec consumption) → ⑤-b (events+tools+ratify+role-gate, row 4) → ⑤-c (descriptor
dual-review policy + gh.pr.merge --match-head-commit) → ⑤-d (E2E, rows 5,6,10-dual,11,12,17)`. Rows:
4→⑤-b; 5,6,10-dual,11,12,17→⑤-d; ⑤-a/⑤-c are substrate (direct self-tests). Each sub-slice: Codex
worktree impl → two-channel review → gates (fmt/clippy `-D`/test + ts-rs regen + golden update for
⑤-b; OpenAPI regen for ⑤-b's `/ratify` route) → squash-merge. Doc lands with ⑤-a's PR.

## 6. Resolved open questions

- **Q1 (review.round store)** → **F1: the event spine** + idempotency_key + latest-by-id query + dedup
  guard (B1). NOT KV, NOT a forge operation.
- **Q2 (ratify routing)** → **F2:** `calm.ratify.request` spec tool; `grant`/`deny` User REST route;
  + event-level role-gate arms (B2).
- **Q3 (dual-reviewer tagging)** → **F3:** distinct keys + `context_json`; the primitive = descriptor
  deps + `review.round.channels[]` (≥2) + E2E.
- **Q4 (FU2)** → **F5: DROP.**
- **Q5 (merge fence)** → **F4: POLICY + E2E verification + a runtime `--match-head-commit` head-match
  guard (B3)**; full converged-bit hard-gate is a non-goal (moat). Same trust model underlies the
  spec-authored `converged` value.
- **Q6 (review subject shape)** → distinct **`ReviewSubject{phase,slice_id,pr_number:Option<u64>}`**
  (2a-i).
- **Q7 (⑤-a ownership)** → ⑤ owns the consumption gap; may re-home to a ④ follow-up; lands before ⑤-c.
- **Q8 (plan_template → spec)** → ⑤-a lean: inline in the rendered instructions; `calm.workflow.template`
  read tool is the fallback.

## 7. Disposition history

### ROUND 1 (dual-channel; v2 fold) — 2026-06-22

> Channel A (fresh subagent, correctness/completeness/consistency): **0 blocker / 4 should-fix / 5
> nits**, verdict NOT-CONVERGED — architecture/contract/FSM-soundness/completeness all CORRECT; the
> should-fixes are grounding-cite + one false-safety-claim that would misdirect the implementer.
> Channel B (codex, failure-path/operation-framework/moat lens): **2 blockers / 1 should-fix**,
> verdict NOT-CONVERGED — review.round idempotency + event-level authority + head-match runtime guard.
> No architectural disagreement; the rounds hardened three real substrate gaps.

| finding | channel | disposition |
|---|---|---|
| **B1 — review.round not idempotent** (events table no dedupe key, `0004_events.sql:23-32`; idem index only on operations `0042_operations_parked.sql:96`) | B (BLOCKER) | **FOLDED (F1 hardened + ⑤-b).** `calm.review.round` carries `idempotency_key` + a pre-emit dedup guard. *(Round-1's first cut keyed the cap query on latest-by-event-id; **SUPERSEDED in round 2** by strict-monotonic-`n` + the `max(n)` cap query — see the round-2 ledger.)* Codex's operations-index alternative considered + REJECTED (operations are spawn-shaped; a synchronous emit doesn't fit `ProviderAdapter` — the forge-op-grounding "dead machinery"). Stays a spec-authored event. |
| **B2 — event-level authority hole** (`enforce_role` `Ok(())` default `role_gate.rs:417`; ungated new events) | B (BLOCKER) | **FOLDED (⑤-b 2e).** New `enforce_role` arms: `ReviewRound`/`RatifyRequested` spec-only; `RatifyResolved` User-only; + negative tests (plugin-forged review.round; non-User grant). Closes the "self-approve the human gate" hole. |
| **B3 — head-match detect-after-the-fact** (no pre-merge head guarantee; merge argv `plugins/git-forge/main.rs:390`; `gh` supports `--match-head-commit`) | B (SHOULD-FIX) | **FOLDED (F4 + ⑤-c 3c).** `gh.pr.merge` gains `expected_head_sha` → `--match-head-commit`; joins the merge idem key. `review.round.head_sha` sourced from `forge.pr.diff.read.head_sha`. Runtime head-match guard at the forge layer. Codex REJECTED the narrow "shapes can't support it" framing (the shapes do); the real gap was prevention vs detection. |
| **A1 — `is_hard_fire` is `matches!`, NOT compile-forced** (`observation.rs:115`); a new variant silently defaults to non-hard-fire | A (SHOULD-FIX) | **FOLDED.** Corrected the claim in §0 + §2.5-C; ⑤-b 2f explicitly HAND-ADDS the 3 variants to the `is_hard_fire` `matches!`. Only `to_turn_text` is compile-forced. |
| **A2 — `Observation`/`is_hard_fire`/`to_turn_text` crate path** (calm-types, not calm-server shim) | A (SHOULD-FIX) | **FOLDED.** Re-cited `crates/calm-types/src/observation.rs:18/114/138`; noted the calm-server `harness/observation.rs` re-export shim. |
| **A3 — Spec prompt render site** (`spec_harness_start_adapter.rs:395` `SeededCardRole::Spec`, NOT `codex_adapter.rs:1119` Worker) | A (SHOULD-FIX) | **FOLDED.** ⑤-a files-touched corrected to `operation/spec_harness_start_adapter.rs`; `codex_adapter.rs:1119` noted as the Worker path. |
| **A4 — `TaskKind` crate/line** (`crates/calm-truth/src/model.rs:320`; `calm-types/model.rs:318` is `WaveLifecycle::is_terminal`) | A (SHOULD-FIX) | **FOLDED.** Re-cited. |
| **A nits** — line drift: `ForgeMergeSubject :346` / `ForgePrMerged :777`; `forge_workflow_e2e` fns `:187`/`:406`, `event_rows :1400`, `wait_for_event_count :1422`; `Event` tag `:364`; `wireEventSchema :754`; `neige.kv` dispatch `:198-201`; `invalidationPolicies` path `web/src/app/`; `calm.task.verdict` lifecycle via `commit_spec_verdict`→`apply_requested_transition_in_tx`; goldens 61→64 | A (NIT) | **FOLDED** (all re-anchored in §0). |

**Round-1 rejections: NONE.** Both channels' findings verified against live code at `72f10bee` and
folded. Channel B's lead-3 narrow framing ("shapes can't support the assertion") was self-rejected by
channel B (the shapes do support it); the substantive fix (prevention via `--match-head-commit`) was
folded. No architectural change — the contract + sub-slicing + FSM soundness are unchanged from v1.

### ROUND 2 (dual-channel; v3 fold) — 2026-06-22

> Channel A (subagent): **0 blocker / 1 should-fix / 3 nits** — verified EVERY round-1 fold correct at
> `72f10bee`, **0 regressions**, and refuted one self-raised concern (idempotency_key-in-payload is
> fine; `TaskCompleted`/`TaskFailed` already carry it through goldens/ts-rs). Channel B (codex):
> **1 blocker / 1 should-fix** — confirmed B2/B3/A1–A4 + the B1 in-tx concurrency are sound; the
> residual blocker is the same B1 facet (out-of-order duplicate). The two channels' findings are ONE
> fix. Blocker trajectory: **2 → 1** (the dual-channel drilling the idempotency core to ground).

| finding | channel | disposition |
|---|---|---|
| **B1-r2 — out-of-order duplicate hole** (the v2 guard queried only the LATEST round for S; a delayed stale `(S,n=1)` after `(S,n=2)` falls through + appends as new latest, regressing §4d) | B (BLOCKER) | **FOLDED (F1 + ⑤-b 2b + ⑤-d 4d).** Guard now enforces **strict monotonic `n` per subject**: query `max(n)` over ALL prior rounds for S; idempotent same-`(S,n)`+hash ⇒ no-op; `n ≤ max(n)` diff-payload ⇒ reject; only `max(n)+1` appends. Cap query keys on `max(n)`, not max-event-id. |
| **B1-r2 / A-r2 — dedup guard atomicity** (the query-then-emit must serialize, else two re-drives both read "n absent" + both emit) | A (SHOULD-FIX) + B (fold-check) | **FOLDED.** The dedup read + emit run INSIDE one write tx (`write_with_actor_events`, under the repo's `BEGIN IMMEDIATE` which serializes concurrent spec turns — codex verified). The reject-on-conflict branch is now enforceable. |
| **B3-r2 — merge idem-key backward-compat** (`{repo}:{pr}` → `{repo}:{pr}:{head}` orphans a pre-⑤ op) | B (SHOULD-FIX) | **FOLDED (⑤-c 3c).** Greenfield workflow — no live merge ops to orphan; stated. (Legacy-key fallback noted as unnecessary-here.) |
| **A-r2 nits** — `write_with_actor_events` (no `_typed` suffix); the spec-only role-gate arms are NOVEL (kernel-only arms allow `User`, these reject it — don't copy `:275` verbatim); the `kind_tag`/golden recipe adds 3 arms in two places | A (NIT) | **FOLDED** (2b cite fixed; 2e wording tightened with the `AiSpecSession`→`AiSpec` resolution note; recipe already enumerated). |

**Round-2 rejections: NONE.** Both channels' findings verified at `72f10bee` and folded. Channel A's
"worktree HEAD is `393dd5b6`" side-remark was incorrect (verified: the worktree IS at `72f10bee`,
where all its `git show 72f10bee:` cites were checked — so its review is sound); origin/main has since
advanced to `7adc22a9` (unrelated PRs) — impl branches rebase onto current `origin/main` at PR time.
No architectural change across any round — contract, sub-slicing, FSM soundness unchanged from v1.

### ROUND 3 (dual-channel; v4 fold) — 2026-06-22

> **Channel B (codex): CONVERGED — 0 blocker / 0 should-fix.** Confirmed the strict-monotonic-`n` +
> in-tx (`BEGIN IMMEDIATE`) + `max(n)` cap-query FULLY closes the duplicate/out-of-order class across
> crash, concurrent-turn, replay (replay re-queues observations, does NOT re-emit), and design-vs-PR
> namespace; the greenfield merge idem-key is acceptable; no new blocker. **Channel A (subagent):
> 0 blocker / 1 should-fix / 2 nits** — verified all round-2 folds correct, 0 regressions, and caught
> the one residual MECHANISM gap channel B missed: the in-tx **idempotent-duplicate no-op cannot return
> an empty batch** (`write_with_actor_events` rejects it, `sqlite.rs:6227`). The two-channel divergence
> is the value: B proved the *contract* sound, A caught the *realizability* detail.

| finding | channel | disposition |
|---|---|---|
| **A-r3 — in-tx no-op can't return an empty batch** (`write_with_actor_events` rejects empty, `sqlite.rs:6227`; cf. `plan.rs:978`; `calm.task.verdict` does NO in-tx dedup so isn't the precedent) | A (SHOULD-FIX) | **FOLDED (⑤-b 2b + F1 + §0).** No-op realized via a **pre-read fast-path + early `Ok(no-op)`** (the `plan.rs:966-982` precedent); the in-tx write closure re-reads `max(n)` and maps a racing duplicate (`Err` rollback) to `Ok(no-op)`/reject at the tool layer. Only `max(n)+1` commits a row. Exact mechanism reviewed at impl. |
| **B-r3 — stale round-1 ledger wording** ("latest-by-event-id", superseded by `max(n)`) | B (NIT) | **FOLDED.** Round-1 B1 ledger row annotated as SUPERSEDED by the round-2 `max(n)` fold. |
| **A-r3 nits** — §0 batch-writer cite pointed at `write_with_events:6158` (non-actor); `AiSpec→SpecAgent :110`→`:111` (1-line drift) | A (NIT) | **FOLDED.** Re-cited `write_with_actor_events:6210`/`:6227`; `:111`. |

**Round-3 rejections: NONE.** A-r3's should-fix verified TRUE (the empty-batch rejection + the
`plan.rs` precedent both confirmed at `72f10bee`) and folded; B's contract-convergence verified.
Channel B CONVERGED; channel A's lone should-fix is a realizability detail (now folded), not an
architectural change. 0 regressions.

### ROUND 4 (dual-channel; CONVERGED) — 2026-06-22

> **BOTH channels CONVERGED on v4 — design APPROVED at contract altitude.** Channel B (codex,
> confirmatory): **0/0/0** — the v4 no-op realization is sound (the pre-read fast-path returns early
> only for an already-persisted exact `(S,n)`+hash duplicate — idempotent since events are append-only;
> concurrent new submissions serialize on the in-tx `max(n)` re-read under `BEGIN IMMEDIATE`: one writer
> commits `max(n)+1`, racing duplicates roll back → `Ok(no-op)` if identical else reject); no new defect.
> Channel A (subagent, confirmatory): **0/0/0** — the round-3 should-fix is correctly resolved
> (`write_with_actor_events:6210`/empty-batch-reject `:6227`/`plan.rs:966-982` precedent all verified),
> both cite nits fixed, no stale wording leaked into live mechanism text, all cross-references consistent,
> **implementation-ready end-to-end.**

**CONVERGED after 4 dual-channel rounds.** Blocker trajectory: **2 → 1 → 0 → 0.** Every finding across
all four rounds was a facet of the ONE `review.round` idempotency core + cite/realizability corrections
— **no architectural disagreement in any round**; the contract / sub-slicing (⑤-a..d) / FSM-soundness /
flip-owner are unchanged since v1. **The design is APPROVED.** The only deferred items are
implementation-review deliverables, not design gaps: the EXACT no-op mechanism (pre-read fast-path vs a
`_typed`-sentinel rollback) and the precise `ReviewSubject`/`ChannelVerdict`/`RatifyDecision` payload +
golden shapes. Implementation order: **⑤-a → ⑤-b → ⑤-c → ⑤-d**, each Codex worktree impl → two-channel
review → gates → squash-merge.
