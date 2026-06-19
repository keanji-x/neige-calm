# #760 — Issue→PR Dev-Flow as the First Workflow Plugin (CONVERGED v5 design)

> Synced from GitHub issue #760 (converged after 5 dual-channel review rounds, 2026-06-18).
> This is the in-repo copy of the converged design; it lands with slice ①'s PR.
> `file:line` cites were valid as of HEAD `b358b8f7`; prefer the named symbol where a line has drifted.

---

> ## ✅ Design CONVERGED — 5 dual-channel review rounds (2026-06-18)
>
> The issue→PR dev-flow design has converged. Blocker trajectory across 5 rounds of independent dual-channel review (fresh subagent + `codex exec` read-only, archived in-repo as `docs/_760-design-review-{subagent,codex}-v{1..5}.md`): **5 → 1 → 3 → 4 → 0**. The round-3/4 rise was the two channels correctly drilling the genuine hard core — **exactly-once execution of an irreversible, output-producing forge action (`gh pr merge`)** — now resolved (§2.5-A). Round 5: both channels 0 blocker / 0 should-fix.
>
> **Key resolved decisions:** forge/worktree actions are **first-class parked/idempotent operations** modeled on the `task-verify` gate pattern — held-handshake so nothing irreversible runs before durable park; **post-park** go-token release; exit-code + **bounded** `--json`-field result extraction into **typed** events; recovery **probes** "did it land" rather than re-running. The kernel stays free of **workflow** git logic (argv/extractor/probe are plugin payload *data*; typed event shapes are shared-types definitions). Ratify uses `failed`(terminal) vs `blocked`(pause) with a per-subject `{phase,slice_id,pr_number}` merge fence (`head_sha` = reviewed revision).
>
> The full converged v5 doc follows in 5 comments (§0–§7). The two remaining open items are **slice-⑥ implementation scope** (post-park-release mechanism variant; exact bounded-extractor grammar), not design gaps. This doc lands in-repo with slice ①'s PR.
>
> ⚠️ `file:line` cites are valid as of HEAD `b358b8f7`; prefer the named symbol where a line has drifted (the repo auto-syncs and advances HEAD).

---

_**Part 1/5 — §0–§2** (title, context/goals/non-goals, oracle trace, architecture)._

# #760 Design v5 — CONVERGED after 5 dual-channel review rounds (2026-06-18) — Issue→PR Dev-Flow as the First Workflow Plugin

_Blocker trajectory 5→1→3→4→0 across 5 rounds (the round-3/4 rise was the dual-channel drilling the forge-action exactly-once core, since resolved). Remaining open items are slice-⑥ implementation scope (post-park-release mechanism variant; bounded ForgeEventSpec extractor grammar), not design gaps._

> Scratch design doc for GitHub issue #760. Grounded in file:line. Lands in-repo with
> slice ①'s PR. **Do not git-commit this file standalone.** Disposition history (§7) is
> filled across the dual-channel review rounds. **v5** folds round-4 dual-channel review
> (channel A correctness/completeness/consistency lens — CONVERGED with 3 nits; channel B
> failure-path/operation-framework lens — found **4 real blockers in the forge-action contract**)
> on top of v4's round-3 fold. **Round-4 headline: the v4 "faithful copy of `TaskVerifyAdapter`"
> was NOT sound for the forge-action operation** — task-verify is a *resultless idempotent gate*,
> whereas a forge action is *irreversible with a typed OUTPUT*, so a naive copy reopens four holes
> (R4-1 pre-park release window; R4-2 Ready can't emit oracle events; R4-3 no typed result/event
> wire contract; R4-4 subject-key unsoundness). §2.5-A is re-designed to a **PURPOSE-BUILT
> exactly-once completion/recovery/result contract** (NOT a naive task-verify copy, NOT a generic
> exec seam): **POST-PARK go-token release** (nothing irreversible runs until durably parked),
> **ALL oracle-visible actions use the parked-completion helper** (the read-only→`Ready` shortcut is
> removed for any action that must emit an event), a **BOUNDED typed result-extraction** wire
> contract (typed event variants in shared types + a bounded exit-code|named-stdout-field extractor
> spec in the plugin payload — replacing v4's exit-code-only without resurrecting v3's unbounded
> JSON-predicate DSL), and a **LOGICAL subject key** `{phase,slice_id,pr_number}` with `head_sha` as
> the reviewed *revision*. R4-1/2/3 are treated as ONE root cause (the operation needs its own
> exactly-once contract); the exact stdin-handoff mechanism + extractor grammar are scoped to slice
> ⑥'s implementation+review. See §7 for every finding's disposition (incl. the A=converged /
> B=4-blockers divergence).
>
> **file:line cites valid as of HEAD `b358b8f7`; prefer the named symbol if a line has
> drifted.** The tree moved under this doc twice (round-1→2 and a mid-round-2 external pull),
> so every cite pairs a symbol/function name anchor with the line. **Crate qualification
> (round-2 B-1/SF-4):** all `event.rs` cites are `crates/calm-types/src/event.rs` (the enum,
> `kind_tag`/`metadata`/`topics`/`SYNC_EVENT_VERSION`/`from_kind_and_payload`/fixtures/ts-rs
> derive all live there); `crates/calm-server/src/event.rs` is a 5-line `pub use` re-export shim —
> the same B11 rule the v2 doc applied to `wave_vcs` but missed for `event.rs`. (Note: a new event
> needs per-variant arms only in `kind_tag`/`metadata`/`topics`; `from_kind_and_payload` is generic
> serde and needs no arm — round-2 SF-3.)

---

## §0 Context, Goals, Non-Goals

### Problem
Drive a full **issue → PR software-development workflow** on a neige-calm WAVE, steered by
its spec agent (scan → challenge → design+dual-review → slice → impl → dual-review one PR's
diff → fix-loop → gate → squash-merge → close), with the architect supervising at ratify
points. **The dev-flow is an external *workflow plugin*, not kernel code.**

### Layering principle (the moat)
The kernel stays **generic worker infrastructure** — workflow-agnostic. "Issue development"
is the **first** workflow plugin; future workflows (research, content, ops, refactor-audit)
are more plugins reusing the same primitives.

| Kernel (stays generic) | Issue-dev workflow plugin (external) |
|---|---|
| wave / card / spawn·supervise·reap | plan template (slice shape) |
| observe / turn / scheduler / gate | what each gate checks (`gate_json`) |
| parked ops / snapshot / wave-vcs | review protocol (dual-channel, cap) |
| concrete `ForgeActionAdapter` (compiled-in; forge-specific exactly-once contract: POST-PARK release + atomic parked-completion helper for ALL oracle actions + bounded typed result-extraction) + workspace-lease primitive (both workflow-agnostic) | git/forge EXECUTION semantics — *which* `gh`/`git` argv, which verbs, which probe argv, which `--json` field paths (all supplied to the adapter via the op payload as DATA) |
| `calm.*` primitive tools | spec-agent instructions |
| event spine, role gate, FSM | git/forge toolset (worktree/branch/PR/checks/merge) |

> **Moat claim, narrowed honestly (round-2 SF-1/S-1).** The original framing "the kernel knows
> nothing about git" is **false** in two ways the code already exposes, so the moat is restated as
> **"no *workflow* git logic in the kernel."** (1) `crates/calm-server/src/routes/fs.rs:552-559`
> (`git_root` → `git rev-parse --show-toplevel`; `git_output` at `:567`) already shells `git`
> inside the kernel for a read-only file-browse REST surface — unrelated to the dev-flow, but it
> means "git-free" was never literally true. **Note (round-3 N-3): fs.rs git is *read-only*
> (`rev-parse`, browse) whereas the forge adapter does *mutating* git/gh, so the two are not the
> same risk class — the retraction is honest, but the mutating side-effect is exactly why the forge
> adapter must be a crash-safe OPERATION, not a bare shell-out.** (2) The `ForgeActionAdapter`
> (§2.5-A) is a **compiled-in kernel-crate type** (`ProviderAdapter` impls all live in
> `crates/calm-server/src/operation/*.rs`; `build_operation_adapters` returns
> `Vec<Arc<dyn ProviderAdapter>>` of concrete kernel types — `crates/calm-server/src/state.rs:350`
> `build_operation_adapters`), and there is **no plugin-provided-adapter path** (the only
> kernel→plugin reach is the **request/response RPC** `crates/calm-server/src/plugin_host/mcp.rs:507`
> `tools_call` — it `await`s a parsed `CallToolResult`, round-3 N-1 — which is **not** an operation).
> So the forge adapter that shells `gh`/`git` necessarily lives in calm-server. The DECISION
> (§5-Q2, re-sharpened round-4): the kernel hosts a **concrete `ForgeActionAdapter` with a
> PURPOSE-BUILT exactly-once contract** — borrowing task-verify's *held-handshake spawn shape* but
> NOT its semantics (a forge action is irreversible-with-a-typed-output, not a resultless idempotent
> gate, so a naive copy reopens R4-1/2/3): **POST-PARK go-token release** (R4-1 — nothing
> irreversible runs until the op is durably parked), **ALL oracle-visible actions complete via the
> custom parked-completion helper** (R4-2 — no `Ready` shortcut for anything that must emit an
> event), and a **BOUNDED typed result-extraction** wire contract (R4-3 — typed event variants in
> shared `calm-types`; the payload carries the target typed event kind + a bounded
> exit-code|named-stdout-field extractor spec + recovery probe argv — §2.5-A); the
> **git/gh EXECUTION semantics** — which argv, which verbs, which probe argv, which `--json` field
> paths — are **supplied by the workflow plugin via the operation payload**, so **no git/gh
> verb-execution logic compiles into the kernel**. The argv *strings* + the bounded extractor *spec*
> transit the kernel as DATA; the *taxonomy/policy* that produces them stays plugin-side. **SF-1/C7
> tension resolved (round-4 R4-3):** the typed event DATA shapes (e.g. `ForgePrMerged{merge_sha}`)
> live in shared `calm-types` as the issue-dev workflow's contribution to the shared event enum —
> data definitions, **no logic**; the verb-execution logic stays plugin-supplied payload data. This
> is "forge as operation" = a **concrete kernel adapter** (compiled-in, like the existing 10) with a
> forge-specific exactly-once contract — NOT a naive task-verify copy (round-4 R4-1/2/3) and NOT a
> generic plugin-recovered exec seam (the operation framework does not provide one; round-3 C1/C2/C3
> are one root cause: §2.5-A's "generic thin exec adapter recovered generically by the kernel" does
> not fit the framework).

### Why this is tractable today
The kernel is already half a "workflow interpreter": **plan is data** (`calm.plan.upsert`
= tasks + deps), **gate is data** (`gate_json` shell steps — `task_verify_adapter.rs:660-665`
parses `tasks.gate_json` into a `GateSpec`), **lifecycle is an FSM**
(module-doc edge list `crates/calm-types/src/wave_lifecycle.rs:30-44`; the live match arms are
`:252-278` inside `validate_transition` `:170-295` — round-3 channel-A clarification).
A plugin that, given a goal, emits *plan + gates + agent
instructions* is largely executable by the generic kernel **today**. The inner loop is
empirically confirmed: a tier-1 smoke run today did `POST /api/coves` then `POST /api/waves`
and auto-minted `spec_card` + `report_card` with health 200 (`Smoke OK cove=… wave=…
spec_card=… report_card=…`).

### Goals
- A golden **oracle trace** (§1) that is the E2E acceptance oracle and the slice driver.
- A kernel/plugin split (§2) that keeps git/forge **out** of the kernel.
- Per-gap designs (§3) and an executable slice plan ①→⑦ (§4).
- A **Durability & Recovery** design (§2.5) making forge/worktree actions crash-safe operations.

### Non-Goals (this issue)
- **Replacing codex-as-spec.** Codex stays the spec agent (only injectable-turn app-server).
  The plugin supplies instructions/policy; reopening the orchestrator-model choice is out of
  scope.
- **Migrating wave-vcs to real git** as the *projection* store. wave-vcs remains the SQLite
  projection archive (§5). Real code diffs, if added, live on a **separate** git backend.
- Multi-workflow generality proofs (research/content/ops). #760 ships only issue-dev; the
  plugin surface is designed to generalize but only one consumer is built.

---

## §1 ORACLE TRACE — the golden end-to-end sequence (north star)

**Preamble — what the trace asserts.** A real wave run is **stochastic**: agent prose,
slice names, commit messages, and even the number of fix rounds vary run-to-run. The oracle
trace therefore asserts the **invariant backbone**, never agent content:
1. **Event-kind backbone** — the ordered set of event *kinds* that MUST appear (e.g.
   `wave.lifecycle_changed`, `task.dispatched`, `task.gate_result`). These are deterministic
   regardless of what the agent writes.
2. **FSM legality** — every `wave.lifecycle_changed {from,to}` is a legal transition
   (module-doc edge list `crates/calm-types/src/wave_lifecycle.rs:30-44`; the live match arms are
   `:252-278`, enforced by `validate_transition` `crates/calm-types/src/wave_lifecycle.rs:170-295` —
   round-3 channel-A clarification).
3. **Required git/forge effects** — a branch exists, a PR exists, `gh pr checks` is green, the
   issue is closed. These are observable forge facts, not prose.

The E2E (§6) asserts *real trace's backbone ⊇ oracle backbone* + *a short list of REQUIRED
pairwise ordering invariants* + *required artifacts exist* + *FSM legal*, and is tolerant of
content. Each ⚠️/❌ row is a **design target → slice**; ✅ rows are grounded in event kinds
that exist today.

**Two backbone branches (round-2 SF-2 — `blocked` is a PAUSE, not terminal).** The trace asserts a
**CONVERGE** branch (the happy path: review approves within cap → merge → close → `done`) AND a
**CONVERGENCE-FAILURE** branch — and the failure branch has **two distinct sub-terminals that the
v2 doc wrongly conflated**:
- **cap-exhausted GIVE-UP → `reviewing→failed`** (`crates/calm-types/src/wave_lifecycle.rs:274`): a
  **terminal** state. The run is over; the merge tail MUST be absent for the whole run.
- **awaiting-human-ratify → `working→blocked`** (`crates/calm-types/src/wave_lifecycle.rs:270`): a
  **PAUSE**, NOT terminal. `blocked→working` is a legal edge (`:278`), so a granted run may resume
  `blocked→working→reviewing→done` (`:278,:271,:273`) and **legally re-enter the CONVERGE branch
  with a full merge tail**. Treating `blocked` as terminal (as v2 did) would make the E2E **fail a
  legal granted-then-reconverged run**.

A run that takes either failure sub-path is still a *legal* run. The **enforceable** cap assertion
is therefore NOT "merge tail absent for the whole run" but the **temporal, SUBJECT-KEYED** invariant
(round-3 C5/N-5, **subject key corrected round-4 R4-4**): *no `forge.pr.merged` for SUBJECT S may
appear while the latest `review.round` **FOR SUBJECT S** has `converged:false`* (§6). The **LOGICAL
subject key** is `{phase, slice_id, pr_number}` (round-4 R4-4 — `head_sha` is NOT part of the
grouping key); `head_sha` is instead carried as the reviewed **REVISION** (a field). A subject groups
all review rounds across head revisions of the same PR, so the "latest round for S" advances as the
PR is fixed and re-pushed — **a later CONVERGED revision supersedes an earlier unconverged one**.
(The v4 key `{phase,slice_id,pr_number,head_sha}` was UNSOUND: with `head_sha` in the grouping key,
an old unconverged head stays "latest" for its own singleton subject forever, and a later converged
head — a *different* subject — never supersedes it, so the fence could never clear.) The subject key
is carried in BOTH the `review.round` payload AND the `forge.pr.merged` payload, plus the
`head_sha`/revision; a design-phase round (no `pr_number`) is a different subject so it never masks a
per-PR merge fence. **The merge head MUST == the latest CONVERGED revision for S** (R4-4): a merge
whose head_sha is not the head that the latest converged round reviewed is illegal. (The events table
`crates/calm-truth/migrations/0004_events.sql:23-32` has no schema column for the subject; it lives
in the payload, evaluated per-subject by the E2E.) The `reviewing→failed` sub-path additionally
asserts the whole-run merge-tail absence for that subject (terminal); the ratify→`blocked` sub-path
asserts merge-tail absence **only until a `ratify.resolved(grant)` appears**, after which the merge
tail is allowed again. This keeps `n ≤ cap` enforced per subject (rows 11/17) without false-failing a
resumed run: at `n == cap` unconverged for S, no merge for S may fire *while that round is the latest
and unconverged for S*; a later granted round for S (any revision) that converges may merge, and the
merge head must match that converged revision.

**Per-event scope (B4).** Every NEW event carries the *narrowest* scope (`crates/calm-types/src/event.rs:167-182`
— pick `System` only when no cove/wave/card fits; `topics()` falls back to `"*"` without
payload ids). The scope column below records the chosen `EventScope` for each NEW event so
dispatcher routing (`SubscribeFilter` by wave) and `enforce_role` per-card scope both work.

**Column legend.** `seq | phase | actor | trigger/MCP tool | git/forge effect | observable
event(s) (REAL kind + file:line; "NEW" = to design) | scope | invariant assertion
(deterministic) | status ✅/⚠️/❌`.

| seq | phase | actor | trigger / MCP tool | git/forge effect | observable event(s) | scope | invariant assertion | status |
|---|---|---|---|---|---|---|---|---|
| 1 | 0 issue→wave | human | `gh issue view <n>` → `POST /api/waves` (title=issue body) | — | `wave.updated` `event.rs` + `card.added`×2 (spec+report) + `overlay.set` (layout) — ALL emitted in one tx by `create_wave_with_spec_harness` (`routes/waves.rs:539-550` — the `vec![` opens at `:539`: `WaveUpdated` :542, `CardAdded`×2 :547,548, `OverlaySet` :549; round-2 N-1 tightens the clipped `:542` start). **No `wave.lifecycle_changed` here** — new waves seed at `Draft` (`crates/calm-truth/src/db/sqlite.rs:738-744`); the `Draft→planning` flip happens LATER at the first plan-upsert (row 5) | wave / card×2 / wave | wave row exists (lifecycle==`Draft`); spec_card+report_card minted; layout overlay set (empirically `Smoke OK`) | ⚠️ create exists; `gh issue view` ingestion ❌ |
| 2 | 0 issue→wave | spec-agent | reads goal observation (WaveGoal) | — | spec turn injected via `/spec/input` registry (`routes/cards.rs:118,650`) | card (spec) | first spec turn fires; goal text == issue body | ⚠️ goal manual today; no `gh issue view` primitive |
| 3 | 1 entry-scan | spec-agent | **NEW** `gh.pr.list{state:open}` (plugin tool, routed through a forge OPERATION — see §Durability A8/A9) | — | **NEW** `forge.scan.completed{wave_id, overlapping_prs}` (wave_id in payload per C6) | wave | scan emits ≥1 event recording open-PR set; if overlap → pause | ❌ no GitHub-read primitive (`mcp_server/` zero `gh` hits) |
| 4 | 2 challenge/ratify | spec-agent | `/spec/input` (passive) OR **NEW** `calm.ratify.request` (kernel/spec-authored — see B5) | — | **NEW** `ratify.requested{reason}` then `wave.lifecycle_changed{to:blocked}` (legal FSM: `crates/calm-types/src/wave_lifecycle.rs:270`, edge `working→blocked`) | wave | risky slice → wave parks in `blocked`; resumes only on human verdict event | ⚠️ `/spec/input`+`Blocked` exist; structured ratify gate ❌ |
| 5 | 3 design+dual-review | spec-agent | `calm.plan.upsert` (design+review tasks) | — | `plan.updated{changed_keys}` `crates/calm-types/src/event.rs:685-691` (`mcp_server/tools/plan.rs`); **the `Draft→planning` auto-promotion fires HERE** via `auto_promote_draft_in_tx` (`mcp_server/tools/plan.rs:807`) → `wave.lifecycle_changed{to:planning}` in the SAME tx | wave | plan carries ≥2 review tasks with disjoint reviewer roles; first plan-upsert promotes `Draft→planning` (legal FSM `crates/calm-types/src/wave_lifecycle.rs:252`) | ⚠️ plan exists; **multi-reviewer primitive** ❌ |
| 6 | 3 design+dual-review | reviewer×2 | two `codex`/`terminal` **design-review** tasks (the kernel claims them — see A4) | — | `task.dispatched{kind}`×2 (kernel-driven: `scheduler.rs:553-555` authors as `KernelDispatcher`, kernel-only per `crates/calm-truth/src/role_gate.rs:220-239`); `task.completed`×2 `emit.rs:161-166`; **NEW** `review.round{wave_id, subject:{phase:design,slice_id}, n, cap}` (wave_id+subject in payload, C6; logical subject key — no `pr_number`/`head_sha` for a design round, R4-4) | task×2 / wave | **DESIGN-PHASE dual review** runs BEFORE any impl task is dispatched: two INDEPENDENT design-reviewer cards run; both verdicts recorded | ❌ no dual-reviewer primitive; projection renders one `report.md` (`crates/calm-truth/src/wave_vcs.rs:364-439`, single `report.md` entry at :380-382) |
| 7 | 4 slice + worktree | kernel | scheduler claims slice task (`compute_ready` `scheduler.rs:118-145`) | **NEW** `git worktree add .claude/worktrees/<slice>` via the **isolated-workspace-lease OPERATION** (§Durability B2) | `task.dispatched{kind:codex}` (kernel-driven, `scheduler.rs:553-555`); **NEW** `workspace.leased{path,lease_id}` (kernel, workflow-agnostic) + plugin-layer `worktree.provisioned{path}` | card (task) / card | each claimed Codex task runs in a DISTINCT leased cwd under `.claude/worktrees/`; cwd∈payload; lease held by the op | ❌ cwd dropped for Codex (`scheduler.rs:153-162`); no worktree; budget=1 (`scheduler.rs:71-75`); no resource-disjointness check (`compute_ready` is budget-arithmetic only `scheduler.rs:118-145`) |
| 8 | 5 impl → branch+commits | worker | codex worker spawns in worktree | **NEW** `git checkout -b <slice>` + local commits | `runtime.started` (real Codex emit `crates/calm-server/src/operation/codex_adapter.rs:329`); `runtime.status_changed` (real Codex emits `crates/calm-server/src/operation/codex_adapter.rs:1486,1590` — round-3 channel-A drift fix; was cited :1481,1585 which are the `.await?` lines above each emit); hooks `crates/calm-types/src/event.rs:552-585` | card (worker) | worker spawn/run/exit ordered; **NEW** branch ref exists with ≥1 commit | ⚠️ spawn/run/supervise OK; **no git branch/commit** (shared tree) |
| 9 | 5 impl → PR | worker | **NEW** `gh.pr.create{base,head}` (plugin tool → forge OPERATION) | **NEW** PR opened; pushes branch | **NEW** `forge.pr.opened{wave_id, pr_number, head_sha}` (wave_id in payload per C6) | card (pr) | PR number recorded on the slice card; head_sha == branch tip | ❌ nothing opens a PR (lifecycle→`done` is FSM only) |
| 10 | 6 per-PR diff + dual review | reviewer×2 | **NEW** `gh.pr.diff{pr_number}` (real code diff, branch+merge-base; forge OPERATION) | — | **NEW** `forge.pr.diff.read{wave_id, pr_number, base_sha}` (wave_id in payload per C6); `task.dispatched`×2 (kernel-driven, `scheduler.rs:553-555`) | card (pr) / task×2 | both reviewers read EXACTLY one PR's CODE diff (not projection); base==merge-base(main,head) | ❌ `calm.wave.diff` diffs PROJECTION docs (`wave_history.rs:21-58`, `crates/calm-truth/src/wave_vcs.rs:688-703`); no branch/merge-base |
| 11 | 7 fix-loop (CONVERGE) | spec-agent | review verdict → `calm.plan.upsert` (fix task) | — | `plan.updated{changed_keys}` `crates/calm-types/src/event.rs:685-691`; **NEW** `review.round{wave_id, subject:{phase,slice_id,pr_number}, head_sha (reviewed revision), n, cap}` (wave_id+subject in payload, C6; **logical subject key `{phase,slice_id,pr_number}`, `head_sha` is the reviewed revision not a key part — R4-4**) persisted as a forge/review OPERATION + plugin store | wave | round-N monotone per subject; **n ≤ cap ENFORCED per subject** (see row 17 for the cap-hit branch); a later converged revision supersedes an earlier unconverged one (R4-4); a fix ALWAYS re-dispatches BOTH review channels | ⚠️ loop+snapshot durable (`snapshot.rs:24-57`); round/cap/root-cause live only in agent memory |
| 12 | 7 fix-loop | reviewer | re-review task after each fix (kernel-claimed) | — | `task.dispatched` (kernel-driven, `scheduler.rs:553-555`); `task.completed`/`task.failed` `emit.rs:161-219` | task | every fix is followed by a fresh re-review event before convergence | ⚠️ re-review by convention; not asserted/persisted |
| 13 | 8 gate | kernel gate runner | `gate_json` shell steps (`task_verify_adapter.rs:660-665`) | — | `task.gate_result{passed,failing_step,exit_code,attempt}` `crates/calm-types/src/event.rs:732-749` (kernel-only gate `crates/calm-truth/src/role_gate.rs:250-269`, `KernelDispatcher`) | task | fmt/clippy/test gate emits `gate_result`; `passed==true` before merge | ⚠️ shell gate OK; **`gh pr checks` / red-pending awareness ❌** (no PR) |
| 14 | 8 gate | kernel/worker | **NEW** `gh.pr.checks{pr_number}` (plugin tool → forge OPERATION) | — | **NEW** `forge.pr.checks{wave_id, pr_number, conclusion}` (wave_id in payload per C6) | card (pr) | `gh pr checks` conclusion==success (all CI green) before merge | ❌ no `gh pr checks` primitive |
| 15 | 9 merge (CONVERGE) | worker | **NEW** `gh.pr.merge{squash:true,delete_branch:true}` (forge OPERATION) | **NEW** squash-merge to main; branch deleted; worktree pruned | **NEW** `forge.pr.merged{wave_id, subject:{phase,slice_id,pr_number}, head_sha (merged revision), merge_sha}` (wave_id+subject in payload per C6; **merge head_sha == latest converged revision for S — R4-4**); **NEW** `workspace.released{lease_id}` + plugin `worktree.removed` | card (pr) / card | merge_sha on main; head branch gone; worktree directory removed; **fires ONLY on the CONVERGE branch** | ❌ no git merge / branch delete / worktree prune spine |
| 16 | 9 close (CONVERGE) | spec-agent | **NEW** `gh.issue.close{n}` (only if whole issue done; forge OPERATION) | **NEW** issue closed (Closes/Resolves) | **NEW** `forge.issue.closed{wave_id, n}` (wave_id in payload per C6); `wave.lifecycle_changed{to:done}` `crates/calm-types/src/event.rs:371-380` | wave | issue state==closed; wave lifecycle==done (legal FSM: `crates/calm-types/src/wave_lifecycle.rs:273`, edge `reviewing→done`); both observed; **fires ONLY on the CONVERGE branch** | ❌ "done" is FSM state, not a merged PR + closed issue |
| 17 | 7→fail CONVERGENCE-FAILURE | spec-agent | round == cap AND last verdict non-approving → NO `gh.pr.merge`; **GIVE-UP** → `calm.plan.upsert{lifecycle:failed}` (terminal) OR **ASK-HUMAN** → `calm.ratify.request` (pause) | **NONE** (no merge, no branch delete; worktree freed via lease release) | **NEW** `review.round{wave_id, subject:{phase,slice_id,pr_number}, head_sha (revision), n==cap, converged:false}` (logical key — R4-4); THEN either (GIVE-UP) `wave.lifecycle_changed{reviewing→failed}` (`crates/calm-types/src/wave_lifecycle.rs:274`, **terminal**) OR (ASK-HUMAN) **TWO edges** (cap-hit is detected in `reviewing`; there is **NO `reviewing→blocked` edge** — round-3 C4/SF-B): first `ratify.requested{reason:cap_exhausted}` + `wave.lifecycle_changed{reviewing→working}` (`crates/calm-types/src/wave_lifecycle.rs:272`), THEN `wave.lifecycle_changed{working→blocked}` (`:270`, **PAUSE — `blocked→working` legal at `:278`, run may resume to `done`**); **NEW** `workspace.released{lease_id}` | wave | **CAP ENFORCED (temporal, subject-keyed, FSM-sound — round-2 SF-2 + round-3 C5):** no `forge.pr.merged`/`forge.issue.closed`/`wave.lifecycle_changed{to:done}` **for subject S** may appear **while the latest `review.round` FOR SUBJECT S (logical key `{phase,slice_id,pr_number}` — R4-4) has `converged:false`**; and any `forge.pr.merged` for S MUST carry the `head_sha` that the latest CONVERGED round for S reviewed (merge head == latest converged revision — R4-4). GIVE-UP sub-path: `→failed` present and merge tail absent for the whole run (terminal). ASK-HUMAN sub-path: the **two-edge** `reviewing→working→blocked` path present, and merge tail absent **until** a `ratify.resolved{grant}` appears — after a grant the run legally re-enters CONVERGE (`blocked→working→reviewing`) and the merge tail is permitted (do NOT assert whole-run absence here, else a granted-then-reconverged legal run false-fails) | ❌ cap/round live only in agent memory; no terminal-failure assertion today |

