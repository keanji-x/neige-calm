# Today Launchpad — SUBAGENT review r2 (targeted CONFIRM, code-grounded)

Round-2 confirm review of `docs/_today-launchpad-design.md` (r2) against the r1
subagent review (`docs/_today-launchpad-design-review-subagent.md`). NOT a full
re-review: (a) confirm r2 closes the r1 findings; (b) find NEW holes the r2
additions introduce. Every claim grounded in canonical trees (`crates/`, `web/`,
`plugins/`); `.claude/worktrees/*` ignored.

## (a) Verdict

**fix-again, then ship — 0 blockers, 5 major, 4 minor. No approach rethink.**
r2 **closes the r1 blocker** (B1): the survey/propose carve-out moves off
`visible_to_roles` onto a **handler-level fail-closed concierge gate**, which is
buildable — the `tools/call` handler receives the fully-resolved
`ToolCallIdentity { card_id, wave_id, cove_id, … }` (`transport.rs:610-612`,
`registry.rs:101-110`), so `is_concierge(identity)` is the real boundary. r1's
M1/M2/M3/m1/m5/m6 are all substantively addressed. The core skeleton
(propose-then-confirm, persisted marker, handler gate, `AiSpec` self-scope arm,
attach-time repo identity, POST dedup) is sound and I endorse it. But the r2
*additions* introduce five MAJOR precision/plumbing holes that must be fixed in
the doc before slicing — all Slice-local, none touching the approach: (1) the
marker must **not** live on the public `NewWave` DTO; (2) the marker closes only
*ensure-vs-ensure* — the legacy client `ensureTodayWave` still races; (3) the
`role_gate` arm **cannot** verify "is the launchpad card" (no marker source in
`enforce_role`) — it can only self-scope; (4) the dedup key is defeated unless the
**server** normalizes `repo` at write (today it stores `workflow_input` verbatim
and the schema delegates normalization to the client); (5) the proposal render is
more net-new than "reuse `NewTaskForm`" implies. Fix these five and it is
ready-to-slice.

---

## (b) Findings

### r1 → r2 closure confirmation (targeted)

| r1 finding | r2 response | Status |
|---|---|---|
| **B1** survey visible-only-to-concierge unbuildable via `visible_to_roles`; global read leaks to every spec | §0.2 + Slice B: handler-level fail-closed gate at `tools/list` **and** `tools/call` | **CLOSED-OK** (see NEW-4/NEW-7 refinements) |
| **M1** I1 "read-only" overclaim; concierge keeps full spec write authority | §3 I1 reworded → "one own-wave proposal write … blast radius bounded to system-cove wave by cove-confinement — state explicitly" | **CLOSED-OK** |
| **M2** concierge prompt selected where predicate data absent; running harness won't repick prompt | §0.1 + Slice A (server-owned create, prompt injected before thread start, migrate existing) + Slice E (marker-selected template) | **CLOSED-OK** (see RESIDUAL-A) |
| **M3** no actionable-artifact renderer; reuse `NewTaskForm` | §0 + Slice F: "confirm reuses `NewTaskForm`"; render each `LaunchpadProposal` as editable confirm card | **CLOSED-OK on create half** (see RESIDUAL-C) |
| **m1** concierge liveness unproven, hard dep for turn+write | Slice A "UI must handle dormant/start-failed (201 ≠ live)"; §6 oracle asserts commit + live harness | **CLOSED-OK** |
| **m2** Today wave not server-singleton; 2 tabs → 2 waves → >1 concierge | §0.4 persisted marker + transactional ensure | **RESIDUAL** → NEW-2 |
| **m3** git-remote = new kernel subprocess at tool-call time | Slice D: resolve/normalize once at attach, persist per-folder, shell-free, bounded | **CLOSED-OK** (see RESIDUAL-D) |
| **m4** new Event variant triggers full FE checklist; prefer card/overlay | §0 + Q-B: typed `LaunchpadProposal` chosen deliberately over card/overlay | **CLOSED-OK, choice sound** (see NEW-6 cost) |
| **m5** oracle missing load-bearing negatives | §6 "Must-add negatives/edges" enumerates denied-direct-call, forged-scope, zero-POST, dedup, concurrent-ensure, migration, redaction, dormant | **CLOSED-OK** |
| **m6** Today page can't reach the spec card | Slice A ensure returns `{wave_id, spec_card_id, terminal_card_id, terminal_id}` | **CLOSED-OK** |

