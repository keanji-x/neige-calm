# Design: Area Admin for a Track's Planner

Status: design, not implemented. Revision 3 (re-check fixes: caller-bound recorder, column-based
binding, probe ordering, typed refusals). Revision 2, folding in the two L2 design reviews (channel A:
approve-with-changes; channel B: rework) and the owner's narrowed scope.

**v1 scope.** A person-granted **Admin** state on a Track. The Admin's Planner may add Tracks
(`neige_track_add`, which moves under Admin) and close Tracks **it added**. A Planner asks to be
promoted with one new `neige_user_ask` action, `grant_admin`, that only a person's click grants.
There is no delete, no reopen of another Track and no Area-wide close in v1; those are listed in
the follow-up issue at the end. Builds on #2104 K1 (`neige_track_add`), #2209 (`neige_user_ask`),
#2348 (person-only Planner permission) and `docs/conventions/agent-commands.md` ("§n" below).
Paths are under `crates/calm-server/src/` unless named otherwise.

## 1. What exists today

| Capability | Today | Evidence |
|---|---|---|
| Add a Track | Every Planner on an unbound, top-level Track that no Track added. Keyed and replayable. At most `--track-add-max-open` (16) open added Tracks per creator, checked in the create transaction. | `mcp_server/tools/track_add.rs:142` (roles), `:201-206` (closed creator), `:255-290` (`authorize_creator`); `routes/tracks/add.rs:36-60`; `config.rs:175-182` |
| Creator provenance | `tracks.creator_track_id` / `creator_key`, both or neither, no FK. | `calm-truth/migrations/0147_track_creator_provenance.sql` |
| Close | A Planner closes its own Track (`neige_track_close`, `message` required, idempotent, in one write transaction). Managed Tracks under `kernel_controls_lifecycle` are refused. | `mcp_server/tools/track_state.rs:355-424`, `:362`, `:370` |
| Reopen ask | `neige_user_ask` `action: "reopen_track"`; only a click on the kernel's first option reopens. | `mcp_server/tools/user_ask.rs:111-121`; `ask/reopen.rs:11-57`, `:61-111`; `ask.rs:371-397` |
| Person-only REST check | `require_rest_user_actor_for` compares the raw header to `user`. The `closed` PATCH guard uses `to_actor_id()`, which folds every `ai:<id>` except `ai:codex` into `User`. | `routes/track_report_blocks.rs:73-89`; `actor.rs:41-52`; `routes/tracks.rs:2722`, `:2736-2741` |
| Tool gating | `ToolRegistry::lookup`: managed grant, fence, closed input, roles, handler. `filter_profile` hides tools per Track (discovery only; `tools/call` dispatches by name). | `mcp_server/registry.rs:425-451`; `mcp_server/transport/catalog.rs:149-159` |
| Running work | Task statuses `dispatched`/`running`/`verifying` in an inline SQL predicate; forge actions by a pool read with a documented TOCTOU window. | `planner_hibernation.rs:21-22`; `operation/workspace_lease/mod.rs:91-98` |

**Gaps.** No Track state says "a person let this Planner manage Tracks": the add right is
implied by shape and given to every Planner. No agent path closes a Track it spawned, so the
invest manifest asks a person to close superseded research Tracks
(`plugins/invest/manifest.json:9`).

## 2. Decisions

### D1. Admin state: `tracks.admin_granted_at`, written only by a person

**Storage.** Nullable `tracks.admin_granted_at INTEGER` (unix ms), surfaced as
`Track.admin_granted_at: Option<i64>` beside `closed_at` (`calm-types/src/model.rs:240`).
`TrackUpdated` already carries the whole row, so every change is audited and reaches `fe/` with
no new event kind. The column-only part of eligibility is a named constraint added with the
column, the 0147 form: `track_admin_is_top_level CHECK (admin_granted_at IS NULL OR
(parent_track_id IS NULL AND creator_track_id IS NULL AND COALESCE(purpose,'') <> 'area-chat'))`.