**Backbone summary — CONVERGE branch (deterministic kinds the E2E asserts present; see
"REQUIRED ORDERING INVARIANTS" in §6 for the few that are also ordered):**
`wave.updated` + `card.added`(spec,report) + `overlay.set` (row 1, unordered within the tx) →
`forge.scan.completed` → `plan.updated` + `wave.lifecycle_changed(planning)` (row 5) →
`review.round(design)` + `task.dispatched`(design-review×2) [**design review BEFORE impl
dispatch**] → `task.dispatched`(impl) → `workspace.leased`/`worktree.provisioned` →
`runtime.started` → `forge.pr.opened` → `task.dispatched`(review×2) + `forge.pr.diff.read` →
`task.gate_result(passed)` → `forge.pr.checks(success)` → `forge.pr.merged` →
`forge.issue.closed` → `wave.lifecycle_changed(done)`.

**Backbone summary — CONVERGENCE-FAILURE branch (round-2 SF-2 + round-3 C4/C5 — temporal,
subject-keyed, not whole-run):**
… → `review.round(subject:{phase,slice_id,pr_number}, head_sha=revision, n==cap, converged:false)`
(logical key — R4-4) →
**NO `forge.pr.merged`/`forge.issue.closed` FOR THAT SUBJECT while that round stays the
latest-and-unconverged for the subject; and any merge for S must carry the latest converged
revision's head_sha (R4-4)** → either
**GIVE-UP:** `wave.lifecycle_changed(reviewing→failed)` (terminal — merge tail absent for the whole
run); **or**
**ASK-HUMAN (TWO edges — there is NO `reviewing→blocked` edge, round-3 C4):** `ratify.requested` +
`wave.lifecycle_changed(reviewing→working)` (`wave_lifecycle.rs:272`) + `wave.lifecycle_changed(working→blocked)`
(`:270`) (PAUSE). On `ratify.resolved(grant)` the run RESUMES `blocked→working→reviewing` and may
legally converge to the full CONVERGE merge tail; on deny/timeout it ends at `blocked`/`failed`. The
merge-tail-absence is asserted **relative to the latest unconverged round FOR THE SUBJECT**, never
for the whole run on the ratify sub-path.

---

## §2 Architecture — kernel vs issue-dev workflow plugin

### Kernel = generic worker infrastructure (`calm.*` primitives)
The kernel owns the substrate, all of which is **workflow-agnostic**:
- **Event spine** — `Event` enum + `kind_tag()` (`crates/calm-types/src/event.rs:958-990`), `metadata()`
  (`crates/calm-types/src/event.rs:788-952`), `topics()` (`crates/calm-types/src/event.rs:1035-1151`); persisted via `write_with_event`
  /`write_with_events`/`log_pure_event` (`db/mod.rs:548-637`) into the events table
  (`0004_events.sql:23-32`) with scope columns (`0007_events_scope.sql:29-34`); replayed over
  WS (`ws/events.rs:469-484`, `:214`).
- **Role gate** — `crates/calm-truth/src/role_gate.rs`; `ActorId::Plugin(_)` is **unrestricted at
  the per-card role gate** (module-doc point 5 at `:43-49`; per-gate `Plugin(_) => {}` no-op arms at
  `:140,191`), so plugins are first-class event producers *for non-kernel-only kinds* — but two
  carve-outs apply (round-2 N-3): (a) kernel-only events `task.dispatched`/`task.gate_result` are
  gated to User/Kernel/`KernelDispatcher` and REJECT Plugin (`NotKernelForTaskDispatched`
  `crates/calm-truth/src/role_gate.rs:224-234`; `NotKernelForTaskGateResult` `:254-264`); and
  (b) lifecycle transitions reject Plugin (`ActorId::Plugin → ActorKind::Other`, rejected for all
  edges — see A4/B5). So "unrestricted" means the per-card gate only.
- **Scheduler** — claims plan tasks within budget (`scheduler.rs:118-145,532-548`), default
  budget=1 (`scheduler.rs:71-75`).
- **Gate runner** — runs `tasks.gate_json` shell steps (`task_verify_adapter.rs:660-665`).
- **Durability** — `HarnessSnapshot{phase,push_watermark,pending_queue}`
  (`snapshot.rs:24-57`); parked-ops fence survives restart.
- **wave-vcs** — SQLite content-addressed projection archive (`wave_vcs.rs:1-16`,
  `0039_wave_vcs.sql:1-42`).

### Issue-dev workflow plugin = policy + git/forge tools (external)
The plugin owns everything **workflow-specific**:
- **Plan template** — the slice shape (design → impl → dual-review → fix → gate → merge).
- **Gate set** — which `gate_json` steps run (fmt/clippy/test/OpenAPI/`gh pr checks`).
- **Review protocol** — dual independent channels, round-N, diminishing cap, always-re-review,
  systemic-root-cause (durable in the plugin's own store, group C).
- **Spec-agent instructions** — the workflow prompt/policy fed to codex-as-spec.
- **git/forge toolset** — `git.worktree.add`, `gh.pr.create`, `gh.pr.diff`, `gh.pr.checks`,
  `gh.pr.merge`, `gh.issue.view/close`. **This is where git/forge EXECUTION semantics live — the
  plugin decides which `gh`/`git` argv to run, supplies the recovery probe argv, and supplies the
  BOUNDED result-extraction spec (target typed event kind + exit-code|named-stdout-field extractors
  over `--json` output, round-4 R4-3) in the `forge-action` op payload; the kernel's concrete
  `ForgeActionAdapter` (§2.5-A) RUNS that argv durably with its forge-specific exactly-once contract
  (POST-PARK release so nothing irreversible runs pre-park — R4-1; the atomic parked-completion
  helper for EVERY oracle action — R4-2; the bounded extractor builds the named typed event — R4-3),
  so no git/gh verb-execution logic compiles into the kernel (round-2 SF-1 + round-3 C1/C2/C3 +
  round-4 R4-1/2/3; the kernel is not literally "git-free" — see the moat note in §0 and
  `routes/fs.rs`).**
- **Its own card kinds** — e.g. a `pr` card with backend validation/lifecycle.

### Where git/forge lives (decision — v2 durability, v3 moat-honesty fix per SF-1)
git/forge **semantics** are plugin-owned, but the **side-effects execute as kernel `forge-action`
OPERATIONS** (§2.5-A), NOT as bare plugin tools + events. v1's "plugin tools + bare events"
position is crash-unsafe: the events table has no dedupe key (`0004_events.sql:23-32`), so a
crash mid-`gh pr merge` would double-run or be lost. Routing through the parked-op machinery gives
idempotency `(kind, idempotency_key)` (`0042_operations_parked.sql:96-98`) + `recover_parked`
restart recovery for free, while the kernel holds no git/gh verb-execution logic: the **concrete
`ForgeActionAdapter`** (a compiled-in kernel type with a forge-specific exactly-once contract —
round-3 C1/C2/C3 + round-4 R4-1/2/3) runs whatever argv the plugin put in the op payload, and the
only fixed knowledge in the kernel is "spawn argv held at a stdin go-token, record artifacts + park,
**then release the go-token from the POST-PARK observer** (R4-1 — nothing irreversible runs until
durably parked); on completion run the bounded result-extractor + emit the named typed event via the
parked-completion helper (R4-2/R4-3); on the dead/boot path run the supplied **recovery probe argv**
(probe exits 0 ⇒ landed) and, where the typed event needs OUTPUTS, re-extract them from the probe's
`--json` output" + a workflow-agnostic workspace lease (§2.5-B). It is NOT literally git-free
(round-2 SF-1: the adapter shells `git`/`gh`; `routes/fs.rs:552-559` already shells `git` for
file-browse — read-only there, mutating here, round-3 N-3) — the moat is *no workflow git
verb-execution logic in the kernel crate*; the typed event DATA shapes are shared `calm-types`
definitions (round-4 R4-3, SF-1/C7 resolved). Today the tool channel also runs
the **wrong direction** for plugin→worker tools: `POST /api/plugins/:id/tool-call` only accepts
`neige.*` and routes iframe→kernel (`routes/plugins.rs:907-948`); the `neige.*` dispatch table
(`callbacks.rs:185-203`) has overlay/card/event/kv but **no** plugin-exposes-tools-to-worker
channel, and worker/spec tools come from a static registry (`register_default_tools` at `mcp_server/tools/mod.rs:29`; round-2 N-5/A — `:21` was the module preamble). That
reverse channel — discovery + permissioning + routing — is gap B3 (re-scoped per B8).

### ASCII layering
```
┌──────────────────────────────────────────────────────────────────────────┐
│  ISSUE-DEV WORKFLOW PLUGIN  (external; first of many workflows)            │
│  ┌────────────┐ ┌──────────┐ ┌────────────────┐ ┌──────────────────────┐  │
│  │plan template│ │gate set  │ │review protocol │ │ git/forge TOOLSET    │  │
│  │(slice shape)│ │(gate_json)│ │(dual,round,cap)│ │ git.worktree.add     │  │
│  └────────────┘ └──────────┘ └────────────────┘ │ gh.pr.{create,diff,  │  │
│  ┌────────────┐ ┌──────────────────────────────┐│   checks,merge}      │  │
│  │spec instrs │ │ plugin card kinds (pr card)   ││ gh.issue.{view,close}│  │
│  └────────────┘ └──────────────────────────────┘└──────────────────────┘  │
│        │ registers workflow descriptor (B1) + tools (B3) + card kind (B2)  │
│        │ + supplies forge-action argv/recovery-probe in the op PAYLOAD     │
└────────┼───────────────────────────────────────────────────────────────────┘
         │  ActorId::Plugin(_)  (ids.rs:75; per-card-gate-unrestricted calm-truth
         │  role_gate.rs:43-49,140,191; kernel-only kinds + lifecycle REJECT Plugin)
┌────────▼───────────────────────────────────────────────────────────────────┐
│  KERNEL — generic worker infrastructure (the moat = no WORKFLOW git logic)  │
│  wave/card · spawn·supervise·reap · observe·turn · scheduler · gate runner  │
│  parked ops · snapshot · wave-vcs · event spine · role gate · lifecycle FSM │
│  CONCRETE ForgeActionAdapter (forge-specific exactly-once: POST-PARK release │
│    + atomic parked-completion for ALL oracle actions + bounded typed extract; │
│    payload argv+extractor spec, NO git verb-exec logic; R4-1/2/3) ·          │
│    workspace-LEASE primitive (dir+row, truly no git) · (NB: routes/fs.rs    │
│    shells git read-only for file-browse — not literally git-free, SF-1/N-3) │
│  observation/recovery plumbing (live push + boot replay, §2.5-C)           │
│  calm.* primitive tools (plan.upsert, lifecycle, verdict, report, wave.diff)│
└─────────────────────────────────────────────────────────────────────────────┘
```

---


_**Part 2/5 — §2.5** (Durability & Recovery: forge/worktree as operations; observation plumbing)._

---

## §2.5 Durability & Recovery (forge/worktree as operations; observation plumbing)

> **Why this section exists (round-1 B1/B2/B3).** v1 modeled the git/forge spine as *plugin
> tools + bare events*. That is not crash-safe: the events table has **no dedupe key**
> (`crates/calm-truth/migrations/0004_events.sql:23-32` — columns are `id,kind,payload,actor,at,
> correlation` only), and a tool-call that crashes mid-`gh pr merge` would either re-run
> (double-merge) or be lost. The kernel already has the right primitive for this — the **durable
> parked OPERATION** — and the spec agent already has a recovery path that bare events do **not**
> traverse. v2 re-bases the forge/worktree spine on both.

### 2.5-A — Forge/worktree actions are first-class parked/idempotent OPERATIONS (B1)
**The existing pattern (grounded, not invented).** Every long-running, resumable side-effect in
the kernel is an `Operation` with a phase lifecycle and a `ProviderAdapter`:
- **Phase state machine** (`Phase` enum `crates/calm-server/src/operation/mod.rs:269`; round-2
  N-4 — was cited `:223`): `Pending → TxCommitted → [AppServerInteract] → SpawnStarted →
  SpawnSucceeded | Parked → Succeeded | Compensating → Failed | Stuck`. `Parked` = awaiting async
  completion (observer), not failed.
- **Idempotency** is a **partial unique index** `(kind, idempotency_key)` on the operations
  table (`crates/calm-truth/migrations/0042_operations_parked.sql:96-98`), so a post-crash
  resubmit with the same key idempotency-matches instead of double-running. Keys MUST be **pure
  functions of frozen domain rows** (`scheduler.rs:151-184` builds payloads as pure functions of
  task rows; `stable_payload_hash` is then deterministic) — materializing process-env (HOME, cwd
  from `getcwd()`) at submit breaks post-crash idempotency.
- **`ProviderAdapter` contract** (trait at `crates/calm-server/src/operation/mod.rs:559`; round-2
  N-4 — was cited `:491`): `kind()`, `phases()`, `validate()`, `prepare_tx()` (freeze config +
  acquire leases + mutate rows in the tx), optional `app_server_interact()`,
  `spawn_side_effect() -> SpawnOutcome::Ready|Parked`,
  `recover_parked(&self, _op, _artifacts, alive, _mode, ctx: &SpawnCtx) -> ParkedRecovery
  {LeaveParked|Complete|Fail}` (full sig at `crates/calm-server/src/operation/mod.rs:596-611`;
  it **receives a `SpawnCtx`**, so an adapter CAN shell a `probe_argv` during recovery — round-3
  C3/SF-A), `plan_compensation(from_phase)`,
  `compensate_step()` (signature `(&self, step, output, op, ctx)` at `:621` — **no `Tx` param**,
  load-bearing for §2.5-B).
- **Restart recovery** (`recover_on_boot()` at `crates/calm-server/src/operation/driver.rs:240`,
  called from `crates/calm-server/src/lib.rs:124`; round-2 N-5 — the prior cite `mod.rs:1030-1063`
  was a `#[cfg(test)]` harness fixture, not the boot fn): fetches abandoned ops in any non-terminal
  phase and drives or verifies them; the parked-fence model
  (`crates/calm-server/src/operation/parked_fence_model.rs:1-109`) is exhaustively model-checked to
  guarantee a **single winner** (observer OR sweep) under crash races.
- **Registration & the compiled-in nature of adapters (round-2 SF-1).** Adapters are concrete
  kernel-crate types built by `build_operation_adapters`
  (`crates/calm-server/src/state.rs:350`, returns `Vec<Arc<dyn ProviderAdapter>>` of concrete
  types) and wired one-line each into `fn dispatcher_operation_runtime`
  (`crates/calm-server/src/dispatcher.rs:160`; the adapter vec at `:244-255` holds exactly 10
  adapters today — round-3 N-2 corrected the register cite from `:158`, which is a brace, to the
  fn at `:160`). There is **no plugin-provided-adapter seam** — a plugin process is reachable
  only via the **request/response outbound RPC** `plugin_host/mcp.rs:507` `tools_call` (it
  `await`s a parsed `CallToolResult`, round-3 N-1), which is NOT an operation. **Consequence:**
  the new `ForgeActionAdapter` necessarily lives in calm-server, so it MUST be designed as a
  *concrete adapter modeled on `TaskVerifyAdapter`* (the git/gh taxonomy is supplied by the plugin
  via the payload — see below), NOT as a generic plugin-recovered exec seam (no such seam exists —
  this is the root cause of round-3 C1/C2/C3) and NOT as a place to encode git verbs.

**v5 design — a CONCRETE `ForgeActionAdapter` with a PURPOSE-BUILT exactly-once contract (round-4
R4-1+R4-2+R4-3, ONE root cause; SUPERSEDES v4's "faithful copy of `TaskVerifyAdapter`").** v3 framed
this as a "generic thin exec adapter recovered generically by the kernel"; round-3 corrected that to
"copy `TaskVerifyAdapter`." **Round-4 found the copy is NOT sound** — task-verify is a *resultless,
idempotent gate* (re-running it is harmless and it emits a verdict computed purely from an exit
code), whereas a forge action is *irreversible and carries a typed OUTPUT* (`pr_number`,
`merge_sha`). A naive copy reopens four holes; R4-1/2/3 are three facets of one root cause — the
forge-action operation needs **its own exactly-once completion/recovery/result contract** — so they
are fixed together, NOT patched separately (R4-4 is the orthogonal subject-key fix, §6):
- **R4-1 (pre-park release window NOT closed):** v4 copied task-verify's release-BEFORE-park
  ordering. **Verified at HEAD `b358b8f7`:** in `task_verify_adapter.rs` the go-token release
  (`stdin.write_all(b"go\n")` :929, inside the `record_release` block :922-934) completes and returns
  `SpawnArtifacts` BEFORE the observer closure is built (:961), and `set_parked` commits only AFTER
  (driver.rs:456, then `tokio::spawn(observer)` :457). So the gate is *released before park*. For a
  forge action that is fatal: a crash after go-token release but before `set_parked` commits leaves
  the op in `SpawnStarted`, which boot maps to **generic re-drive** (`plan_recovery_for` →
  `RecoveryItem::Recover` driver.rs:914-918 → `apply_recovery_item`/`drive_one` :947 → the
  `Phase::SpawnStarted` arm :430 re-runs `spawn_side_effect`), **NOT** `recover_parked` — so
  `gh pr merge` runs **twice**. The fix: **release the go-token from the POST-PARK observer.**
- **R4-2 (Ready shortcut can't emit events):** v4 let read-only checks/scan/diff use
  `SpawnOutcome::Ready`, but `Ready(SpawnHandle)` (`operation/mod.rs:242-243`) carries no result and
  the driver just flips `Phase::Succeeded` (`driver.rs:340`) — it CANNOT emit
  `forge.pr.checks`/`forge.scan.completed`/`forge.pr.diff.read` atomically. So **every oracle-visible
  forge action MUST use the parked-completion helper**; `Ready` is reserved for truly
  resultless/non-oracle actions only.
- **R4-3 (no typed result/event wire contract):** `complete_forge_op_with_result` must emit TYPED
  events, but exit-code-only (v4) can't carry action OUTPUTS (`forge.pr.opened{pr_number}`,
  `forge.pr.merged{merge_sha}`); and a fully-generic kernel can't pick the typed variant without
  either baking verbs in (reopens SF-1) or a **bounded typed result spec in the payload**. The fix
  is a bounded result-extraction wire contract (point 1).

**The fix is a PURPOSE-BUILT forge-action contract** that borrows task-verify's held-handshake spawn
*shape* (stdin go-token launcher; artifacts recorded under the 60s `RELEASE_TIMEOUT` fence) but
DEPARTS from it in three load-bearing ways: **POST-PARK release** (R4-1), **parked-completion for ALL
oracle actions** (R4-2), and a **bounded typed result-extraction** instead of exit-code-only (R4-3).
The `ForgeActionAdapter` is a **compiled-in kernel type** (no plugin-adapter seam exists), declaring
`phases() = [Pending, TxCommitted, SpawnStarted, Parked, Succeeded]`. The git/gh EXECUTION semantics
— which argv, which verbs, which probe argv, which `--json` field paths — are still **plugin-supplied
via the op payload** (the SF-1 moat holds: the adapter runs argv + applies a bounded extractor; the
verbs/argv/field-paths come from the plugin as DATA). The typed event DATA shapes live in shared
`calm-types` (SF-1/C7 resolved — definitions, no logic). New file
`crates/calm-server/src/operation/forge_action_adapter.rs` (modeled on `task_verify_adapter.rs`'s
spawn/park/recover skeleton, but with the forge-specific contract — NOT a line-for-line copy).

1. **Kind + payload (argv/probe/extractor are payload data, not adapter code).**
   `pub const FORGE_ACTION_KIND: &str = "forge-action";`
   `struct ForgeActionPayload { wave_id, card_id, cwd_lease, argv: Vec<String>, idem_key: String,
   event_spec: ForgeEventSpec, probe: Option<ProbeSpec>, result_path: PathBuf,
   await_mode: Ready | Parked{deadline} }`.
   - **`ForgeEventSpec` is the BOUNDED typed result-extraction contract (R4-3).** It names the
     **target typed event kind** (a shared-`calm-types` enum tag, e.g. `forge.pr.merged`) plus a
     **bounded field-extractor map**: each event field is filled from EITHER the action's exit code OR
     a **named stdout field path** over the action's `--json` output (e.g. `merge_sha ← .oid`,
     `pr_number ← .number`). The grammar is **strictly bounded = `{exit_code}` | a list of
     `{event_field ← json_field_path}` over the verb's `--json` document** — NOTHING ELSE. This
     **replaces v4's exit-code-only** (too weak: it cannot carry `pr_number`/`merge_sha`) **WITHOUT
     resurrecting v3's unbounded JSON-predicate DSL** (no boolean predicates, no expressions, no
     array logic — just named field reads). The kernel adapter applies the extractor, fills the named
     typed variant's fields, and emits it via the parked-completion helper (point 6). **The exact
     bounded extractor grammar is slice ⑥'s deliverable, reviewed at impl** (a design doc converges
     on the contract; the grammar's precise shape — field-path syntax, missing-field handling, type
     coercion — is implementation+review scope).
   - **`ProbeSpec` is `{ probe_argv: Vec<String> }`** — its **exit code** is the did-it-land signal
     (probe exits 0 ⇒ landed); where the typed event needs OUTPUTS after a crash, the probe's
     `--json` output is re-run through the SAME bounded extractor. **There is NO predicate-over-JSON
     boolean DSL — the v3 `RecoverSpec.predicate` stays DELETED (round-3 C3/SF-A).** The plugin
     encodes the merge-state semantics inside the probe argv (e.g.
     `gh pr view <n> --json state -q '.state=="MERGED"'`, which exits 0 iff merged).
   The verb taxonomy (`ScanOpenPrs/PrCreate/PrDiff/PrChecks/PrMerge/IssueClose`) lives **in the
   plugin**, which lowers each verb to `argv` + `event_spec` + `probe_argv` + `idem_key` before
   submitting — it is NOT an enum baked into the kernel adapter. (Honest caveat: the adapter still
   *shells* `git`/`gh`, so the kernel is not literally "git-free" — but it carries no git/gh
   verb-execution logic; see §0 moat note + §5-Q2.)