Verified-right claims from r1 still hold in r2: I2 (POST re-validates
trusted-workflow/input-schema/cwd/FolderConflict — `waves.rs:344-467,494-531`),
I3's core (`AiSpec` is unscoped for non-enumerated events —
`role_gate.rs:413-459` `if let AiCodex|AiClaude` does NOT match `AiSpec`, falls to
`Ok(())` at `:466`), and `enforce_role_resolving_session` gating on active Spec
session (`decision_gate.rs:87,106-112`).

---

### MAJOR

#### NEW-1 — the launchpad marker must NOT be a field on the public `NewWave` DTO (Slice A)
- **Evidence.** `create_wave` deserializes the POST `/api/waves` body directly as
  `Json(mut p): Json<NewWave>` (`crates/calm-server/src/routes/waves.rs:317-320`).
  If the marker is added as a `NewWave.purpose` field, any client can POST
  `purpose:"launchpad"` into any cove. With the proposed global partial-unique
  index (`… ON waves(purpose) WHERE purpose='launchpad'`), the second such POST
  **500s a user action**; the first mints a rogue "launchpad" wave that the gate
  then treats as a concierge. The codebase's own precedent is the opposite:
  kernel-owned markers are stamped by a **dedicated server tx**, never the client
  DTO — `cove_create_system_tx` is a separate fn from `cove_create_tx`
  (`crates/calm-truth/src/db/sqlite/cove.rs:71`), and `CardRole::Spec` is stamped
  by the route, not by `NewCard`.
- **Fix.** Slice A must mint the marker via a dedicated `wave_create_launchpad_tx`
  (mirror `cove_create_system_tx`) invoked only from `POST /api/today/launchpad/ensure`.
  The marker is never on `NewWave`. The partial-unique index (feasible; exact
  precedent `idx_coves_one_system`, `migrations/0009_coves_kind.sql:34-35`) stays
  as the race backstop.

#### NEW-2 (RESIDUAL of r1 m2) — the marker closes only *ensure-vs-ensure*; the legacy client `ensureTodayWave` still races → duplicate Today waves (Slice A)
- **Evidence.** There is **no** unique index on `waves(cove_id,title)` — only
  non-unique `idx_waves_cove ON waves(cove_id, sort)` (`migrations/0001_init.sql:29`;
  grep of all migrations confirms). The "Today" wave is minted **only client-side**
  today: `ensureTodayWave` does `waves.find(w => w.title==='Today')` else
  `createWave({title:'Today', cwd:'/', …})` (`web/src/hooks/useTodayTerminal.ts:173-197`),
  a check-then-act with zero DB backstop. r2 adds `ensure` with a partial-unique
  index on `purpose='launchpad'` — but that index **never sees a legacy row whose
  `purpose` is NULL**. So if the still-live client path creates a title='Today'
  wave concurrently with (or before) `ensure`, you get two Today waves → the
  concierge predicate matches >1 → the exact m2 failure r2 claims to have closed.
- **Fix.** Slice A/F must additionally (a) **retire/redirect** `ensureTodayWave`
  so the terminal bootstrap consumes `ensure`'s returned `terminal_card_id`
  (r2 already returns it — finish the unification), and (b) **migrate** any
  pre-existing `title='Today'` wave by stamping the marker inside the ensure tx,
  ordered so a concurrent legacy create can't slip a second NULL-purpose row in.
  Note: the terminal binding itself is safe — `localStorage.calm.todayCardId`
  stores the **terminal** card id (`useTodayTerminal.ts:33`), separate from the
  spec card, so adopting the wave's spec thread doesn't disturb it.