**Eligibility** is one function, `area_admin::eligible_tx(tx, track)`, used by every grant
writer and by `can_admin`. A Track is eligible when it is unbound
(`tracks.plugin_scope IS NULL`, the column `resolve_track_owner_binding` reads as Unbound,
`track_binding/mod.rs:105-111`; read in the transaction, no plugin host needed), top level, not added, not an area chat, not in the system
Area (`AreaKind::System`, `routes/tracks.rs:937`), and has no `managed_track_identities` row.
`authorize_creator` (`track_add.rs:255-290`) is deleted; its three rules are part of this list.
Eligibility gates the **grant** only; revoking is always allowed.

**Writers** are all a person's act:

| Writer | Rule |
|---|---|
| `POST /api/tracks` `admin: bool` (default false) | When `admin` is true, `require_rest_user_actor_for` runs first, before any read or keyed replay. `admin` joins `CreateRequestShape` (`routes/tracks/create.rs:101-117`) so a key reused with a different `admin` is `idempotency_key_reused`. Eligibility is checked in the mint transaction. |
| `PATCH /api/tracks/{id}` `admin: Option<bool>` | Whenever `admin` is present (grant, revoke, same-state), `require_rest_user_actor_for` runs first. **Not** the `closed` guard at `routes/tracks.rs:2736`. Grant checks eligibility in the write transaction; same state is a silent success. |
| `grant_admin` ask answer (D3) | The existing answer route, already strict (`routes/track_asks.rs:57`). |

Every non-REST create path (`neige_track_add`, child Tracks, managed and plugin creates) inserts
`admin_granted_at = NULL` explicitly. An added Track is ineligible (`creator_track_id` set), so
Admin never passes from agent to agent. A closed Admin keeps the flag but cannot use it (D2).

**Migration** `0165_track_area_admin.sql` (renumber at rebase if taken) adds the column with its
constraint, then **grandfathers** creators that already used the add right and meet every
storage-representable eligibility rule:

```sql
UPDATE tracks SET admin_granted_at =
  (SELECT MIN(a.created_at) FROM tracks a WHERE a.creator_track_id = tracks.id)
WHERE id IN (SELECT creator_track_id FROM tracks WHERE creator_track_id IS NOT NULL)
  AND parent_track_id IS NULL AND creator_track_id IS NULL
  AND COALESCE(purpose,'') <> 'area-chat'
  AND NOT EXISTS (SELECT 1 FROM managed_track_identities m WHERE m.track_id = tracks.id)
  AND plugin_scope IS NULL
  AND (SELECT kind FROM areas WHERE areas.id = tracks.area_id) <> 'system';
```

The stamp is the first add's time: provenance of a grandfathered right, not a recorded person
grant. A missing Area row yields NULL and excludes the row (fails safe). A creator excluded here loses `neige_track_add` and asks once. The invest
portfolio and SPY overview recipes (`plugins/invest/portfolio-recipe.md:16`,
`plugins/paper-trading/spy-recipe.md:5`) and the invest `planner_instructions`
(`plugins/invest/manifest.json:9`) gain one sentence: "If `neige_track_add` is not in your
tools, ask with `neige_user_ask` `action: "grant_admin"` and skip coverage this step." Recipes
get no `admin` field: content must not request authority at creation.

### D2. One authorization: `area_admin::require_admin_tx`

`require_admin_tx(tx, caller_track_id)` reads the **caller's live row inside the write
transaction** and requires `admin_granted_at IS NOT NULL` and `closed_at IS NULL`. It is the only
Admin check, shared by both Admin writes:

- **Add:** called from `CreatorAdmission::admit_tx` (`routes/tracks/add.rs:36`), in the same
  `BEGIN IMMEDIATE` transaction as the cap. A revoke or close that commits first is seen; one
  that commits later is ordered after the add. A keyed replay that finds its Track already
  minted writes no row and returns the recorded Track, as today.
- **Close another Track:** called first inside the close transaction (D4).

Each refusal travels a typed channel out of the transaction (as `OpenCap` does through the
`refused` cell, `routes/tracks/add.rs:203-211`), so the tool returns -32403 `not_admin`, not a
generic error; `close_tx` does the same for `unknown_track` and `active_work`.

