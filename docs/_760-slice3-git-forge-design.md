# #760 slice ③ — Git/forge toolset: sub-slicing design (③-a..d) — **v3** (CONVERGED)

> Synced from GitHub issue #798 (the slice-③ implementation home; child of #760).
> Sub-design under the converged #760 design (`docs/_760-issue-dev-workflow-design.md`,
> §3 A5/A6/A8/A9, §4-③, §6). Grounded at HEAD `a8f8a49b` (① ② ⑥ ⑦ + #795 merged).
> `file:line` cites prefer the named symbol on drift (the repo auto-syncs and advances HEAD).
> This doc lands in-repo with ③-a's PR.
>
> **v3 folds round 1 (6 P0s) + round 2 (subagent: 0 P0 / 6 P1; codex: 3 P0 / 5 P1).**
> Partition + seam (A) confirmed sound across both rounds; the rounds drilled the
> feasibility core to convergence. Round-2 resolutions folded here:
> - **Field precedence (codex P0-1):** `build_forge_event` merges plugin `context` +
>   extracted output fields FIRST, **rejects reserved keys**, inserts kernel-authoritative
>   `wave_id`/`subject` **LAST** (action output can never override them); `subject` is
>   **required-at-validate** for `forge.pr.merged` (a per-event required-INPUT-field gate —
>   an Optional subject must not reach a post-merge deserialize).
> - **Worktree provisioning (codex P0-2; parent §2.5-B/A4 authoritative).** `git worktree add`
>   is a **PLUGIN forge-action** ("the issue-dev plugin layers git worktree add/remove on the
>   leased path via a forge-action op; the kernel does not know it is git" — §2.5-B, §3-A4).
>   The kernel lease stays dir+row (① unchanged). The ONE remaining hard fork is the
>   **ORDERING** (parent invariant-3 wants `worktree.provisioned < runtime.started`, but the
>   lease/`card_id` are minted INSIDE the worker op — codex P0-2's "no stable pre-worker
>   path"). This is **isolated to ③-c** (③-a/③-b/③-d do not depend on it) and is firmed when
>   ③-c is implemented (it interacts with ④'s workflow binding + ①'s lease lifecycle).
> - **Idempotency (codex P0-3 + both P1):** `payload_hash` covers the **semantic key subset
>   ONLY** (excludes `result_path`/`deadline_ms`/volatile argv text); per-verb keys enumerated
>   for ALL mutating verbs; `gh.pr.diff` keyed on `(repo,pr_number,base_sha,head_sha)`.
> - **Threading (both P1):** `OperationRuntime` reaches the MCP transport via a **late-bound
>   `OnceCell`** (the `plugin_host_cell` precedent); `ToolKind` is **retained through dispatch**;
>   trusted-plugin-id allowlist gates op-submission until ④'s binding.
> - **cwd anchor (codex P1):** resolve `repo_root = git -C wave.cwd rev-parse --show-toplevel`
>   and anchor under that, not raw `wave.cwd`.
> - **Diff artifact (both P1):** `ForgePrDiffRead` carries an `artifact_path` (+`head_sha`);
>   the diff body lands at the op's `result_path` for ⑤ to read.
> - **wave-vcs (both P2):** the projection is `crates/calm-truth/src/wave_vcs.rs` (the server
>   file is a `pub use` shim); add all 5 forge + 2 worktree kinds to BOTH the exhaustive no-op
>   arm AND the **skip-commit set** — `commit_delta_in_tx` has no empty-delta short-circuit, so a
>   lone forge event would otherwise spuriously commit an empty tree.
> - **Resultless = `event_spec: Option<ForgeEventSpec>`** (None=resultless), not a sentinel kind;
>   **`is_hard_fire`** is a non-exhaustive `matches!` so new hard-fire forge kinds must be added
>   explicitly (the compiler won't force it).

## 0. Scope, ground truth, corrections
③ aggregates **A5** (branch+commits, row 8), **A6** (per-PR diff, row-10 source), **A8/A9**
(gh/git primitives, rows 2,3,9,13,14,15,16). It splits into ③-a..d. Deps ① ② ⑥ ⑦ are merged.

**Confirmed ground truth (round-1 verified):**
- ⑥: `ForgeActionAdapter` + `forge.pr.merged` (only supported kind) + the event-add template.
  `SYNC_EVENT_VERSION=7`. **The payload is merge-shaped:** `ForgeActionPayload.subject:
  ForgeMergeSubject` is required and `build_forge_event` injects `subject` for every event.
  `validate_payload` requires a supported `event_spec.event_kind` — **no resultless mode**.
- ②: discovery (namespaced `plugin.<id>_<tool>`), permissioning (`PLUGIN_TOOL_ROLES=[Spec,Worker]`),
  routing (`dispatch_plugin_tools_call` → `McpClient::tools_call` returns a parsed `CallToolResult`,
  today serialized straight back). Plugins run line-delimited JSON-RPC over stdin/stdout with
  `NEIGE_PLUGIN_TOKEN` — NOT a Unix socket. `ExposedTool` is `{name, description?}`, serde-default-
  extensible; discovery currently discards extra metadata. `AppContext` (the MCP registry ctx) has
  NO `OperationRuntime`.
- ①: `acquire_workspace_lease_tx` creates an **empty dir** at the **RELATIVE** path
  `.claude/worktrees/<wave>/<card>`, with a TODO deferring repo-root anchoring to "slices 3/6".
  The adapter **hard-rejects** a non-absolute `cwd_lease`/`result_path`. Lease acquired in
  `CodexWorkerAdapter::prepare_tx`.
- ⑦: the §2.5-C pattern. **`forge.pr.merged` has NO spec-push wiring.** ③-a retro-wires it.
- `wave.cwd` is validated only as an absolute cove-claimed dir — **not guaranteed a git repo**.

---

## 1. ③-a — generalize the forge adapter + 5 typed events + §2.5-C (substrate; flips no row)
**1a. Generalize ⑥'s adapter.** Today the adapter is merge-only. Change:
- `ForgeActionPayload.subject: Option<ForgeMergeSubject>` (`#[serde(default)]` on both
  `ForgeActionPayload` and `FrozenForge` so already-merged ⑥ `tx_output` rows still parse). Add a
  `context: serde_json::Map` (`#[serde(default)]`; plugin-supplied per-event payload fields).
- **`build_forge_event` field precedence:** merge plugin `context` + the bounded-extractor output
  fields **FIRST**, then **REJECT reserved keys** (`wave_id`, `subject`), then insert kernel-
  authoritative `wave_id` (always) + `subject` (when present) **LAST** — so plugin/action output
  can never override kernel-authoritative fields.
- **`subject` presence is exactly iff `event_kind == "forge.pr.merged"`:** a per-event input gate
  in `validate_payload` — `forge.pr.merged ⇒ subject.is_some()`; every other kind (and resultless)
  ⇒ `subject.is_none()`. Rejected BEFORE the side effect.
- **Resultless mode:** `event_spec: Option<ForgeEventSpec>` (None = "succeed without emitting a
  typed event") — for `git.commit` and intermediate local git that produce no oracle event.
  `validate_payload` accepts it; `build_forge_event` returns no event; the op completes Succeeded
  with no decision-event/envelope.
- **`required_output_fields` stays `forge.pr.merged`-only in ③-a.** Per-new-kind output-vs-context
  field gating is deferred to ③-b, where the plugin authors each verb's `event_spec`. The
  `Event::from_kind_and_payload` deserialize is ③-a's backstop that every required payload field
  is present (whether context- or extraction-sourced).

**1b. The 7 NEW variants** in `crates/calm-types/src/event.rs` (real enum; the
`crates/calm-server/src/event.rs` `pub use` shim is NOT edited):

| variant | kind | payload (besides wave_id) | flips (with later slice) |
|---|---|---|---|
| `ForgeScanCompleted` | `forge.scan.completed` | `overlapping_prs: Vec<u64>` | row 3 |
| `ForgePrOpened` | `forge.pr.opened` | `pr_number, head_sha` | row 9 |
| `ForgePrDiffRead` | `forge.pr.diff.read` | `pr_number, base_sha, head_sha, artifact_path` | row-10 source |
| `ForgePrChecks` | `forge.pr.checks` | `pr_number, conclusion` | rows 13,14 |
| `ForgeIssueClosed` | `forge.issue.closed` | `issue_number` | row 16 |
| `WorktreeProvisioned` | `worktree.provisioned` | `card_id, path` | ③-c provisioning |
| `WorktreeRemoved` | `worktree.removed` | `card_id, path` | ③-c teardown |

The 5 `forge.*` variants mirror `ForgePrMerged` (metadata `entity_kind="wave"`, topics wave-only).
`WorktreeProvisioned`/`WorktreeRemoved` mirror `WorkspaceLeased`/`WorkspaceReleased` (metadata
`entity_kind="card"`, topics card+wave). Branch creation stays implicit in `forge.pr.opened` per A5.

**Per-variant recipe** (mirror `ForgePrMerged`): variant + `#[serde(rename)]` → arms in `kind_tag`,
`metadata`, `topics` → `event_serde_goldens.rs` `kind_tag_list_matches_enum` arm + a
`tests/goldens/events/*.json` golden + `ALL_KIND_TAGS` count + `goldens_cover_every_event_variant`
count → in-module `event.rs` tests (`metadata_coverage_events`, `kind_tag_new_variants_pinned`,
`new_variants_round_trip`) → web hand-edited `web/src/api/schemas.ts` (zod+union+type) +
`web/src/app/invalidationPolicies.ts` (noop) + `web/src/api/version.ts` mirror; `generated-events.ts`
ts-rs-auto via `npm run gen:api` → NO `from_kind_and_payload` arm (generic serde) → **`wave_vcs.rs`**
no-op arm + **skip-commit set** for each new kind → one batched `SYNC_EVENT_VERSION 7→8` + history.

**1c. §2.5-C spec-push** (hard-fire): `forge.scan.completed`, `forge.pr.opened`, `forge.pr.checks`,
`forge.issue.closed`, **retro `forge.pr.merged`**, `worktree.provisioned`. For each: `dispatcher.rs`
filter kinds vec, `event_warrants_spec_push_with_role` arm→`true`, `replay_harness_events_since` boot
kinds, `Observation` variant + `is_hard_fire` + `to_turn_text` (exhaustive — compile-forces),
`harness_observation_from_event` arm, live arm in `handle_envelope`. **NOT pushed:** `forge.pr.diff.read`
(persisted for the E2E ordering assertion + reviewer-task input; the spec reacts to review *verdicts*
not the diff-read — Q1) and `worktree.removed` (teardown bookkeeping). Non-pushed kinds are persisted
Event variants only — they never enter the subscription filter, so they need NO `Observation` /
`handle_envelope` arm.

**Acceptance.** Each variant round-trips (golden+serde); the adapter accepts non-merge + resultless
payloads (new unit tests); each pushed event traverses §2.5-C. **Size L. Deps: none new.**

---

## 2. ③-b — plugin→forge-action seam + git-forge plugin (substrate; flips no row)
**2a.** Thread op-submission into the MCP transport: add an `Arc<OperationRuntime>` (or a narrow
`submit` handle) to the transport ctx via a **late-bound `OnceCell`** so `dispatch_plugin_tools_call`
can submit ops.

**2b. The seam — DECISION (A).** Add `ExposedTool.kind: Option<ToolKind>` (a typed, validated manifest
enum: `ForgeAction | …`), parsed at manifest-load and **propagated through discovery**. When a
Spec/Worker calls a `ForgeAction` tool, routing forwards to the plugin; the plugin returns **structured
content** = the lowered payload `{argv, event_spec?, probe?, idem_key, subject?, context}`. The kernel,
at a **single explicit parse-and-validate boundary**: (1) parse as `PluginForgePayload`, reject
malformed (new typed error); (2) validate plugin-supplied fields (`event_kind ∈
SUPPORTED_FORGE_EVENT_KINDS` or resultless, `idem_key` non-empty, argv non-empty — the kernel never
trusts plugin-supplied `cwd_lease`/`result_path`/`wave_id`/`cove_id`); (3) fill kernel-derived fields +
`submit`. Routing keys on **manifest metadata** (`ToolKind`), NOT the tool-output shape.

**2c. Per-tool cwd policy + absolutization.** `cwd_lease` is kernel-derived: Worker git/PR ops use the
caller worker card's held lease canonicalized to absolute (`<wave.cwd>/.claude/worktrees/<wave>/<card>`);
Spec ops (`gh.issue.view/list/close`, `gh.pr.list`) use the wave's absolute `cwd`; `git.worktree.add`
carries explicit source+target in argv with `cwd_lease=<wave.cwd>`.

**2d. Sync-await vs async-parked.** Async-parked (slow `gh` net ops): kernel submits, returns
`{op_id, parked}`; result reaches the spec via the §2.5-C event. Await-synchronous (fast local git:
`git.worktree.add`/`branch.create`/`commit`): kernel submits and awaits (`OperationRuntime::wait`),
returns inline — makes worktree provisioning safe (the caller has the worktree before editing).

**2e. Trusted-plugin gate.** Until ④'s binding, gate op-submission on a minimal trusted-forge-plugin
allowlist + the existing `PLUGIN_TOOL_ROLES`. Emitted forge events are `KernelDispatcher`-attributed.

**2f. The plugin** `plugins/git-forge/` — manifest (`exposes_tools` with `kind:"forge-action"`) + a
small stdin/stdout JSON-RPC process implementing `tools/list` + a `tools/call` that **only lowers**
each verb to a payload (argv/event_spec/probe/idem_key/context). It shells nothing.

**Acceptance.** A test plugin exposes a fake `forge-action` tool; a Spec/Worker `tools/call` →
parse/validate → submit (await returns inline; parked returns `{op_id}`) → the typed event lands; a
malformed payload is rejected without submit. **Size L. Deps: ③-a, ②, ⑥.**

---

## 3. ③-c — real git on the leased worktree (A5; flips row 8) — worktree-add is a PLUGIN forge-action
**Settled in ③-c:**
- (i) **`repo_root` + precondition:** `repo_root = git -C <wave.cwd> rev-parse --show-toplevel`
  (read-only); fail cleanly if `wave.cwd` is not a git repo. The leased path is anchored under
  `repo_root` so `cwd_lease` is **absolute** (resolves ①'s TODO).
- (ii) **Lease-dir vs `git worktree add` collision:** ① does `create_dir_all` so the dir pre-exists;
  `git worktree add <existing-dir>` errors. Fix: don't pre-create when worktree-backed, or `--force`.
- (iii) **Worktree realize = a PLUGIN forge-action** (`git.worktree.add`), emitting
  `worktree.provisioned{wave_id,card_id,path}`. The real slice branch (`git checkout -b`), commits
  (`git.commit`, resultless), PR ops ride ③-b.
- (iv) **Teardown ownership:** worktree/branch teardown is owned by the slice/PR lifecycle (③-d),
  operation-rollback compensation, and a wave-level final sweep. Per-task lease release
  (`decision_sink`, reaper, scheduler-timeout) never touches git; it only frees the resource slot.
  This decoupling means normal completion or dead-worker reclaim cannot destroy unmerged commits, and
  there is no preserve/reclaim race at lease release.

**THE ONE OPEN ③-c ITEM (provisioning ORDERING fork) — decide when implementing ③-c:** parent
invariant-3 wants `worktree.provisioned < runtime.started`, but the lease/`card_id` are minted inside
the worker op's `prepare_tx`, so a separate pre-worker provision node has no stable target path.
Candidates: **(α)** pre-spawn step in the worker op (submit+await `git.worktree.add` between
lease-acquire and daemon-spawn); **(β)** reserve lease/card_id at plan time + a separate provision task;
**(γ)** worker-self-provision (first action), relaxing invariant-3. Lean **(α)**; confirm against ④
when ③-c lands.

**Acceptance (flips row 8).** A claimed Codex task runs in a REAL git worktree under
`.claude/worktrees/` (absolute, anchored under `repo_root`); branch ref exists with ≥1 commit;
collision + teardown handled; ordering per the chosen (α/β/γ). **Size M→L. Deps: ③-a, ③-b, ①.**

**③-c implementation note.** Literal α was rejected: the codex-worker op mints the lease/card inside
`prepare_tx`, while daemon spawn happens later, and `OperationRuntime::submit`/`wait` drive under the
same mutex; submitting a nested forge-action op from the worker adapter would deadlock. ③-c uses
α-prime instead: codex-worker opts into the existing durable `AppServerInteract` phase and provisions
between `TxCommitted` and `SpawnStarted` in a separate drive cycle. No new phase enum/migration was
needed. This phase runs kernel-internal `git worktree add` directly via shared lease-core helpers,
which intentionally relaxes §2.5-B only for automatic worker-spawn provisioning; the
`dev.neige.git-forge` `git.worktree.add` verb remains the explicit agent-driven path. The phase
persists `worktree.provisioned{wave_id,card_id,path}` and then `runtime.started` for the worker path,
so invariant-3 is asserted as durable event order before daemon spawn.

Worktree/branch teardown is keyed to the slice/wave lifecycle, not normal per-task worker-lease
release: worker `task.completed`/`task.failed` releases only the lease row and preserves the
`neige/<wave>/<card>` branch for downstream PR operations. Timeout/dead-worker reclaim and wave
teardown remove worktrees/branches; the precise PR-flow teardown point is finalized in ③-d.

---

## 4. ③-d — PR ops end-to-end + E2E (flips 2,3,9,13,14,15,16 + diff-source of 10)
**4a. Wire the verbs:** `gh.issue.view`→goal ingestion (row 2), `gh.pr.list`→scan (row 3),
`gh.pr.create`→row 9, `gh.pr.diff`→row-10 source, `gh.pr.checks`→rows 13/14, `gh.pr.merge`→row 15,
`gh.issue.close`→row 16. **Row-10 diff artifact contract:** `gh.pr.diff` writes the diff BODY to the
op's `result_path`; the `forge.pr.diff.read` event carries `{pr_number, base_sha}` + the artifact path;
⑤'s reviewer tasks read that artifact.

**4b. E2E — local bare repo + `gh` shim.** New case `e2e/cases/120-issue-workflow.sh`. Host pre-creates
a **bare git repo** on a **mounted volume** (survives container kill); `init_workspace` adds it as
`origin`. A minimal **`gh` shim** on PATH emulates `pr create/checks/merge/view` + `issue view/close`
over **state files on the mounted volume**. New helper `fetch_persisted_events(wave_id)`.

**4c. E2E assertion scope (forge-only subset ③ owns).** ③-d asserts ONLY: backbone forge kinds present,
and ordering **4a** `forge.pr.opened < forge.pr.diff.read`, **5** `forge.pr.checks(success) <
forge.pr.merged`, **7** `forge.pr.merged < forge.issue.closed`, **8** `forge.issue.closed <
wave.lifecycle_changed(done)`. Deferred to ⑤'s full-trace case: invariant 4's `diff.read <
review-dispatch`, **6**, **5b**. Plus the **crash-recovery sub-test**: kill mid-`forge.pr.merged`,
assert no double-merge (idem) + lease reclaim + event recovery.

**Acceptance (flips 2,3,9,13,14,15,16 + diff-source of 10).** Case 120 completes (≤40 min), forge
backbone + the owned ordering subset hold, stable ×3, crash-recovery passes. **Size L. Deps: ③-a/b/c.**

---

## 5. Dependency chain & flip-owner
`③-a (gen+events+§2.5-C) → ③-b (seam+plugin+op-thread) → ③-c (real git, row 8) → ③-d (PR ops+E2E)`.
Rows: 2,3→③-d; 8→③-c; 9,13,14,15,16→③-d; 10-source→③-d (dual-channel half ⑤); substrate ③-a/b flip
none. Each: Codex worktree impl → two-channel review → gates → squash-merge.

## 6. Resolved open questions (dual-channel synthesis)
- **Q1 (diff.read push?)** → **No push.** Persist for E2E ordering + reviewer-task input.
- **Q2 (seam A; return op_id?)** → **(A), return `{op_id, parked}` incl. op_id.**
- **Q3 (worktree provisioning)** → `git worktree add` is a **PLUGIN forge-action**; the **ordering**
  mechanism (α/β/γ) is the one open ③-c item.
- **Q4 (gh shim vs pure-git)** → **gh shim** + **state on the mounted volume** for crash-recovery.
- **Q5 (split ③-a/③-b?)** → **Split**; ③-a owns the adapter generalization + resultless/worktree-event.
- **Q6 (idem_key)** → **per-verb keys + a `payload_hash` over the SEMANTIC subset only**.

## 7. Convergence status
**Converged (rounds 1-2 dual-channel + round-3 confirm):** ③-a, ③-b, ③-d designs; the partition +
flip-owner; seam (A); the adapter generalization; op-threading via `OnceCell`; `ToolKind`-on-dispatch;
per-verb idem keys + semantic `payload_hash`; cwd anchored on `repo_root`; the diff-artifact contract;
the wave-vcs skip-commit guard; the E2E forge-only ordering subset.
**The single remaining open item:** the ③-c worktree-provisioning **ordering** mechanism (α/β/γ, §3) —
isolated to ③-c. It does NOT block ③-a, ③-b, or ③-d.

## 8. Implementation order
**③-a first** (fully converged), then ③-b → ③-c (firm the ordering fork) → ③-d. Each: Codex worktree
impl → two-channel review → gates → squash-merge. Doc lands in-repo with ③-a's PR.