#### NEW-3 — the `role_gate` arm cannot enforce "AiSpec(card) must be the launchpad card"; `enforce_role` has no marker source (Slice C)
- **Evidence.** §5 Slice C: *"for `Event::LaunchpadProposal`, `AiSpec(card)` must
  be the launchpad card and scope must match its home (mirror
  `enforce_card_self_scope`)."* But `enforce_role`'s signature carries only
  `cache: &CardRoleCache` (card→role, card→wave) and `wave_cove_cache:
  &WaveCoveCache` (wave→cove) — **no launchpad-marker source**
  (`crates/calm-truth/src/role_gate.rs`, and it is synchronous inside the write tx
  with no `repo`/`ctx` to look one up). So the arm **can** enforce self-scope
  (good news below) but **cannot** verify "this is *the* launchpad card." As
  worded, any Spec card emitting a self-scoped `LaunchpadProposal` into its own
  card would pass the arm.
- **Good news (the reuse question).** `enforce_card_self_scope(card_id, scope,
  cache, wave_cove_cache)` (`role_gate.rs:479-529`) is a **pure function of
  (card_id, scope, caches)** — its own doc says the Worker-variant name is
  "historical" and the semantic "applies equally." It reuses cleanly for
  `AiSpec(card)`: the concierge spec card is in both caches (Spec-roled, wave→cove
  write-through-populated). The arm slots in as a new section mirroring (2.8)
  ReviewRound/RatifyRequested (`role_gate.rs:353-371`): `if matches!(event,
  Event::LaunchpadProposal{..})` → `AiSpec(card)` ⇒ role==Spec + self-scope; `_` ⇒
  deny. Clean, additive, deny-by-default.