The registry gets no Admin step and `ToolDescriptor` no new field. Listing is discovery only:
`filter_profile` drops the names in `area_admin::ADMIN_TOOLS` (v1: `neige_track_add`) for a
caller whose Track has a null `admin_granted_at`. It resolves card → Track through the pool, as
`managed_track::reports_only_card` does (`managed_track.rs:255-273`). Hiding applies to
attributed callers; bootstrap discovery before attribution lists the union
(`transport/catalog.rs:15-23`).
A non-Admin calling the tool by name reaches the handler and gets the in-transaction refusal.
`neige_user_ask` needs no Admin gate: `grant_admin` is how a non-Admin asks.

### D3. `grant_admin` ask

`AskAction` (`calm-types/src/event.rs:316`) gains the unit variant `GrantAdmin`; the enum keeps
`Copy`, and recorded `ReopenTrack` asks still deserialize. `user_ask`'s `action` enum gains
`"grant_admin"` (`user_ask.rs:48`, `:119-120`). It takes no `track_id`; the closed-input check
already refuses unknown keys. Like `reopen_track`: one title, options omitted, kernel options
`Grant admin` / `Not now`, `Wake` delivery, in a new `ask/grant_admin.rs` beside `ask/reopen.rs`.

- **Ask time:** the asker's own Track must be eligible and not Admin; one pending `grant_admin`
  per asker Track, the `scope_track` check of `ask/reopen.rs:35-44`.
- **Answer:** only `Option(0)` grants. In the answer transaction the kernel re-checks
  eligibility and not-already-Admin, stamps `admin_granted_at`, and emits `TrackUpdated` on the
  asker's Track: the same scope `ask_answered_tx` already uses (`ask.rs:390-396`), so no
  per-event scope change. A refusal is 400 and leaves the ask open, as reopen does.
- **Wake text:** "Admin granted." Whether a provider re-reads `tools/list` within a session is
  open question 1.

### D4. Close a Track this Admin added

`neige_track_close` gains optional `track_id` (§4 `<noun>_id`). Absent or equal to the caller's
own Track, it is today's path for every Planner. Otherwise:

1. **Target = only Tracks this Admin added.** Inside the write transaction:
   `require_admin_tx(caller)`, then the target row must have `creator_track_id == caller`.
   Any other id, unknown or not added by the caller, is -32404 `unknown_track`. The ownership
   check also runs **first**, before any target-specific pre-transaction read (forge check), so
   a probe of a foreign id learns nothing. Added Tracks are top level, never area chats, never Admin and never managed,
   and share the caller's Area, so those fences are structurally unnecessary.
2. **No running work.** Refused (-32409 `active_work`, `data.active: [{key, status}]`) while the
   target has a task in `dispatched`, `running` or `verifying`. The status set is lifted from
   `planner_hibernation.rs:21-22` into one named const both use, and is read in the
   transaction. The forge check `track_has_active_forge_action` runs before the transaction,
   with its documented window (`workspace_lease/mod.rs:91-98`). The Admin mails the target's
   Planner with `neige_mail_send` to settle its work.
3. **Existing close path.** `track_close` is split into a shell and `close_tx(tx, target,
   message)`, used by both cases. The **decision recorder always binds to the caller's own
   Track** (`resolve_track_for_identity`, the id `require_admin_tx` reads): its containment
   rule denies a session whose card is on another Track (`calm-truth/src/decision_gate.rs:426-432`,
   `:500-503`), so one transaction proves "this session is the live Planner of an open Admin
   Track". `close_tx` takes the target only for the row update and the event scope. The
   already-closed idempotency (returns `closed_at`) stays. `kernel_controls_lifecycle` stays on
   the own-close path; it is structurally false for an added Track and is skipped on
   close-other. The event is `TrackUpdated`
   scoped to the **target**, actor the Admin's Planner session, `agent_message` = `message`.

The open-added cap is unchanged and close frees slots. CLI: `neige track close` gains
`--track-id` mechanically (§2). `neige_area_ls` rows gain `creator_track_id`, so an Admin can
find the Tracks it may close (`mcp_server/tools/report_links.rs:83-133`).

