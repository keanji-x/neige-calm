# PR10 Design Doc — IngestQuery type migration + CardRuntime/runtime-* name retirement · #679 cleanup

**Verified against `origin/main` @ `04d39bdb` (PR9b-iv "drop runtimes table", #759 merged).** This document now lives in a worktree checked out at `origin/main @ 04d39bdb` — the TRUE current state — so every `file:line` below was re-grounded by `git grep`/`rg`/`cat -n` against this tree, not a stale local checkout. (The v1 header described the worktree as "stale at 76642d68"; that is no longer accurate and the framing is dropped. Citations remain origin/main-anchored, which is the same tree this file is in.) Predecessors of record (local working notes, **not git-tracked** in this tree — cited by topic, not line anchor): `docs/_679-body-new.md`, `docs/_design-679-pr9a-v2-current.md` (RATIFY-6 wire-freeze), `docs/_design-679-pr9b.md` (RATIFY-5 struct-deferral, irreversibility). Closes #762.

**This is the #679 epic's final cleanup.** PR9 retired the `runtimes` *table*; PR10 retires the `runtimes`-table-era *repo/struct/identifier vocabulary* (Rust trait + struct + identifier family) and makes the one remaining wire-typed escape hatch — `IngestQuery.card_id` — honest. There is no behavioral change in scope; this is a typing + naming convergence PR with one load-bearing wire decision (Q-BC). **The event-wire `Runtime*` names are deliberately NOT in scope — they are frozen by BC (see §0.3).**

**Revision history:**
- **v1** (Plan agent draft, 2026-06-17) — initial draft. Resolved the 6 open questions, audited A/B/C for value, proposed a 4-slice plan, isolated the architect-level forks.
- **v2** (Revision agent, 2026-06-17) — round-1 dual-channel review convergence. R3 collision resolved in-design (rename `RuntimeRepo`→`WorkerSessionProjectionRepo`, NOT merge into `SessionRepo`); census recounted with stated methodology; empty/absent BC asymmetry corrected (empty→403, absent→400→unified-to-403, latent); `NEIGE_HOOK_URL` 4th producer documented; "frozen legacy wire vocabulary" §0.3 added; all drifted citations fixed. Disposition table in §7.
- **v3** (Polish agent, 2026-06-17) — round-2 dual-channel review convergence (both channels APPROVE-WITH-NITS, **zero blocking findings**). Doc-polish only, no design change: corrected `WorkerSessionProjectionRepo` description (reads PLUS legacy status/complete mutators, not "reads only"); added 3 missing crate rows + repo-wide total **3610** to the §0.1 census; switched the §0.5 `RunStatus` artifact counts to word-boundary (6/2/2, `WaveFsRunStatus` excluded); fixed the override-missing-card_id (400 extractor failure before the handler) and absent-case wording; corrected the `IngestQuery` struct/`#[derive]` line anchors; clarified the `event.rs` variant anchors point at serde-tag lines; re-labelled untracked predecessor docs as local working notes (line anchors dropped); flagged the `RoleViolation` cross-crate import for the PR10-a implementer. Round-2 disposition in §7. **CONVERGED — no further review round.**

---

## 0. Status snapshot (what PR9 left behind)

PR9b dropped the table (`migration 0055`, #759). On `origin/main @ 04d39bdb`:

- **No production `FROM/INTO/UPDATE runtimes` remains** — verified `git grep 'FROM runtimes|INTO runtimes|UPDATE runtimes'` over `crates/calm-truth/src` + `crates/calm-server/src` returns empty. The `runtimes_` matches that survive are dead/test/helper names (`runtimes_recover_harnesses_on_boot`, `runtimes_active_for_kind`, parity-sweep test names), not live SQL.
- **`runtimes_recover_orphans_on_boot` is GONE** (PR9b retired the lease-reaper). The only surviving `CardRuntime` producers are the `RuntimeRepo` projection-read methods over `worker_sessions` **plus** the lifecycle helper `session_start_runtime_tx` (`crates/calm-truth/src/db/sqlite.rs:3257`, returns `CardRuntime` via `RuntimeResult`/`RuntimeTx` import aliases — see §2.C and finding F6).
- **`CardRuntime` is already a pure projection** of `WorkerSession + cards.id`: every read path SELECTs `worker_sessions` and converts via `card_runtime_from_ws_join_row` → `card_runtime_from_session` (`crates/calm-truth/src/runtime_row.rs:100-148`). The three fields `lease_owner` / `lease_until_ms` / `terminal_ref` are ALWAYS `None` — verified: `terminal_ref: None` (runtime_row.rs:88, :214), `lease_owner: None` (runtime_row.rs:93, :219), `lease_until_ms: None` (runtime_row.rs:94, :220). No production code reads them; only a test asserts they are `None` (`crates/calm-truth/src/db/sqlite.rs:6902-6904`).
- **`RuntimeId = String` is already a `pub type` alias**, NOT a newtype (`crates/calm-types/src/runtime.rs:17`). Same precedent exists at `crates/calm-truth/src/runtime_repo.rs:16` (`pub type CardId = String`) and `crates/calm-truth/src/decision_gate.rs:12` (`pub type WorkerSessionRow = WorkerSession`). This materially lowers the Q-Aliases cost.
- **`IngestQuery { pub card_id: String }`** (`crates/calm-server/src/routes/codex.rs:58-61`, `pub struct` at :59 (`#[derive]` at :58), field at :60) is the last bare-`String` wire-typed `card_id` on a hot path. Required, no serde attrs, shared by `/internal/codex/hook` (route reg codex.rs:55, handler `Query` codex.rs:142) and `/internal/claude/hook` (claude.rs:11 import, claude.rs:27 handler `Query`).

### 0.1 The real census (RECOUNTED — methodology stated)

Counts on `origin/main @ 04d39bdb`. **Two distinct methodologies, stated so they reconcile and future re-grounding reproduces them:**

- **Word-boundary identifier count** — `rg -oP "\bIDENT\b" -g '*.rs' crates | wc -l`. This counts *whole-identifier* hits (what a rename actually touches per type name); it excludes prefix/substring inflation (e.g. `RuntimeRepoError` does NOT inflate `RuntimeRepo`).
- **Raw substring count** — `rg -o "STR" -g '*.rs' crates | wc -l`. This counts every textual occurrence including inside longer identifiers; used only for the family-wide `Runtime|runtime_` sweep weight.

| Token | Count | Methodology | Notes |
|---|---|---|---|
| `RunStatus` (word) | **615** | `\bRunStatus\b` | ts-rs wire type; near-twin of `WorkerSessionState` |
| `RuntimeKind` (word) | **330** | `\bRuntimeKind\b` | ts-rs wire type |
| `RuntimeInit` (word) | **144** | `\bRuntimeInit\b` | public struct; constructed in 36 calm-server/tests files (see §0.2) |
| `CardRuntime` (word) | **96** | `\bCardRuntime\b` | the struct + returns (does NOT include `CardRuntimeView`) |
| `RuntimeId` (word) | **59** | `\bRuntimeId\b` | already a `pub type` alias |
| `RuntimeRepoError` (word) | **31** | `\bRuntimeRepoError\b` | public error enum |
| `CardRuntimeView` (word) | **21** | `\bCardRuntimeView\b` | frontend-facing projection — **STAYS** (§2.B) |
| `RuntimeRepo` (word) | **20** | `\bRuntimeRepo\b` | trait name (does NOT include `RuntimeRepoError`) |
| `runtime_get_` (substring) | **287** | `runtime_get_` | trait read methods + call sites |
| `runtime_` (substring) | **2534** | `runtime_` | dominant; concentrated in `runtime_lookup.rs`, `runtime_repo.rs`, `runtime_row.rs` |
| `runtimes_` (substring) | **67** | `runtimes_` | mostly test/helper names, NOT live SQL |
| `Runtime` (substring) | **1076** | `Runtime` | any `Runtime…` identifier prefix |

**Per-crate `Runtime|runtime_` substring weight** (`rg -o "Runtime|runtime_" -g '*.rs' <dir> | wc -l`):

| Dir | Count |
|---|---|
| `crates/calm-server/tests` | **1862** |
| `crates/calm-server/src` | **870** |
| `crates/calm-truth` | **726** |
| `crates/calm-types` | **118** |
| `crates/calm-truth-test-harness` | **19** |
| `crates/calm-proc-supervisor` | **11** |
| `crates/calm-exec` | **4** |
| `crates/calm-codex-bridge` | **0** |
| **repo-wide total** (`rg -o "Runtime\|runtime_" -g '*.rs' crates`) | **3610** |

**(v1 correction):** the v1 per-crate figures "calm-server/src 713, calm-server/tests 52" were wrong — the real raw-substring weights are **870** (src) and **1862** (tests). The `calm-server/tests` weight is ~36× the v1 figure. This is the number that disciplines the §2.C rescope and the PR10-d blast-radius estimate.

**(v3 correction):** the v2 table totalled **3576** over five hand-picked dirs but `rg -o "Runtime|runtime_" -g '*.rs' crates` is **3610** repo-wide; the missing 34 are `calm-truth-test-harness` **19**, `calm-proc-supervisor` **11**, `calm-exec` **4** — now added as rows so PR10-d scope is honest. Some of these are real rename call sites of renamed PUBLIC types (e.g. `calm-truth-test-harness/src/lib.rs:28` imports `RuntimeInit`/`RuntimeKind`/`RuntimeRepo` from `calm_truth::runtime_repo`), so they are MANDATORY churn (§0.2 class), not droppable.

### 0.2 Test-call-site reality (the rescope's real cost)

The v1 framing treated `calm-server/tests` churn as a droppable "blanket internal sweep." That conflates two distinct classes:

- **MANDATORY churn — forced call sites of renamed PUBLIC types.** These compile-break the moment the type is renamed; they MUST land in the same slice. Verified:
  - **`RuntimeInit`** is constructed across **36 `calm-server/tests` files** (`rg -l "RuntimeInit" crates/calm-server/tests` = 36; 116 word-hits in tests), plus 42 struct-literal files repo-wide. Renamed in PR10-d.
  - **`RunStatus`** (615), **`RuntimeKind`** (330), **`CardRuntime`** (96), **`RuntimeRepo`** (20) — pervasive in those same tests as renamed-public-type references. Renamed in PR10-b/c/d.
- **DROPPABLE churn — internal private-fn / local-variable / test-NAME renames** (e.g. a `#[test] fn runtime_status_matrix_golden` or a `let runtime_row = …` local). These are name-stable to the compiler regardless of the public-type rename; renaming them buys near-zero comprehension value and consumes the merge-conflict budget. THESE are what §2.C drops.

So PR10-d's blast radius is **not** "~1200-1500 tokens in 3-4 files." The public-surface renames force test edits across **≥36 calm-server/tests files** (RuntimeInit alone) plus the calm-truth/calm-types definition sites. Honest estimate: the renamed-public-type call-site churn is the dominant cost, an order of magnitude above v1's stated figure. The rescope still *halves* the family by dropping the internal `runtime_*` local/private/test-name sweep, but PR10-d remains a large mechanical PR (see §5 PR10-d).

### 0.3 Frozen legacy wire vocabulary (NOT in PR10 scope — BC-protected)

**Even after PR10, "Runtime" vocabulary REMAINS by design on the wire.** These are FROZEN-VECTOR / BC-protected and MUST NOT be renamed in PR10 — renaming any of them is a wire break (and the event-serde-tag renames would also be a vector-gate concern):

- **`Event::RuntimeStarted` / `Event::RuntimeStatusChanged` / `Event::RuntimeSuperseded`** event variants, with serde tags **`runtime.started` / `runtime.status_changed` / `runtime.superseded`** (`#[serde(rename=…)]` tag lines `crates/calm-types/src/event.rs:389/397/404`; the variant idents are on the following lines :390/:398/:405; `kind_tag()` strings at :968-970). The anchors above point at the serde-tag lines — the frozen-wire-relevant part. These tags are the persisted/streamed event vocabulary — renaming the serde tag breaks every consumer and every stored event.
- **`CardRuntimeView`** (`crates/calm-types/src/model.rs:435`) exposed as **`Card.runtime`** (`model.rs:482`, `#[ts(optional)] pub runtime: Option<CardRuntimeView>`). It is the only frontend-facing projection, with computed-only fields (`source`, `thread_status`) absent from `WorkerSession`. The web viewer consumes it by this exact name (`web/src/wave-fs-viewers/builtins/card-runtime-viewer.tsx:1` imports `CardRuntimeView`).

**Stated goal (narrowed from v1):** PR10 retires the **runtimes-TABLE-era repo/struct/identifier vocabulary** (the `RuntimeRepo` trait, the `RuntimeInit`/`RuntimeRepoError` structs, the `runtime_*` identifier family, the `CardRuntime` internal struct, the duplicate `RunStatus` enum). The event-wire `Runtime*` variant names, their `runtime.*` serde tags, and `CardRuntimeView`/`Card.runtime` are **deliberately frozen** and explicitly out of scope. This reinforces the rescope: "no more Runtime vocabulary" was never the achievable goal — "no more *dropped-table-era* Runtime vocabulary" is.

### 0.4 Wire-typed (ts-rs) members — the cost amplifier for component C

These live in `crates/calm-types/src/runtime.rs` and ALL carry `#[ts(export, export_to = "web/src/api/generated-events.ts")]` (e.g. `RunStatus` at runtime.rs:42-44): `RuntimeKind`, `AgentProvider`, `RunStatus`, `TerminalRunRef`, `CardRuntime`. A Rust-side rename of any of these is NOT shielded by a Rust alias — ts-rs emits the *real* Rust name, so the THREE CI-gated generated artifacts change in lockstep (see §0.5).

**Sharp finding:** `WorkerSessionState` (`crates/calm-types/src/worker.rs:360`) is NOT ts-rs exported — the file header says "None of these types are TS-exported … the new vocabulary stays off the wire until a later PR deliberately surfaces it" (worker.rs:8-10), and the enum itself has no `#[ts]` derive (worker.rs:358-368). So **retiring `RunStatus` in favor of `WorkerSessionState` forces `WorkerSessionState` to GAIN `TS`/`#[ts(export)]` AND renames the web wire enum `RunStatus` → `WorkerSessionState`.** Variant *names* are identical (serde `snake_case`, both 7 variants) so the JSON is byte-identical, but the TS *type name* and import surface churn across the frontend. This is a genuine wire-surface cost, not pure aesthetics.

### 0.5 CI-gated regen targets (the drift gate)

The OpenAPI/types-drift CI job runs `git diff --exit-code -- web/src/api/openapi.json web/src/api/generated.ts web/src/api/generated-terminal.ts web/src/api/generated-events.ts web/src/editor/types/` (`.github/workflows/ci.yml:528`). The renamed wire types live in THREE of these artifacts (verified **word-boundary** hits, `rg -oP '\bX\b'`): **`web/src/api/generated-events.ts`** (`RunStatus`×6, `CardRuntime`×1, `RuntimeKind`×4), **`web/src/api/generated.ts`** (`RunStatus`×2, `RuntimeKind`×2), **`web/src/api/openapi.json`** (`RunStatus`×2, `RuntimeKind`×2). `web/src/api/generated-terminal.ts` is CLEAN (0 hits) and `web/src/editor/types/` is unaffected. **`WaveFsRunStatus` is a SEPARATE, unrelated type** (×3 in each of those three artifacts) that must NOT be renamed — earlier substring counts (`RunStatus`×9/×5/×5) were inflated by it; the PR10-c `RunStatus`→`WorkerSessionState` rename MUST use word-boundary matching so it never touches `WaveFsRunStatus`. (Likewise `CardRuntime`'s substring ×3 in events.ts is inflated by the frozen `CardRuntimeView` ×2 — word-boundary `CardRuntime` is ×1.) **Any slice renaming a wire-typed member MUST regenerate and commit all three of generated-events.ts + generated.ts + openapi.json, or the drift gate fails.**

---

## 1. Scope & Goal

Three in-scope components, per #762:
- **A. `IngestQuery.card_id` type migration** (RATIFY-6) — `String` → typed. The one load-bearing wire decision.
- **B. `CardRuntime` struct disposition** (RATIFY-5) — collapse into `WorkerSession` view, or rename to a projection name.
- **C. Identifier mass-rename** — the `Runtime*`/`runtime_*` family (table-era vocabulary only; wire vocab frozen per §0.3).

Out of scope: any behavioral change, any new ingest wire (session-id POST), `card_mcp_tokens` retirement (separate cleanup), frontend CardRoleCache UI (→ PR9c), and the frozen event-wire `Runtime*` names + `CardRuntimeView`/`Card.runtime` (§0.3).

---

## 2. Value / complexity audit (per `feedback_challenge_before_implement`)

The mandate: do NOT assume the full rename is worth it because the issue lists it. Each component gets proceed / rescope / drop.

### A. `IngestQuery.card_id: String` → `Option<CardId>` — **PROCEED (rescoped to type-safety only, no new wire)**

- **Value:** Today `card_id` is an un-validated bare string; the empty-string error case is implicit (a present-but-empty `?card_id=` propagates to the role-gate `RoleViolation::EmptyAiCardId` rejection at `crates/calm-truth/src/role_gate.rs:71` variant / :123-126 guard). RATIFY-6 pre-committed (`docs/_design-679-pr9a-v2-current.md`, RATIFY-6 / C-G — local working notes, not git-tracked) to making it explicit *before* any session-id wire is added, per `feedback_required_over_option`. Making it `Option<CardId>` with an explicit empty→reject closes the last implicit-typing hole on a hot wire and is the documented prerequisite RATIFY-6 deferred to PR10.
- **Cost:** LOW. One struct (codex.rs:58-61), two handler forwards, one shared helper signature (`ingest_provider_hook` takes a resolved card-id string). No new wire, no migration, no vector edit (Q-MCP-vector). All producers in-repo (see §4).
- **Recommendation: PROCEED**, but **rescope away from "retire the `?card_id=` wire."** Claude is hard-pinned to `?card_id=` forever (`crates/calm-server/src/routes/claude_cards.rs:227-228`), so "retirement" is impossible. PR10 retypes the field for safety; the `?card_id=` form stays permanent. State this explicitly so reviewers don't expect a wire removal.

### B. `CardRuntime` struct — **RESCOPE: keep as a thin renamed projection; do NOT collapse into `WorkerSession` in PR10**

- **Value of full collapse:** removes one redundant indirection layer (`WorkerSession` → `CardRuntime` → consumers). Genuine but modest — `CardRuntime` is already a derived view, so the indirection costs *reads*, not correctness.
- **Cost of full collapse:** ~12 `RuntimeRepo` method return-type changes + impls, the `runtime_row.rs` conversion helpers, the `kind`-deriving sites replaced by a shared `kind_from_session_identity(provider, contract)` helper, an open design question on how readers carry `card_id` (no `WorkerSession` column for it — needs a `SessionForCard { session, card_id }` wrapper), and reconciling `RuntimeId String` vs `WorkerSessionId` at ~30 call sites. That is a behavioral-adjacent refactor of the truth-read layer landing in the SAME PR as a large mass-rename — high merge-conflict surface, high review burden, and it muddies the "no behavioral change" contract of PR10.
- **`CardRuntimeView` stays regardless** (§0.3) — it is the only frontend-facing projection (`model.rs:435`, embedded as `Card.runtime` model.rs:482, rendered by `web/src/wave-fs-viewers/builtins/card-runtime-viewer.tsx`), with computed-only fields absent from `WorkerSession`.
- **Recommendation: RESCOPE.** In PR10, treat `CardRuntime` as part of the **C rename** — rename it to a projection name (`WorkerSessionProjection`, or keep `CardRuntime`) and drop the 3 always-`None` dead fields (`lease_owner`, `lease_until_ms`, `terminal_ref`). The field-drop is a pure deletion with **no production call-site cost** (no consumer reads them), BUT it forces (i) updating the test that asserts they are `None` (`sqlite.rs:6902-6904`) and (ii) regenerating the ts-rs/OpenAPI artifacts (the dropped fields disappear from `generated*.ts`). **Defer the structural collapse** (return `WorkerSession`+`card_id` wrapper, kill the conversion layer) to a separate, later, behavioral-review PR if still wanted. Rationale: the collapse's value is "remove indirection," reversible to pursue later; bundling it with the rename inflates blast radius for marginal gain. Honors `feedback_challenge_before_implement`.

### C. Identifier mass-rename — **RESCOPE: rename the wire-typed + public-surface members; DROP the blanket internal sweep**

This is the component the audit mandate exists for. Breaking it down by *what each rename buys*:

- **High value (PROCEED):** the **wire-typed types** (`RunStatus`, `RuntimeKind`, `CardRuntime`, `TerminalRunRef`; `AgentProvider` already neutral) and the **`RuntimeRepo` trait + its public method names** (`runtime_get_*` etc, ~287 substring). These are the names a reader of the codebase trips over — "runtimes" is now a *dropped table*, so a `RuntimeRepo` trait that operates over `worker_sessions` is actively misleading. **Renaming `RuntimeRepo` → `WorkerSessionProjectionRepo` (NOT `SessionRepo` — see R3, the collision is real and confirmed).** Note `RuntimeRepo` is **not pure-reads**: it is the card-runtime projection facade over `worker_sessions` carrying read methods (`runtime_get_*`, returning `CardRuntime`) **PLUS a few legacy status/complete mutators** (`runtime_set_status_for_card` at runtime_repo.rs:135, `runtime_complete_for_card`/`runtime_complete_for_terminal` at runtime_repo.rs:181/197, the `runtime_*_tx` start/supersede/bind family) — the `session_projection_*` method prefix (R3) covers both reads and these mutators. Renaming `RunStatus` → `WorkerSessionState` removes a genuine duplicate type (twin confirmed, worker.rs:355-357).
- **Low value (DROP / defer):** the long tail of **internal-only** `runtime_*` locals, private fn names, test NAMES, and module-internal helpers that no external reader or wire touches. Renaming these delivers near-zero comprehension gain over the public-surface rename while consuming the entire review/merge-conflict budget against any in-flight work. `feedback_challenge_before_implement` flags this as "pure aesthetic churn." (NOTE: this is the *droppable* class from §0.2 — call sites of renamed PUBLIC types are NOT in this droppable bucket; they are mandatory.)
- **Trait-adjacent public surface that MUST move with `RuntimeRepo`:**
  - `RuntimeRepoError` (public enum, runtime_repo.rs:20-24; 31 word-hits).
  - `RuntimeInit` (public struct, runtime_repo.rs:51-67; 144 word-hits, 36 test files).
  - `ThreadAttribution.runtime_id` — a public field `pub runtime_id: RuntimeId` (runtime_repo.rs:43-44). (v1 omitted this.)
  - the public type aliases **`pub type Result<T>`** and **`pub type Tx<'a>`** (runtime_repo.rs:17-18, over `RuntimeRepoError`/`Sqlite Transaction`) — and their **local import-renames** `Result as RuntimeResult` / `Tx as RuntimeTx` at `runtime_lookup.rs:10` and `db/sqlite.rs:48,50`. (v1 wrongly listed `RuntimeResult`/`RuntimeTx` as declared types; they are import aliases, see F9.)
  - `runtimes_recover_harnesses_on_boot` (runtime_repo.rs:136) and its single cross-crate caller `crates/calm-server/src/harness/mod.rs:195` (`repo.runtimes_recover_harnesses_on_boot().await?`) — must rename in lockstep.
- **`RunStatus` retirement specifics:** because `WorkerSessionState` is not currently ts-rs exported (§0.4), this rename adds `TS`/`#[ts(export)]` to `WorkerSessionState`, deletes `RunStatus`, regenerates all three CI-gated artifacts (§0.5), and updates web imports — including the HAND-WRITTEN `web/src/api/schemas.ts` (`runStatusSchema` zod enum at :111, `type RunStatus = z.infer<…>` at :120), which mirrors the wire vocabulary by name and will churn. The 1:1 variant mapping means JSON is unchanged — **only the type name moves.**
- **Recommendation: RESCOPE to a "public-surface + wire-typed" rename.** Rename: `RuntimeRepo`→`WorkerSessionProjectionRepo`, `RuntimeRepoError`/`RuntimeInit` (public structs), the `pub type Result`/`Tx` aliases + their import-renames, `ThreadAttribution.runtime_id`, `RunStatus`→`WorkerSessionState` (de-dup), `RuntimeKind`→ session-kind name, `CardRuntime`→ projection name, the `runtime_get_*`/`runtime_set_*`/`runtimes_*` trait methods (new prefix `session_projection_*`, see R3), the `runtime_lookup.rs`/`runtime_repo.rs`/`runtime_row.rs` *module names*. **Do NOT** sweep every internal `runtime_`-prefixed local/private fn/test name — leave those as a follow-up or drop them. Net: drops roughly the internal half of the `runtime_*` substring mass; the renamed-public-type call-site churn (≥36 test files for `RuntimeInit` alone, §0.2) is the irreducible floor.

**Net audit verdict:** A proceed (cheap, mandated). B rescope (rename + dead-field drop now; collapse deferred). C rescope (public/wire surface yes; blanket internal sweep dropped). This drops the internal-sweep half of the family while delivering all the *comprehension* value (no more dropped-table "Runtime" repo/struct vocabulary) and the one *correctness/typing* value (explicit `Option<CardId>`). It does NOT achieve "zero Runtime vocabulary" — the event-wire names are frozen (§0.3).

---

## 3. Resolution of the 6 open questions

### Q-BC — hard-cutover vs accept-both? → **HARD-CUTOVER is safe; keep permanent dual-ACCEPT of `?card_id=` (not a deprecation window)** · confidence **HIGH**

The framing in #762 ("rides the MCP tools/call wire path codex daemon → calm-server") is **inaccurate** and this is the single most important correction in the doc. `IngestQuery` is an **HTTP query-param struct** for the loopback routes `/internal/codex/hook` and `/internal/claude/hook` (`codex.rs:55,142`; `claude.rs:27`), deserialized via `axum::extract::Query`. It has **zero presence in the MCP transport** (`rg IngestQuery crates/calm-server/src/mcp_server` empty — verified) and zero presence in `tests/vectors/` (verified empty).

The producers are **all in-repo and shipped atomically with the server** (see §4 for the full producer contract including the `NEIGE_HOOK_URL` override). **The external codex daemon is NOT a producer of this field** (it resolves MCP identity server-side). So there is no out-of-band release coupling; hard-cutover of the *Rust type* is safe. **BUT** the bytes on the wire must not change: every producer emits a bare `?card_id=<str>`, and the server must keep deserializing that to `Some(CardId(str))` byte-identically. This is not a deprecation-then-removal window — it is a **permanent dual-accept** (claude can never drop `?card_id=`, claude_cards.rs:227-228). So "accept-both" in the temporal sense is moot: there is no second wire form to time out; the existing form is permanent and a hard-cutover of the *internal type representation* is what ships.

**Decision:** hard-cutover the type (`String` → `Option<CardId>`), update the consumer + tests in one PR, keep the `?card_id=` HTTP form permanent. No rollout window. No accept-both parser (no JSON body; it's a query param).

### Q-Enum — `Option<CardId>` vs 3-variant enum? → **`Option<CardId>`** · confidence **HIGH**

(1) The live value-space is exactly one inhabitant: a non-empty card-id string (the bridge skips the POST entirely when it can't resolve — `resolve_card_id_for_hook` returns `None` at `crates/calm-codex-bridge/src/main.rs:85-117`, and the caller skips on `None` at main.rs:68-73). (2) A 3-variant enum `CardScoped|DaemonTrust|Unbound` would create two variants with **zero producers**. (3) `DaemonTrust` specifically collides with the unrelated MCP-auth `DaemonTrust` concept (`docs/_679-body-new.md` — local working notes, not git-tracked). (4) `feedback_required_over_option` says required cross-path fields should be typed required, not `Option`+default — but here `card_id` is *genuinely becoming optional on the wire* the moment a future session-id POST omits it, so `Option<CardId>` is the honest model, **provided `None` is rejected loudly, never silently defaulted.** That clause is the bridge to Required-Over-Option compliance: `Option` is acceptable here only because the absent case is a hard error, not a default.

**Empty/absent edge (the one real subtlety) — corrected from v1:** `axum`/`serde_urlencoded` deserializes a present-but-empty `?card_id=` to `Some(CardId(""))`, NOT `None`. The two error cases today are **DIFFERENT status codes**:
- **present-but-empty `?card_id=`** → `Some("")` (with current `card_id: String`, also `""`) → reaches the role gate → `RoleViolation::EmptyAiCardId` (`crates/calm-truth/src/role_gate.rs:71` variant, returned at :123-126) → surfaced as `CalmError::Forbidden(violation.to_string())` (`crates/calm-truth/src/db/sqlite.rs:5897`/`:6143`) → **403** (`crates/calm-server/src/error.rs:175` maps `Forbidden`→`StatusCode::FORBIDDEN`).
- **absent (no `?card_id` param at all)** → axum `Query<IngestQuery>` missing-required-field extraction failure → **400 Bad Request**.

Under the proposed `Option<CardId>`, mapping BOTH `None` (absent) and `Some(CardId(""))` (empty) to the same empty-reject path **unifies the absent case from 400 → 403**. This is a **deliberate, latent, unobserved change**: no producer or test exercises the absent case (all producers always emit `?card_id=`; see §4). It is BC-safe in practice (no caller hits it) but it is NOT "preserving current behavior" for the absent branch — call it out honestly. The empty branch is unchanged (403 either way).

The migration MUST make the empty-reject explicit and **preserve 403/`EmptyAiCardId` semantics** (NOT a 400 `BadRequest`). See §4 step 2 for the corrected error-construction.

### Q-CardRuntime — collapse into `WorkerSession` vs rename to projection? → **Rename to a projection + drop 3 dead fields; DEFER the structural collapse** · confidence **MEDIUM**

See §2.B. I rescope to rename-now/collapse-later because the collapse is a truth-read-layer refactor (the `card_id` carrying question, the ~30 `RuntimeId` reconciliation sites) that does not belong in the same PR as a mass-rename, and its value (remove indirection) is fully preservable for a later behavioral PR. Keep `CardRuntimeView` untouched (frozen, §0.3). **Confidence MEDIUM** — judgment call; if RATIFY-5 is read as *mandating* the collapse, escalate (§8 FORK-2).

### Q-Slice — split (a) bridge+types BC-critical vs (b) mechanical rename, or one atomic PR? → **SPLIT** · confidence **HIGH**

Split. (a) `IngestQuery` is a wire-touching type change with a real (if low) BC surface and deserves a focused review; (b) the renames are mechanical churn with a merge-conflict profile but no BC. Bundling means the rename's noise buries the one wire change a reviewer must scrutinize. See §5 slice plan (a/b/c/d).

### Q-Aliases — `pub type RuntimeRepo = …;` deprecation runway vs hard rename? → **HARD rename, no alias runway** · confidence **HIGH**

No alias runway. (1) The entire surface is **in-repo** — no external Rust consumers a deprecation alias would protect; an alias only adds a second name to grep for. (2) For the **wire-typed** members an alias is *useless* anyway: ts-rs emits the real Rust name regardless of a Rust-side `pub type` alias, so `pub type RuntimeKind = WorkerSessionKind` would NOT keep the old TS name — web must change in lockstep no matter what. Hard rename, web in the same PR. (Note `RuntimeId`/`CardId` are *already* `pub type X = String` aliases at runtime.rs:17 / runtime_repo.rs:16 — pre-existing String aliases, not deprecation shims; rename or inline them.)

### Q-MCP-vector — does the wire change need the vector-gate marker? → **NO** · confidence **HIGH**

Confirmed against `.github/workflows/ci.yml` (job `frozen-vectors` begins at :128; gate spans ~:128-175). The gate guards `VEC_DIR='crates/calm-server/tests/vectors/'` and requires the commit-message marker **`FROZEN-VECTOR-CHANGE:`** — the actual `git log -1 --format=%B "$sha" | grep -q 'FROZEN-VECTOR-CHANGE:'` check is at **ci.yml:166** (v1 cited :149, wrong). The vectors capture serde shapes of `ActorId`/`EventScope`/`Event`, not `IngestQuery`; `rg -i ingest crates/calm-server/tests/vectors/` is empty (verified). Changing `IngestQuery.card_id` edits `routes/codex.rs` only and touches no vector file, so the gate stays quiet.

**One caveat to enforce at brief time:** the gate WOULD engage if a slice changed the serialized JSON of `Event::CodexHook`/`Event::ClaudeHook.card_id`. That field is *already* a `#[serde(transparent)]` `CardId` (`crates/calm-types/src/ids.rs:56-59`), so a CardId-preserving change emits identical bytes. **Constraint: PR10 must NOT wrap `Event::CodexHook.card_id` in `Option` or an adjacently-tagged enum.** If a future slice must edit a vector, it carries `FROZEN-VECTOR-CHANGE:` in the squash-merge commit message AND bumps `EXPECTED_VECTOR_COUNT` (51, `frozen_gate_vectors.rs:59`) / `EXPECTED_PRINCIPAL_DELTA_VECTOR_COUNT` (32, `frozen_gate_vectors_transport.rs:47`) in the same commit. **Also note (§0.3): renaming the `runtime.*` event serde tags is independently forbidden as a wire break.**

---

## 4. BC strategy for `IngestQuery` (load-bearing)

**Surface (the honest one, correcting #762):** HTTP query string `?card_id=<value>` on `POST /internal/codex/hook` and `POST /internal/claude/hook`. Server-side `axum::Query<IngestQuery>` extractor. NOT MCP, NOT a JSON body.

**Producers that must keep emitting `?card_id=<str>` after the change — the full BC contract:**
1. **bridge live POST** — `crates/calm-codex-bridge/src/main.rs:388-394`: `format!("{}{}?card_id={}", base.trim_end_matches('/'), provider.endpoint(), url_encode(card_id))`.
2. **`NEIGE_HOOK_URL` override (4th producer, v1 missed it)** — at `crates/calm-codex-bridge/src/main.rs:65-67` the bridge reads `NEIGE_HOOK_URL` (non-empty), and at `post_hook` (main.rs:388) `hook_url.map(String::from).unwrap_or_else(|| format!(…?card_id=…))` — i.e. **when set, the override REPLACES the FULL URL verbatim, including (or omitting) the `?card_id=` query; the bridge does NOT append `?card_id=` to it.** BC consequence: an operator-supplied override that omits `card_id` → **currently** (`card_id: String`, required) the absent query param fails `axum::Query<IngestQuery>` extraction → **400 Bad Request** *before* the handler body (codex.rs:142), so it never reaches the role gate; **post-PR10** (`Option<CardId>`) the same absent case → `None` → empty-reject → **403** (the absent→403 unification of §3 Q-Enum). Under hard-cutover that is **acceptable fail-loud** — an override that drops card_id was already broken (400 today, 403 after); both are loud errors, neither is silent. Document the override as part of the contract so reviewers know the server cannot assume `?card_id=` is always present on the bridge path.
3. **server fallback-replay POST** — `crates/calm-server/src/lib.rs:373`: `format!("{}?card_id={}", provider.endpoint(), url_encode(card_id))`. The on-disk `HookFallbackRecord { card_id: String }` (`crates/calm-server/src/lib.rs:178-179`, consumed at :281 `post_hook_fallback(base_url, provider, &record.card_id, &body)`) need NOT change — the server rebuilds the URL from it (lib.rs:373).
4. **claude spawn-time hardcoded URL** — `crates/calm-server/src/routes/claude_cards.rs:227-228`: `format!("{}/internal/claude/hook?card_id={}", …)`. Claude is hard-pinned to `?card_id=` permanently.

**The external codex daemon is NOT a producer** (resolves MCP identity server-side).

**Target wire-shape:** unchanged on the byte level. `?card_id=card-xyz` stays. Only the Rust *type* of the deserialized field changes.

```rust
// crates/calm-server/src/routes/codex.rs
#[derive(Debug, Deserialize)]
pub struct IngestQuery {
    pub card_id: Option<CardId>,   // was: card_id: String
}
```

**Migration mechanics:**
1. `CardId` is `#[serde(transparent)]` over `String` (ids.rs:56-59), so `?card_id=card-xyz` deserializes to `Some(CardId("card-xyz"))` byte-identically — no custom deserializer needed.
2. **Explicit empty-normalization in both handlers — preserving 403/`EmptyAiCardId`, NOT a 400.** `EmptyAiCardId` is a `RoleViolation` variant (`crates/calm-truth/src/role_gate.rs:71`), NOT a `CalmError` variant; the 403 is produced via `CalmError::Forbidden(violation.to_string())`. Two compliant approaches:
   - **(preferred) construct the Forbidden directly:** map present-but-empty / absent → reject with the same string the role gate would emit:
     ```rust
     let card_id = q.card_id
         .filter(|c| !c.as_str().is_empty())
         .ok_or_else(|| CalmError::Forbidden(RoleViolation::EmptyAiCardId.to_string()))?;
     ```
   - **(equivalent) let empty fall through** to the existing role-gate path (don't pre-empt it) — the role gate already rejects `AiCodex(CardId(""))` with 403. Either way the **status code is 403** and the violation string is `EmptyAiCardId`.
   **PR10-a implementer note (import cost):** the *preferred* form names `RoleViolation` inside `calm-server`'s `routes/codex.rs`, which currently imports `RoleViolation` nowhere — it needs a new `use` (either `use crate::role_gate::RoleViolation;` via the local re-export `crates/calm-server/src/role_gate.rs:1` `pub use calm_truth::role_gate::*;`, or `use calm_truth::role_gate::RoleViolation;` directly). The *equivalent* fall-through form needs **zero new imports** (empty just reaches the existing role-gate path). Recommendation unchanged (both are listed); flagging only so the implementer expects the cross-crate import on the preferred path.
   This makes the currently-implicit empty path explicit (RATIFY-6 / Required-Over-Option). **Behavior note (§3 Q-Enum):** this also makes the *absent* case 403 (was 400) — deliberate, latent, BC-safe (no producer hits absent). The empty case is unchanged (403).
3. The internal helper keeps taking the resolved `&str`/`String` — the typed-ness is layered at the boundary; the idempotency-key input (`hook_idempotency_key(provider, &card_id_str, &payload)`, codex.rs:162) and the `card_get(&card_id_str)` lookup key (codex.rs:195) use the **exact same string with NO trim/normalization**, to preserve dedupe + replay parity. **HARD CONSTRAINT: any normalization beyond empty-reject (trim, lowercase, etc.) breaks idempotency and fallback replay.**
4. Producer call sites are NOT touched — producers emit the *wire* (a query string), only the *consumer* type changes. The only Rust churn is the struct + the two handlers + any test that constructs `IngestQuery { card_id: "…".to_string() }` directly → `Some(CardId(…))`.

**Rollout window:** NONE. No external producer, no wire-byte change, all producers ship atomically with the server. The `?card_id=` form is **permanent** (not deprecated) because claude is hard-pinned to it.

**On-disk fallback compatibility:** old `HookFallbackRecord` files written pre-PR10 carry `card_id: String` (lib.rs:178-179, struct unchanged) and replay by the server rebuilding `?card_id=` (lib.rs:373) — that path is unchanged, so old fallback files replay against the new server. Confirm with a test that replays a pre-PR10 fallback fixture.

---

## 5. Slice plan

Ordered, each independently reviewable and (except where noted) reversible. Sizes are rough.

**PR10-a — `IngestQuery.card_id` → `Option<CardId>` + explicit empty-reject (403/EmptyAiCardId)** · ~80 LOC · **REVERSIBLE** (pure type revert; no migration, no wire change) · **risk: LOW**
- Scope: `routes/codex.rs:58-61` struct; both handlers' empty-normalization preserving **403** (§4 step 2); tests constructing `IngestQuery`; one fallback-replay regression test (pre-PR10 fixture). No producer changes, no vector edit, no `FROZEN-VECTOR-CHANGE:` marker.
- Lands the design doc (`docs/_design-679-pr10.md`) per workflow.
- Gate: confirm `Event::CodexHook.card_id` serde-transparent CardId is untouched (else vector gate trips — §3 Q-MCP-vector).

**PR10-b — `CardRuntime` projection rename + drop 3 dead fields** · ~150 LOC + test/regen churn · **REVERSIBLE** · **risk: LOW-MED**
- Scope: rename `CardRuntime` struct (to a projection name) + drop `lease_owner`/`lease_until_ms`/`terminal_ref` (always-`None`). `CardRuntimeView` UNTOUCHED (frozen, §0.3).
- **Mandatory churn:** the `CardRuntime`-asserting test at `sqlite.rs:6902-6904` (drops the 3 `is_none()` asserts), all `\bCardRuntime\b` call sites (96 word-hits incl. `session_start_runtime_tx` return at sqlite.rs:3257), and **regenerate all three CI-gated artifacts** — generated-events.ts (CardRuntime×1), generated.ts, openapi.json (§0.5) — the dropped fields also disappear from the generated TS.
- Risk: ts-rs/OpenAPI regeneration drift; verify `cargo` regen + web typecheck.

**PR10-c — `RunStatus` → `WorkerSessionState` de-dup** · ~615 Rust word-hits + TS sites · **REVERSIBLE** · **risk: MED** (largest single-type blast radius)
- Scope: delete `RunStatus`, route all sites to `WorkerSessionState`, ADD `TS`/`#[ts(export, export_to="…generated-events.ts")]` to `WorkerSessionState` (worker.rs:358-368), regenerate **all three** CI-gated artifacts (§0.5), and update web consumers: `generated-events.ts` (`\bRunStatus\b`×6) + `generated.ts` (×2) + `openapi.json` (×2), the HAND-WRITTEN `web/src/api/schemas.ts` (`runStatusSchema` :111, `type RunStatus` :120, used at :125/:234/:243-244), and any `wave-fs-viewers/{schemas.ts,chips.tsx}` that name `RunStatus`. **Match on word boundary — do NOT touch the unrelated `WaveFsRunStatus` type** (§0.5). JSON unchanged (1:1 snake_case variants); only the TS type *name* moves.
- **NOT affected (name-stable, verified):** `web/src/wave-fs-viewers/builtins/card-runtime-viewer.{tsx,test.tsx}` import `CardRuntimeView` (the frozen type, model.rs:435), not `CardRuntime`/`RunStatus` — they do not churn from this slice.
- Risk: web lockstep; the variant-order difference (RunStatus declares `Failed`(runtime.rs:50)-before-`Exited`(:51); WorkerSessionState declares `Exited`(worker.rs:365)-before-`Failed`(:366)) is name-keyed serde so JSON is byte-identical, BUT **the generated-TS union may reorder → snapshot/diff churn** — flag for reviewers and re-run the snapshot.

**PR10-d — `RuntimeRepo`/`RuntimeKind`/`runtime_*` public-surface + module rename** · large (≥36 test files for `RuntimeInit` alone; see §0.2) · **REVERSIBLE** (mechanical) · **risk: MED** (merge-conflict against in-flight work, NOT correctness)
- Scope (public surface + module names):
  - `RuntimeRepo` → **`WorkerSessionProjectionRepo`** (R3; distinct from existing `SessionRepo`). Update the supertrait list `Repo: RouteRepo + RepoSyncDomainRaw + RuntimeRepo + SessionRepo` (db/mod.rs:977) and `RouteRepo: … + RuntimeRepo` (db/mod.rs:965).
  - trait methods → new prefix **`session_projection_*`** (R3; collision-free): `runtime_get_active_by_thread`→`session_projection_active_by_thread`, `runtime_get_active_by_session`→`session_projection_active_by_session`, `runtime_get_active_for_card`→`session_projection_active_for_card`, `runtime_get_projectable_for_card(s)`→`session_projection_projectable_for_card(s)`, `runtime_active_shared_thread_attribution`→`session_projection_active_shared_thread_attribution`, `runtimes_active_for_kind`→`session_projection_active_for_kind`, `runtime_get_by_id`→`session_projection_by_id`, `runtime_set_status_for_card`→`session_projection_set_status_for_card`, `runtime_complete_for_card`→`session_projection_complete_for_card`, `runtime_complete_for_terminal`→`session_projection_complete_for_terminal`, `runtimes_recover_harnesses_on_boot`→`session_projection_recover_harnesses_on_boot` (+ its cross-crate caller `crates/calm-server/src/harness/mod.rs:195`).
  - public types: `RuntimeRepoError`, `RuntimeInit` (≥36 test files), `ThreadAttribution.runtime_id` field, the `pub type Result`/`Tx` aliases (runtime_repo.rs:17-18) + import-renames `Result as RuntimeResult`/`Tx as RuntimeTx` (runtime_lookup.rs:10; sqlite.rs:48,50; and the `session_start_runtime_tx` signature at sqlite.rs:3257 that uses them).
  - `RuntimeKind` → session-kind name (ts-rs regen, all three artifacts §0.5).
  - module names `runtime_lookup.rs`/`runtime_repo.rs`/`runtime_row.rs`.
  - inline/rename the `RuntimeId`/`CardId` `pub type` String aliases.
- **DROP from scope:** the blanket sweep of internal-only `runtime_`-prefixed locals/private fns/test NAMES (§0.2 droppable class). Leave as optional later cleanup.
- **Mandatory churn floor (§0.2):** renamed PUBLIC types force call-site edits across ≥36 calm-server/tests files (`RuntimeInit`) plus the calm-truth/calm-types definition + impl sites — this is the irreducible cost, an order of magnitude above v1's "~1200-1500 tokens."
- Risk: mechanical; do LAST so it rebases over a/b/c; coordinate timing against any in-flight `runtime_*`-touching branch (#741 follow-ups).

**Slice ordering rationale:** a first (isolates the one wire change for focused review); b/c next (wire-typed renames that force the three-artifact regen, kept separate so each TS lockstep is reviewable); d last (the big mechanical churn rebases over everything else). Atomic alternative rejected per Q-Slice.

**Note on B-collapse:** the deferred `CardRuntime`→`WorkerSession` *structural* collapse, if pursued, is a SEPARATE later PR (PR10-e/PR11) with its own behavioral review — not part of this plan.

---

## 6. Risk register

- **R1 — empty/absent edge changes behavior.** If the migration maps empty/absent to a silent no-op instead of reject, hooks would be dropped silently. Also: the absent case silently moves 400→403 (deliberate, §3 Q-Enum). **Mitigation:** explicit empty→reject preserving **403/`EmptyAiCardId`** (§4 step 2); regression test asserting `?card_id=` (empty) still **403 Forbidden** with rollback; note the absent 400→403 unification in the PR description. Severity: HIGH if missed, easy to get right.
- **R2 — idempotency/replay parity break.** Any trim/normalize of `card_id` beyond empty-reject changes the idempotency-key input (codex.rs:162) and `card_get` key (codex.rs:195), breaking dedupe + fallback replay. **Mitigation:** hard constraint "no normalization beyond empty-reject"; reuse exact string. Severity: HIGH.
- **R3 — `RuntimeRepo` → `SessionRepo` would NOT compile (CONFIRMED collision; RESOLVED in-design).** `pub trait SessionRepo` already exists (`crates/calm-truth/src/session_repo.rs:39`) with lifecycle/mutation methods (`session_insert_tx`, `session_get`, `sessions_nonterminal`, `session_set_liveness`, `session_record_activity[_by_thread]`, `session_state_transition_tx`, `session_commit_exit`, `session_list_by_wave`, `dead_root_candidates`); `pub trait RuntimeRepo` (`runtime_repo.rs:70`) is the CardRuntime-projection facade over `worker_sessions` — it carries projection READS (`runtime_get_*`) PLUS a few legacy status/complete MUTATORS (`runtime_set_status_for_card`, `runtime_complete_for_card`/`_for_terminal`, the `_tx` start/supersede/bind family); it is not pure-reads. **Both are listed in the `Repo` supertrait** (`db/mod.rs:977`: `Repo: RouteRepo + RepoSyncDomainRaw + RuntimeRepo + SessionRepo`) and `RuntimeRepo` is also in `RouteRepo` (`db/mod.rs:965`), both impl'd on `SqlxRepo`. Renaming `RuntimeRepo`→`SessionRepo` is a hard duplicate-trait compile error. **RESOLUTION:** rename `RuntimeRepo` → **`WorkerSessionProjectionRepo`** (a DISTINCT name, do NOT merge into `SessionRepo`). Rationale: a merge would fold the card-runtime projection surface (its reads plus the few legacy status/complete projection-mutators) into the full session-lifecycle trait and inflate blast radius for no behavioral gain — counter to the value-audit philosophy; the projection/lifecycle split is worth keeping. Method prefix `runtime_*`→**`session_projection_*`** (verified collision-free: `rg "session_projection_" crates` is empty, and the prefix does not clash with any `SessionRepo` `session_*` method — the `_projection_` infix disambiguates). This makes the renamed trait COHERENT (a `…ProjectionRepo` whose `session_projection_*` methods read — and run the legacy status/complete writes — over `worker_sessions`). No longer a blocker; mechanically resolvable in PR10-d. Severity: was BLOCKER, now resolved.
- **R4 — vector gate trips unexpectedly.** Only if a slice changes `Event::CodexHook/ClaudeHook.card_id` serialization, or renames the frozen `runtime.*` event serde tags (§0.3 — independently forbidden). **Mitigation:** keep that field serde-transparent CardId; do not touch event variants/tags. Severity: LOW (caught by CI).
- **R5 — TS lockstep / 3-artifact OpenAPI drift.** b/c/d all regenerate wire types living in generated-events.ts + generated.ts + openapi.json (§0.5); the drift gate (ci.yml:528) diffs all three. **Mitigation:** each slice runs `cargo` regen + commits all three + web typecheck before review. Severity: LOW-MED.
- **R6 — merge-conflict against in-flight `runtime_*` work.** #741 follow-ups and parked branches touch `runtime_*`. PR10-d is the conflict magnet. **Mitigation:** sequence d last, dispatch when the #741 reaper-convergence arc is quiescent; the rescope (dropping internal sweep) shrinks the conflict surface. Severity: MED.
- **R7 — re-grounding discipline.** All briefs re-resolve `file:line` via `git grep` against `origin/main` at dispatch. (This v2 already re-grounded every citation against `04d39bdb`.) Severity: LOW.

---

## 7. §Disposition-History

### v3 — round-2 dual-channel review disposition · **CONVERGED** (Polish agent, 2026-06-17)

**Both review channels (codex + subagent panel) returned APPROVE-WITH-NITS with ZERO blocking findings.** All round-2 findings are doc-polish NITs — no design decision changed. Each NIT was VERIFIED against the worktree at `04d39bdb` (a couple of the raised NITs were themselves slightly off and were corrected to the verified value before folding). The 8 NITs folded:

| # | NIT (doc-polish) | Disposition | Verified value / evidence |
|---|---|---|---|
| 1 | **`WorkerSessionProjectionRepo` is not pure-reads** | **FOLDED** | `RuntimeRepo` (runtime_repo.rs:70) carries reads (`runtime_get_*`→`CardRuntime`) PLUS legacy mutators: `runtime_set_status_for_card` (:135), `runtime_complete_for_card`/`_for_terminal` (:181/:197), `runtime_*_tx` start/supersede/bind/set-status family. Corrected the "reads only" framing in §2.C, §6 R3 (name kept `WorkerSessionProjectionRepo`, NOT re-renamed; `session_projection_*` prefix covers reads + mutators). |
| 2 | **Census total off + 3 missing crates** | **FOLDED** | Repo-wide `rg -o "Runtime\|runtime_" -g '*.rs' crates` = **3610** (v2 said 3576 over 5 dirs). Missing 34 = `calm-truth-test-harness` **19** (real rename call site lib.rs:28), `calm-proc-supervisor` **11**, `calm-exec` **4** — added as §0.1 rows with repo-wide total. |
| 3 | **`RunStatus` web counts were substring (incl. `WaveFsRunStatus`)** | **FOLDED** | Word-boundary `\bRunStatus\b`: generated-events.ts **6**, generated.ts **2**, openapi.json **2** (v2's 9/5/5 were inflated by `WaveFsRunStatus`×3 each). Noted `WaveFsRunStatus` is a SEPARATE type; PR10-c must word-boundary-match. Fixed §0.5 + §5 PR10-c. |
| 4 | **Override-missing-card_id wording** | **FOLDED** | With required `card_id: String`, an ABSENT param fails axum `Query<IngestQuery>` extraction → **400** *before* the handler (codex.rs:142), never reaching the role gate. Reworded §4 producer #2: currently 400 (extractor), post-PR10 → `None` → 403. (Consistent with the §3 absent 400→403 unification.) |
| 5 | **`IngestQuery` struct line anchor** | **FOLDED** | codex.rs:58 = `#[derive(Debug, Deserialize)]`; :59 = `pub struct IngestQuery`; :60 = field. Changed §0 to ":59 (`#[derive]` at :58)", matching finding-#10 disposition. |
| 6 | **`event.rs` variant anchors off-by-one** | **FOLDED** | :389/:397/:404 are `#[serde(rename=…)]` tag lines; variant idents are :390/:398/:405. §0.3 now cites the serde-tag lines explicitly (the frozen-wire-relevant part) + the ident lines; §7 finding-5 updated for consistency. |
| 7 | **Predecessor-doc citations untracked** | **FOLDED** | `ls docs/_design-679-pr9a*` no match; `git ls-files docs/ \| rg pr9\|679-body` empty — `_679-body-new.md` / `_design-679-pr9a-v2-current.md` / `_design-679-pr9b.md` are local working notes, not git-tracked. Header + the two in-body cites relabelled "local working notes (not git-tracked)" with line anchors dropped. |
| 8 | **PR10-a brief note: `RoleViolation` cross-crate import** | **FOLDED** | `RoleViolation` lives in `calm_truth::role_gate` (role_gate.rs:69); calm-server imports it nowhere. The *preferred* empty-reject pseudocode needs a new `use` (`crate::role_gate::RoleViolation` via re-export role_gate.rs:1, or `calm_truth::role_gate::RoleViolation`); the *equivalent* fall-through needs none. Added an implementer note to §4 step 2; recommendation unchanged (both listed). |

**No settled design decision was touched.** Re-affirmed as-is: Q-BC hard-cutover, Q-Enum=`Option`, Q-CardRuntime rename+defer-collapse, Q-Slice split (a/b/c/d), Q-Aliases hard-rename, Q-MCP-vector no-marker, the `WorkerSessionProjectionRepo`/`session_projection_*` R3 resolution, and the FORK-1/2/3 architect list.

**CONVERGENCE DECLARED — no further review round.** Per the design-doc review-loop policy, APPROVE-WITH-NITS from both channels with zero blocking findings + NIT-only folds is terminal. Next step is architect RATIFY of §8 FORK-1/2/3, not another review pass.

### v2 — round-1 dual-channel review disposition (Revision agent, 2026-06-17)

Both channels returned REVISE. Each finding below was VERIFIED against the worktree at `04d39bdb` before action.

| # | Finding (source) | Disposition | Evidence |
|---|---|---|---|
| 1 | **[BLOCKER] R3 `SessionRepo` collision** (both) | **PATCHED — resolved in-design** | Confirmed: `trait SessionRepo` session_repo.rs:39, `trait RuntimeRepo` runtime_repo.rs:70, both in `Repo` supertrait db/mod.rs:977, `RuntimeRepo` also in `RouteRepo` db/mod.rs:965. Rename→`WorkerSessionProjectionRepo`, methods→`session_projection_*` (collision-free, verified `rg session_projection_` empty). All "pending/contradictory/verify-at-brief" hedging removed from §2.C, §5 PR10-d, §6 R3. Moved out of §8 FORK list. |
| 2 | **[MAJOR] empty/absent BC + error construction** (both) | **PATCHED** | Verified empty→403: role_gate.rs:71 variant + :123-126 guard; sqlite.rs:5897/6143 `CalmError::Forbidden(violation.to_string())`; error.rs:175 Forbidden→FORBIDDEN. Absent→400 (axum Query missing field). §3 Q-Enum + §4 step 2 corrected: 403/`EmptyAiCardId` (RoleViolation, not CalmError variant), absent 400→403 marked deliberate+latent+BC-safe (not "preserves current behavior"). No-trim constraint reaffirmed (codex.rs:162/195). |
| 3 | **[MAJOR] `NEIGE_HOOK_URL` 4th producer** (codex) | **PATCHED** | Verified main.rs:65-67 reads override; main.rs:388 `hook_url.map(...).unwrap_or_else(...)` REPLACES full URL verbatim (does NOT append `?card_id=`). Documented as producer #2 in §4; override-omits-card_id → None → 403 fail-loud, confirmed acceptable. |
| 4 | **[MAJOR] Census wrong / recount** (both) | **PATCHED** | Recounted with stated methodology (word-boundary `rg -oP "\bX\b"` vs raw substring `rg -o`). Per-crate raw `Runtime|runtime_`: src **870**, tests **1862** (v1 said 713/52). §0.1 table + §0.2 test-churn split rewritten; `RuntimeInit` in **36** test files verified (`rg -l RuntimeInit crates/calm-server/tests`=36). PR10-d blast radius re-estimated honestly. |
| 5 | **[MAJOR] Frozen wire vocabulary subsection** (both) | **PATCHED** | Verified `Event::Runtime{Started,StatusChanged,Superseded}` + serde tags `runtime.*` event.rs:389/397/404 (idents :390/398/405), kind_tag :968-970; `CardRuntimeView` model.rs:435 as `Card.runtime` model.rs:482. Added §0.3; narrowed goal to "retire dropped-table-era vocabulary; event-wire names frozen." |
| 6 | **[MINOR] producer census + dead-field** (codex) | **PATCHED** | `session_start_runtime_tx` (sqlite.rs:3257, returns CardRuntime) added as producer (§0, §5 PR10-b/d). Dead-field-drop kept; verified `terminal_ref/lease_owner/lease_until_ms: None` runtime_row.rs:88/93/94 (+:214/219/220), test read sqlite.rs:6902-6904; noted test + generated-TS churn the drop forces. |
| 7 | **[MINOR] RunStatus de-dup order** (both) | **PATCHED** | Verified RunStatus Failed(runtime.rs:50)/Exited(:51) vs WorkerSessionState Exited(worker.rs:365)/Failed(:366). Noted generated-TS union may reorder→snapshot churn (§5 PR10-c). Confirmed WorkerSessionState NOT ts-rs exported (worker.rs:8-10 header + no `#[ts]` derive :358-368). |
| 8 | **[MINOR] Regen + web-consumer scope** (subagent) | **PATCHED** | Verified drift gate ci.yml:528 diffs openapi.json+generated.ts+generated-terminal.ts+generated-events.ts. Added §0.5 (all three carry the types; terminal clean). Verified card-runtime-viewer.tsx imports `CardRuntimeView` (NAME-STABLE, not affected); schemas.ts `runStatusSchema`(:111)/`type RunStatus`(:120) DOES churn. Both in §5 PR10-c. |
| 9 | **[MINOR] RuntimeResult/RuntimeTx + ThreadAttribution** (subagent) | **PATCHED** | Verified `Result as RuntimeResult`/`Tx as RuntimeTx` are local import aliases (runtime_lookup.rs:10; sqlite.rs:48,50); real public aliases `pub type Result`/`Tx` runtime_repo.rs:17-18. Corrected §2.C/§5. Added `ThreadAttribution.runtime_id` (runtime_repo.rs:43-44) + `runtimes_recover_harnesses_on_boot` (runtime_repo.rs:136) + caller harness/mod.rs:195 to PR10-d. |
| 10 | **[MINOR] Fix ALL citations** (both) | **PATCHED** | Header rewritten (worktree IS at 04d39bdb; dropped "stale" framing). role_gate.rs → calm-truth (calm-server is 1-line re-export, verified). ci.yml marker→:166, job→:128. HookFallbackRecord→lib.rs:178-179, consumption :281, replay lib.rs:373. IngestQuery def→codex.rs:59 (within :58-61 struct), dropped claude.rs:21 (route reg, not IngestQuery). Other touched refs spot-fixed. |

**Items independently CONFIRMED solid (NOT re-litigated):** Q-BC hard-cutover safety; Q-Enum=`Option`; Q-Slice=split; Q-Aliases=hard-rename; Q-MCP-vector=no-marker; CardId serde-transparency; RunStatus↔WorkerSessionState 1:1 twin; `IngestQuery` absent from mcp_server/ + vectors/; no producer constructs `IngestQuery` directly.

**NEW issue discovered while verifying:** none that block the spec. One nuance worth recording: the `NEIGE_HOOK_URL` override (finding 3) is broader than "a 4th producer" — because it replaces the full URL, an operator could point the bridge at an arbitrary endpoint with no `card_id` at all; today (`card_id: String`) such a call already 400s on the missing query field, post-PR10 it 403s. Both are fail-loud; no silent path. Captured in §4 producer #2.

### v1 — Plan agent draft (2026-06-17)

Initial draft. Opened for round-1 dual-channel review with these pending verification deltas (all now resolved in v2): R3 `SessionRepo` collision (→ confirmed real, resolved); FORK-2 RATIFY-5 reading (→ still a fork, §8); Q-Enum empty-edge fail-loud equivalence (→ corrected, empty/absent asymmetry documented).

- round-2 convergence → **see the v3 entry above** (both channels APPROVE-WITH-NITS, zero blocking; 8 NITs folded; CONVERGED).
- **architect ratification of §8 forks (2026-06-17):** **FORK-1 = ACCEPT RESCOPE** (ship public-surface + wire-typed renames; drop the blanket internal `runtime_*` name sweep; defer the `CardRuntime`→`WorkerSession` collapse). **FORK-2 = RENAME-NOW / DEFER-COLLAPSE** (RATIFY-5 read as permissive; PR10-b renames the struct + drops dead fields only). **FORK-3 = CONFIRMED** (`IngestQuery` is HTTP-query, no external producer; hard-cutover safe). Implementation cleared to proceed: PR10-a → b → c → d.

---

## 8. Architect-level forks (need user RATIFY) vs resolved sub-decisions

> **✅ RATIFIED 2026-06-17 (architect):** FORK-1 = **accept rescope** · FORK-2 = **rename-now / defer-collapse** · FORK-3 = **confirmed (hard-cutover safe)**. Implementation cleared: PR10-a → b → c → d.

**Genuine forks — irreversibility / scope / wire-BC — for the user to ratify:**

- **FORK-1 (scope) — Accept the value-audit rescope of #762?** The doc proposes shipping the *public-surface + wire-typed* renames and **dropping the blanket internal `runtime_*` local/private/test-NAME sweep** (the droppable half of the `runtime_*` substring mass, §0.2). It also **defers the `CardRuntime`→`WorkerSession` structural collapse** to a later behavioral PR. NOTE: this rescope does NOT shrink the mandatory test-call-site churn (≥36 files for `RuntimeInit` etc, §0.2) — that floor is irreducible once the public types are renamed. The user must ratify "rescope" vs "do the full literal #762."
- **FORK-2 (scope, conditional) — Does RATIFY-5 mandate the `CardRuntime` structural collapse?** If RATIFY-5's binding text requires the collapse (not just struct retirement), Q-CardRuntime flips from "rename-now/defer-collapse" to "collapse-now" and PR10-b grows into a truth-read-layer refactor. Needs the user to confirm the RATIFY-5 reading. (I read pr9b as "removing the struct is PR10 work" — permissive, not mandating collapse-vs-rename.)
- **FORK-3 (wire-BC framing) — Confirm `IngestQuery` is HTTP-query, not MCP.** #762 frames this as riding "the MCP tools/call wire path." Verification says it is an HTTP query-param on loopback routes with NO external producer (§3 Q-BC, §4) — though note the `NEIGE_HOOK_URL` override (§4 producer #2) lets an operator repoint the bridge URL out-of-band. The hard-cutover safety rests on no external producer SILENTLY omitting card_id; the override path fails loud (403), which is acceptable. The user should confirm there is no other out-of-band-released producer before PR10-a's hard-cutover lands. (Reversible if wrong: PR10-a is a pure type change.)

**Resolved sub-decisions (NOT escalated):**
- **R3 `RuntimeRepo`→`WorkerSessionProjectionRepo` rename (NOT merge into `SessionRepo`)** — was an architect fork in v1; now fully resolved in-design (§6 R3). Distinct-name + `session_projection_*` method prefix, collision-verified. No longer requires escalation.
- Q-Enum → `Option<CardId>` (no enum). HIGH; single-inhabitant value-space + DaemonTrust-name collision.
- Q-Aliases → hard rename, no alias runway. HIGH; aliases don't shield ts-rs/web; no external Rust consumers.
- Q-MCP-vector → no marker needed. HIGH; verified vectors corpus + gate mechanics (ci.yml:128-175).
- Q-Slice → split into a/b/c/d. HIGH.
- Empty/absent normalization → explicit empty→reject preserving 403/`EmptyAiCardId`; absent 400→403 deliberate+latent. HIGH.
- `RunStatus`→`WorkerSessionState` de-dup including adding ts-rs export to WorkerSessionState. MED-HIGH; 1:1 variant twin confirmed.