2. **Idempotency key** = the plugin-supplied `idem_key`, a pure function of frozen domain
   rows, e.g. `(repo_id, verb, pr_number_or_issue_or_head_sha)`. `PrMerge(pr=42, head_sha=abc)`
   collapses to one operation no matter how many times a crashing plugin resubmits — the
   **double-merge-on-RESUBMIT hazard is structurally removed** by the `(kind, idempotency_key)`
   index. (Resubmit is NOT the C1 hazard, though; C1 is a pre-park CRASH re-run, fixed by the
   held-handshake below.)
3. **`prepare_tx`** freezes the payload (argv + `event_spec` + probe_argv + key + result_path) into
   `tx_output` (idempotency-keyed, like `FrozenVerify`), mirroring `TaskVerifyAdapter::prepare_tx`.
   For the merge class the plugin marks the action guard-on-conflict; the kernel applies this
   generically (it does not know "merge" specifically).
4. **`spawn_side_effect` — HELD-HANDSHAKE spawn with POST-PARK release (R4-1 fix; a FRAMEWORK
   addition, not a copy).** The forge argv is wrapped in a launcher **held at a stdin go-token**
   (`(read -r _go || exit 75); exec <action-cmd>` — borrowing `task_verify_adapter.rs:328`
   `read -r _go || exit 75`). The adapter (a) spawns the wrapped command held, (b) records spawn
   artifacts `(pid, pgid, starttime, boot_id, result_path)` under the 60s `RELEASE_TIMEOUT` fence
   (`task_verify_adapter.rs:75`; `ctx.record_spawn_artifacts` :921), and (c) returns
   `SpawnOutcome::Parked{deadline, observer}` **WITHOUT releasing the go-token** — **the observer
   (which runs only AFTER `set_parked` commits, `driver.rs:456-457`) releases the go-token, THEN waits
   the action and completes via the atomic helper (point 6).**

   **CONTRACT (R4-1):** *nothing irreversible runs until the op is durably parked.* A pre-park crash
   (any time before `set_parked` commits) ⇒ the launcher's stdin is never written, the action NEVER
   ran ⇒ the `SpawnStarted` re-drive (`driver.rs:430` re-runs `spawn_side_effect`) is **safe** (it
   re-spawns a held launcher whose prior instance also never ran). A post-park crash ⇒ the op is
   `Parked`, so boot takes `recover_parked` (probe, never re-run — point 5). The pre-park re-drive
   window that R4-1 identified is thereby closed at the contract level.

   **HONEST FEASIBILITY NOTE — this is BIGGER than a copy; it is a small FRAMEWORK addition that
   slice ⑥ lands (grounded at HEAD `b358b8f7`):** today the observer **cannot** own the release.
   `task_verify_adapter`'s go-token release happens INSIDE the `record_release` block (the
   `stdin.write_all(b"go\n")` at :929) which completes and drops the child's stdin BEFORE the observer
   closure is constructed (:961); the observer captures the already-released `child` by move (:961-962)
   and has no stdin handle. The `ParkedObserver` type (`operation/mod.rs:244`,
   `Pin<Box<dyn Future<Output=()>>>`) takes **no input parameters**, so there is no current way to
   hand the child's stdin to the observer. Deferring release post-park therefore requires a small,
   workflow-agnostic framework change, which **slice ⑥ lands**, ONE of:
   - **(a)** `spawn_side_effect` constructs the observer closure with the child's stdin MOVED IN (not
     dropped at spawn time), and the observer writes `go\n` as its first step after park; OR
   - **(b)** a new `SpawnOutcome::ParkedDeferredRelease { deadline, release_handle, observer }`
     variant whose `release_handle` the driver writes to **after** `set_parked` commits (driver.rs:457
     region), before spawning the observer.
   Either way the change is **generic** (no git knowledge): "release a held side-effect's go-token
   only after the op is durably parked." This also tightens the **#653 §3.2 record-before-release
   contract** ("spawn_side_effect must record every gate process that can execute a step before
   returning"): the artifacts ARE recorded in `spawn_side_effect` (step b, unchanged), so the durable
   PID/starttime/boot_id triple exists before the action can run; only the *release* moves to the
   post-park owner. **Slice ⑥ acceptance must include the R4-1 pre-park-crash test** (a kill after the
   held launcher spawns but before the observer's release leaves the action UN-RUN). The precise
   stdin-handoff mechanism (variant (a) vs (b)) is slice-⑥ implementation+review scope.

   **R4-2:** only truly resultless/non-oracle actions may take the `SpawnOutcome::Ready` path; **every
   oracle-visible action (incl. read-only checks/scan/diff, which emit `forge.pr.checks`/
   `forge.scan.completed`/`forge.pr.diff.read`) takes the Parked + parked-completion path** so its
   typed event is emitted atomically (point 6). `set_parked` requires artifacts recorded
   (`WHERE spawn_artifacts_json IS NOT NULL`, `operation/mod.rs:700`, fn opens :682; call site
   `driver.rs:456`) and the observer is spawned only AFTER park commits (`driver.rs:457`).
5. **`recover_parked` — PROBE recovery + bounded re-extraction (R4-3, C3 fix).** Liveness first
   (`verify_owned_pid`); for dead work, run the plugin-supplied `probe.probe_argv` and apply
   **`verdict_from_exit_code`** (`task_verify_adapter.rs:408`) — probe exits 0 ⇒ landed, non-zero ⇒
   `Fail`/`LeaveParked` per `await_mode`. **No boolean JSON-predicate DSL.** Where the typed event
   needs OUTPUTS (e.g. `merge_sha`), the probe is run with `--json` and its output is passed through
   the SAME bounded `ForgeEventSpec` extractor (point 1) so the recovered completion emits the same
   typed event the live path would have. `recover_parked` receives a `SpawnCtx`
   (`operation/mod.rs:596-611`), so shelling the probe during recovery is contract-legal. Boot mode
   spawns a reattach observer that polls until dead then re-reads the probe (borrowing
   `task_verify_adapter.rs:1077-1099`); `PastDeadline` returns `Fail{reason: action-timeout}`. This
   is what makes "did my merge happen, and with what `merge_sha`?" answerable after a crash via a
   side-effect-free probe.
6. **`complete_forge_op_with_result` — CUSTOM atomic completion helper for ALL oracle actions (R4-2 +
   R4-3, C2 fix).** Modeled on `complete_gate_op_with_result` (`task_verify_adapter.rs:263`): ONE tx
   combining (a) `complete_parked_tx` (op `Parked→Succeeded/Failed`, `task_verify_adapter.rs:275`),
   (b) the **named typed forge event built by the bounded `ForgeEventSpec` extractor** (`Event::
   ForgePrMerged{merge_sha, …}` etc. — the variant tag and field-fill both come from the payload's
   `event_spec`, R4-3) emitted directly in the completion tx (mirroring `apply_gate_result_in_tx` +
   `Event::TaskGateResult`, `:176`/`:214-224`), and (c) plugin-consumer state write — all commit
   together or roll back on `AlreadyResolved`. The forge event therefore **cannot exist without the
   side-effect having committed** (and vice-versa). **R4-2:** because EVERY oracle-visible action (not
   just merge/create — also checks/scan/diff) routes through this helper rather than the
   `SpawnOutcome::Ready` shortcut, each emits its typed event atomically; the **generic
   `complete_parked_tx`/`SpawnOutcome::Ready` path CANNOT do this** — the adapter MUST ship this
   helper. The construction of the typed variant from the bounded extractor is the load-bearing R4-3
   mechanism (and is anchored as feasible: task-verify already builds `Event::TaskGateResult` from a
   deterministic verdict struct in the completion tx — the forge helper does the same from the
   `event_spec`-driven extraction).

**Residual risk carried from the re-anchor (acknowledged, not hand-waved):** (a) `gh pr merge` is
not idempotent once released — recovery MUST be a side-effect-free probe ("is PR merged?"), never a
re-run, and MUST `Fail` gate-infra if the probe answer is unknown; (b) the action wrapper MUST write
`result_path` via tmp+rename (atomicity, like `neige_gate_finish`) so a mid-write SIGKILL leaves no
partial file; (c) the wrapper MUST `exec` the action so SIGKILL reaches it directly (no swallowing
signal handler); (d) split-brain observer-vs-reattach is serialized by `complete_parked_tx`'s
`phase=='parked'` guard + lease fence (the one ordering guarantee) — observers idempotently re-probe
the same idem key and the first committer wins; (e) **(round-4 R4-1)** because the go-token release
now happens from the post-park observer, the observer is the release-AND-wait owner — if the observer
dies after releasing but before completing, the action is in the post-release window and recovery is
probe-only (d/a), exactly as for any post-release crash; a pre-release observer death leaves the
held launcher un-released → it EOFs and exits 75 → action un-run.

**`ProviderAdapter` procedure for the ForgeActionAdapter** (folded from the durable-ops re-anchor +
the gate-pattern skeleton + round-4 contract): define kind const + payload struct (argv +
`event_spec` + `probe_argv` + key + `result_path`) → derive idempotency key from frozen domain rows →
`prepare_tx` freezes the payload → `spawn_side_effect` spawns the held launcher, records artifacts,
parks with an observer **WITHOUT releasing the go-token (R4-1)** → driver/observer **releases the
go-token only AFTER `set_parked` commits (R4-1 framework addition — variant (a)/(b) above)** → the
observer waits the action → `recover_parked` does liveness + probe + bounded re-extraction (no boolean
DSL — R4-3) → `complete_forge_op_with_result` commits op flip + the bounded-extractor-built named
typed forge event atomically for EVERY oracle action (R4-2/R4-3) → register one line in
`dispatcher_operation_runtime` (`dispatcher.rs:160`, N-2) → boot-recovery test injecting each phase
(incl. the **R4-1 pre-park crash**: assert a kill before the post-park go-token release leaves the
action un-run; and an oracle-action completion test asserting the typed event carries the extracted
OUTPUT fields).

### 2.5-B — Worktree LEASE + filesystem compensation + orphan reclaim (B2)
**The gap (grounded).** v1's slice ① ran `git worktree add` inside the **kernel Codex adapter**,
and (a) compensation only reverts **card/terminal/process rows**, NOT worktrees/branches:
`CodexAdapter::plan_compensation` returns a single `cleanup_codex_worker` step
(`crates/calm-server/src/operation/codex_adapter.rs:916-1016`), and `compensate_worker_rows`
(`crates/calm-server/src/operation/worker_cleanup.rs:13-90`) only flips card/terminal to
rolled-back; (b) the scheduler sweep **leaves running Codex workers alone** on restart
(`scheduler.rs:918` `TaskStatus::Running => {}`, comment at `:915-917` — "a running codex worker
survives restarts… the scheduler holds no liveness judgment"; round-2 N-4). So a kernel restart
while a worker holds a worktree
**orphans the worktree directory and its branch**.

**Architectural decision (resolving the kernel-vs-plugin tension).** The doc's principle is
"git/forge = plugin tools, keep kernel generic." But provisioning an isolated workspace at
*claim-time* is kernel-adjacent (the scheduler claims tasks; the worker boots into a cwd). v2
resolves this explicitly:

> **The kernel provides a generic, workflow-AGNOSTIC `isolated-workspace-lease` primitive. The
> git-ness stays in the plugin.**

The kernel primitive knows only: "allocate a leased, disjoint filesystem path for this card;
hold the lease; release/reclaim it on exit, compensation, or restart." It does **not** know the
path is a git worktree. The issue-dev plugin layers `git worktree add`/`remove` *on top of* the
leased path via a `forge-action` op (2.5-A). **Justification:** (1) the resource-disjointness
guarantee budget>1 needs (B7) is a generic scheduling concern, not a git concern — a future
"research" workflow leasing scratch dirs needs the same primitive; (2) the lease itself carries
**no git** (it is just a directory + a row) — this is the genuinely-clean half of the kernel/plugin
split (round-2 SF-1: unlike the `forge-action` adapter, which does shell `git`/`gh`, the lease
primitive has no git dependency at all); (3) orphan-reclaim on restart belongs to whoever owns
liveness judgment, which is the kernel, not the plugin.

**Design — the lease.**
- **Lease row** `workspace_leases { lease_id, card_id, wave_id, path, state ∈
  {held, releasing, released}, lease_owner, lease_until_ms, boot_id }` (new migration). The
  lease is acquired in `prepare_tx` of the worker operation (atomic with the claim), mirroring
  how `TaskVerifyAdapter::prepare_tx` bumps `gate_attempt` and freezes the gate spec in-tx.
- **Path** = `.claude/worktrees/<wave>/<card>` — disjoint by construction, so two leases never
  collide. `cwd` flows into the Codex payload (slice ①) but stays `None`-preserving:
  materializing `default_cwd()` would make `stable_payload_hash` depend on HOME and break
  idempotency (same rationale as the Terminal comment at `scheduler.rs`); the **lease path**, not
  HOME, is the frozen domain value.
- **Filesystem compensation.** Extend the compensation chain with a `release_workspace_lease`
  step (and, plugin-side, a `git worktree remove --force` + `git branch -D` step). **Caveat
  (verified round-2):** `compensate_step` has **no tx parameter today** (signature
  `(&self, step, output, op, ctx)` at `operation/mod.rs:621`), so it cannot bundle the lease-row
  flip with the filesystem removal in one tx. Two options, decided here: **(i)** add an optional
  `tx: Option<&mut Tx>` to `compensate_step` (touches all adapters) so the lease row flip + fs
  removal commit together; **(ii)** make the fs removal idempotent ("remove if exists") and flip the
  lease row in a follow-up `complete`-style tx. v2 picks **(i)** for worktrees specifically because
  a half-removed worktree with a still-`held` lease would block budget>1 re-claims — but the
  tx-param extension is **DEFERRED to slice ③**, where `git worktree remove` needs the fs+lease
  bundle. Slice ⑥'s forge compensation releases the workspace lease pool-based via
  `release_workspace_lease_by_id`, exactly as slice ①'s Codex compensation already does, so no
  cross-adapter signature change was needed in ⑥.
- **Orphan reclaim on restart.** `recover_on_boot()` (`operation/driver.rs:240`, N-5) already
  reattaches abandoned ops; the lease recovery hook (using `recover_parked(.., Boot)` semantics,
  which **ignores lease TTL** to reattach crashed-process leases — `claim_parked_for_boot` at
  `crates/calm-server/src/operation/mod.rs:743`; round-2 N-3/N-4 — was cited `:650`) re-checks each
  `held` lease: if the owning card/process is dead (no live pid by boot_id), it drives the lease to
  `releasing` and runs the fs-compensation step. This is the missing counterpart to scheduler.rs:918
  — the *operation/lease* layer reclaims the worktree even though the *scheduler* deliberately
  doesn't judge worker liveness.
- **Budget>1 guard (B7).** `compute_ready` (`scheduler.rs:118-145`) today is **budget arithmetic
  only** — it counts in-flight vs `parallelism_budget` with **no cwd/resource-disjointness check**
  (cwd is nullable, `crates/calm-truth/migrations/0041_tasks.sql:10`). Budget>1 is therefore only
  permitted once claim-time lease acquisition is in place: a task is "ready" only if a disjoint
  workspace lease can be acquired for it. The lease acquisition IS the resource guard.

### 2.5-C — Observation/recovery plumbing every new spec-facing event must traverse (B3)
**The gap (grounded).** A new `forge.*`/`review.*`/`ratify.*` event would be **stored but never
reach or recover the spec agent**. The spec's observation pipeline handles only a **fixed**
task/report/hook set at every stage:
- **Live push predicate** `event_warrants_spec_push_with_role` (fn at `dispatcher.rs:70`; round-2
  N-2 — `:62` is the thin `event_warrants_spec_push` wrapper) matches
  `TaskCompleted/Failed/GateResult/WaveReportEdited/CodexHook/ClaudeHook` and **defaults to
  `false`** (`_ => false` at `:93`) for everything else.
- **Boot recovery query** `replay_harness_events_since` (fn at `crates/calm-server/src/harness/
  mod.rs:89`) queries `events_for_wave` with a **hardcoded kinds array**
  (`task.completed, task.failed, task.gate_result, wave.report_edited, codex.hook, claude.hook`,
  lines 100-108). A new kind not in this array is **silently dropped on boot**.
- **Observation mapping** `harness_observation_from_event` (`dispatcher.rs:1108`) and the
  `Observation` enum (`crates/calm-types/src/observation.rs:18`) + `is_hard_fire()` (`:77`) +
  `to_turn_text()` (`:95`) are likewise closed sets — `to_turn_text` is an **exhaustive `match self`
  with no `_` arm**, so a missing variant fails to compile (a safety feature, not a panic — S-2).

**v2 design — the 6-stage traversal each new spec-facing event MUST complete** (folded verbatim
from the recovery-observation re-anchor; missing ANY stage silently loses the observation, on
the live path or the boot path or both):
1. **Event variant** in `Event` enum + `kind_tag()` + `metadata()` + **an explicit `topics()` arm
   (round-3 C6).** `topics()` has signature `topics(ev: &Event)` (`crates/calm-types/src/event.rs:1035`)
   — it does **NOT** receive an `EventScope`, and WS replay filters by `topics(&ev)` BEFORE rendering
   (`crates/calm-server/src/ws/events.rs:333`); so the new event MUST carry `wave_id` (+ subject ids)
   in its **PAYLOAD** and the `topics()` arm MUST emit `wave:<id>` from that payload, else it routes
   only via `"*"` and is invisible to per-wave `SubscribeFilter` + replay — see §3 event-add procedure.
2. **Dispatcher subscription filter** — add the kind to the kinds vec (`let kinds: Vec<String>` at
   `dispatcher.rs:637`) **and** the filter test (`dispatcher_filter_matches_push_kinds`
   `dispatcher.rs:1257`, `SubscribeFilter` at `:1258`; round-2 N-6); else the event never reaches
   `handle_envelope`.
3. **Live push predicate** — add a match arm to `event_warrants_spec_push_with_role`
   (`dispatcher.rs:70`, `_ => false` default at `:93`) returning `true` for events that must wake
   the spec.
4. **Boot recovery query** — append the kind to the hardcoded array in
   `replay_harness_events_since` (`crates/calm-server/src/harness/mod.rs:100-108`); else
   crash-window events are lost on restart.
5. **Observation mapping + enum** — add `Observation::ForgeXxx{…}` (`observation.rs:18`), classify
   in `is_hard_fire()` (`:77` — forge/review/ratify verdicts are hard-fire, they must wake the spec
   immediately), implement `to_turn_text()` (`observation.rs:95` — **a missing arm fails to COMPILE,
   not a runtime panic**: the `match self` is exhaustive over the enum with no `_` arm, so the type
   system FORCES the new branch; do NOT write a runtime panic-path test, round-2 S-2/N-1), and add
   the `harness_observation_from_event` arm (`dispatcher.rs:1108`).
6. **Live dispatch arm** in `Inner::handle_envelope` (`dispatcher.rs:815`) routing the kind to
   `observe_harness`; the delivery itself flows through `observe_harness_under_lock`
   (`dispatcher.rs:959`, per-wave lock + push-cursor dedup) live, and through
   `replay_harness_events_since` → `snapshot.pending_queue`/`pending_envelope_ids`
   (`crates/calm-server/src/harness/snapshot.rs:25-58`, a Tier-A persisted contract) on boot.

**Scope requirement.** Every new event MUST carry a wave (or narrower) scope — `observe_harness_
under_lock` returns early without a wave scope (it can't resolve the spec card). This is why the
§1 scope column pins each NEW event to wave/card scope, never `System`.

This plumbing is non-trivial and is its **own slice** (§4 slice ⑦), or scoped per-event into ③/⑤.

---


_**Part 3/5 — §3–§4** (per-gap design; slice plan ①→⑦)._

---

## §3 Per-gap design

### A4 — Per-worker git worktree isolation (cheapest first brick)
**Current state.** `build_worker_payload` for `TaskKind::Codex` builds
`CodexWorkerOperationPayload` with **no `cwd`** (`scheduler.rs:153-162`), while `Terminal`
includes `cwd: task.cwd.clone()` (`scheduler.rs:164-181`, line 179). The `cwd` field already
flows in via `plan.rs` into the tasks table; the scheduler silently drops it for Codex.
`DEFAULT_WAVE_TASK_BUDGET=1` is deliberate because "workers and gates share one directory
tree today (no worktrees, risk R2)" (`scheduler.rs:71-75`).
**Proposed change shape.** The kernel provides a **workflow-agnostic isolated-workspace-lease
primitive** (§2.5-B); the git semantics stay in the plugin. The lease *path* is the frozen
domain value; `git worktree add` on that path is a plugin `forge-action` op (§2.5-A), not bare
adapter code.
1. Add `cwd` to `CodexWorkerOperationPayload`; pass `cwd: task.cwd.clone()` in
   `build_worker_payload` (`scheduler.rs:153-162`). **Preserve the Terminal determinism
   rule**: `None` stays `None` (materializing `default_cwd()` would make `stable_payload_hash`
   depend on `HOME` and break idempotency on restart — same rationale as the Terminal comment
   in `scheduler.rs`).
2. In `CodexWorkerAdapter::prepare_tx`, **acquire a workspace lease** (row + path
   `.claude/worktrees/<wave>/<card>`) atomically in the tx (mirrors `TaskVerifyAdapter::
   prepare_tx` freezing the gate spec in-tx). The plugin layers `git worktree add` on the leased
   path; the kernel does not know it is git.
3. Cleanup on exit/compensation: a `release_workspace_lease` compensation step (kernel) +
   plugin-side `git worktree remove`/`branch -D`. **Caveat:** `compensate_step` has no tx param
   today (`worker_cleanup.rs:13-90` / `codex_adapter.rs:916-1016` only flip card/terminal rows) —
   add the optional `tx` param (§2.5-B option (i)) so the lease flip + fs removal bundle.
4. **Budget>1 guard (B7).** `compute_ready` (`scheduler.rs:118-145`) is budget-arithmetic only
   with no disjointness check. Gate budget>1 on **claim-time lease acquisition**: a task is
   "ready" only if a disjoint workspace lease can be acquired. The lease IS the resource guard;
   budget>1 must NOT be permitted before this lands.
5. **Orphan reclaim on restart:** `recover_on_boot` (`operation/driver.rs:240`, N-5) reclaims
   `held` leases whose owning process is dead (boot mode ignores lease TTL — `claim_parked_for_boot`
   `mod.rs:743`) — the counterpart to scheduler.rs:918 leaving live workers alone.
**New events/tools.** `workspace.leased{path,lease_id}` (kernel, workflow-agnostic),
`workspace.released{lease_id}`; plugin-layer `worktree.provisioned{path}`, `worktree.removed`.
All wave/card-scoped (B4).
**Oracle rows flipped.** Row 7 ❌→✅ — **owned by slice ① ALONE** (the lease primitive + cwd
plumbing + budget guard fully satisfy row 7's invariant: "a claimed Codex task runs in a DISTINCT
leased cwd under `.claude/worktrees/`, cwd∈payload, lease held by the op" — no git is part of that
assertion). The git-on-lease layering (③/⑥) is **substrate that ① unblocks, NOT a co-flipper of row
7** (round-2 S-5 — see ①'s "Foundation" note; the flip-owner table gives row 7 to ① only).

### A5 — Real git artifacts (branch + commits)
**Current state.** No real `.git` per worker. wave-vcs is SQLite content-addressed
projection objects (`wave_vcs.rs:1-16`, `0039_wave_vcs.sql:1-42`); a single linear HEAD per
wave (`wave_vcs_refs.head_hash`); rendered entries are projection docs (index.md, report.md,
cards/*/conversation.md), NOT code (`wave_vcs.rs:364-439,3029-3041`).
**Proposed change shape.** Worker, inside its leased worktree (A4), creates a branch
(`git checkout -b <slice>`) and commits real code. This requires a real source-code git repo
(the wave's `cwd` checkout), **separate** from wave-vcs (which stays the projection archive,
§5). git operations are **plugin-requested `forge-action` operations** (§2.5-A), not bare
plugin tools — so a crash mid-commit/push is crash-safe (held-handshake spawn with POST-PARK go-token
release: a pre-park crash leaves the action un-run — round-4 R4-1) and recoverable (recovery
`probe_argv` + bounded re-extraction, not a boolean JSON DSL — round-4 R4-3) and **no git/gh
verb-execution logic enters the kernel** (the plugin supplies the `argv`/`event_spec`/`probe_argv` in
the op payload; the concrete `ForgeActionAdapter` with its forge-specific exactly-once contract runs
it — round-2 SF-1 + round-3 C1/C2/C3 + round-4 R4-1/2/3, the kernel is not literally git-free). The
`forge.*` events these ops emit are committed in
the same tx as the op's terminal phase (durable), and traverse the observation plumbing (§2.5-C)
so the spec actually sees the branch/PR facts.
**New events/tools.** `git.branch.create`, `git.commit` plugin actions dispatched as
`forge-action` ops; **NEW** events `forge.branch.created{ref}` (optional — branch creation may
be implicit in `forge.pr.opened`). Wave/card-scoped (B4).
**Oracle rows flipped.** Row 8 ⚠️→✅.

### A6 — Per-PR code diff with branch + merge-base
**Current state.** `calm.wave.diff` (`TOOL_WAVE_DIFF`) is a hidden drill-in
(`visible_to_roles: &[]`, `wave_history.rs:21-58`) that calls
`wave_vcs::diff_with_patches(from,to,path)` (`wave_vcs.rs:688-703`) — diffs two **projection
TreeManifest snapshots**, commit-to-commit, **no merge-base, no branch semantics**. The
patches are over report.md/conversation.md/card JSON (`wave_vcs.rs:1365-1405`), not code.
**Proposed change shape.** A new `gh.pr.diff{pr_number}` plugin action (dispatched as a
`forge-action` op) returns the **real code diff** of one PR against `merge-base(main, head)`.
**It is read-only but oracle-visible (it emits `forge.pr.diff.read`), so it takes the PARKED +
parked-completion path, NOT the `SpawnOutcome::Ready` shortcut (round-4 R4-2)** — `Ready` carries no
result and cannot emit the event atomically. This runs on the separate git backend (A5), not
wave-vcs.
Reviewers read exactly one PR's diff. wave-vcs's `calm.wave.diff` stays for projection-document
drill-in. (Crate qualification, B11: `calm.wave.diff`'s impl is `diff_with_patches` in
`crates/calm-truth/src/wave_vcs.rs:688-703`; `crates/calm-server/src/wave_vcs.rs` is only a
`pub use calm_truth::wave_vcs::*;` re-export.)
**New events/tools.** `gh.pr.diff` action; **NEW** event `forge.pr.diff.read{pr_number,base_sha}`
(card(pr)-scoped, B4).
**Oracle rows flipped.** Row 10 ❌→✅ (diff-*source* half; the dual-channel half is owned by C/⑤).

### A8/A9 — GitHub/CI primitives
**Current state.** `mcp_server/` has zero `gh`/GitHub command paths. The gate runner only
runs local `gate_json` shell steps (`task_verify_adapter.rs:660-665`); no `gh pr checks`
awareness. "done" is an FSM state (`crates/calm-types/src/wave_lifecycle.rs:30-44`,
edge `reviewing→done`; enforced by `validate_transition` `:170-295`), not a merged PR.
**Proposed change shape.** A git/forge plugin exposes:
`gh.issue.view`, `gh.pr.list`, `gh.pr.create`, `gh.pr.checks`, `gh.pr.merge` (squash + delete
branch), `gh.issue.close`. **Each is dispatched as a `forge-action` OPERATION (§2.5-A), NOT a
bare tool+event** — so the `gh` side-effect is crash-safe (held-handshake spawn with **POST-PARK
go-token release** closes the pre-park re-drive window — round-4 R4-1) and idempotent on resubmit (the
`(kind, idempotency_key)` index removes the double-merge-on-resubmit hazard) and crash-recoverable
(`recover_parked` runs the recovery `probe_argv` + bounded re-extraction — round-4 R4-3, no boolean
JSON DSL). **Every oracle-visible action (incl. read-only checks/scan/diff) takes the
parked-completion path, NOT `SpawnOutcome::Ready` (round-4 R4-2)**, so its typed event is emitted
atomically in the op-flip tx.
Each wraps the `gh` CLI under the plugin's sandbox/host access. Merge is gated on `gh pr checks`
conclusion==success AND local gate `passed==true`. Re-grep callsites before moving/removing
exported symbols is a plugin gate step.
**New events/tools.** Actions above (as `forge-action` ops); **NEW** events — each a **DISTINCT,
TYPED enum variant** (round-3 C7; §5-Q5 is now a DECISION, NOT an envelope):
`forge.scan.completed`, `forge.pr.opened`, `forge.pr.checks`, `forge.pr.merged`,
`forge.issue.closed` — emitted **in the same tx as each op's terminal phase** via
`complete_forge_op_with_result` (§2.5-A point 6, durable). **Why distinct variants, not a single
`forge.event{kind}` envelope (round-3 C7 — decision recorded in §5-Q5):** the event spine is typed
enum arms + exact `events.kind` filter + a TS union + `metadata()` + `topics()`; an envelope hides
merge/check/open facts behind payload parsing and weakens replay/query/oracle (the oracle keys on
exact kinds). The version-bump cost is **per-release, not per-event** (round-2 N-2), so the envelope
buys nothing the spine needs. **SF-1/C7 tension resolved (round-4 R4-3):** these typed variants are
event DATA shapes — `ForgePrOpened{pr_number,head_sha}`, `ForgePrMerged{merge_sha,…}` etc. — that
live in shared `calm-types` as the issue-dev workflow's contribution to the shared event enum
(definitions, **no git/gh logic**). The kernel adapter fills the named variant's OUTPUT fields via the
payload's bounded `ForgeEventSpec` extractor (§2.5-A point 1: exit-code | named `--json` field paths,
NOT a predicate DSL), so no verb-execution logic compiles into the kernel while the typed event can
still carry action outputs. Each NEW variant needs the full gap-shape: variant + `kind_tag` arm +
`metadata` arm + **`topics` arm (round-3 C6 — see below)** + emission site + metadata-coverage test
(B10: also add the variant to the `metadata_coverage_events()` fixture list at
`crates/calm-types/src/event.rs:1988`, which only iterates fixtures — e.g. it omits `TaskGateResult`
today) + **generated TS bindings** (B10: `ts-rs` `#[ts(export…)]` on `Event` at
`crates/calm-types/src/event.rs:342` writes `web/src/api/generated-events.ts` via
`cargo test export_bindings_`) + a `SYNC_EVENT_VERSION` bump **batched once per shipping release, NOT
once per event** (round-2 N-2: v3 added TWO kinds in ONE bump per the version history at
`crates/calm-types/src/event.rs:303-326`; current value 4 at `:327`).
**WAVE_ID IN THE PAYLOAD + AN EXPLICIT `topics()` ARM ARE MANDATORY (round-3 C6).** `topics()` has
signature `topics(ev: &Event)` (`crates/calm-types/src/event.rs:1035`) — it does **NOT** receive an
`EventScope`, and the WS replay path filters by `topics(&ev)` BEFORE rendering
(`crates/calm-server/src/ws/events.rs:333`). A `forge.pr.merged{…}` whose payload lacks `wave_id`
therefore routes only via the `"*"` fallback (it cannot derive the wave from scope inside `topics()`),
so it is invisible to a per-wave `SubscribeFilter` and to replay. **Every NEW forge/review/ratify
event MUST carry `wave_id` (and the subject ids — `slice_id`/`pr_number`/`head_sha` for the
subject-key, round-3 C5) in its PAYLOAD, and MUST add an explicit `topics()` arm** that emits the
`wave:<id>` topic from the payload. This is the per-event obligation; the §2.5-C plumbing rides on it.
**No `from_kind_and_payload` arm is needed (round-2 SF-3):** `from_kind_and_payload`
(`crates/calm-types/src/event.rs:1016`) is **generic serde** (`serde_json::from_value` over a
synthesized `{ev,data}` envelope), NOT a per-variant `match` — a new variant with the serde tag is
handled automatically. The per-variant `match self` blocks that DO need an arm are exactly
`kind_tag`/`metadata`/`topics`. These events are **wave/card-scoped** (B4), and each MUST traverse
the §2.5-C observation plumbing (else the spec never sees the forge facts). The events table has **no dedupe key** (`0004_events.sql:23-32`);
dedupe lives on the **operation** (idempotency key), not the event.
**Oracle rows flipped.** Row 2 ⚠️→✅ (`gh.issue.view` drives goal ingestion — issue body
becomes the wave goal, replacing today's manual title entry); rows 3, 9, 14, 15, 16 ❌→✅;
row 13 ⚠️→✅ (gate gains `gh pr checks`). (Row 1 is NOT flipped here — its events are the
existing `WaveUpdated`+`CardAdded`×2+`OverlaySet` create tx, B6; the goal-ingestion change
touches row 2.)

### B1 — Workflow registration descriptor
**Current state.** `Manifest.exposes_tools` is documentation-only — the kernel rediscovers via
MCP `tools/list` but never reads/enforces the field (`manifest.rs:72-74`). No workflow
descriptor exists; "a workflow" is an ad-hoc spec prompt.
**Proposed change shape.** Add a `workflows: [{ workflow_id, entrypoint, plan_template,
gate_set, spec_instructions, card_kinds, inputs_schema, outputs_schema }]` field to the
manifest schema, with kernel validation in `plugin_host/manifest.rs`. The kernel binds a wave
to a registered workflow at create time; the descriptor supplies the plan template + gates +
instructions the generic kernel already knows how to execute.
**New events/tools.** **NEW** event `workflow.registered{workflow_id}`; binding recorded on
the wave.
**Oracle rows flipped.** Enables rows 1, 4, 5 to be plugin-driven (turns the ⚠️ "policy in
ad-hoc prompt" into declared workflow); no row goes red→green alone, but it is the
substrate for the issue-dev plugin to own the trace.

### B2 — Plugin card kind with backend semantics
**Current state.** `PluginUiCardHandler` matches `ui://` prefix
(`crates/calm-truth/src/card_kind/builtins.rs:161-175`, matcher `Prefix("ui://")` at `:168-170`);
`validate_payload` returns `Ok(())` with no inspection
(`crates/calm-truth/src/card_kind/builtins.rs:172-174`) — render-only, **no backend lifecycle**.
Only kernel built-ins (codex/claude/terminal/spec/wave-report) carry semantics.
**Proposed change shape.** Let a workflow register a card kind with backend hooks
(`on_create`/`on_update`/`on_delete` + payload validation against a manifest schema). The
issue-dev plugin registers a `pr` card kind (PR number, head_sha, checks status, review
verdicts) with real validation, distinct from opaque `ui://`.
**New events/tools.** New `CardKindHandler` methods; uses existing `card.added`/`card.updated`
events (`crates/calm-types/src/event.rs:382-396`).
**Oracle rows flipped.** Supports rows 9, 14 (PR card carries pr_number/checks state).

### B3 — Plugin-registers-tools-for-spec/worker (RE-SCOPED per B8 — much bigger than v1 stated)
**Current state (B8, verified).** "Plugin exposes tools to spec/worker" is far larger than a
one-way reverse switch:
- The tool channel is iframe→kernel ONLY: `POST /api/plugins/:id/tool-call` **hard-gates** to
  `neige.*` (`routes/plugins.rs:907-948`; non-`neige.` is 403); the plugin's own server tools
  are unreachable from this path.
- The `neige.*` callback table (`callbacks.rs:185-203`) is **built-in only** (overlay/card/
  event/kv) — there is no plugin-extensible dispatch.
- `manifest.exposes_tools` is **metadata-only** (`manifest.rs:72-74`) — the kernel never reads/
  enforces it.
- Worker/spec MCP tools come from a **STATIC registry** (`register_default_tools`
  `mcp_server/tools/mod.rs:29`; round-2 N-5/A; transport `mcp_server/transport.rs:383-483`; registry
  `mcp_server/registry.rs:204-295`) — there is no merge point for plugin-declared tools.
- An **outbound** plugin MCP `tools/call` exists (`plugin_host/mcp.rs:498-522`) but it is the
  *kernel→plugin-server* direction and is **NOT wired** to worker-facing tool discovery,
  permissioning, or routing.
**Proposed change shape (re-scoped).** B3 must deliver three things, not one:
1. **Worker-facing tool discovery** — a merge point so plugin-declared tool descriptors join the
   static `register_default_tools` set the spec/worker MCP registry serves (`mcp_server/tools/
   mod.rs:21-37`, `registry.rs:204-295`).
2. **Permissioning** — the kernel reads `manifest.exposes_tools` (today metadata-only) and a
   per-wave grant so only the bound workflow's tools are visible to its spec/worker; mirrors the
   existing iframe `neige.*` gate but for the *outbound* direction.
3. **Call routing** — a spec/worker `tools/call` for a plugin tool routes to the plugin process
   via the existing outbound `plugin_host/mcp.rs:498-522` path (a request/response RPC — round-3
   N-1; currently has no worker-facing caller). For git/forge tools the route lands a `forge-action`
   op (§2.5-A), not a raw pass-through RPC.
**New events/tools.** Registration dispatch; per-tool descriptors injected into the MCP registry;
**NEW** `plugin.tool.registered{tool_name}`.
**Oracle rows flipped.** Prerequisite for rows 3, 9, 10, 14, 15, 16 (all the NEW git/forge
tools); none flip without B3.

### C — Dual-independent-reviewer primitive + coded convergence strategy in the workflow's own store
**Current state.** The loop and durability are solid: `HarnessSnapshot{phase,push_watermark,
pending_queue}` persists every turn (`snapshot.rs:24-57`); parked-ops give a model-checked
single-winner fence across restart. **But** (a) there is **no dual-independent-reviewer
primitive** — review is by convention (one spec thread, one `report.md` projection entry,
`crates/calm-truth/src/wave_vcs.rs:364-439`), with nothing asserting that two reviewer cards
with disjoint roles run and both verdicts are recorded; and (b) round-N counting, the
diminishing-returns cap, "always re-review", and systemic-root-cause judgment live only in
codex-thread memory — not durable, not asserted.
**Proposed change shape.** This gap owns the **dual-independent-reviewer primitive**: the plan
template carries ≥2 review tasks with **disjoint reviewer roles** (the two channels), the
workflow dispatches both as INDEPENDENT reviewer cards, and **both verdicts are recorded**
(distinct from today's single-thread convention). On top of that, add a per-workflow durable
store (plugin KV via `neige.kv.*` already exists — `callbacks.rs:185-203`) holding
`{review_round, cap, channels[], last_root_cause}`. The plugin reads/writes it each turn; a fix
increments `round`, refuses beyond `cap`, and always re-dispatches BOTH review channels. The
state is **observable** so the E2E can assert it.
**Design-phase dual review (A5/oracle row 6).** The dual-channel review runs at TWO phases: the
**design** phase (row 6 — before any impl task dispatches) AND the **per-PR** phase (row 10).
The plan template emits the design-review tasks BEFORE the impl tasks (dep edges), and the E2E
asserts the **design-review-before-impl-dispatch** ordering (§6). Both phases are kernel-claimed
(`task.dispatched` is `KernelDispatcher`-authored from the plugin-authored plan, A4 — NOT
plugin-emitted).
**Convergence-FAILURE (A1; FSM-soundness fixed round-2 SF-2 + round-3 C4/C5; subject key corrected
round-4 R4-4).** The `review.round` op records `converged: bool` keyed by a **LOGICAL SUBJECT KEY
`{phase, slice_id, pr_number}`** (round-4 R4-4 — `head_sha` is NOT in the grouping key; it is the
reviewed REVISION, a field). The subject key is carried in the `review.round` payload AND mirrored in
the `forge.pr.merged` payload (with `head_sha`/revision) so the merge fence is evaluated PER SUBJECT;
a later converged round for PR B cannot clear PR A's unconverged fence, and — crucially — **a later
CONVERGED revision of PR A supersedes an earlier unconverged revision of PR A** because both share
the same logical subject S (the v4 key, which put `head_sha` in the grouping, made each revision its
own singleton subject so an old unconverged head stayed "latest" forever and never got superseded —
R4-4). At `round == cap` for subject S with a non-approving last verdict it sets `converged: false`
and the merge ops MUST NOT be requested for S **while that round stays the latest unconverged round
for S**; and any merge for S MUST carry the head_sha of the latest CONVERGED revision (merge head ==
latest converged revision — R4-4). The spec then drives one of two sub-terminals: **GIVE-UP
`reviewing→failed`** (terminal — give up on the slice) or **ASK-HUMAN** a **TWO-EDGE path**
`reviewing→working` (`wave_lifecycle.rs:272`) THEN `working→blocked` (`:270`) — **there is NO
`reviewing→blocked` edge** (round-3 C4/SF-B: cap-exhaustion is detected in `reviewing`, since the
merge is `reviewing→done` `:273`; I verified `grep 'Reviewing, WaveLifecycle::Blocked'` → none, so
the wave MUST first return `reviewing→working` before parking `working→blocked`). `blocked` is a
PAUSE: `blocked→working` is legal at `:278`, so a `ratify.resolved(grant)` resumes the run
(`blocked→working→reviewing`) and a later converging round for S may legally merge. The intermediate
`wave.lifecycle_changed{reviewing→working}` is a REQUIRED backbone edge on the ASK-HUMAN sub-path
(§1 backbone summary + §6). The cap is **enforced by the durable round state** (temporal
latest-round-per-subject invariant, §6), not agent memory — and crucially NOT as whole-run
merge-tail absence, which would false-fail a granted-then-reconverged run.
**Durability.** `review.round` is recorded as a forge/review OPERATION (or, minimally, the plugin
KV store via `neige.kv.*`, `callbacks.rs:185-203`) — and the `review.round` event MUST traverse
the §2.5-C observation plumbing so the spec sees its own round state after a restart.
**New events/tools.** **NEW** `review.round{wave_id, subject:{phase,slice_id,pr_number},
head_sha (reviewed revision), n, cap, converged}` event (wave-scoped, B4; **logical subject key
`{phase,slice_id,pr_number}`, `head_sha` is the reviewed revision not a key part — round-4 R4-4**;
**`wave_id` + subject ids in the PAYLOAD with an explicit `topics()` arm**, round-3 C6); reuses
existing `task.dispatched`×2 (kernel-claimed, per channel) and `neige.kv.set`.
**Oracle rows flipped.** Row 5 (dual-reviewer-primitive `❌`-part) ⚠️→✅; row 6 ❌→✅
(two independent **design-phase** reviewer cards, both verdicts); rows 11, 12 ⚠️→✅; **row 17
→ ✅** (cap-exhausted convergence-failure branch enforced); and the **review half of
row 10** (both reviewers read exactly one PR's diff — the diff *source* is A6/slice ③, but the
two-independent-channel requirement is owned here).

### D — Human ratify gate
**Current state.** `/spec/input` injects a human message (`routes/cards.rs:118,650`) and the
agent can idle; `Blocked` lifecycle exists. But there is no **structured** "park until the
architect approves" primitive — challenge-before-impl, preview signoff, pause-on-overlap are
convention only.
**Ratify authority decision (B5 — CANNOT be plugin-authored as v1 implied).** The ratify gate
flips wave lifecycle to `blocked` and back. But `ActorId::Plugin(_)` classifies as
`ActorKind::Other` (`crates/calm-types/src/wave_lifecycle.rs:110`) which `validate_transition`
**rejects for ALL transitions** (`crates/calm-types/src/wave_lifecycle.rs:188-196`,
`NotAuthorized`). So `calm.ratify.*` **cannot** be a plugin-authored lifecycle write.
**Decision: `calm.ratify.*` is a SPEC-authored primitive** (the spec agent calls it; the
lifecycle flip is authored by `ActorId::AiSpec(_)` → `ActorKind::SpecAgent`, which is authorized
for `working→blocked` line 270 and `blocked→working` line 278). The plugin's role is to *instruct
the spec* to call ratify at the right points (challenge-before-impl, cap-exhausted); it does not
itself drive lifecycle. The human verdict (`calm.ratify.grant`/`deny`) is User-authored (User is
also authorized for `blocked→working`, line 278). This matches the existing authority model — the
spec drives lifecycle, the plugin supplies policy.
**Proposed change shape.** A first-class ratify primitive: `calm.ratify.request{reason}` (spec
tool) parks the wave in `blocked` (legal FSM, `crates/calm-types/src/wave_lifecycle.rs:270` edge
`working→blocked`; enforced by `validate_transition` `:170-295`) and emits a typed
`ratify.requested`; a human verdict (`calm.ratify.grant`/`deny`, User-authored, or a
`/spec/input` verdict) emits `ratify.resolved` and resumes (`blocked→working`, line 278). Unlike
passive `/spec/input`, this is a deterministic gate the E2E and the architect can both see. The
`ratify.*` events traverse the §2.5-C observation plumbing.
**New events/tools.** **NEW** `calm.ratify.request` (spec)/`calm.ratify.grant` (User) tools;
**NEW** `ratify.requested`/`ratify.resolved` events (wave-scoped, B4).
**Oracle rows flipped.** Row 4 ⚠️→✅; underpins the row-17 ratify path.

---

## §4 Slice plan ①→⑦

**Ownership rule (B9 — adopted).** *Exactly ONE slice FLIPS each oracle row to ✅.* "Enables"
is NOT "flips" and must NEVER be listed as a slice's acceptance. A slice whose only effect is to
make later rows *reachable* (substrate) has acceptance stated as a **direct, self-contained
check** (a test of its own deliverable), and the rows it unblocks are listed separately under
"unblocks (not acceptance)". The §1 oracle has **17 rows**; the flip-owner table at the end of
§4 assigns each a single owning slice.

> **Note on row 1.** Row 1 is **not** a flip target. Its events (`WaveUpdated`+`CardAdded`×2+
> `OverlaySet`) already fire today (B6); the only ⚠️ in row 1 is `gh issue view` ingestion, which
> is the **row-2** flip (goal text). Row 1 carries no slice and is asserted as-is.

### ① Workspace-lease primitive + cwd plumbing + budget guard (A4 / §2.5-B)
- **Scope.** Stop dropping `cwd` for `TaskKind::Codex`; add the **kernel workflow-agnostic
  isolated-workspace-lease primitive** (lease row + disjoint path) acquired in `prepare_tx`;
  add the **budget>1 resource guard** (lease-acquirable ⇒ ready); orphan-reclaim on boot.
  Git-on-lease layering is NOT in ① (it belongs to ③/⑥) — ① delivers the generic lease only.
- **Files touched.** `scheduler.rs:153-162` (add `cwd` to Codex payload, `None`-preserving),
  `CodexWorkerOperationPayload` (codex_adapter), `CodexWorkerAdapter::prepare_tx` (acquire
  lease), `worker_cleanup.rs:13-90` / `codex_adapter.rs:916-1016` compensation (add
  `release_workspace_lease` step; add optional `tx` param to `compensate_step` — §2.5-B (i)),
  `scheduler.rs:118-145` `compute_ready` (lease-acquirable gate), `recover_on_boot`
  (`operation/driver.rs:240`, called from `lib.rs:124` — round-2 N-5; NOT the `#[cfg(test)]`
  fixture at `mod.rs:1730`) lease reclaim, new migration `workspace_leases`.
- **New events/tools.** `workspace.leased{path,lease_id}`, `workspace.released{lease_id}`
  (wave/card-scoped). These traverse §2.5-C plumbing (folded into ⑦ if ⑦ lands first; else
  in-slice).
- **Acceptance (FLIPS row 7).** **Oracle row 7 → ✅**: a claimed Codex task runs in a DISTINCT
  leased cwd under `.claude/worktrees/`, `cwd∈payload`, `None` stays `None`, the lease row is
  `held` during the run and `released` after, and a boot with a `held` lease whose process is
  dead reclaims it (test).
- **Size.** **M** (was S in v1; the lease + budget guard + boot reclaim enlarge it).
- **Dependencies.** None. Foundation: ③/⑥ layer git on the lease.

### ② Plugin-exposes-tools surface (B3, re-scoped)
- **Scope.** Worker-facing tool **discovery** (merge plugin descriptors into the static
  registry) + **permissioning** (kernel reads `manifest.exposes_tools` + per-wave grant) +
  **call routing** (spec/worker `tools/call` → plugin process via outbound MCP). Three deliverables,
  per the B8 re-scope.
- **Files touched.** `plugin_host/manifest.rs:72-74` (read `exposes_tools`), `plugin_host/
  callbacks.rs:185-203` (register dispatch), `register_default_tools` `mcp_server/tools/mod.rs:29`
  + `mcp_server/registry.rs:204-295` (merge point), `mcp_server/transport.rs:383-483` (serve merged
  set), `plugin_host/mcp.rs:507` `tools_call` (wire outbound call to a worker-facing caller),
  `routes/plugins.rs:907-948` (registration endpoint).
- **New events/tools.** `plugin.tool.registered`; registration dispatch.
- **Acceptance (substrate — FLIPS no row).** Direct self-contained check: a test plugin
  registers a no-op tool; a spec/worker MCP session **discovers** it in `tools/list`, is
  **permissioned** (denied if not granted), and a `tools/call` **routes** to the plugin process
  and returns its result. *Unblocks (not acceptance): rows 3/9/10/14/15/16.*
- **Size.** **L** (was M; the three-deliverable re-scope enlarges it).
- **Dependencies.** None structurally; ordered after ① so worktrees exist for ③.

### ⑥ Forge-actions-as-OPERATIONS (B1 / §2.5-A) — durability spine + the forge-specific exactly-once contract
- **Scope.** The concrete `ForgeActionAdapter` with its **PURPOSE-BUILT exactly-once contract**
  (round-4 R4-1/2/3 — NOT a naive `TaskVerifyAdapter` copy): idempotency key from frozen domain rows,
  `prepare_tx` freezing argv + `event_spec` + `probe_argv`, **held-handshake `spawn_side_effect` with
  POST-PARK go-token release** (R4-1 — nothing irreversible runs until durably parked), the
  **R4-1 adapter-local variant (a)** stdin-into-observer handoff that lets the observer release the
  held go-token as its first post-park step, **`complete_forge_op_with_result` atomic completion
  helper for EVERY oracle action** emitting
  the bounded-extractor-built typed forge event in the op-flip tx (R4-2 — no `SpawnOutcome::Ready`
  shortcut for any oracle-visible action; R4-3 — the bounded `ForgeEventSpec` extractor:
  exit-code | named `--json` field paths), **probe `recover_parked`** via `verdict_from_exit_code` +
  bounded re-extraction of OUTPUTS (NO boolean JSON DSL — R4-3), `plan_compensation`/`compensate_step`;
  registration in `dispatcher_operation_runtime`. This is the durable spine ③ rides on (NOT bare
  tools+events). **The exact stdin-handoff mechanism (R4-1) and the exact bounded extractor grammar
  (R4-3) are this slice's implementation+review deliverables** — the design doc fixes the contract;
  the precise shapes are reviewed at impl. **Implementation note:** R4-1 shipped as adapter-local
  variant (a): the observer owns the child stdin and writes `go\n` first post-park; this needed no
  operation-framework change and no `SpawnOutcome::ParkedDeferredRelease`. The `compensate_step`
  tx-param extension (§2.5-B (i)) is **DEFERRED to slice ③**; ⑥ releases the workspace lease
  pool-based via `release_workspace_lease_by_id`, matching slice ①'s Codex compensation and avoiding
  a cross-adapter signature change in ⑥.
- **Files touched.** New `crates/calm-server/src/operation/forge_action_adapter.rs` (modeled on
  `task_verify_adapter.rs`'s spawn/park/recover skeleton, with the forge-specific contract:
  post-park-release `spawn_side_effect`, `complete_forge_op_with_result` atomic helper, probe +
  bounded-extractor `recover_parked`), `operation/mod.rs` (no NEW phases — reuses
  Pending/TxCommitted/SpawnStarted/Parked/Succeeded/Compensating),
  `dispatcher.rs:160` (`fn dispatcher_operation_runtime`) + adapter vec `:244-255` (register one
  line — round-3 N-2: `:158` is a brace, not the register site).
- **New events/tools.** Defines the `forge.pr.merged` Event variant as the mechanism-test vehicle
  for the exactly-once completion contract; slice ③ still flips oracle row 15 by wiring the real
  `gh pr merge` verb to emit it and owns the other forge events. ⑥ still FLIPS no oracle row.
  `forge-action` op kind constant; the bounded `ForgeEventSpec` extractor type.
- **Acceptance (substrate — FLIPS no row).** Direct self-contained check: a `forge-action`
  op for a fake/echo action is idempotent under resubmit (same key ⇒ one op), parks + completes
  via observer (the typed event committed atomically in `complete_forge_op_with_result`), and
  `recover_on_boot` reattaches/compensates it correctly via the recovery `probe_argv` (boot-recovery
  test injecting each phase, per the durable-ops procedure). **Plus the R4-1 pre-park crash test:** a
  kill AFTER `spawn_side_effect` spawns the held launcher but BEFORE the **post-park** go-token release
  leaves the action UN-RUN (the launcher EOFs on stdin and exits 75); the `SpawnStarted` re-drive
  re-spawns a held launcher and does NOT re-run the prior instance, because nothing irreversible ran
  before park (consistent with the post-park-release contract in §2.5-A). **Plus the R4-2/R4-3 typed-output test:** an oracle action (fake `pr.merge`) completes
  through the parked-completion helper and emits a typed `forge.pr.merged{merge_sha,…}` whose OUTPUT
  fields were filled by the bounded extractor from the action's `--json` output; and a crash-then-probe
  recovery re-extracts the same OUTPUT fields. *Unblocks (not acceptance): the durable correctness of
  rows 3/9/14/15/16.*
- **Size.** **M→L** (the R4-1 post-park release + the bounded extractor + the parked-completion-for-
  all-oracle-actions contract enlarge it beyond the v4 "copy").
- **Dependencies.** ① (lease primitive for workspace interplay; the `compensate_step` tx param is
  deferred to ③).

### ⑦ Observation/recovery plumbing for new spec-facing events (B3 / §2.5-C)
- **Scope.** Own the **observation/recovery PATTERN + the shared 6-stage plumbing** so `forge.*`,
  `review.round`, `ratify.*`, `workspace.*` events reach the spec live AND are recovered on boot.
  Each *event-defining* slice (①/③/⑤) wires its OWN §2.5-C arms for the events it introduces; ⑦
  delivers the reusable pattern + the cross-cutting stage edits (the filter vec, the predicate, the
  boot kinds array, the enum-mapping shape) that every such event rides.
- **Files touched.** `dispatcher.rs:637` (`let kinds: Vec<String>` filter vec) + `:1257`/`:1258`
  (`dispatcher_filter_matches_push_kinds` / `SubscribeFilter`), `dispatcher.rs:70`
  (`event_warrants_spec_push_with_role` arms, `_ => false` at `:93`), `harness/mod.rs:100-108`
  (recovery query kinds array), `crates/calm-types/src/observation.rs:18`/`:77`/`:95` (enum +
  `is_hard_fire` + `to_turn_text` — the latter EXHAUSTIVE, so a missing arm is a COMPILE error, no
  panic-path test, S-2), `dispatcher.rs:1108` (`harness_observation_from_event`), `dispatcher.rs:815`
  (live dispatch arm in `handle_envelope`).
- **New events/tools.** None of its own; it carries the events ①/③/⑤ define.
- **Acceptance (substrate — FLIPS no row).** **Resolved ordering (round-2 S-3 — the v2 "no deps +
  self-contained test + no events of its own" trio was circular):** ⑦'s pattern lands first, but its
  end-to-end self-test RIDES the first event-defining slice. The natural carrier is **①'s
  `workspace.leased`** (① has no deps and lands earliest), so ⑦'s acceptance is exercised JOINTLY
  with ①: emit a `workspace.leased` (wave-scoped) to the DB, trigger `spawn_recovered_harness` →
  `replay_harness_events_since`, assert the observation lands in the recovered
  `snapshot.pending_queue` and a turn issues; and a LIVE emit pushes it through
  `observe_harness_under_lock`. (Alternative if ⑦ must be tested in true isolation: a single
  test-only throwaway `Observation::__RecoveryProbe` variant — but riding ①'s real event is
  preferred.) *Unblocks (not acceptance): the spec reacting to every forge/review/ratify event in
  rows 3/9/11/14/15/16/17.*
- **Size.** **M**.
- **Dependencies.** **① for the first carrier event** (its self-test rides `workspace.leased`); the
  *pattern* otherwise has no structural deps and lands before/alongside ③/⑤ (each event-defining
  slice wires its own arms). The dependency graph below adds the ⑦←① carrier edge.

### ③ Git/forge toolset plugin (A5/A6/A8/A9) — rides ⑥+⑦
- **Scope.** A plugin exposing `gh.issue.view`, `gh.pr.list/create/diff/checks/merge`,
  `gh.issue.close`, `git.branch.create`, `git.commit` — **each dispatched as a `forge-action`
  op (⑥)**; real code-diff against merge-base; squash-merge + delete branch; close issue.
- **Files touched.** New plugin crate/manifest; NEW event variants in
  **`crates/calm-types/src/event.rs`** (the real enum; `crates/calm-server/src/event.rs` is the
  5-line `pub use` shim — round-2 B-1/SF-4, do NOT edit the shim). Full gap-shape each: variant +
  `kind_tag`+`metadata`+`topics` arms (**NOT** `from_kind_and_payload` — generic serde, round-2
  SF-3) + emission site + coverage test + **fixture-list entry
  `crates/calm-types/src/event.rs:1988`** + **generated TS bindings**
  (`#[ts(export…)]` on `Event` at `crates/calm-types/src/event.rs:342`, B10); a **single batched**
  `SYNC_EVENT_VERSION` bump for ③'s release, not one per event (round-2 N-2; current value 4 at
  `crates/calm-types/src/event.rs:327`); §2.5-C wiring per event;
  migration `0XXX_*.sql` only if envelope shape changes (events table itself has no dedupe key —
  dedupe is on the op).
- **New events/tools.** Tools above (as `forge-action` ops); events `forge.scan.completed`,
  `forge.pr.opened`, `forge.pr.diff.read`, `forge.pr.checks`, `forge.pr.merged`,
  `forge.issue.closed`.
- **Acceptance (FLIPS rows 2,3,8,9,13,14,15,16 + diff-source-half of 10).** **Row 2 → ✅**
  (`gh.issue.view` drives goal ingestion); **row 8 ⚠️→✅** (real branch + commits on the
  separate git backend, A5); **rows 3, 9, 14, 15, 16 → ✅**; **row 13 ⚠️→✅** (gate gains
  `gh pr checks`); and the **diff-*source* half of row 10** (one PR's real code diff against
  `merge-base(main,head)`, A6 — the dual-channel half is ⑤'s flip). Each merge/close fires on
  the CONVERGE branch only (the FSM/cap discipline is ⑤'s).
  **(Gap→slice aggregation note — round-2 S-4):** slice ③ aggregates the rows from THREE per-gap
  designs whose individual "rows flipped" lists are grouped differently — **A5** (row 8, branch +
  commits), **A6** (row-10 diff-SOURCE half), and **A8/A9** (rows 2,3,9,13,14,15,16). The
  flip-owner table is authoritative; this note saves the reader the 4-way merge across A5/A6/A8/A9.
- **Size.** **L**.
- **Dependencies.** ① (lease/worktree), ② (tool channel), ⑥ (forge-action ops), ⑦ (observation).

### ④ Workflow registration descriptor (B1 + B2)
- **Scope.** Declare "issue development" as the first workflow: manifest `workflows` field
  (plan template + gate set + spec instructions + card kinds), kernel validation, wave→workflow
  binding; register the `pr` card kind with backend validation.
- **Files touched.** `plugin_host/manifest.rs` (schema + validate), new backend-semantics card
  handler alongside `crates/calm-truth/src/card_kind/builtins.rs:161-175` (the existing
  render-only `PluginUiCardHandler` is the contrast point — ④ adds a handler *with* lifecycle
  hooks), wave-create binding, scheduler/spec plumbing to read the descriptor.
- **New events/tools.** `workflow.registered`; uses `card.added`/`card.updated`.
- **Acceptance (substrate — FLIPS no row).** Direct self-contained check: the `pr` card kind
  validates its payload (rejects a card missing PR number / head_sha / checks status), distinct
  from opaque `ui://`; and a wave binds to a registered workflow at create time (the binding row
  exists). *Unblocks (not acceptance): rows 5/6 become plugin-driven (the descriptor supplies
  the plan template the spec emits), and row 4's ratify gate hangs off the workflow binding —
  but those rows are FLIPPED by ⑤, not here.*
- **Size.** **M**.
- **Dependencies.** ② (tool channel), ③ (the tools the descriptor references).

### ⑤ Dual-reviewer primitive + convergence (incl. FAILURE branch) (C) + ratify gate (D)
- **Scope.** The **dual-independent-reviewer primitive** at BOTH design (row 6) and per-PR
  (row 10) phases (plan template carries ≥2 review tasks with disjoint reviewer roles, dispatched
  **kernel-claimed** from the plugin-authored plan — `task.dispatched` is `KernelDispatcher`-
  authored, A4; both verdicts recorded); durable
  `{subject:{phase,slice_id,pr_number}, head_sha (reviewed revision), round, cap, channels,
  root_cause, converged}` (LOGICAL subject key — round-4 R4-4: `head_sha` is the reviewed revision,
  not part of the grouping key, so a later converged revision supersedes an earlier unconverged one;
  round-3 C5); always-re-review; the **convergence-FAILURE branch** with TWO
  sub-terminals (cap-exhausted ⇒ no merge ⇒ **GIVE-UP `reviewing→failed` (terminal)** OR
  **ASK-HUMAN: the TWO-edge path `reviewing→working` (`:272`) THEN `working→blocked` (`:270`)** — there
  is NO `reviewing→blocked` edge, round-3 C4 — PAUSE, resumable to `done` on grant, row 17 — round-2
  SF-2 + round-3 C4); **spec-authored** `calm.ratify.request` + User-authored `grant` (B5).
- **Files touched.** Plugin store logic / `review.round` op (uses `neige.kv.*` or a review op),
  plan-template dual-review wiring (design + per-PR, with design-before-impl dep edges),
  NEW `ratify.*` tools (spec/User-authored, B5) + events, `review.round` event (subject-keyed payload
  + `wave_id` + explicit `topics()` arm, round-3 C5/C6; + §2.5-C wiring),
  `crates/calm-types/src/wave_lifecycle.rs` (no rule change — reuses `reviewing→working` :272 +
  `working→blocked` :270 for the ASK-HUMAN two-edge path, `blocked→working` :278, `reviewing→failed`
  :274; spec/User authority already in place).
- **New events/tools.** `calm.ratify.request` (spec)/`grant` (User); events `ratify.requested`,
  `ratify.resolved`, `review.round{wave_id, subject:{phase,slice_id,pr_number}, head_sha (revision),
  n, cap, converged}` (logical subject key + `head_sha` as reviewed revision — round-4 R4-4;
  wave_id+subject in payload, C6); reuses `task.dispatched`×2 (kernel-claimed, per channel).
- **Acceptance (FLIPS rows 4,5,6,11,12,17 + dual-channel-half of 10).** **Row 5 (dual-reviewer
  part) → ✅**, **row 6 → ✅** (two independent DESIGN-phase reviewer cards, both verdicts,
  before impl dispatch), **the dual-channel half of row 10 → ✅** (both reviewers read exactly
  one PR's diff via two channels; diff *source* is ③), **rows 4, 11, 12 → ✅**, **row 17 → ✅**
  (the temporal SUBJECT-KEYED cap-enforcement assertion — round-2 SF-2 + round-3 C5 + round-4 R4-4: no
  `forge.pr.merged` for subject S (LOGICAL key `{phase,slice_id,pr_number}`) while the latest
  `review.round` FOR S is `converged:false`, AND any merge for S carries the latest converged
  revision's `head_sha` (merge head == latest converged revision — R4-4); GIVE-UP→`failed` asserts
  whole-run merge-tail absence, ASK-HUMAN asserts the two-edge `reviewing→working→blocked` path
  (round-3 C4) and absence only until `ratify.resolved(grant)`, then CONVERGE may merge).
- **Size.** **L** (was M; the failure branch + design-phase review + B5 authority enlarge it).
- **Dependencies.** ④ (workflow owns the store + protocol), ③ (the PR diff source), ⑦ (observation
  for `review.round`/`ratify.*`).

### Flip-owner table (B9 — exactly one flipping slice per row)
| oracle row | owning slice (FLIPS) | notes |
|---|---|---|
| 1 | — (asserted as-is) | events already fire (B6); the ⚠️ is `gh issue view`, flipped via row 2 |
| 2 | ③ | `gh.issue.view` goal ingestion |
| 3 | ③ | `forge.scan.completed` (rides ⑥+⑦) |
| 4 | ⑤ | spec-authored ratify gate (B5) |
| 5 | ⑤ | dual-reviewer primitive (④ only *enables* the plan template) |
| 6 | ⑤ | design-phase dual review before impl dispatch |
| 7 | ① | workspace lease + budget guard |
| 8 | ③ | real branch + commits |
| 9 | ③ | `forge.pr.opened` |
| 10 | ③ (source half) + ⑤ (dual-channel half) | the ONLY split row; each half has a distinct, named deliverable |
| 11 | ⑤ | `review.round`, round≤cap |
| 12 | ⑤ | re-review after each fix |
| 13 | ③ | gate gains `gh pr checks` |
| 14 | ③ | `forge.pr.checks` |
| 15 | ③ | `forge.pr.merged` (CONVERGE branch) |
| 16 | ③ | `forge.issue.closed` (CONVERGE branch) |
| 17 | ⑤ | convergence-FAILURE branch (GIVE-UP→`failed` / ASK-HUMAN→`blocked`), temporal cap |

Substrate slices ②/④/⑥/⑦ FLIP **no** row; their acceptance is a direct self-test (above; ⑦'s
self-test rides ①'s `workspace.leased` per round-2 S-3, but it still flips no row).
Row 10 is the single deliberate split (source vs dual-channel) — each half is a distinct,
independently-testable deliverable, so it does not violate "one flipping slice" (it is two
disjoint sub-rows).

### Dependency graph (restated)
```
①  workspace-lease + budget guard      (no deps)
②  plugin-exposes-tools (discovery/perm/route)   (no deps; after ① for ③)
⑥  forge-actions-as-operations          (deps: ①)
⑦  observation/recovery plumbing        (pattern no-deps; ① for the first CARRIER event — S-3)
③  git/forge toolset plugin             (deps: ① ② ⑥ ⑦)
④  workflow registration descriptor     (deps: ② ③)
⑤  dual-reviewer + convergence + ratify (deps: ③ ④ ⑦)

   ①
   ├──────┬───────┐
   ▼      ▼       ▼
   ⑥      ⑦       ②       (⑦←① = carrier-event edge for ⑦'s self-test, S-3)
    \     │      /
     \    ▼     /
      └──▶③◀───┘
            │
            ▼
            ④
            │
            ▼
            ⑤
```
Topological order ①→②→⑦→⑥→③→④→⑤ has no back-edge (the ⑦←① carrier edge keeps the graph acyclic).

---


_**Part 4/5 — §5–§6** (open questions/decisions; E2E acceptance strategy)._

---

## §5 Open design questions

1. **Real git per worker vs wave-vcs-becomes-real-git-backend.** wave-vcs today is SQLite
   content-addressed projection (`wave_vcs.rs:1-16`), explicitly chosen over git2-rs/gix/jj
   (`docs/_explore-wave-versioning.md:62-72`), with git-per-wave explicitly rejected for
   operational burden and event-transaction mapping (`_explore-wave-versioning.md:88-91`).
   **Caution anchor:** `docs/_runbook-718-vcs-cleanup.md:1,9` documents a **208 GB wave-VCS
   bloat** from ~17.5k live commits in one wave — a content-addressed snapshot store can blow
   up under high commit cadence. **Recommendation:** keep wave-vcs as the projection archive;
   add a **separate** real-git backend (the wave's `cwd` source checkout) for code diffs.
   Do NOT make wave-vcs the code-diff store. (For review: confirm the separate-backend split.)
2. **Where git/forge lives. [DECISION — round-2 SF-1 + round-3 C1/C2/C3 + round-4 R4-1/2/3; NOT
   "kernel git-free"; forge = a CONCRETE kernel adapter with a PURPOSE-BUILT exactly-once contract,
   not a naive task-verify copy and not a generic seam.]** Plugin-*requested* but kernel-*executed*
   as `forge-action` OPERATIONS (§2.5-A): the plugin owns the git/forge *execution semantics* (the
   verb taxonomy, which `gh`/`git` argv, the recovery `probe_argv`, the bounded result-extractor
   `event_spec`, the slice shape) and supplies them in the op **payload** as DATA; the side-effect
   runs through the durable parked-op machinery so it is crash-safe + recoverable. The pure "plugin tools + bare events" position from
   v1 is **rejected** (no event dedupe key, not crash-safe — B1). **The v2 claim "the kernel stays
   git-free" is RETRACTED as false** (round-2 SF-1/S-1): (a) the forge adapter is a compiled-in
   kernel-crate type with no plugin-adapter seam (`build_operation_adapters` `state.rs:350`; the only
   kernel→plugin reach is the **request/response RPC** `plugin_host/mcp.rs:507` `tools_call`, round-3
   N-1, not an operation), so it shells `git`/`gh` in calm-server; and (b) `routes/fs.rs:552-559`
   already shells `git` for file-browse (read-only there, mutating here — round-3 N-3). **The
   decision (round-3 systemic fix, SHARPENED round-4):** v3's "generic thin exec adapter recovered
   generically by the kernel" does not fit the framework; round-3 said "copy `TaskVerifyAdapter`";
   **round-4 found the copy is unsound** (task-verify is a resultless idempotent gate; a forge action
   is irreversible-with-typed-output — R4-1/2/3), so the kernel hosts a **CONCRETE
   `ForgeActionAdapter` with a PURPOSE-BUILT exactly-once contract**: **(R4-1)** the held-handshake
   go-token is released from the **POST-PARK observer** (nothing irreversible runs until durably
   parked — a small workflow-agnostic FRAMEWORK addition slice ⑥ lands, since today the observer
   cannot own the release: release happens inside `record_release` before the observer is built and
   `ParkedObserver` takes no params); **(R4-2)** EVERY oracle-visible action completes via the custom
   `complete_forge_op_with_result` helper (no `SpawnOutcome::Ready` shortcut for anything that emits
   an event); **(R4-3)** recovery is probe + **bounded result-extraction**
   (`verdict_from_exit_code` for did-it-land + a bounded exit-code|named-`--json`-field-path extractor
   for OUTPUTS — replacing v4's exit-code-only WITHOUT resurrecting v3's unbounded JSON-predicate
   DSL). **No git/gh verb-execution logic** (verb taxonomy, which argv, merge-state semantics, field
   paths) compiles into the kernel — those are payload data (argv + `event_spec` + `probe_argv`)
   authored by the plugin. **SF-1/C7 tension resolved (R4-3):** the typed event DATA shapes live in
   shared `calm-types` (definitions, no logic); the verb-execution logic stays plugin-supplied payload
   data. This is a compiled-in concrete adapter like the existing 10, NOT a plugin-supplied adapter
   (no such seam). The clean-generic half is the **workspace lease** (§2.5-B): a dir + a row, with
   genuinely no git. So the honest moat is *"no WORKFLOW git in the kernel,"* not *"no git in the
   kernel."*
3. **Spec-agent model.** codex-as-spec stays for now — it is the only injectable-turn
   app-server; the plugin supplies instructions/policy. Reopening orchestrator-model choice is
   out of scope. (Confirm: no model swap in #760.)
4. **Ratify-gate primitive shape. [RESOLVED v2; holds in v3.]** First-class `calm.ratify.*` (§3 D) for E2E
   observability — AND it is **spec-authored, not plugin-authored** (B5: `ActorId::Plugin` →
   `ActorKind::Other` is rejected for all lifecycle transitions, `wave_lifecycle.rs:110,188-196`).
   The spec calls ratify; the human grants; the plugin only instructs the spec when to call it.
5. **NEW-event count & shape. [DECISION — round-3 C7: DISTINCT TYPED ENUM VARIANTS, not a single
   `forge.event{kind}` envelope.]** ③ introduces ~6 forge events + ⑤ ~3; each forces the full event
   gap-shape (variant + `kind_tag`/`metadata`/`topics` arms + emission + fixture + TS bindings — but
   **NOT** a `from_kind_and_payload` arm, which is generic serde, round-2 SF-3). **`SYNC_EVENT_VERSION`
   bumps ONCE per shipping RELEASE, not once per event** (round-2 N-2: the version history at
   `crates/calm-types/src/event.rs:303-326` shows v3 added TWO kinds in ONE bump; current value 4 at
   `:327`) — so ③'s ~6 + ⑤'s ~3 are at most two bumps (one per slice's release), not nine.
   **The Q5 fork (enum-per-event vs single envelope) is now CLOSED in favor of DISTINCT VARIANTS
   (round-3 C7).** Reasoning recorded: the event spine is **typed enum arms** (`event.rs:340-788`),
   an **exact `events.kind` filter**, a **TS union** (ts-rs over the tagged enum, `:342`),
   per-variant **`metadata()`** (`:788`) and **`topics()`** (`:1035`), and replay/query/oracle that
   key on exact kinds. A single `forge.event{kind,…}` envelope would **hide merge/check/open facts
   behind payload parsing** — weakening replay filtering, the kind-keyed oracle backbone, and
   `topics()`/`metadata()` precision (every consumer would re-parse the envelope to recover the kind).
   Since the version-bump cost is **per-release, not per-event** (N-2), the envelope buys nothing the
   spine needs. **Decision: one distinct typed variant per oracle-significant forge/review event.**
   **How the typed variant's OUTPUT fields get filled without baking verbs in (round-4 R4-3, SF-1/C7
   resolved):** the variant DATA shapes (`ForgePrOpened{pr_number,head_sha}`,
   `ForgePrMerged{merge_sha,…}`) live in shared `calm-types` as the issue-dev workflow's contribution
   to the shared event enum — data definitions, NO git/gh logic. The kernel adapter fills the named
   variant's fields via the payload's **bounded `ForgeEventSpec` extractor** (exit-code | named
   `--json` field paths — §2.5-A point 1), so the kernel constructs the typed event from action
   OUTPUTS without any verb-execution logic compiling in. This is what makes "distinct typed variants"
   compatible with "no git in the kernel": the SHAPES are shared types; the FILLING is bounded payload
   data.

---

## §6 E2E acceptance strategy

A real wave run is stochastic; the E2E asserts the **invariant level**, not content:
- **Backbone ⊇ oracle backbone.** The assertion is a **set-superset of event *kinds* plus a
  REQUIRED ORDERING INVARIANTS list** — NOT a total ordered-subsequence match. Collect the event
  kinds the real run emitted (via WS replay, `ws/events.rs:469-484`, or the events table) and
  assert (1) the §1 backbone-summary kinds for the run's branch are all **present** (set ⊇), and
  (2) the pairwise ordering invariants below hold. Do **not** assert a total order: stochastic
  interleaving is allowed where the workflow doesn't constrain it (e.g. the two `task.dispatched`
  review events vs `task.gate_result`, or the two review channels relative to each other, may
  arrive in either order). Kinds are deterministic (`kind_tag()`, `crates/calm-types/src/event.rs:958-990`).
- **REQUIRED ORDERING INVARIANTS (A2/A5 — these MUST be asserted; without them a merge with
  red/pending CI or a skipped design review would still pass).** As a `first_index(X) <
  first_index(Y)` (or for per-PR, scoped-to-the-same-PR) pairwise check over the emitted log:
  1. `wave.lifecycle_changed(planning)` BEFORE any `task.dispatched`.
  2. **`review.round(phase:design)` / design-review `task.dispatched` BEFORE the first impl
     `task.dispatched`** (A5 — a run that skips design review must FAIL; design-review-before-
     impl-dispatch). The `phase` field distinguishes the design round from per-PR rounds (round-3
     N-5): the cap-enforcement merge fence (below) keys on the latest `review.round` of **`phase`
     matching the merge subject** (i.e. the per-PR round for that PR), so a transient
     `converged:false` DESIGN round never blocks a later legitimate PR merge.
  3. `worktree.provisioned`/`workspace.leased` (for a slice) BEFORE that slice's
     `runtime.started`.
  4. `forge.pr.opened` BEFORE `forge.pr.diff.read` BEFORE the per-PR review `task.dispatched`.
  5. **`forge.pr.checks(conclusion:success)` BEFORE `forge.pr.merged`** (A2 — a merge with a
     red/pending checks conclusion must FAIL the E2E; checks-success-before-merge).
  5b. **(round-4 R4-4) `forge.pr.merged` for subject S MUST carry the `head_sha` of the LATEST
     CONVERGED `review.round` for S** (merge head == latest converged revision; a merge of a revision
     that the latest converged round did not review must FAIL).
  6. **each `task.gate_result(passed:true)` BEFORE `forge.pr.merged`** (A2 — local gate green
     before merge).
  7. **`forge.pr.merged` BEFORE `forge.issue.closed`** (no closing an issue whose PR didn't
     merge).
  8. `forge.issue.closed` BEFORE `wave.lifecycle_changed(done)`.
  9. **(ASK-HUMAN sub-path only, round-3 C4)** `wave.lifecycle_changed(reviewing→working)` BEFORE
     `wave.lifecycle_changed(working→blocked)` — cap-exhaustion is detected in `reviewing` and there
     is NO `reviewing→blocked` edge, so the two-edge detour is a REQUIRED, ordered pair on this
     sub-path (a run that emits a bare `working→blocked` without the preceding `reviewing→working`,
     or that attempts `reviewing→blocked`, must FAIL).
  Everything else is interleaving-tolerant.
- **Cap enforcement — a TEMPORAL, SUBJECT-KEYED invariant on the latest round PER SUBJECT, not
  whole-run mutual-exclusion (round-2 SF-2 + round-3 C5/N-5; supersedes the v2 "branch
  mutual-exclusion" framing, which was FSM-unsound on the ratify path because `blocked` is a PAUSE,
  not terminal — `blocked→working` is legal at `wave_lifecycle.rs:278`; subject key corrected
  round-4 R4-4).** The **LOGICAL subject key** is `{phase, slice_id, pr_number}` (round-4 R4-4 —
  `head_sha` is the reviewed REVISION, a field, NOT part of the grouping key), carried in BOTH the
  `review.round` and `forge.pr.merged` payloads (plus the `head_sha` revision). The enforceable,
  FSM-sound assertion is:
  > **No `forge.pr.merged` for subject S (and no `forge.issue.closed` / `wave.lifecycle_changed(done)`)
  > may appear while the latest `review.round` FOR SUBJECT S has `converged:false`; AND any
  > `forge.pr.merged` for S MUST carry the `head_sha` that the latest CONVERGED `review.round` for S
  > reviewed (merge head == latest converged revision).**
  Concretely the E2E groups `review.round` events BY SUBJECT KEY and, for each subject, walks the log
  in order: at the index of the last `review.round` for S, if its `converged==false`, then NO
  `forge.pr.merged` for S / `forge.issue.closed` / `done` appears at a LATER index *unless* a
  `ratify.resolved(grant)` AND a subsequent `review.round(subject:S, converged:true)` intervene
  (after which the merge for S must carry that converged round's reviewed `head_sha` — R4-4).
  Subject-keying on the LOGICAL key `{phase,slice_id,pr_number}` (round-4 R4-4) is what stops a later
  converged round for PR B from clearing PR A's unconverged fence (round-3 C5), AND lets a later
  converged REVISION of PR A supersede an earlier unconverged revision of PR A (same subject S — the
  v4 `head_sha`-in-key bug, R4-4); a `phase:design` round (no `pr_number`) is a DIFFERENT subject so
  it never masks a per-PR merge fence (round-3 N-5). Two terminal sub-cases:
  - **GIVE-UP (`reviewing→failed`, terminal):** the merge tail is absent for the whole run, and
    `(failed)` is present. (Whole-run absence is sound here because `failed` cannot resume.)
  - **ASK-HUMAN (TWO edges `reviewing→working→blocked`, PAUSE — round-3 C4):** `(blocked)` is present,
    preceded by `(reviewing→working)` (there is NO `reviewing→blocked` edge), and the merge tail for
    S is absent **only while the cap-hit round remains the latest unconverged round for S**. After
    `ratify.resolved(grant)` → `blocked→working` → a NEW converging `review.round(subject:S)`, the
    run legally re-enters CONVERGE and the merge tail is allowed. Asserting whole-run absence here
    would **false-fail a legal granted-then-reconverged run** — the v2 bug this fixes.
  The CONVERGE branch (no trailing unconverged latest round for the subject) asserts the merge tail
  (invariants 5-8) present. This makes `round ≤ cap` a real per-subject assertion: a merge for S may
  never fire while the most recent review verdict FOR S is unconverged-at-cap.
- **Required artifacts exist (CONVERGE branch).** Branch ref exists; PR number recorded;
  `gh pr checks` conclusion==success; issue state==closed; worktree under
  `.claude/worktrees/<...>` existed then removed (lease `released`).
- **FSM legal.** Every `wave.lifecycle_changed{from,to}` is a legal transition (module-doc edge list
  `crates/calm-types/src/wave_lifecycle.rs:30-44`; live match arms `:252-278`, enforced by
  `validate_transition` `:170-295` — round-3 channel-A clarification).
  End-state ∈ {`done` (CONVERGE, terminal), `failed` (GIVE-UP, terminal), or **parked at `blocked`
  awaiting ratify, reached via the TWO-edge `reviewing→working→blocked` path (round-3 C4 — there is
  NO `reviewing→blocked` edge; NON-terminal — `blocked→working` `:278` may resume the run; this is
  exactly why cap enforcement is the temporal subject-keyed latest-round invariant above, not
  whole-run absence)**}.
- **Content tolerance.** Do NOT assert prose, slice names, commit messages, or exact fix-round
  count — only that round ≤ cap and that each fix is followed by a re-review event (row 12).
- **Stability.** Run the wave N times (≥3) and confirm the backbone is stable run-to-run;
  flakiness in the backbone (not the prose) is a real defect. (Branch choice may vary if the
  agent genuinely converges some runs and not others; assert each run's tail matches its branch.)

**Harness.** Extend `e2e/cases/110-multitask-golden-path.sh` (the tier-2 multitask golden
path: `POST /api/coves` → `POST /api/waves` with a 2-worker title, polls
`/api/waves/<id>/cards` + checks files in container, asserts lifecycle==done). The #760 E2E
adds: assert the forge backbone kinds present, assert `.claude/worktrees/<slice>` provisioned
(row 7), assert PR/checks/merge/issue-closed facts (rows 9/14/15/16), and assert the REQUIRED
ORDERING INVARIANTS list above. Each slice extends the case incrementally — ① asserts row 7 +
lease-reclaim; ⑥/⑦ add their substrate self-tests (idempotent forge op; recovered observation);
③ adds the forge backbone + ordering invariants 4-8; ⑤ adds the design-review-before-impl
ordering (invariant 2), the round≤cap + re-review assertion, the **temporal SUBJECT-KEYED
cap-enforcement check** (no `forge.pr.merged` for subject S while the latest `review.round` FOR S is
`converged:false` — round-3 C5; GIVE-UP→`failed` asserts whole-run merge-tail absence,
ASK-HUMAN→`blocked` asserts absence only until `ratify.resolved(grant)` — round-2 SF-2, NOT a
whole-run mutual-exclusion), and the **ASK-HUMAN two-edge ordering** (invariant 9:
`reviewing→working` BEFORE `working→blocked` — round-3 C4). Note (from project memory): CI e2e lacks Codex terminal bytes and
dispatcher daemon-spawn flakes under parallel load — assert on persisted events, not live
terminal output, and avoid wall-clock-tight worker timing. **Crash-recovery sub-test (B1/B2/B3):**
a dedicated case kills + reboots the kernel mid-`forge.pr.merged` op and asserts (a) no
double-merge (idempotency key), (b) the held workspace lease is reclaimed, and (c) the
`forge.*`/`review.round` events that landed during the down-window are recovered into the spec's
`pending_queue` (§2.5-C).

---


_**Part 5/5 — §7** (disposition history — full audit trail of all 5 review rounds)._

---

## §7 Disposition history

| round | finding | disposition |
|---|---|---|
| 0 / completeness-critic | Grounding errors (FSM cite `wave_lifecycle.rs:68-74` was module-doc tail, not logic; row-1 `card.added` anchored to Claude-worker `claude_adapter.rs:337` not the wave mint site; B2 `card_kind.rs:161-175` pointed at `#[cfg(test)]` `TestHandler` not `PluginUiCardHandler`) + ownership holes (dual-reviewer primitive rows 5/6 had no owning §3 gap/slice; orphan rows 2/8; row-4 double-ownership; vague slice-④ acceptance; ambiguous crate-unqualified citations; weak row-6 test/default anchors; over-strong "backbone ⊇" total-order claim). | **Folded in v1.** Re-pointed FSM cites to `crates/calm-types/src/wave_lifecycle.rs:30-44` (rules table) + `:170-295` (`validate_transition`); re-anchored row-1 mint to `routes/waves.rs:547-548`; fixed B2 to `crates/calm-truth/src/card_kind/builtins.rs:161-175`; gave §3 C + slice ⑤ explicit ownership of the dual-independent-reviewer primitive (rows 5/6/12 + review half of 10); added row 2 to A8/A9 + slice ③ and row 8 to slice ③; resolved row-4 (D/⑤ owns the flip, ④ only enables); crate-qualified all ambiguous citations; swapped row-6 weak anchors to `scheduler.rs:555` (prod `task.dispatched` emit) + `crates/calm-truth/src/wave_vcs.rs:364-439` (single `report.md`); clarified "backbone ⊇" as set-superset-of-kinds + explicit pairwise ordering invariants (not total order). |
| 1 / A1 (test/AC) | Oracle trace all happy-path; backbone ends at `done`; "n ≤ cap" (row 11) is a tautology on a converged run. | **FIXED.** Verified FSM edges live (`working→blocked` `wave_lifecycle.rs:270`, `reviewing→failed` :274). Added CONVERGENCE-FAILURE branch: new oracle **row 17** (cap-exhausted → no merge → ratify`blocked`/`reviewing→failed`); §6 "Branch mutual-exclusion" makes the cap **enforced** (at `n==cap` unconverged: merge rows 15/16 absent, exactly one terminal-failure transition present). |
| 1 / A2 (test/AC) | §6 backbone is set-superset only → a merge with red/pending CI would pass; "checks before merge" not asserted. | **FIXED.** Added §6 **REQUIRED ORDERING INVARIANTS** list incl. `forge.pr.checks(success)` BEFORE `forge.pr.merged`, each `task.gate_result(passed)` BEFORE `forge.pr.merged`, `merged` BEFORE `issue.closed`, plus the design-review-before-impl and pr-opened/diff/review chain. Kept interleaving tolerance elsewhere. |
| 1 / A3 (test/AC) | Row 8 cites `claude_adapter.rs:337-344/:515-523` but the worker is CODEX. | **FIXED.** Verified real Codex emits: `RuntimeStarted` `crates/calm-server/src/operation/codex_adapter.rs:329`; `RuntimeStatusChanged` `codex_adapter.rs:1481,1585`. Re-anchored row 8. |
| 1 / A4 (test/AC) | Slice ⑤/§3-C imply the plugin emits `task.dispatched`×2; but `TaskDispatched` refuses `ActorId::Plugin` (kernel-only) — the scheduler emits it. | **FIXED.** Verified `role_gate.rs:220-239` rejects `Plugin`; scheduler authors as `KernelDispatcher` (`scheduler.rs:553-555`). Reworded rows 6/10/12 + §3-C + slice ⑤: review-task dispatch is **kernel-claimed from a plugin-authored plan**, not plugin-emitted. |
| 1 / A5 (test/AC) | Design-phase dual review (oracle row 6) has no backbone entry/ordering anchor; a run skipping design review passes. | **FIXED.** Made row 6 the explicit DESIGN-phase dual review (kernel-claimed, before impl dispatch); added `review.round(design)`; §6 ordering invariant 2 = design-review-before-impl-dispatch; §3-C "Design-phase dual review" paragraph. |
| 1 / A6 (test/AC) — round-0 ownership reconcile vs B9 | Channel A said round-0 ownership patch closed orphan/double-own holes; codex (B9) disagreed. | **RECONCILED toward B9.** Adopted the rule "exactly ONE slice FLIPS each row; enables ≠ acceptance"; rewrote §4 with a flip-owner table (17 rows). Channel A's "clean" verdict held for *coverage* (no orphan rows), but B9 was right that "enables" was being mislabeled as acceptance — §4 now separates FLIPS from "unblocks (not acceptance)". |
| 1 / B1 (durability) | Git/forge spine not crash-safe: proposed as tools+events, but events have NO dedupe key; durable safety comes from PARKED OPERATIONS. | **DESIGNED-IN §2.5-A.** Verified no event dedupe (`0004_events.sql:23-32`) and op dedupe `(kind, idempotency_key)` (`0042_operations_parked.sql:96-98`). Designed a `forge-action` operation kind grounded in `operation/mod.rs` Phase/ProviderAdapter/recovery anchors. New **slice ⑥**. §5-Q2 resolved. |
| 1 / B2 (durability) | Worktree provisioning leaks git into kernel adapter; compensation cleans card/terminal NOT worktrees/branches; scheduler sweep leaves running Codex workers → restart orphans worktree. Resolve kernel-vs-plugin tension. | **DESIGNED-IN §2.5-B.** Verified compensation gap (`codex_adapter.rs:916-1016`, `worker_cleanup.rs:13-90`) + scheduler `Running => {}` (`scheduler.rs:917-918`). Decision: **kernel provides a workflow-agnostic isolated-workspace-LEASE primitive; git stays in the plugin** (justified). Added lease row + fs compensation (`compensate_step` tx-param extension) + boot orphan-reclaim. Slice ① re-scoped (now M). |
| 1 / B3 (durability) | New forge/review/ratify events stored but NEVER reach/recover the spec; observation/recovery plumbing handles a fixed set only. | **DESIGNED-IN §2.5-C.** Verified the fixed sets: live predicate `dispatcher.rs:62-95` (defaults false), boot query hardcoded kinds `harness/mod.rs:99-111`, mapping `dispatcher.rs:1108-1183`, `observation.rs:16-67`. Designed the 6-stage traversal; new **slice ⑦**. |
| 1 / B4 (wire) | Per-event scope: the new events are card/task/wave-scoped, not wave/System; `topics()` falls back to `"*"` without payload ids. | **FIXED.** Added a **scope column** to §1 with the chosen `EventScope` per NEW event; §3 gaps now state scope; §2.5-C requires wave-or-narrower scope (else `observe_harness_under_lock` early-returns). Verified `crates/calm-types/src/event.rs:167-182` guidance. |
| 1 / B5 (lifecycle authority) | Ratify-as-plugin conflicts: `ActorId::Plugin → ActorKind::Other` is REJECTED for transitions. | **FIXED.** Verified `wave_lifecycle.rs:110` (`Plugin → Other`) + `:188-196` (Other rejected). **Decision: `calm.ratify.*` is SPEC-authored** (lifecycle flip by `AiSpec` → `SpecAgent`, authorized for `working→blocked` :270, `blocked→working` :278); human grant is User-authored. §3-D + §5-Q4 + slice ⑤ updated. |
| 1 / B6 (wire) | Oracle row 1 STALE: create emits `WaveUpdated`+`CardAdded`×2+`OverlaySet`; new waves insert as Draft; Draft→planning auto-promotion happens later at plan-upsert. | **FIXED.** Verified `routes/waves.rs:542-549` (the 4 create events, NO `lifecycle_changed`), Draft seed `db/sqlite.rs:738-744`, `auto_promote_draft_in_tx` at plan-upsert `mcp_server/tools/plan.rs:807`. Rewrote rows 1/2/5; row 1 is no longer a flip target; planning promotion moved into row 5. |
| 1 / B7 (durability) | Slice ① budget>1 hazard underplayed: scheduler counts in-flight with NO cwd/resource-disjointness check; cwd nullable. | **FIXED.** Verified `compute_ready` budget-arithmetic only (`scheduler.rs:118-145`) + cwd nullable (`0041_tasks.sql:10`). Added claim-time lease-acquirable guard to slice ① (§2.5-B + ① acceptance); budget>1 forbidden before the lease lands. |
| 1 / B8 (scope) | "Plugin exposes tools" is much bigger: iframe→kernel only, callbacks built-in only, `exposes_tools` metadata-only, worker/spec tools from a STATIC registry; outbound plugin MCP exists but unwired to worker-facing discovery. | **FIXED (re-scoped).** Verified `routes/plugins.rs:907-948` (neige.* hard-gate), `callbacks.rs:185-203` (built-in), `manifest.rs:72-74` (metadata-only), `mcp_server/tools/mod.rs:21-37`+`registry.rs:204-295` (static), `plugin_host/mcp.rs:498-522` (outbound, unwired). Re-scoped slice ② + §3-B3 to three deliverables: discovery + permissioning + routing (size M→L). |
| 1 / B9 (ownership) | Oracle row ownership not one-slice-clean: row 10 split, row 5 "enabled" in ① but flipped later, rows 13/14 overlap, row 15 overlaps ①. Adopt "exactly ONE slice FLIPS each row; enables ≠ acceptance". | **FIXED.** Adopted the rule; rewrote §4 with a flip-owner table (one owning slice per row). Row 10 is the single deliberate split (source vs dual-channel) — two disjoint, independently-testable sub-rows, not a violation. Substrate slices ②/④/⑥/⑦ FLIP no row; acceptance is a direct self-test. |
| 1 / B10 (nit) | Event-add procedure misses generated TS bindings + the metadata coverage test only checks fixtures (fixture list omits e.g. `TaskGateResult`). | **FIXED.** Verified `ts-rs` bindings `crates/calm-types/src/event.rs:329-334` + coverage test iterates `metadata_coverage_events()` `crates/calm-types/src/event.rs:1434-1444` and the fixture list (`~1988-2107`) omits `TaskGateResult` (contains only `TaskCompleted`/`TaskDispatched` in range). Added both to slice ③ files-touched (TS bindings + fixture-list entry). |
| 1 / B11 (nit) | `wave_vcs` diff impl is in `crates/calm-truth/src/wave_vcs.rs:688-703`; `crates/calm-server/src/wave_vcs.rs` is only a re-export — qualify. | **FIXED.** Verified `crates/calm-server/src/wave_vcs.rs` = `pub use calm_truth::wave_vcs::*;`. Qualified in §3-A6 and the row-10 cite. |

### ROUND 2 (dual-channel; v3 fold)

> **No round-1 regressions.** Both channels independently re-verified every round-1 fold against
> live code and confirmed all hold (channel A: "Round-1 regressions: 0"; channel B: "No round-1 fix
> regressed"). The round-2 findings are all properties of the NEW v2 material (the B1 forge-as-ops
> decision and the A1 failure branch) or incompletely-applied round-1 folds (B11/B10), not
> regressions. Verified at HEAD `b358b8f7`.

| round | finding | disposition |
|---|---|---|
| 2 / A-BLOCKER + B-SF4 (citation class) | Every bare `event.rs:NNN` cite resolves to `crates/calm-server/src/event.rs`, a 5-line `pub use` shim; the real enum/`kind_tag`/`metadata`/`topics`/`SYNC_EVENT_VERSION`/fixtures/ts-rs all live in `crates/calm-types/src/event.rs` (numbers correct for THAT file). Same B11 re-export rule applied to `wave_vcs` but missed for `event.rs`. | **FIXED.** Verified the shim (`crates/calm-server/src/event.rs` = `pub use calm_truth::event_bus::*;` + `pub use calm_types::event::{…}`) and the real anchors in `crates/calm-types/src/event.rs` (`SYNC_EVENT_VERSION` :327, `kind_tag` :958, `metadata` :788, `topics` :1035, `from_kind_and_payload` :1016, fixtures :1988, ts-rs derive :342). Global-replaced ALL bare `event.rs:` cites → `crates/calm-types/src/event.rs:`; added the rule to the header note + slice ③ files-touched ("do NOT edit the shim"). |
| 2 / B-SF1 (architectural) | The §2.5-A forge-as-`ProviderAdapter` fold collides with "kernel stays git-free": `ProviderAdapter` impls are compiled-in kernel-crate types (`build_operation_adapters` `state.rs:350`) with NO plugin-provided-adapter path (outbound `plugin_host/mcp.rs` `tools_call` is fire-and-forget RPC, not an op), so a `forge_action_adapter` with `gh`/`git` argv lives in calm-server — FALSIFYING §5-Q2's `[RESOLVED] kernel git-free`. | **DESIGNED-IN (decision, not text tweak).** Verified `build_operation_adapters` (`state.rs:350`, concrete kernel types) + `dispatcher_operation_runtime` (`dispatcher.rs:158`, 10 adapters) + no plugin-adapter seam (`plugin_host/mcp.rs:507` `tools_call` is RPC). **Decision:** the kernel hosts a **THIN, workflow-agnostic exec adapter** ("run argv, park, recover via the supplied probe argv"); the git/gh SEMANTICS (verb taxonomy, which argv, merge-state predicate) are **supplied by the plugin in the op PAYLOAD** — no workflow git logic compiles into the kernel. §2.5-A points 1/4/5 rewritten (payload carries `argv`/`RecoverSpec`/`idem_key`; verb enum lives in the plugin); §5-Q2 changed from `[RESOLVED]` to `[DECISION — NOT git-free]` with the v2 "git-free" claim explicitly RETRACTED; §0 moat table row + ASCII diagram + §2 "Where git/forge lives" reconciled. |
| 2 / A-S1 (paired with SF-1) | "Kernel knows nothing about git" is already false — `routes/fs.rs` shells `git` in the kernel. | **FIXED.** Verified `crates/calm-server/src/routes/fs.rs:552-559` (`git_root` → `git rev-parse --show-toplevel`; `git_output` :567). Narrowed the moat to "no WORKFLOW git in the kernel" in §0/§2/§2.5-B/§5-Q2, each acknowledging fs.rs so a reviewer doesn't catch it as a contradiction. |
| 2 / B-SF2 (failure-path, architectural) | Oracle row 17's ratify→`blocked` mutual-exclusion is FSM-unsound: `blocked` is a PAUSE not terminal (`blocked→working` `wave_lifecycle.rs:278`), so a granted-then-reconverged run emits BOTH `review.round(converged:false,n==cap)` AND the merge tail, and the §6 "merged absent for the whole run" invariant would FAIL a legal run. | **DESIGNED-IN (decision).** Verified `blocked→working` :278, `working→reviewing` :271, `reviewing→done` :273 (blocked IS resumable). **Decision:** distinguish TWO failure sub-terminals — cap-exhausted GIVE-UP → `reviewing→failed` (terminal, whole-run merge-tail absence sound) vs awaiting-human ASK-HUMAN → `working→blocked` (PAUSE, resumable). Restated the §6 invariant as the **temporal** "no `forge.pr.merged` while the LATEST `review.round` has `converged:false`" (not "absent for whole run"). Rewrote oracle row 17, the "Two backbone branches" + CONVERGENCE-FAILURE backbone summary, §3-C "Convergence-FAILURE", §6 "Cap enforcement", §6 FSM-legal end-state, slice ⑤ scope/acceptance, and the §6 harness line. |
| 2 / B-SF3 (wire) | Event-add procedure prescribes a per-variant `from_kind_and_payload` arm, but that fn (`event.rs:1016`) is GENERIC serde, not a per-variant match — no arm to add. | **FIXED.** Verified `from_kind_and_payload` (`crates/calm-types/src/event.rs:1016`) is `serde_json::from_value` over a synthesized `{ev,data}` envelope — a new variant is handled automatically. Removed the `from_kind_and_payload` step from §3 A8/A9 + slice ③ files-touched + the header note; kept the real per-variant steps (`kind_tag`/`metadata`/`topics`) + TS bindings + metadata_coverage fixture entry. |
| 2 / A-S2 + B-N1 (mechanism) | §2.5-C stage 5 says a missing `to_turn_text` arm "panics" — but `to_turn_text` is EXHAUSTIVE (no `_`), so a missing arm is a COMPILE error (a safety feature), not a runtime panic; could mislead ⑦'s acceptance toward a panic test. | **FIXED.** Verified `to_turn_text` (`crates/calm-types/src/observation.rs:95`) is `match self` with no `_` arm. Reworded §2.5-C stage 5 + the §2.5-C "gap" bullet + slice ⑦ files-touched to "a missing arm fails to COMPILE (the type system forces the branch); do NOT write a runtime panic-path test." |
| 2 / A-S3 (slice consistency) | Slice ⑦ has circular acceptance: "no deps" + "no events of its own" yet its self-test needs an event only ①/③/⑤ define. | **FIXED.** Reframed ⑦ to own the observation/recovery PATTERN + shared 6-stage plumbing; each event-defining slice wires its OWN §2.5-C arms. ⑦'s end-to-end self-test RIDES the first event-defining slice — ①'s `workspace.leased` (① has no deps, lands earliest). Added a `(deps: ① for the first carrier event)` note + a ⑦←① carrier edge to the dependency graph (still acyclic; topo order ①→②→⑦→⑥→③→④→⑤). |
| 2 / A-S4 (consistency) | The per-gap "rows flipped" lists and the slice-③ acceptance use different groupings (A5 row 8 / A6 row-10-source / A8-A9 rows 2,3,9,13,14,15,16) — hard to audit (4-way merge). | **FIXED.** Added a "Gap→slice aggregation note" under §4-③ acceptance making the A5/A6/A8-A9 → ③ aggregation explicit; flagged the flip-owner table as authoritative. |
| 2 / A-S5 (consistency) | §3-A4 "Oracle rows flipped" names ⑥/③ in the same breath as "row 7 owned by slice ①", reintroducing the enables-vs-flips ambiguity; the flip-owner table correctly gives row 7 to ① alone. | **FIXED.** Rewrote §3-A4's rows-flipped clause: row 7 is owned by ① ALONE (lease + cwd + budget guard fully satisfy its invariant; no git in the assertion); the git-on-lease layering (③/⑥) is substrate ① unblocks, NOT a co-flipper. Moved the ⑥/③ mention to ①'s Foundation note. |
| 2 / A-N1 + B-(drift) | Row 1 event-vec cite `routes/waves.rs:542-549` clips the `vec![` opening at `:539`. | **FIXED.** Re-cited `routes/waves.rs:539-550` (vec opens :539; `WaveUpdated` :542, `CardAdded`×2 :547,548, `OverlaySet` :549). |
| 2 / A-N2 (drift) | `event_warrants_spec_push_with_role` cited `dispatcher.rs:62-95`; the fn starts at `:70` (`:62` is the `event_warrants_spec_push` wrapper). | **FIXED.** Re-cited the fn at `dispatcher.rs:70` (`_ => false` at `:93`) throughout §2.5-C + slice ⑦. |
| 2 / A-N3 + B-N (drift) | `claim_parked_for_boot` cited `mod.rs:650-699`; the fn is at `:743` (`set_parked` `:682`). | **FIXED.** Re-cited `claim_parked_for_boot` `operation/mod.rs:743`, `set_parked` `:682` in §2.5-A/§2.5-B/§3-A4. (Channel A's `:676` was itself stale; B's `:743` confirmed against code.) |
| 2 / A-N4 (drift) | Scheduler `Running => {}` cited `scheduler.rs:917-918`; the empty arm is `:918`, comment `:915-917`. | **FIXED.** Re-cited `scheduler.rs:918` (arm) + `:915-917` (comment) in §2.5-B/§3-A4. |
| 2 / A-N5 + B-N5 (wrong-file anchor) | `recover_on_boot` cited `mod.rs:1030-1063`, which is a `#[cfg(test)]` fixture; the real fn is `operation/driver.rs:240` (called from `lib.rs:124`). | **FIXED.** Re-cited `recover_on_boot` `operation/driver.rs:240` (+ `lib.rs:124`) in §2.5-A + slice ①; noted the test fixture is at `mod.rs:1730`. |
| 2 / A-N5/A (drift) + N-6 | `register_default_tools` cited `tools/mod.rs:21-37` (fn at `:29`); subscription-filter test `:1256-1271` (filter at `:1258`); filter kinds vec confirmed `:637`. | **FIXED.** Re-cited `register_default_tools` `mcp_server/tools/mod.rs:29` (3 sites: §2, §3-B3, slice ②); the filter test `dispatcher_filter_matches_push_kinds` `:1257`/`SubscribeFilter` `:1258`; the prod kinds vec `dispatcher.rs:637` in §2.5-C + slice ⑦. |
| 2 / B-N2 (cost) | "one `SYNC_EVENT_VERSION` bump per event" over-states cost — v3 added TWO kinds in ONE bump (version history `event.rs:303-326`). | **FIXED.** Reworded §3 A8/A9, slice ③, and §5-Q5 to "ONE bump per shipping RELEASE, not per event"; ③'s ~6 + ⑤'s ~3 are at most two bumps. |
| 2 / B-N3 (precision) | "`ActorId::Plugin(_)` is unrestricted" overstates — kernel-only event gates reject Plugin (`NotKernelForTaskDispatched`/`GateResult`) and lifecycle rejects Plugin (`Other`). | **FIXED.** Softened the §2 role-gate bullet to "unrestricted at the per-card role gate" with the two carve-outs (kernel-only kinds `role_gate.rs:224-234`/`:254-264`; lifecycle `Other`); updated the ASCII diagram annotation. |
| 2 / B-N4 (drift) | Pervasive 30–90-line drift: Phase `:223`→269, ProviderAdapter `:491`→559, `set_parked` `:615`→682, `claim_parked_for_boot` `:650`→743, `event_warrants_spec_push` `:62`→70. | **FIXED.** Re-anchored §2.5-A (Phase `:269`, ProviderAdapter trait `:559`, `compensate_step` no-tx `:621`, `set_parked` `:682`) + added the header note "cites valid as of `b358b8f7`; prefer the named symbol if a line has drifted" with a symbol anchor alongside each line. |
| 2 / B-N1 (filesystem note, NOT a doc defect) | The doc + round-1/2 review outputs exist only as untracked/stash blobs, not in the working tree — cites are un-followable until the doc is restored. | **NOTED (environment, not doc).** Out of scope for the doc content; the doc is restored in the working tree as `docs/_760-design-v1.md` and co-lands with slice ①. Recorded as a residual environment caveat. |

**Round-2 rejections (findings rejected with evidence): NONE.** Every channel-A and channel-B
round-2 finding verified TRUE against live code at HEAD `b358b8f7` and was folded (the two
architectural finds SF-1/SF-2 as DECISIONS, the rest as fixes). The only cross-channel nuance: A-N3
cited `claim_parked_for_boot` at `:676` while B cited `:743`; **B was correct** (verified
`grep -n` → `:743`), so the doc uses B's anchor. Channel B's filesystem note (B-N1) is an
environment caveat, not a doc defect (recorded above, not rejected).

### ROUND 3 (dual-channel; v4 fold)

> **No round-1/2 regressions (BOTH channels confirm).** Channel A re-verified every round-1/2 fold
> at HEAD `b358b8f7` ("Round-1/2 regressions: 0"; 18-row regression table all ✓); channel B's
> blockers are all properties of the NEW v3 §2.5-A material (the forge-as-ops decision), not reverts.
> **Both-channel agreement on C4/SF-B** (the ASK-HUMAN FSM trace is incomplete: no `reviewing→blocked`
> edge) and **on C5/N-5** (the merge fence needs a subject key). **C3 escalation:** channel B
> (C3, BLOCKER) and channel A (SF-A, SHOULD-FIX) independently flagged the v3 `RecoverSpec.predicate`
> JSON-DSL as hand-waved/under-specified; both proposed the SAME fix (exit-code recovery), which the
> systemic reframe adopts (DSL deleted). Verified at HEAD `b358b8f7`.
>
> **The systemic fix: C1+C2+C3 are ONE root cause, fixed together (not patched separately).** v3's
> §2.5-A "generic thin exec adapter recovered generically by the kernel" does not fit the operation
> framework. §2.5-A is reframed to a **CONCRETE `ForgeActionAdapter` modeled exactly on
> `TaskVerifyAdapter`** (the framework's only proven durable-side-effect pattern): **C1** (pre-park
> irreversibility) fixed by a **held-handshake spawn** (argv held at a stdin go-token; artifacts
> recorded + parked BEFORE release; a pre-park crash EOFs stdin → launcher exits 75 → action un-run —
> copying `task_verify_adapter.rs:328,921,935`); **C2** (no generic result/event contract) fixed by a
> **custom `complete_forge_op_with_result`** atomic helper emitting the typed forge event in the
> op-flip tx (copying `complete_gate_op_with_result` `:263`); **C3** (no JSON-predicate wire format,
> no evaluator in the tree) fixed by **exit-code recovery** via `verdict_from_exit_code` `:408`
> (plugin-supplied `probe_argv` exits 0 ⇒ landed) with the v3 `RecoverSpec.predicate` JSON-DSL
> **DELETED** — preserving the SF-1 moat (argv/`probe_argv` are plugin payload; the adapter is generic
> exec+exit-code). Honest framing added: "forge as operation" = a concrete compiled-in kernel adapter
> per the task-verify precedent (like the existing 10), NOT a generic plugin-recovered seam (none
> exists). **0 regressions introduced.**

| round | finding | disposition |
|---|---|---|
| 3 / C1 (B-BLOCKER) | forge-action not crash-safe in the pre-park window: recovery runs only after `Phase::Parked`; a crash after `gh pr create/merge` but before `set_parked` commits restarts at `SpawnStarted` and re-runs `spawn_side_effect`, re-executing the irreversible action (idempotency key doesn't help — not a resubmit). (`driver.rs:340,425,905`) | **DESIGNED-IN (systemic, with C2/C3).** Verified the `SpawnStarted` recovery path (`driver.rs:425`), the Ready→Succeeded flip (`:340`), and that the held-handshake mechanism exists in `task_verify_adapter.rs` (`read -r _go || exit 75` :328; artifacts recorded under 60s `RELEASE_TIMEOUT` :75/:935 BEFORE go-token release :921/:928; `set_parked` requires artifacts + spawns observer only after park commits `driver.rs:456-457`). **Fix:** §2.5-A `spawn_side_effect` now does a HELD-HANDSHAKE spawn — spawn argv held at stdin go-token, record artifacts + park, ONLY THEN release; a pre-park crash EOFs stdin → launcher exits 75 → irreversible action NEVER ran. §2.5-A point 4 rewritten; slice ⑥ adds a pre-park-crash acceptance test; §0/§2/§5-Q2 reframed; residual_risk (non-idempotent verbs, tmp+rename result file, exec-to-reach-SIGKILL) recorded. |
| 3 / C2 (B-BLOCKER) | operation framework lacks the generic result/event contract §2.5-A assumed: `SpawnOutcome::Ready` carries no result; the generic Ready path just flips Succeeded; `complete_parked_tx` updates only the op row; task-verify gets atomic verdict+event via a CUSTOM helper, not the generic driver. (`mod.rs:242, driver.rs:340, task_verify_adapter.rs:259`) | **DESIGNED-IN (systemic, with C1/C3).** Verified `SpawnOutcome::Ready(SpawnHandle)` carries no result (`operation/mod.rs:242-243`); the generic Ready path flips `Phase::Succeeded` (`driver.rs:340`); `complete_gate_op_with_result` (`task_verify_adapter.rs:263`) composes `complete_parked_tx` (`:275`) + `apply_gate_result_in_tx` (`:176`, emits `Event::TaskGateResult` `:214-224`) in ONE tx. **Fix:** §2.5-A point 6 now ships a custom `complete_forge_op_with_result` atomic helper (copy of the gate helper) emitting the typed forge event in the op-flip tx — NOT the generic Ready/`complete_parked_tx` path. §2.5-A "v4 design" + §5-Q2 + slice ⑥ updated. |
| 3 / C3 (B-BLOCKER) + SF-A (A-SHOULD-FIX, escalation) | `RecoverSpec.predicate` "predicate over JSON stdout" is not a wire format (exit-code/stderr/malformed-JSON/array-match/timeout all undefined) and there is NO JSON-predicate evaluator in the tree; it diverges from the one real precedent (`task_verify_adapter::recover_parked`, exit-code-only). Without a bounded DSL the kernel can't evaluate recovery generically. (`mod.rs:596`; A grep: only Rust closures) | **DESIGNED-IN (systemic, with C1/C2).** Verified `recover_parked` receives `_ctx: &SpawnCtx` (`operation/mod.rs:596-611`) so an adapter CAN shell a probe; verified `verdict_from_exit_code` (`task_verify_adapter.rs:408`) + `read_exit_file` (`:397`) are the only proven recovery shape (exit-code). **Fix:** the v3 `RecoverSpec.predicate` JSON-DSL is **DELETED**; recovery is exit-code-based — the plugin's `probe_argv` exits 0 iff the action landed (`gh pr view <n> --json state -q '.state=="MERGED"'`), and the adapter applies `verdict_from_exit_code`. ZERO JSON-predicate logic enters the kernel; the merge-state semantics stay entirely in the plugin's probe argv (moat preserved). §2.5-A points 1/5 rewritten; §0/§2/§5-Q2 reframed. Both channels proposed this same exit-code fix. |
| 3 / C4 = SF-B (BOTH channels) | oracle row 17 ASK-HUMAN path is an incomplete FSM trace: cap-exhaustion is detected in `reviewing`, but there is NO `reviewing→blocked` edge; the doc cited only the single `working→blocked` edge as if already in `working`. (`wave_lifecycle.rs:270,272`) | **FIXED.** Verified `grep 'Reviewing, WaveLifecycle::Blocked'` → none; `reviewing→working` :272 + `working→blocked` :270 both legal; `reviewing→failed` :274 (GIVE-UP) stays direct. **Fix:** the ASK-HUMAN sub-path is now the TWO-edge `reviewing→working` (`:272`) THEN `working→blocked` (`:270`) — added to row 17, the §1 "two backbone branches" + CONVERGENCE-FAILURE backbone summary, §3-C, §6 (new ordering invariant 9 + FSM-legal end-state), and slice ⑤. GIVE-UP stays `reviewing→failed`. Both channels independently found this. |
| 3 / C5 = N-5 (B + A corroborate) | "latest review.round for the wave" can pass an illegal merge with multiple review subjects (a later converged round for PR B clears PR A's unconverged fence); a design-round and a per-PR round share the kind. (`0004_events.sql:17`) | **FIXED.** **Fix:** `review.round` (and the merge fence) now carry a SUBJECT KEY `{phase, slice_id, pr_number, head_sha}` in BOTH payloads; the §6 invariant is restated as "no `forge.pr.merged` for subject S while the latest `review.round` FOR SUBJECT S has `converged:false`" (evaluated per-subject; the events table `0004_events.sql:17` has no subject column, so it lives in the payload). Updated §1 preamble, rows 6/11/17, §3-C, §6 cap-enforcement + ordering invariant 2 (phase distinguishes design vs per-PR), §5-Q5 reasoning, slice ⑤, and the `review.round` event definition. Channel A's N-5 (phase-scope) is the same defect, folded here. |
| 3 / C6 (B-SHOULD-FIX) | wave scope alone is insufficient for WS topic routing: `topics(&Event)` does NOT receive `EventScope`, and replay filters by `topics(&ev)` before rendering; a `review.round{…}` without `wave_id` in payload routes only via `"*"`. (`ws/events.rs:333, event.rs:1035`) | **FIXED.** Verified `topics(ev: &Event)` (`crates/calm-types/src/event.rs:1035`) has no `EventScope` param; WS replay filters `topics(&ev)` BEFORE rendering (`crates/calm-server/src/ws/events.rs:333`). **Fix:** every NEW spec-facing/forge event MUST carry `wave_id` (+ subject ids) in its PAYLOAD and add an explicit `topics()` arm emitting `wave:<id>` from the payload. Stated in §2.5-C stage 1, the §3-A8/A9 event-add procedure, and each new event's definition (rows 6/11/17, `review.round` + forge events). |
| 3 / C7 (B-SHOULD-FIX) | §5-Q5 open fork (envelope vs distinct variants) should close in favor of DISTINCT ENUM VARIANTS for oracle-significant forge/review events; an envelope hides facts behind payload parsing and weakens replay/query/oracle. (`event.rs:340,788,958`) | **DESIGNED-IN (decision).** Verified the typed-enum spine: tagged enum (`event.rs:340`), `metadata()` (`:788`), `kind_tag()` (`:958`), `topics()` (`:1035`), ts-rs union (`:342`). **Decision:** §5-Q5 moved from OPEN to DECIDED — DISTINCT TYPED VARIANTS per oracle-significant forge/review event (NOT a `forge.event{kind}` envelope). Reasoning recorded (typed arms + exact `events.kind` filter + TS union + per-variant `metadata()`/`topics()` + kind-keyed oracle; version bump is per-release not per-event, so the envelope buys nothing). §3-A8/A9 + §5-Q5 updated. |
| 3 / N-1 (A-nit) | `tools_call` is request/response RPC, not "fire-and-forget" (`mcp.rs:507` awaits `CallToolResult`). | **FIXED.** Verified `plugin_host/mcp.rs:507-522` does `self.call("tools/call", params).await?` and returns a parsed `CallToolResult`. Replaced "fire-and-forget RPC" → "request/response RPC" in §0 moat note, §2.5-A registration bullet, §5-Q2 (the "not an operation" point is what's load-bearing and intact). |
| 3 / N-2 (A-nit) | `dispatcher.rs:158` register cite is one line off — fn is `:160`, vec `:244-255` (`:158` is a brace). | **FIXED.** Verified `:158` = `}`, `fn dispatcher_operation_runtime` `:160`, adapter vec `:244-255` (exactly 10 adapters). Re-cited the fn `:160` / vec `:244-255` in §2.5-A registration bullet + slice ⑥ files-touched. |
| 3 / N-3 (A-nit) | the moat's lease-vs-fs.rs claim is slightly overstated: fs.rs git is read-only; the forge adapter does mutating git/gh — not the same risk class. | **FIXED.** Verified `git_root`/`git_output` (`routes/fs.rs:552,567`) are `rev-parse`/browse (read-only). Added one sentence to the §0 moat note: fs.rs git is read-only, the forge adapter is mutating, which is exactly why the forge adapter must be a crash-safe OPERATION (sharpens, doesn't change, the moat argument). |
| 3 / N (codex_adapter drift) | `RuntimeStatusChanged` cited `codex_adapter.rs:1481,1585`; actual emit sites are `:1486,1590` (the cited lines are `.await?` above each emit). | **FIXED.** Verified `Event::RuntimeStatusChanged` at `codex_adapter.rs:1486` and `:1590`. Re-cited row 8. |
| 3 / N (FSM rules-table) | `wave_lifecycle.rs:30-44` is the module-doc edge list, not the logic; the live match arms are `:252-278` inside `validate_transition` `:170-295`. | **FIXED.** Verified `:30-44` is the doc-comment edge table, `match (from, to)` at `:247`, arms `:252-278`, `validate_transition` `:170-295`. Clarified the three load-bearing cites (§0 "why tractable", §1 FSM-legality, §6 FSM-legal) to "module-doc edge list :30-44; live match arms :252-278; validate_transition :170-295". |
| 3 / NIT (B) | no-plugin-adapter-seam claim is accurate (confirms). | **CONFIRMED, no change.** Channel B re-verified that `build_operation_adapters` returns concrete kernel types and the only kernel→plugin reach (`tools_call`) is not an operation — the no-plugin-adapter-seam claim (load-bearing for the concrete-adapter reframe) holds. |

**Round-3 rejections (findings rejected with evidence): NONE.** Every channel-A and channel-B
round-3 finding verified TRUE against live code at HEAD `b358b8f7` and was folded — the three
blockers C1/C2/C3 as ONE systemic DECISION (concrete `ForgeActionAdapter` ≅ `TaskVerifyAdapter`),
C4–C7 as decisions/fixes, the nits as cite/wording fixes. Channel A's SF-A is the same finding as
channel B's C3 (both proposed exit-code recovery); channel A's N-5 is the same as C5 (subject key).
Both channels independently found C4 (the missing `reviewing→blocked` edge). 0 regressions introduced
by the v4 fold (every v1/v2/v3 fold spot-checked still holds at `b358b8f7`).

**Round-1 rejections (findings rejected with evidence): NONE.** Every channel-A and channel-B
finding verified TRUE against live code (file:line above). The only partial-pushback is A6 vs B9:
channel A's "round-0 patch is clean" was correct on *coverage* (no orphan rows) but B9 was
correct that "enables" was mislabeled as acceptance — reconciled toward B9, not a rejection of
either.

### ROUND 4 (dual-channel; v5 fold)

> **Channel divergence recorded honestly.** Channel A (correctness/completeness/consistency, fresh
> subagent) verdict: **CONVERGED — blocker-free, should-fix-free**, with only 3 nits (the two
> pre-known citation nits + a fresh same-class per-event `wave_id`-shorthand consistency nit). Channel
> B (codex, failure-path/operation-framework lens) found **4 REAL BLOCKERS in the forge-action
> contract** (R4-1..R4-4). **Both lenses are valid:** A's coherence/faithful-copy spot-checks all
> passed (the v4 doc *does* faithfully describe `TaskVerifyAdapter`), but B's deeper operation-
> framework lens caught that a faithful copy of a *resultless idempotent gate* is the WRONG contract
> for an *irreversible-with-typed-output* forge action — a defect invisible to a copy-fidelity check.
> This is exactly the systemic-root-cause discipline: R4-1/2/3 are ONE root cause (the forge-action
> operation needs its own exactly-once completion/recovery/result contract), redesigned together in
> §2.5-A; R4-4 is the orthogonal subject-key soundness fix. **Grounded re-anchor confirmed at HEAD
> `b358b8f7`:** post-park release is NOT currently feasible (release happens inside `record_release`
> before the observer is built; `ParkedObserver` takes no params; the `SpawnStarted` re-drive re-runs
> `spawn_side_effect`), so R4-1 is designed to the real constraint as a small framework addition slice
> ⑥ lands, NOT hand-waved as a copy. **0 regressions** of any round-1/2/3 fold (channel A's 18-row
> spot-check + the round-4 re-anchor both hold at `b358b8f7`).

| round | finding | disposition |
|---|---|---|
| 4 / R4-1 (B-BLOCKER) | Pre-park release window NOT closed: v4 copied task-verify's release-BEFORE-park ordering; a crash after go-token release but before `set_parked` commits leaves the op in `SpawnStarted`, which boot maps to generic re-drive (re-runs `spawn_side_effect`) NOT `recover_parked` → irreversible `gh pr merge` runs twice. (`task_verify_adapter.rs:929/961`, `driver.rs:430/456/914-918/947`) | **DESIGNED-IN (systemic, with R4-2/R4-3).** Verified at HEAD `b358b8f7`: the go-token release (`stdin.write_all(b"go\n")` :929) completes inside `record_release` (:922-934) BEFORE the observer closure is built (:961, captures already-released `child` by move); `set_parked` commits only after (`driver.rs:456`, observer spawned :457); `SpawnStarted` boot → `plan_recovery_for`→`Recover` (:914-918)→`drive_one` (:947)→re-runs `spawn_side_effect` (:430). **Fix:** the held go-token is released from the **POST-PARK owner (the observer)** so nothing irreversible runs until durably parked; a pre-park crash leaves the action un-run and the `SpawnStarted` re-drive is safe. **Honest scope:** the observer can't own the release today (`ParkedObserver` `mod.rs:244` takes no params; stdin is dropped at spawn), so §2.5-A point 4 specifies a small workflow-agnostic FRAMEWORK addition (stdin-into-observer handoff OR a `SpawnOutcome::ParkedDeferredRelease` variant) that **slice ⑥ lands** — bigger than a copy, recorded as such. §2.5-A intro+point 4 rewritten; slice ⑥ scope/acceptance/size (M→L) updated; §0/§2/§5-Q2 reframed. |
| 4 / R4-2 (B-BLOCKER) | Ready shortcut can't emit events: v4 let read-only checks/scan/diff use `SpawnOutcome::Ready`, but `Ready(SpawnHandle)` carries no result and the driver just flips `Succeeded` — can't emit `forge.pr.checks`/`forge.scan.completed`/`forge.pr.diff.read` atomically. (`mod.rs:242-243`, `driver.rs:340`) | **DESIGNED-IN (systemic, with R4-1/R4-3).** Verified `Ready(SpawnHandle)` carries no result (`operation/mod.rs:242-243`); generic Ready path flips `Phase::Succeeded` (`driver.rs:340`). **Fix:** ALL oracle-visible forge actions (incl. read-only checks/scan/diff) MUST use the parked-completion helper; `Ready` is reserved for truly resultless/non-oracle actions. §2.5-A point 4 (R4-2 clause) + point 6 + §3-A6 (diff no longer `Ready`) + §3-A8/A9 + slice ⑥ acceptance updated. |
| 4 / R4-3 (B-BLOCKER) | No typed result/event wire contract: `complete_forge_op_with_result` must emit TYPED events, but exit-code-only (v4) can't carry OUTPUTS (`forge.pr.opened{pr_number}`, `forge.pr.merged{merge_sha}`); a fully-generic kernel can't pick the typed variant without baking verbs in (reopens SF-1) or a bounded typed result spec in the payload. | **DESIGNED-IN (systemic, with R4-1/R4-2).** Verified (anchor): task-verify builds `Event::TaskGateResult` from a deterministic verdict struct in the completion tx (`apply_gate_result_in_tx` :176, fields :214-224) — typed-event construction at completion is feasible. **Fix:** a **BOUNDED typed result-extraction** wire contract — typed event VARIANTS are enum arms in shared `calm-types` (data shapes, no logic — per C7); the plugin payload carries `{target typed event kind, a bounded `ForgeEventSpec` extractor (exit-code | named stdout field paths over the action's `--json`), recovery probe argv}`. The kernel adapter runs argv, applies the bounded extractor, builds the named typed variant, emits it via the parked-completion helper. **SF-1/C7 tension resolved explicitly:** typed event DATA shapes are the issue-dev workflow's contribution to the shared event enum (definitions, no logic); NO git/gh verb-EXECUTION logic compiles into the kernel (argv + extractor spec are plugin-supplied DATA). This **replaces v4's exit-code-only** (too weak for output fields) **WITHOUT resurrecting v3's unbounded JSON-predicate DSL** (bounded = exit-code | named field paths only; no booleans/expressions/array logic). The exact bounded extractor grammar = slice ⑥, reviewed at impl. §2.5-A point 1/5/6 rewritten; §0/§2/§5-Q2/§5-Q5/§3-A8-A9 reframed; slice ⑥ scope/acceptance updated. |
| 4 / R4-4 (B-BLOCKER) | Subject key unsound: `{phase,slice_id,pr_number,head_sha}` as the grouping key means an old unconverged head stays "latest" forever; a later converged head never supersedes it. | **FIXED.** **Fix:** the subject is the **LOGICAL key `{phase,slice_id,pr_number}`**; `head_sha` is the reviewed **REVISION** (a field, not part of the grouping key). With the v4 key, each head_sha was its own singleton subject so an old unconverged head stayed "latest" forever and a later converged head (a different subject) never superseded it — the fence could never clear. With the logical key, all revisions of a PR share one subject, so a later CONVERGED revision supersedes an earlier unconverged one. Restated the §6 invariant: 'no `forge.pr.merged` for subject S unless merge head == the latest CONVERGED revision for S'. Updated §1 preamble, rows 6/11/17, §1 CONVERGENCE-FAILURE backbone, §3-C, §6 cap-enforcement + new ordering invariant 5b, the `review.round` event def (§3-C + slice ⑤), and slice ⑤ store/acceptance. |
| 4 / channel-A Nit-1 (citation) | `0004_events.sql:17` is a COMMENT line; the real `CREATE TABLE events` DDL (no subject/dedupe column) is at `:23-32`. | **FIXED.** Re-cited the §1 preamble events-table cite from `0004_events.sql:17` to `:23-32` (the substantive "no subject column" claim was always true; the line now points at the DDL). The round-3 disposition row's `:17` is left verbatim as historical record. |
| 4 / channel-A Nit-2 (citation) | The §2.5-A point-4 spawn_artifacts guard string `WHERE spawn_artifacts_json IS NOT NULL` was paired with `driver.rs:456` (the call site) + `mod.rs:682` (fn head); the guard clause is in `set_parked` at `operation/mod.rs:700`. | **FIXED.** Re-cited the guard clause at `operation/mod.rs:700` (fn opens :682), with `driver.rs:456` named as the call site and `:457` as the observer spawn, in the §2.5-A point-4 rewrite. |
| 4 / channel-A Nit-3 (consistency) | The C6 blanket rule (every NEW forge/review/ratify event carries `wave_id` in payload) was authoritative, but the per-event payload shorthands (rows 3/9/10/14/15/16) didn't show `wave_id`, so a reader implementing one event from its shorthand alone wouldn't see the requirement co-located. | **FIXED.** Normalized every per-event forge shorthand to show `wave_id` (and, for merge/review, the subject + `head_sha` revision per R4-4): `forge.scan.completed{wave_id,…}` (row 3), `forge.pr.opened{wave_id,…}` (row 9), `forge.pr.diff.read{wave_id,…}` (row 10), `forge.pr.checks{wave_id,…}` (row 14), `forge.pr.merged{wave_id, subject, head_sha, merge_sha}` (row 15), `forge.issue.closed{wave_id,n}` (row 16). The C6 blanket rule remains authoritative; the shorthands now co-locate it. |

**Round-4 rejections (findings rejected with evidence): NONE.** Every channel-B blocker (R4-1..R4-4)
verified TRUE against live code at HEAD `b358b8f7` and was folded — R4-1/2/3 as ONE systemic DECISION
(the purpose-built forge-action exactly-once contract: post-park release + parked-completion-for-all-
oracle-actions + bounded typed result-extraction), R4-4 as the logical-subject-key fix. Channel A's 3
nits (2 pre-known citations + the fresh `wave_id`-shorthand consistency nit) were all confirmed
accurate and folded. **The A=converged / B=4-blockers divergence is the headline:** channel A's
copy-fidelity lens correctly passed every "is this a faithful `TaskVerifyAdapter` copy?" check, but
the copy ITSELF was the wrong contract — caught only by channel B's deeper operation-framework lens
(resultless-idempotent gate ≠ irreversible-typed-output action). Recorded so future rounds keep both
lenses. 0 regressions introduced by the v5 fold (every v1/v2/v3/v4 fold spot-checked still holds at
`b358b8f7`).

### ROUND 5 (dual-channel; CONVERGED) — 2026-06-18

> **Both channels CONVERGED — design APPROVED at contract altitude.** Channel B (codex,
> failure-path/operation-framework lens): **0 blocker / 0 should-fix / 1 nit**, verdict
> **"CONVERGED AT CONTRACT LEVEL"** — all four round-4 blockers (R4-1..R4-4) confirmed resolved, 0
> regressions; the lone nit was the slice-⑥ acceptance wording (the `SpawnStarted` re-drive
> "recovery sees no landed action and does not re-run it" phrasing), reworded so the re-drive **does
> not re-run the prior instance because nothing irreversible ran before park** — folded. Channel A
> (correctness/completeness/consistency, fresh subagent): **0 blocker / 0 should-fix / 4 nits / 0
> regressions**, verdict **"CONVERGED YES"**. **Channel A's 4 nits are pure line-number drift on
> otherwise-correct symbol anchors** (the repo auto-syncs and HEAD advances, so exact line numbers
> drift run-to-run); they are **ABSORBED by the doc's existing "prefer the named symbol if a line has
> drifted" convention** (§-preamble) and are deliberately **NOT chased line-by-line** — chasing
> ephemeral line numbers is churn the convention exists to avoid. **Convergence recorded: the design
> is APPROVED at contract altitude;** the only two remaining items are **slice-⑥ implementation +
> review scope** (the post-park-release mechanism variant — stdin-into-observer handoff vs.
> `SpawnOutcome::ParkedDeferredRelease`; and the bounded `ForgeEventSpec` extractor grammar), not
> design gaps.

| round | finding | disposition |
|---|---|---|
| 5 / channel-B (codex) | **0 blocker / 0 should-fix / 1 nit. Verdict: "CONVERGED AT CONTRACT LEVEL".** All four round-4 blockers (R4-1 post-park release, R4-2 parked-completion-for-all-oracle-actions, R4-3 bounded typed result-extraction, R4-4 logical subject key) confirmed RESOLVED; 0 regressions. Lone nit: slice-⑥ acceptance wording — the `SpawnStarted` re-drive "recovery sees no landed action and does not re-run it" should say the re-drive does NOT re-run the prior instance because nothing irreversible ran before park. | **FOLDED.** Reworded the slice-⑥ R4-1 pre-park-crash acceptance sentence so the `SpawnStarted` re-drive "does NOT re-run the prior instance, because nothing irreversible ran before park (consistent with the post-park-release contract in §2.5-A)" — one clean sentence aligned with the post-park-release contract. No other change required; contract-level design unchanged. |
| 5 / channel-A (subagent) | **0 blocker / 0 should-fix / 4 nits / 0 regressions. Verdict: "CONVERGED YES".** The 4 nits are pure LINE-NUMBER drift on otherwise-correct symbol anchors (the repo auto-syncs and HEAD advances between rounds, so exact line numbers drift; the named symbols/functions are all correct). | **ABSORBED, deliberately NOT chased.** Per the doc's standing "prefer the named symbol if a line has drifted" convention (§-preamble), line-number-only drift on correct symbol anchors is covered by the symbol anchor and is NOT chased line-by-line — chasing ephemeral line numbers is exactly the churn that convention exists to avoid. 0 substantive findings; verdict CONVERGED. |

**Round-5 convergence.** Both channels converged: **0 blockers, 0 should-fix on both sides.** The
single channel-B nit (slice-⑥ wording) is folded; channel A's 4 nits are line-number drift absorbed
by the symbol-anchor convention. **The design is APPROVED at contract altitude.** Blocker trajectory
across all five rounds: **5→1→3→4→0** (the round-3/4 rise was the dual-channel drilling the
forge-action exactly-once core to ground, since resolved as the purpose-built post-park-release +
parked-completion + bounded-typed-extraction contract). The **two remaining open items are slice-⑥
implementation + review scope** — the post-park-release mechanism variant (stdin-into-observer handoff
vs. a `SpawnOutcome::ParkedDeferredRelease` variant) and the bounded `ForgeEventSpec` extractor
grammar — **not design gaps.** 0 regressions of any v1/v2/v3/v4/v5 fold.

<!-- Populate across dual-channel review rounds (fresh subagent + codex read-only) to convergence. -->