| Cause | Code | `refusal` |
|---|---|---|
| Caller not Admin, or closed (add, close-other) | -32403 | `not_admin`: "ask with neige_user_ask action grant_admin" |
| Target unknown or not added by the caller | -32404 | `unknown_track` |
| Target has running work | -32409 | `active_work` |
| `grant_admin` on an Admin or ineligible Track | -32409 / -32403 | `already_admin` / `ineligible: <rule>` |
| `neige_track_add` past the cap | -32409 | `open_cap` (unchanged) |

### D5. UI and `can_admin`

`TrackDetail.can_admin` is computed in the route beside `can_close` (`routes/tracks.rs:740-742`)
as `admin_granted_at.is_some() || eligible`, meaning "the toggle applies"; the UI infers no
policy. `fe/` per `fe/AGENTS.md`: an "Area admin" checkbox in the create form (off by default;
help: "This Track's Planner may add Tracks and close the ones it added"), an Admin toggle and
badge on the Track page, and the ask drawer rendering `grant_admin` with the kernel's options.

## 3. Slices

Each slice is one PR and ships alone: it carries its `fe/core` schema widening (zod is
type-pinned to generated `wire.ts`, `fe/core/api/schemas.contract.test.ts:201-206`, and
`decodeWireEvent` drops non-matching frames, `fe/core/api/schemas.ts:1093`) and minimal
rendering of any ask kind it adds. The grant path lands before the add gate.

| Slice | Tier | Content | Focused tests (mutation-verified marked *) |
|---|---|---|---|
| **S1 Admin state and grant** | L2: migration, authority | Migration 0165; `Track.admin_granted_at`; `area_admin::eligible_tx`; strict POST/PATCH `admin`; `CreateRequestShape.admin`; `can_admin`; `AskAction::GrantAdmin` and answer; regenerated OpenAPI, `wire.ts`, registry golden; `fe/core` widening of `trackObjectSchema` (`schemas.ts:45`), the ask `action` unions (`schemas.ts:625`, `fe/core/domain/track.ts:153`), `NewTrackBody` / `TrackPatchBody` (`track.ts:360`, `:548`); drawer renders `grant_admin`. No behaviour change for existing tools. | `admin_backfill_grandfathers_eligible_creators_only` (fixture with managed, system-Area, child and chat creators); `the_database_refuses_an_admin_child_or_added_track`; `admin_patch_refuses_ai_actor_headers`*, `create_admin_refuses_ai_actor_headers`* (several `ai:*`; no row, event or replay effect); `admin_patch_refuses_an_ineligible_track`; `admin_revoke_is_allowed_when_ineligible`; `create_key_reused_with_other_admin_conflicts`; `grant_admin_click_grants_and_wakes`; `grant_admin_other_answers_grant_nothing`*; `grant_admin_answer_rechecks_eligibility`; `old_reopen_track_asks_still_deserialize`; fe unit: schemas accept `admin_granted_at` and `grant_admin`. |
| **S2 Add under Admin** | L2: authority | `require_admin_tx` in `admit_tx`; `authorize_creator` removed; `ADMIN_TOOLS` listing filter; recipe and manifest sentence; byte budget for the changed descriptions. | `track_add_refuses_a_non_admin_planner`*, `track_add_refuses_a_closed_admin`, `track_add_admits_an_admin_planner` (kernel socket); `track_add_after_revoke_commits_nothing` (revoke between handler entry and tx); `tools_list_hides_track_add_from_a_non_admin`*; `track_add_creates_a_non_admin_track`; the old scope/depth tests retargeted at eligibility. |
| **S3 Close own-added Tracks** | L2: authority | `track_id` on `neige_track_close`; `close_tx` split; the shared running-work const; `neige_area_ls.creator_track_id`; prompt text and byte budget. | `admin_closes_a_track_it_added_with_message` (event scoped to target); `non_admin_close_of_other_track_is_refused`*; `own_close_with_own_track_id_needs_no_admin`; `close_of_track_not_added_by_caller_is_unknown`*; `close_refuses_running_work_listing_keys`*; `close_after_revoke_commits_nothing`; `close_other_from_a_superseded_session_is_refused`; `area_ls_rows_carry_creator_track_id`. |
| **S4 fe UI** | L1 | Create checkbox, Track-page toggle and badge, driven by `can_admin`. Ships any time after S1. | Browser: create posts `admin: true`; toggle hidden when `can_admin` is false; toggle revokes; grant-ask click posts the answer only. Preview per `docs/development-preview.md`. |