- **Fix.** Restate the arm as **self-scope-ONLY** (drop "must be the launchpad
  card"), and make explicit that the **who-boundary is the handler `is_concierge`
  gate applied to `calm.launchpad.propose`** (§3 I4 has it; §5 Slice C omits it —
  add it). The real safety stack is: handler `is_concierge` gate (who) +
  handler-derives-scope-from-identity (where) + self-scope arm (belt). If a
  first-class marker check in the gate is truly wanted, it needs a new
  `WaveLaunchpadCache` threaded into `enforce_role` (touches every caller — heavier;
  not recommended for v1).

#### NEW-5 (I2) — the dedup key is defeated unless the SERVER normalizes `repo` at write; and the key must be composite (repo, issue_number)
- **Evidence.** `workflow_input` is `TEXT NULL` on `waves`
  (`migrations/0061_waves_workflow_input.sql:8`), persisted **verbatim**:
  `validate_workflow_input_binding` runs pre-tx and is validation-only
  (`waves.rs:366`), and `create_wave` normalizes only `cwd`, never `repo`. The
  git-forge schema explicitly delegates normalization to the client — `repo` =
  *"owner/name, parsed from issue_url **at the entry surface**"*, `required:
  [issue_url, repo, issue_number]`, `additionalProperties:false`
  (`plugins/git-forge/manifest.json` issue-development input_schema). So callers
  sending `Owner/Repo`, `owner/repo`, `owner/repo.git` persist **distinct
  strings** → any `json_extract('$.repo')`/`'$.issue_number'` dedup key silently
  fails on casing/`.git`. Separately, keying on `issue_number` alone would forbid
  issue #5 in repo A and repo B from coexisting.
- **Fix.** r2 §3 I2 correctly says "keyed by *normalized* repo+issue_number" — make
  it explicit that **the server normalizes `repo` in Rust at the wave-create tx**
  (today it does not), then persist a dedicated `(repo, issue_number)` pair and add
  a **composite** partial-unique index `WHERE issue_number IS NOT NULL` as the hard
  backstop, plus a pre-write `SELECT` inside the existing
  `create_wave_with_spec_harness` closure (`waves.rs:557-645`, same atomic tx that
  already backstops `cove_folders.path` — `waves.rs:404-410`) for a friendly 409.
  Prefer the dedicated column over `json_extract` over a VIRTUAL generated column
  (SQLite `ALTER ADD` forbids STORED). This dovetails with Slice D: one server-side
  `owner/name` normalizer feeds both the persisted folder identity and the dedup key.

#### RESIDUAL-C (of r1 M3) — the proposal render is more net-new than "reuse `NewTaskForm`" implies (Slice C/F)
- **Evidence.** `NewTaskForm` is structurally the right confirm surface (editable
  title/cwd/cove + `workflow_id`/`workflow_input` → `createWave`,
  `web/src/shared/components/NewTaskForm.tsx:456-470`), **but** its props
  (`NewTaskFormProps`) expose only `defaultCoveId`/`onCreated`/`onCancel`/
  `initialFocusRef`/`variant` — there is **no prop to prefill
  title/cwd/workflow_input** from an external artifact; state starts empty
  (`useState('')`, `NewTaskForm.tsx:163-164`). Reusing it as a *pre-populated
  editable* confirm card needs new controlled/initial-value props + a
  `variant:'launchpad'` on a 1166-line component coupled to the cwd→cove
  resolve/conflict flow. Deeper: a top-level `Event::LaunchpadProposal` is **not a
  harness item**, and `useSpecChatHistory` builds the transcript from
  `listHarnessItems` REST + `harness.item.added` only
  (`web/src/pages/useSpecChatHistory.ts:198-202,359-363`) — so the proposal
  **bypasses the existing chat pipeline entirely** even after you add an
  `ev.ev==='launchpad.proposal'` filter; it needs its own event→entry bridge or a
  sibling topic-subscribed component. Agent output is otherwise strictly read-only
  (`SpecConversation.tsx` ItemView renders `<pre>`/`<details>`, no buttons;
  overlays are status-only `overlayRegistry.ts:40-58`) — confirming no reusable
  actionable-artifact renderer.
- **Fix.** Scope Slice F as: **reused** = the create form *mechanics* and
  `createWave`; **net-new** = (i) prefill/`variant` props on `NewTaskForm`, (ii) a
  proposal-event→confirm-card bridge (since it's not a harness item), (iii) the
  clickable card shell. Keep Create user-initiated.

---

### MINOR

#### NEW-4 — the `tools/list` half of the gate is more than `descriptors_for_role`, and still name-leaks in the F7 union; prefer `visible_to_roles: &[]` (Slice B)
- **Evidence.** `descriptors_for_role(role)` filters purely on `CardRole`
  (`registry.rs:327-333`) — no identity, no marker. Gating `tools/list` by the
  concierge marker means inserting a marker filter into the transport dispatch's
  **four** identity-resolving branches (`transport.rs:418-509`); and the
  unresolvable-threadId branches deliberately return the role-visible **union**
  ("discovery wide, dispatch strict", F7 — `transport.rs:461-467,493`), so the
  survey/propose tool **names** still surface there.
- **Fix.** Simpler and strictly tighter: register survey+propose with
  `visible_to_roles: &[]` (mirror `calm.wave.state`, which is `&[]` + identity-wave
  scoped — `crates/calm-server/src/mcp_server/tools/wave_state.rs`) so they appear
  in **no** `tools/list`; the concierge learns the tool from its injected prompt
  (Slice E); sole enforcement is the `tools/call` handler `is_concierge` gate. This
  removes the 4-branch marker plumbing and the F7 name-leak in one move.

#### NEW-7 — `ToolCallIdentity.wave_id` is `Option`; `is_concierge` must fail-closed on `None` and on marker-lookup error (Slice B/C)
- **Evidence.** `ToolCallIdentity.wave_id: Option<String>` (`registry.rs:107`);
  `to_principal()` already returns `None` when it's absent (`registry.rs:130-137`).
  A card-bound no-thread or unresolved call can carry `wave_id=None`.
- **Fix.** State the predicate contract: `is_concierge` returns false (deny) when
  `wave_id` is `None`, when the marker lookup errors, or when the wave lacks the
  marker — never marker-absence-as-benign. (Aligns with the fail-closed-fence memory.)

#### RESIDUAL-A (of r1 M2) — Slice E must thread the marker into the harness-start payload + reset the running Today harness (Slice A/E)
- **Evidence.** `render_spec_developer_instructions` has no cove/title/marker
  parameter (`crates/calm-server/src/operation/spec_harness_start_adapter.rs:209`),
  and with no `workflow_id` the Today wave yields `workflow_descriptor=None` → base
  spec prompt. r2 names the site but not the plumbing: the marker must be loaded at
  boot and passed through `SpecHarnessStartOperationPayload` into the render fn, and
  the already-running Today harness reset (`force_new_thread`) so the concierge
  template takes effect. r2 Slice A's "adopt-or-reset its spec thread deliberately"
  covers the reset intent; make the payload threading explicit.
- **Confirmation for the task's sub-question:** the harness **does** boot cleanly
  with NO goal — `initial_snapshot_with_goal(None)` filters empty/whitespace and
  yields an **empty pending queue** (no seeded turn) (`crates/calm-server/src/harness/mod.rs:227-232`).
  So "no goal" is safe for boot; the concierge behavior comes from the
  marker-selected prompt, not from a goal.

#### RESIDUAL-D (of r1 m3) — Slice D must hook BOTH attach paths and define a drift/refresh trigger
- **Evidence.** `cove_folder_create_tx` writes only `(cove_id, path, created_at)`
  (`crates/calm-truth/src/db/sqlite/cove.rs:199-231`); `cove_folders` (0015) is a
  clean spot for a nullable `owner_name` column. But the tx is reached from **two**
  places: the folder route (`crates/calm-server/src/routes/cove_folders.rs`
  `create_folder` handler) **and** in-tx from `create_wave_with_spec_harness`
  (`waves.rs:559-561`, `attach_folder=true`). The server already shells git with a
  ready `git -C <dir> …` helper (`crates/calm-server/src/routes/fs.rs:552-584`), so
  precedent exists. Resolve+persist should live **inside `cove_folder_create_tx`**
  (or a shared helper both call) so both paths inherit it; and because `origin` can
  change post-attach (`git remote set-url`), specify the refresh trigger (the
  persisted `owner/name` is a snapshot that can drift from the live remote the agent
  re-checks). Tolerate NULL for bare/non-git/absent-origin folders and the
  many-folders→one-origin (monorepo) case.

#### NEW-6 — the typed `LaunchpadProposal` event's FE/backend checklist is real; enumerate it in Slice C (Slice C)
- **Evidence.** A new `Event` variant is **mandatory-to-compile** in 3 FE files
  behind 2 hard `tsc -b` gates — `web/src/api/generated-events.ts` (ts-rs regen),
  `web/src/api/schemas.ts` (new zod schema + discriminated-union entry; gated by the
  `expectTypeOf … toEqualTypeOf<GeneratedEvent>` conformance test,
  `schemas.test.ts:213`), and `web/src/app/invalidationPolicies.ts` (a
  `definePolicies` mapped-type over every `EventKind` — a missing key is a tsc
  error, `invalidationPolicies.ts:21,119`) — plus ~3 backend `cargo test` sites in
  `crates/calm-server/tests/cases/event_serde_goldens.rs` (new golden JSON,
  `ALL_KIND_TAGS` count bump, exhaustive-`match` arm). None can be silently
  skipped (build fails), so it's safe — but Slice C should **budget** it. The
  card-topic transport IS reusable (`stream.addTopic('card:'+cardId)` in
  `useSpecCurrentRun.ts:155-164` / `useSpecChatHistory.ts:345-365`) — the proposal
  arrives over the same `card:<id>` sub — but "just subscribe and render" undercounts:
  no consumer filters the new kind and (per RESIDUAL-C) it isn't a harness item.
- **Fix.** Enumerate the 3-FE-file + 3-backend-site cost in Slice C. r2's Q-B choice
  of a typed event over card/overlay is **sound** (CardAdded would need a new card
  renderer; overlays are status-only) — keep it, just budget it.

---

## (c) Bottom line

r2 is a genuine improvement that **closes the r1 blocker** and correctly reframes
I1/I3/I4. The approach is ready; five MAJOR fixes are all local doc/slice
tightening: **(NEW-1)** marker via dedicated server tx, off `NewWave`;
**(NEW-2)** retire the client `ensureTodayWave` + migrate the legacy row, else the
duplicate-Today race r2 claims closed persists; **(NEW-3)** restate the role_gate
arm as self-scope-only and name the handler `is_concierge` gate as the propose
who-boundary; **(NEW-5)** normalize `repo` server-side at write + composite
`(repo,issue_number)` dedicated-column index; **(RESIDUAL-C)** budget the
proposal-render as net-new (`NewTaskForm` prefill props + non-harness-item event
bridge). Then slice.