For each `*` test, the mutation (drop the check under test) must turn exactly the predicted set
red within its package filter; S2's prediction includes the retargeted eligibility tests.

## 4. Architecture review

- **Abstraction boundaries.** Admin policy and eligibility live in `area_admin`; the truth
  layer owns only the column constraint. The registry and catalog do not authorize. `fe/` reads
  `can_admin`.
- **Duplicate logic.** One `require_admin_tx` for both writes; `authorize_creator` is folded into
  eligibility, not copied; one running-work const; one `close_tx` for own and other close.
- **Hardcoded application assumptions.** Nothing names invest, paper-trading or a template; the
  backfill keys on provenance and storage facts. `AskAction` is a closed set in its owning
  layer. `ADMIN_TOOLS` is a bounded discovery list, not a closed set and not authorization;
  when a second Admin-only tool arrives, reconsider descriptor metadata instead of a parallel
  list.
- **External contract facts.** Provider `tools/list` re-reading is measured, not assumed (Q1).
- **Cost of permanence.** One column, one named CHECK, one `AskAction` variant, one `ADMIN_TOOLS`
  entry. A future Admin-only tool adds one `ADMIN_TOOLS` name and one `require_admin_tx` call;
  no existing descriptor changes. New tests run once per CI workflow in the calm-server nextest
  suite (fe tests in the fe job).

## 5. Open questions for the owner

1. **Re-listing on grant.** If a provider keeps its first `tools/list` for a session, should a
   grant restart the Planner session at its next idle point (the hibernation resume path,
   #2628), or should `neige_track_add` appear only from the next natural session? S1 probes
   each supported provider without real Codex E2E on the shared host.

Resolved in this revision, where both reviews agreed: unbound-only eligibility, no content-keyed
backfill, no Admin spawning Admins, several Admins per Area allowed (with own-added close they
are isolated).

## Follow-up issue (draft)

**Title:** Area Admin: deferred edge cases and hardening after v1 (add + close own-added)

Area Admin v1 (`docs/design-area-admin-planner.md`) ships Admin state, `grant_admin`,
`neige_track_add` under Admin and closing Tracks the Admin added. These items were deliberately
deferred; each needs its own design or test before it is built.

- Delete by ask: typed deletion intent fenced inside the existing locked saga (validate before
  teardown and at final commit, closure-epoch check, answer committed only after success,
  retry/compensation contract).
- Area-wide close: closing person-created Tracks, children, area chats, managed Tracks and other
  Admins' Tracks, each with its own fence.
- Cross-track reopen asks: per-event scopes in `ask_answered_tx`, target lifecycle lock order,
  closure-generation check, pending uniqueness keyed on the target id.
- Event role-gate Area backstop for `AiPlanner` `TrackUpdated`: require a Track scope, payload
  Track id/Area agreement with the scope, and the actor's home Area; deny missing rows.
- Pending asks vs revoke: decide whether revoking Admin withdraws or neutralises pending asks.
- Listing vs call refusal mismatch: `neige tool` lookup reports a hidden tool as absent while
  `tools/call` returns -32403 `not_admin`; unify or test both.
- `neige_area_ls` admin and running-work fields, so an Admin need not find fences by trial.
- Pre-existing: the `closed` (and automation) PATCH guards use `to_actor_id()` and admit `ai:*`
  headers as `User` (`routes/tracks.rs:2722-2741`); switch to `require_rest_user_actor_for`.
- Forge-action check before close is a non-transactional read with a TOCTOU window.
- Keyed `neige_track_add` replay after revoke returns the recorded Track; confirm it starts no
  new work.
- Admin on template-bound Tracks, if a template needs to manage its Area.
- The add path proves Admin through `admit_tx` but runs no decision-recorder probe, so a
  superseded-but-live Planner session can still add; consider the recorder probe on add.
