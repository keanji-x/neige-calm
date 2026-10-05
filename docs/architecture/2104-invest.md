# Invest: a multi-instrument portfolio with long-lived research Tracks

**Baseline:** `origin/main` 9e06ce5ab. The design was first read at 2dfb69d44, and this
worktree's line numbers are from that tree. `git diff --stat 2dfb69d44 9e06ce5ab` touches only
two cited files, both re-read: `plugins/paper-trading/spy-recipe.md` (#2102) and
`routes/cards.rs` (F23 gives both trees' lines). Every other cited file is unchanged, as are the
prompt files and the registry golden behind F16.
Facts marked **4140** come from one read-only query of the production database (2026-10-04,
migration 138).

**Status:** design, revision 6. Review rounds 1–5 are folded in (§7). The research lifecycle is
rebuilt on provenance, one issued key and a lease. Issue: #2104. Review tier: **L2**. Docs only.

**Owner direction (not reopened):** a Python plugin `invest` merges `plugins/paper-trading` and
`plugins/market`; one portfolio Track is the only broker writer; each covered instrument gets an
ordinary top-level research Track (linked recipes); the kernel gaps are `neige_track_add` and plugin
standing instructions; names follow `docs/conventions/agent-commands.md`, after #2087 B0 and B5;
compatibility covers 4140 only.

**Owner decisions (on #2104; none open):** (1) fresh start at cut-over; (2) US only;
(3) `neige_track_add` is listed, K1 trims descriptions and never raises the 30,000 B cap, no CLI
row; (4) retire `market` (7d686d59…'s chart moves, crypto charts go); (5) any stored recipe,
bounded by the cap; (6) an instrument is **held** (weight > 0 or a position) or **watched** (a
research Track, no position), limited by `max_held` / `max_watched`; watched → held is a weight
change; projections aggregate to fit any configured value; the kernel cap covers held + watched.

## 1. Problem, goals, non-goals

**Today.** The paper plugin allocates one symbol, SPY, against cash.
- `SPY.US` is hard-coded in the sizer (`plugins/paper-trading/paper_trading/allocation.py:152`), in
  the SDK checks (`paper_trading/sdk_bridge.py:82,113,169`) and in the tool parameter
  `target_spy_bps` (`plugins/paper-trading/manifest.json:41-45`).
- One Track owns it through config (`allocation.py:44-45`).
- Since #2102 the report is a three-view dashboard, and research lives only in the conversation
  (`spy-recipe.md:17`). No view is tracked over time.

**Goals.**
1. Target weights over US instruments; the AI adds instruments within operator bounds.
2. One portfolio Track writes to the broker; each covered instrument has a long-lived research
   Track with its own calendar and recipe, which never trades.
3. Portfolio views are followed in the research Track, and their status flows back.
4. A generic kernel: Planner-opened recipe Tracks and plugin standing instructions.

**Non-goals.** Real money; other brokers; a Rust SDK; non-US markets; crypto charts; a
ledger-migration framework; changing the renderer or the unit contracts.

## 2. Verified facts

| # | Fact | Evidence |
|---|---|---|
| F1 | A plugin may publish overlays onto any Track. The permission is per `entity_kind`, never per id. | `plugin_host/perms.rs:11-13`; `plugin_host/callbacks.rs:192-241` |
| F2 | A live slot resolves `(report's own track, plugin, kind)`. An invalid unit renders `unavailable`. One unit may be at most 4 MiB. | `mcp_server/tools/track_report_hydrate.rs:79,142-160,248-250`; `kinds.rs:32` |
| F3 | A data unit is a single `Component`; no component has a link field. Ids match `[A-Za-z0-9._-]{1,100}`. A record has a title ≤ 200, a summary ≤ 8,000, ≤ 12 facts and ≤ 8 sections; a section label is ≤ 120. | `native_view/model.rs:105-109,194-233` |
| F4 | `tools/call` carries `_meta["dev.neige/track"] = {id}` and, for agent calls only, `"dev.neige/caller"`. The chart resolver passes the Track but no caller. | `plugin_host/mcp.rs:184-198,414-430`; `report_series/resolver.rs:573-575`; `caller_identity.rs:11-35`; `paper_trading/rpc.py:69-74` |
| F5 | A Track with `plugin_scope` sees only its owner plugin's tools; an unbound Track sees every enabled plugin. Template binding is for trusted forge plugins only. | `mcp_server/tool_visibility.rs:13-37,57-73,75-120`; `forge_trust.rs:9-17` |
| F6 | The SPY recipe uses other plugins' tools (Longbridge, Wisburg), so its Track is unbound. **4140:** 7c0dd087… has `plugin_scope` NULL. | `spy-recipe.md:11-12` |
| F7 | Area links use the form `neige.area.outline` documents, within one area. `neige.report.backlinks` lists inbound links. | `prompts/tools/neige.area.outline.md`; `neige.report.backlinks.md` |
| F8 | Calendar entries belong to their Track, and each occurrence wakes that Track's Planner. | `builtin_plugins/calendar/store.rs:21,108`; `calendar/instructions.md:4,20-24` |
| F9 | Only built-ins inject live Planner instructions. Recipe text is a snapshot taken at creation. | `operation/planner_harness_start_adapter.rs:435-456`; `routes/tracks.rs:1459`; #2098 |
| F10 | There is one create path. `create_track_structure` reads the recipe inside a write transaction (`BEGIN IMMEDIATE`, per the comment at `:1068`). The keyed create binds `(area_id, key)` → track there and delivers a first message once. The REST digest covers the REST fields. | `routes/tracks.rs:1248,1281,1316-1332,1352-1376`; `routes/tracks/create.rs:184-291,334-345,474-508` |
| F11 | A binding row outlives its Track, so a deleted Track's key is dead forever. | `0088_track_create_idempotency.sql:40-48`; `create.rs:520-530` |
| F12 | The child-track route is task-shaped: it charges the tree budget, inherits no recipe and sets `parent_track_id`. | `operation/child_track_adapter.rs:68-75,196-216,250-258,271` |
| F13 | `create_track` takes the area-delete lock and the Claude availability gate. `AppContext` has no `RouteState`. A Planner's MCP actor is `AiPlannerSession`. `require_role`/`require_role_any` refuse with -32602. `TrackUpdated` can carry a `message`. | `routes/tracks.rs:773-787`; `mcp_server/registry.rs:78-81,108-131,155-189`; `tools/track_state.rs:294-297` |
| F14 | Recipes are human-only. | `routes/track_recipes.rs:119-127` |
| F15 | Plugins have only the `kv`, `overlay`, `card` and `event` callbacks, so they cannot read kernel config. Unknown manifest fields are tolerated (the latest manifest version is v4). Plugin rows store the manifest and `enabled`. | `callbacks.rs:124-140`; `plugin_host/manifest.rs:17-22,129-136`; `calm-truth/src/model.rs:238-250` |
| F16 | The Planner tool surface is capped at 30,000 B, with at most 2,048 B per description. It **measures 29,929 B over 30 tools** (§3.4). | `mcp_server/tools/mod.rs:172-176`; `tests/goldens/mcp_tool_registry.json` |
| F17 | Data dir `<plugins_data_dir>/<id>/`. The SDK runs in its own session, so SIGTERM to the plugin does not reach it. CLI: `sdk_bridge.py … snapshot --request '{"since": …}'`. | `plugin_host/process.rs:56,250-258`; `allocation_broker.py:80-85,146-155`; `sdk_bridge.py:138-140,226-252` |
| F18 | Paper-only is proven by the SDK identity check. Cash is USD-only and must be settled. | `sdk_bridge.py:55-77,92-108` |
| F19 | Codex cannot approve an `openWorldHint` tool under `approval_policy: never`. | `plugins/market/README.md:33-46` |
| F20 | `chart.series` needs `VENUE:SYMBOL`, with venue `[A-Z]{2,8}` and symbol `[A-Za-z0-9._-]{1,32}`, and a `neige://plugin/<id>/<tool>` source. | `chart_series.rs:131-139,163-167`; `kinds.rs:115-133` |
| F21 | Unit contracts: time series ≤ 4 datasets × 6 series × 500 points; distribution ≤ 12 slices; records ≤ 4 datasets × 100; table ≤ 32 × 500. No validator checks that values are conserved. | `native_view.rs:122-128,167,227`; `native_view/model.rs:243,441-443` |
| F22 | Reconciliation refuses unowned active orders and holdings that do not match the executions. Budgets: 500 orders, 5,000 executions. | `allocation_reconcile.py:83-84,112-129`; `sdk_bridge.py:146-153` |
| F23 | A Codex Planner gets new instructions only at thread start; reset forces a new thread. Claude re-reads them at every `open_session`. | `routes/cards.rs:198,1454-1455` (`:1199-1200` at 9e06ce5ab); `harness/backend.rs:102`; `claude_planner/wiring.rs:35-46` |
| F24 | **4140:** `dev-neige-market` has no kv rows and no overlays. 7d686d59… and the SPY Track cite it. The SPY Track has 4 calendar rows. | read-only query |

## 3. Decisions

### 3.1 Shape

```
portfolio Track (invest-portfolio)               research Track per live symbol (invest-instrument)
  Planner: instrument_add/set/rm, thesis_add/rm,   Planner: instrument_status, thesis_set
           decision_add, neige_track_add                    own calendar; never trades
  Worker:  execution_add ──► broker (paper)
           ▲ portfolio.*, thesis.board (each tick)  ▲ instrument.position, thesis.records (on its calls)
           └────────── invest ledger: one SQLite file, one reconcile/submit loop ──────────┘
```

Both kinds of Track are ordinary and unbound, and they live in one area (F5, F6, F7).

### 3.2 Linkage: theses in the plugin ledger (D1)

A **thesis** has:
- `thesis_id`: a caller-chosen slug;
- `symbol`;
- `stance` (`bullish|bearish|neutral`);
- `title` (≤ 110 chars), `summary` (≤ 500) and `body` (≤ 6,000), all bounded at input;
- `source_refs`;
- `assessment` (`open|holding|at_risk|broken`);
- `version`.

Lifecycle:
1. **Raise.** The portfolio raises a thesis with `thesis_add` on a live symbol. A symbol has at
   most 3 open theses; a 4th is refused and nothing changes.
2. **Assess.** The research Track holding the symbol's current key assesses it with `thesis_set`
   under `expected_version` (§3.3). The assessment and its own `summary` and `source_refs` are stored
   as the thesis's research assessment; the portfolio-owned fields above never change, so a lost-answer
   replay of `thesis_add` still matches them.
3. **Retire.** `thesis_rm` retires one thesis. `instrument_rm` retires every open thesis of its
   symbol in the same transaction. Retiring is a verb, never an assessment value.
4. **Flow back.** `thesis.board` on the portfolio Track is republished every tick
   (`runtime.py:15-27`, F1, F2), so a status change appears there within one tick. Each
   displayed thesis is a section labeled `<assessment> · <title>`, with the portfolio's `summary` as
   its body, followed by `研究评估：<research summary>` once the research Track has assessed it.
   The assessment therefore shows explicitly: a label is at most 120 chars, and title ≤ 110 leaves
   room for the prefix.

The two reports link each other in prose (F3, F7), and `neige_link_ls` shows inbound links.
Recipes never copy an assessment into prose (the rule at `spy-recipe.md:17`).

### 3.3 Research lifecycle: provenance, one issued key, a lease (D2)

**Kernel provenance (K1).** The kernel puts provenance in `_meta["dev.neige/track"]`:
`{id, creator_track_id, creator_key}`. These are copied from the Track row (`null` when absent);
`creator_key` is the raw `idempotency_key` the creator passed to `neige_track_add`. Nothing in it
names a plugin.

**One issued key per live symbol.** The ledger is the one source of truth.
- `instrument_add` canonicalizes the symbol to upper case. A live symbol S carries a counter `n`
  that never decreases and the current key `invest-<VENUE>-<CODE>-<n>`, which keeps case. The
  key parses unambiguously: the venue `[A-Z]{2,8}` has no `-`, and `n` follows the last `-`.
- The ledger also stores the exact
  `track_add = {recipe_id, title: "<symbol> 研究", idempotency_key, text, message}`.
- **Retries.** A retry of `neige_track_add` within `n` is byte-identical and replays the same
  Track (F10). A first creation under the key with *other* arguments is a portfolio Planner
  mistake. The binding fingerprint then refuses every other request under that key forever
  (`create.rs:225`), so the way out is renewal (n+1, below).
- Renewal, or re-adding a dropped symbol, issues n+1. A key is never reused, so a dead key (F11)
  cannot block coverage.

**Write authority.** A call is *attested for S* exactly when S is `live`,
`creator_track_id == portfolio_track_id`, and `creator_key ==` S's current key. Provenance cannot
prove which recipe or text the creation used.
- Nothing is stored at binding time.
- Attested calls may `thesis_set` on S.
- Each attested call republishes S's research units onto the caller, statelessly (the
  caller's id is the overlay target and is not stored). This is the view exception in
  `agent-commands.md` §3.
- Every call whose provenance names the portfolio and an older key of S, or a dropped S, is
  refused with -32409 `superseded: close this Track`, views included.
- Any other caller is refused with -32403, including the portfolio Track itself, which has no
  creator.
- A repeat call from the current Track is attested again: attestation stores nothing, so calls
  repeat freely. Writes still follow their own rules: a lost-answer retry of `thesis_set` carries the
  `expected_version` that the first call already consumed and gets -32409 with the current version in
  `data`; the caller rereads and retries.

**Lease.** Every attested call sets `last_seen_at` for S. This is **access metadata**, like the
kernel's `last_activity_ms`: it never bumps `version`, never changes authority, and is not domain
state. `portfolio_status` marks S **stale** when either holds:
- the current key was not seen within 120 minutes of issue (a constant), for example because
  the Track was never created or its first turn failed;
- the current key has not been seen for `lease_days` (default 8). The research recipe has a weekly
  calendar entry, and every step starts with `instrument_status`.

**Renewal.** The portfolio renews a stale S with **`instrument_set {symbol, expected_version,
message}`**. It is `set` under §3 ("replace one entry's value under `expected_version`"): the
value replaced is S's issued key, which becomes n+1. The call returns the new `track_add`, which
the Planner passes verbatim to `neige_track_add`.
- **What renewal restores.** Renewal restores coverage. It does **not** close the old Track. A
  live superseded Track closes itself on its next call (`neige_track_close`). A silent one, such
  as a dead Planner, stays open until a human closes it: `neige_track_close` closes only the
  caller's own Track (`track_state.rs:237-245`).
- **Cap refusals.** Silent Tracks count against the kernel cap. The cap's -32409 therefore lists
  the creator's open created Tracks as `{track_id, creator_key}` (immutable, already on the row),
  in the message and in `data` (§3.4).
- **Owner notice.** On that refusal, the portfolio Planner calls `neige_user_notify` (today
  `neige.user.notify`) with that list and the current keys from `portfolio_status`. Each Track
  with an `invest-*` key that is not current is one to close.

**States.**
- `pending`: added, symbol not yet verified. No key exists.
- `live`: verified, key `n` issued.
- `dropped`: removed by `instrument_rm`, or refused by verification (with a reason).

Only `pending` and `live` count toward limits. A dropped symbol may be re-added, which issues
n+1. Verification runs in the loop (F19), so no Track is created before the
symbol is checked.

### 3.4 `neige_track_add` (kernel gap 1, D3)

**Input** `{recipe_id, title, idempotency_key, text, message}`, all required, closed schema:
- `text` (§4) is the verbatim first message to the new Planner.
- `message` (§4) is the audit note, carried on the creation `TrackUpdated` (F13).

**Result** `{track_id, created_at}`; a replay returns the same result. The tool is listed for the
Planner (owner 3).

| Aspect | Decision |
|---|---|
| Who | A Planner on an open creator Track whose plugin scope is `All` (`tool_visibility.rs:57-73`). A bound or fail-closed creator would otherwise escape its fence. A reports-only managed Planner is refused (`managed_track.rs:215-226`). **Authorize before planning.** The handler refuses a non-Planner with -32403 before both the mint and the replay arms. A keyed replay resumes before any create transaction (`routes/tracks.rs:816-822`), and dispatch does not enforce `visible_to_roles` (`registry.rs:310-318,330-335`). The in-transaction gate is the generic backstop for the mint: it makes the creation `TrackUpdated` Planner-only (`calm-truth/src/decision_gate.rs:46-107` → `role_gate.rs:155-183`, `Forbidden` → -32403 as in `tools/plan.rs:703`). |
| Depth | 1. Refused when the creator has a `creator_track_id`, and separately when it has a `parent_track_id`. |
| Errors | Scope and depth refusals are -32403, raised with `RpcError::custom` as `managed_track.rs:221` does, not via `require_role*` (F13). The cap is state, so it is -32409 (agent-commands.md §5). Its message names `--track-add-max-open`, the cap, the open count and the open created Tracks, and `data.open` lists them as `{track_id, creator_key}` (the rows the count already reads). |
| Recipes | Any stored recipe (owner 5; recipes are human-only, F14). |
| Where | The creator's area (`registry.rs:66-74`); a managed workspace (`routes/tracks.rs:937-938`); the creator's Planner provider (`child_track_adapter.rs:240`); the default theme; actor `AiPlannerSession` (F13). |
| Entry point | The keyed create (F10), extracted behind one function that takes an `ActorId`, a key and a fingerprint. REST and the tool both call it, keeping the area-delete lock and the Claude gate (F13). The tool reaches it through `AppContext.track_creator: OnceCell<Arc<dyn TrackCreator>>`, set at boot like `operation_runtime` (`registry.rs:174`); its implementation holds `RouteState`. |
| Idempotency | The existing binding row, keyed `track-add/<creator_track_id>/<idempotency_key>`. REST refuses that prefix. The fingerprint covers the tool's own five inputs only, not derived fields such as the provider (contrast `create.rs:334-345`). A different request under the same key is -32409. |
| Provenance | Migration, numbered last: `tracks.creator_track_id` and `tracks.creator_key` (the raw key), both or neither (a named `CHECK`), no `REFERENCES` (0085's reason), and an index on `creator_track_id`. `parent_track_id` stays NULL and the tree budget is untouched (F12). The provenance reaches plugins via `_meta`, built in **one** typed place that both `tools_call` and `forge_tools_call` use (`mcp.rs:424-430,448-454`). |
| Cap | `--track-add-max-open <u32>`, a clap arg on `Config` (`config.rs:8`, with a range like `:159-166`): 1..=256, default 16, no env var. It counts the creator's open created Tracks inside the create transaction's closure (`routes/tracks.rs:1281`). |
| Cap and plugin limits | The plugin cannot read the cap (F15). It checks statically that `max_held + max_watched ≤ 255` (the cap range minus one spare renewal slot). The runbook sets the cap to at least `max_held + max_watched + 1`. A mismatch, or silent superseded Tracks (§3.3), show up as the -32409 above. S stays stale, and the Planner notifies the owner. |
| Events and UI | The ordinary create events (`routes/tracks.rs:1567-1590`). No visible UI change; the generated `Track` type gains two fields. |
| Creator closes | Nothing cascades. |
| Budget | 29,929 B: summed over the 30 Planner rows of `mcp_tool_registry.json`. Each row counts the description bytes (prompt `trim_end`; `task.verdict` rendered with its guidance; every SHA-256 matched the golden) plus the compact schema bytes. That leaves 71 B. K1 adds about 1.1 KB and trims at least that much from the largest descriptions (`report.commit` 1,535 B, `terminal.input` 1,479 B, `source.capture` 1,397 B, `plan.list` 1,362 B). It re-measures on its own base. |

### 3.5 Plugin standing instructions (kernel gap 2, D4)

**Manifest field.** `planner_instructions: string`, ≤ 2,048 B, legal only at `manifest_version` 5
(F15).

**Which Tracks.** A plugin instructs a Planner when all of these hold:
- its plugin row is enabled;
- its tools are visible to the Track (`TrackPluginScope::allows_manifest`);
- either the built-in rule holds (`planner_harness_start_adapter.rs:436-443`), or the Track's
  current report references `neige://plugin/<id>/` in a view slot, a live table or
  `chart.series`.

The instructions are documentation only (`:435`). One calm-types parser next to
`validate_live_source` (`kinds.rs:123`) finds the references, and hydration's `view_slots` reuses
it. "Bound Tracks only" cannot work, because invest Tracks are unbound (F5, F6).

**Source and timing.** The text is read from the stored manifest of the enabled row (F15), so for
an unbound Track it does not race plugin-host boot. A bound Track whose owner is not running sees
no plugin tools (F5), so it gets no instructions until its next thread start or reset, the same as
`tools/list`. It is assembled in `planner_instructions` (`:417-457`). Codex
picks it up at thread start or reset; Claude at every `open_session` (F23).

**Aggregate cap: 4,096 B over every appended byte.**
- The section is joined to the prompt by `\n\n` (2 B), and the joiner counts.
- A block is `## Plugin <id>\n` + text + `\n`, so it is 44 + text bytes for a 32-byte id.
- The fixed notice `## Plugin instructions omitted (over budget); see the server log\n` is 65 B.
  Those 65 B are always reserved: after the joiner, blocks are admitted in id order while the
  joiner and the blocks fit in 4,031 B.
- If any plugin is left out, the notice is appended once, and each omitted id gets a warning log
  line. The total never exceeds 4,096 B.
- **Boundary fixture:** three plugins with 32-byte ids and 1,990 B texts, so each block is
  2,034 B.
  - Correct: the first block is admitted (2 + 2,034 = 2,036 ≤ 4,031); the second is not
    (4,070 > 4,031); the notice brings the total to 2,101 B.
  - Without the reservation: two blocks fit in 4,096 (4,070); the third is omitted, and the
    notice makes 4,135 B > 4,096, so the test goes red.
- **Joiner fixture:** texts of 1,990, 1,953 and 1,990 B. The second block makes
  2 + 2,034 + 1,997 = 4,033 > 4,031, so only the first is admitted (2,101 B). Leaving the joiner
  out of the count would admit both and append 4,098 B.

**Recipes and trust.** Recipes keep only layout and schedule (#2098). Trust is the same class as
tool descriptions (`docs/architecture/1413-local-plugin-trust.md`).

### 3.6 Tool table (D5)

The plugin id is `invest`, and every tool is served as `plugin_invest_<tool>`. Symbols are written
`VENUE:CODE` (F20). Unit ids use `VENUE.CODE`: the venue has no `.`, so the mapping is injective.
`sdk_bridge.py` converts symbols to `CODE.VENUE`.

| Tool | §3 verb | Caller | Input (§4 names; domain keys in italics) | Replaces |
|---|---|---|---|---|
| `portfolio_status` | `status` V | portfolio Planner/Worker | `{}` → snapshot, positions, targets, instruments (state, `track_add`, held/watched, stale), limits, theses, decisions, orders, fills, errors | `spy.status` (+ instrument list) |
| `decision_add` | `add` W | portfolio Planner | `decision_id`, *`weights`* `[{symbol, bps}]`, `message`, *`source_refs`*, *`valid_until`* | `spy.plan` |
| `execution_add` | `add` W | portfolio Worker | `decision_id` | `spy.execute` |
| `instrument_add` | `add` W | portfolio Planner | *`symbol`*, `message` | — |
| `instrument_set` | `set` W | portfolio Planner | *`symbol`*, `expected_version`, `message` → the new `track_add` (key n+1) | — |
| `instrument_rm` | `rm` W | portfolio Planner | *`symbol`*, `expected_version`, `message`; refused while held | — |
| `instrument_status` | `status` V | research Planner | `{}` → its symbol, position, theses; a superseded caller gets -32409 | — |
| `thesis_add` | `add` W | portfolio Planner | `thesis_id`, *`symbol`*, *`stance`*, `title`, `summary`, `body`, *`source_refs`* | — |
| `thesis_set` | `set` W | attested research Planner | `thesis_id`, *`assessment`*, `summary`, *`source_refs`*, `expected_version` | — |
| `thesis_rm` | `rm` W | portfolio Planner | `thesis_id`, `expected_version`, `message` | — |
| `series_show` | `show` V | chart resolver only | the `market.series` contract, US only | `market.series` |

**Verb rules.**
- Views change no domain state. A view may stamp access metadata (`last_seen_at`) and refresh
  derived projections onto the caller's own Track (§3.3). The convention allows exactly this:
  `docs/conventions/agent-commands.md` §3 (the "A view may stamp access metadata" line).
- Every `set` and `rm` takes `expected_version`.
- There are no compound actions.
- `series_show` declares `openWorldHint: true`. It accepts only calls that carry Track `_meta`
  and **no** `dev.neige/caller`, which is the resolver's shape (F4). Agent calls are refused.

**Deleted:** `spy.refresh` (the loop wakes on every write); `market.quote` and
`market.holdings.*` (F24); `thesis_ls` and `instrument_ls` (folded into the two status views).

**Units:**
- portfolio: `portfolio.{nav, nav_history, account, weights, weight_history, holdings,
  decision_log, fill_log}` (the #2102 layout, `spy-recipe.md:24-53`) and `thesis.board`;
- research: `instrument.position` and `thesis.records`.

### 3.7 Ledger, limits, execution, projections (D6)

Fresh ledger at `<plugins_data_dir>/invest/ledger.sqlite3`, `user_version` 1. It reuses the
`Ledger` session, lock and journal (`ledger.py:20-107`). The tables `sources` and `reviews`
(`ledger.py:31,40`) are not carried over: nothing in `invest` reads them.

```sql
CREATE TABLE instruments (symbol TEXT PRIMARY KEY, state TEXT NOT NULL CHECK (state IN
  ('pending','live','dropped')), key_seq INTEGER NOT NULL, issued_at TEXT, last_seen_at TEXT,
  version INTEGER NOT NULL, body TEXT NOT NULL);         -- body.track_add for key_seq
CREATE TABLE decisions (id TEXT PRIMARY KEY, body TEXT NOT NULL, state TEXT NOT NULL,
  error TEXT, created_at TEXT NOT NULL);                 -- body.weights: {symbol: bps}
CREATE TABLE orders (id TEXT PRIMARY KEY, decision_id TEXT NOT NULL REFERENCES decisions(id),
  symbol TEXT NOT NULL, request TEXT NOT NULL, state TEXT NOT NULL, broker_id TEXT UNIQUE,
  broker_status TEXT, error TEXT, UNIQUE (decision_id, symbol));
CREATE TABLE theses (id TEXT PRIMARY KEY, symbol TEXT NOT NULL REFERENCES instruments(symbol),
  assessment TEXT NOT NULL, version INTEGER NOT NULL, retired_at TEXT, body TEXT NOT NULL);
-- carried over unchanged: meta, fills, valuations (body gains positions{}), journal
```

**Held and watched (owner 6).**
- A counted instrument (`pending` or `live`) is **held** if the latest decision weights it > 0 or
  it has a position. Otherwise it is **watched**.
- `instrument_add` requires watched < `max_watched`.
- `decision_add` requires *held-after* ≤ `max_held`. Held-after is the set of symbols weighted
  > 0 by the new decision, plus every symbol that still has a position. So rotating a full book
  takes two decisions: sell first, then buy once the sells settle.
- If selling pushes watched over its limit, further adds are blocked; nothing is forced.
- Each `opening_positions` symbol starts `pending` and is held. Config refuses more than
  `max_held` of them.
- A weight > 0 requires `live`.

**Execution.**
- Carried over: one unresolved decision at a time (`allocation.py:88-89`); validity ≤ 24 h
  (`:86-87`); each order ≤ `max_order_bps` (`:139-140`); commit before the broker write
  (`:201-202`); no resubmission (`:210-212`); unowned-order and holdings checks per symbol (F22).
- New: one order per symbol beyond `drift_bps`, sells first. Each weight is ≤ `max_weight_bps`,
  and their sum is ≤ 10000 − `cash_buffer_bps`.
- Buys use settled cash only (F18). An unfunded leg ends `noop` at `valid_until`.
- Order remark: `nc-inv-` + `digest({account, decision, symbol})[:32]`, which keeps the 39-char
  check (`sdk_bridge.py:175`). `client_request_id` uses the same digest.
- Budgets: one quote call per tick; the F22 budgets are account-wide.

**Projections.** These fit any configured value by aggregating. Held symbols include
not-yet-live opening positions and are ranked by market value. 其他 is always the exact sum of
what it replaces.

| Unit | Contract | Projection |
|---|---|---|
| `portfolio.weights` | 12 slices | top 10 + 其他 + 现金; slices sum to equity |
| `portfolio.weight_history` | 6 series | top 4 now + 其他 + 现金; each point sums to 100% |
| `portfolio.holdings` | 500 rows | every held symbol + 现金 (≤ 256 rows: no aggregation needed) |
| `portfolio.decision_log` | 50 records; per record ≤ 12 facts, ≤ 12 badges, ≤ 20 disclosures | facts: top 11 weights + 其他. Badges: state, `created_at`, `valid_until`. Disclosures: one per order, top 19 + 其他 |
| `thesis.board` | 100 records × 8 sections | one record per counted symbol (held by weight, then watched): top 99 + one 其他 record with counts by assessment; sections are the ≤ 3 open theses (label `<assessment> · <title>`, body the portfolio's `summary` plus `研究评估：<research summary>` once assessed) |
| `thesis.records` (research) | 100 records | ≤ 3 open + the 20 latest retired |
| `nav_history`, `fill_log` | 500 points, 500 rows | 260 points, latest 500 fills |

**Byte budget.** At the maximum config (255 symbols) with maximum-length CJK text, the board is
about 100 × (200 + 3 × (120 + 500)) chars × 3 B ≈ 0.6 MB, well under the 4 MiB cap (F2). A test
asserts this.

**Config** (closed schema, `manifest_version` 5):
- broker: `account_no`, `broker_home`, `oauth_client_id`, `sdk_python_path`, `access_region`;
- Tracks: `portfolio_track_id`, `instrument_recipe_id`;
- limits: `max_held`, `max_watched` (each ≥ 1, sum ≤ 255), `max_weight_bps`;
- trading: `cash_buffer_bps`, `drift_bps`, `max_order_bps`, `quote_max_age_seconds`,
  `poll_seconds`;
- lease: `lease_days` (the bind window is a constant);
- `opening_positions: [{symbol, shares}]`, immutable per ledger (it generalizes
  `allocation.py:115-122`).

The market is US by code. `profile: spy_cash` is deleted; the paper fence is the identity proof
(F18).

### 3.8 4140 cut-over: fresh start (D7, owner 1)

NAV history restarts; the SPY Track stays readable. The kernel cap is set during the K1 deploy
(a start-script argument, applied by the restart that deploy needs anyway) to at least
`max_held + max_watched + 1`. The rest is run by the owner after the Sat 2026-10-10 verdict,
once K2 and P1–P3 are deployed:
1. Cancel the SPY Track's 4 calendar entries (F24), then close the Track.
2. Wait until every paper decision is final (none in `queued`, `requested`, `submitting`,
   `working` or `unknown`). Disable the paper plugin, and require `pgrep -f sdk_bridge.py` to
   find nothing (F17).
3. From the paper install dir, run:
   `env -i PATH="$PATH" HOME=<broker_home> <sdk_python_path> -I paper_trading/sdk_bridge.py
   --access-region <access_region> --client-id <oauth_client_id> --account <account_no> snapshot
   --request '{"since": null}'`.
   It must report no active order. Record its `shares`.
4. Save both invest recipes, then create the portfolio Track from `invest-portfolio` with no
   first message.
5. Enable `invest` with `opening_positions = [{symbol: "US:SPY", shares: <step 3>}]` and the
   §3.7 config.
6. Reset the portfolio Planner (`POST /api/cards/<planner>/planner/reset`, F23); its first
   thread predates `invest`. Neither creation nor reset seeds a turn, and saving a recipe creates
   no calendar. So **send the first message** in the Track's chat
   (`POST /api/cards/<planner>/planner/input`, `routes/cards.rs:173`; `:171` at 9e06ce5ab), asking it to set up its
   calendar and cover `US:SPY`.
7. Edit Track 7d686d59…'s `chart.series` source to `neige://plugin/invest/series_show`, deleting
   its CRYPTO series (owner 4), then uninstall `market`. K2 will then inject invest's
   instructions into 7d686d59…, so those instructions open by naming the two invest Track kinds
   and say to ignore the rest elsewhere.
8. **Standing duty:** when the portfolio Planner sends a cap notice (§3.3), close the superseded
   research Tracks it lists.

**Acceptance check C:**
- The first `invest` reconciliation succeeds, with holdings equal to `opening_positions` and no
  unowned active order. An extra active order on the fake broker must fail it.
- After step 6, `portfolio_status` shows `US:SPY` `live` with `last_seen_at` set, meaning its
  research Track was created and made its first attested call.
- Both the portfolio Track and the SPY research Track have calendar rows.

## 4. Slices

**Order:** #2087 B0 → K1 ∥ K2 → P1 → P2 → P3 → C (K2 needs none of the #2087 slices; K1 needs only B0's separator for its name; P2 needs B0 and K1/K2, not B5). K1 and K2 are inert until used. P1
through C deploy after the verdict.

**Gates:**
- every slice: `scripts/local-ratchet-gates.sh`;
- K1/K2: the whole `-p calm-server` run and `scripts/local-rust-gates.sh --quick`, plus the
  OpenAPI and `fe` type regeneration for K1 (no visible UI, so no browser gate);
- P*: `python3 -m pytest plugins/invest/tests -q`.

Each mutation is single-factor, and `→ {…}` lists the complete set of tests predicted to go red.

| # | Slice (≈ lines) | Tier | Must go red first |
|---|---|---|---|
| K1 | `neige_track_add`, provenance in `_meta`, description trims (~1k) | L2 | Tests: `track_add_records_provenance_not_parent`; `…_refuses_past_open_cap` (cap 2: two adds, the third refused, nothing closed); `…_counts_only_open_tracks` (cap 2: two adds, close one, the third admitted); `…_refuses_worker` (fresh key: the mint arm); `…_refuses_worker_replay` (a Worker repeats a Planner's already-bound request: -32403, nothing redelivered); `…_cap_refusal_lists_open_tracks`; `…_refuses_bound_creator`; `…_refuses_created_creator`; `…_refuses_child_creator`; `…_replays_and_refuses_changed_request`; `…_fingerprint_ignores_provider`; `…_delivers_text_once`; `plugin_track_meta_carries_provenance` (through `tools_call` and `forge_tools_call`). Mutations: drop the handler role check → {`refuses_worker_replay`} (the mint case stays refused by the backstop); drop the count → {`refuses_past_open_cap`, `cap_refusal_lists_open_tracks`}; also count closed Tracks → {`counts_only_open_tracks`}; drop `data.open` from the cap refusal → {`cap_refusal_lists_open_tracks`}; drop the scope check → {`refuses_bound_creator`}; drop the `creator_track_id` half → {`refuses_created_creator`}; drop the `parent_track_id` half → {`refuses_child_creator`}; omit `creator_key` → {`plugin_track_meta_carries_provenance`} |
| K2 | standing instructions (~500) | L2 | Tests: `plugin_instructions_follow_report_references`, `…_skip_unreferenced_tracks`, `…_skip_disabled_plugin`, `…_read_from_row_before_host_boot`, `…_skip_plugins_hidden_from_track`, `…_aggregate_never_exceeds_cap` (the §3.5 boundary and joiner fixtures: three 32-byte ids), `manifest_v4_refuses_planner_instructions`. Mutations: the report-reference predicate always `true` → {`skip_unreferenced_tracks`}; skip the 65 B reservation → {`aggregate_never_exceeds_cap`}; skip the visibility filter → {`skip_plugins_hidden_from_track`} |
| P1 | invest core (~1k, Python): ledger, decisions, executions, multi-symbol bridge, portfolio units, recipe | L2 | Tests: `test_weights_respect_bounds`, `test_held_after_counts_positions`, `test_sells_before_buys_settled_cash_only`, `test_research_track_cannot_trade`, `test_leg_remarks_are_unique`, `test_unowned_active_order_blocks`, `test_opening_positions_pin_first_reconciliation`, `test_cutover_refuses_unquiesced_account`, `test_projections_conserve_value` (40 held: slices and series sum to the total, 其他 equals the omitted sum), and a port of `caller_identity.rs:45`. Mutations: drop the Track fence → {`research_track_cannot_trade`}; drop 其他 from weights → {`projections_conserve_value`} |
| P2 | instruments, keys, lease, theses, research units and recipe, `planner_instructions` (~800) | L2 | Tests: `test_attestation_requires_portfolio_creator_and_current_key` (cases: a foreign creator; S's thesis written with the current key of another symbol; a `pending` S), `test_superseded_key_refused` (S's older key), `test_never_seen_key_goes_stale`, `test_lease_expiry_goes_stale` (`lease_days = 3`: not stale at day 2, stale at day 4), `test_board_shows_assessment_change` (only the assessment changes; the section label carries the new assessment), `test_set_issues_next_key_with_byte_identical_args`, `test_views_change_no_domain_state`, `test_last_seen_never_changes_authority` (and never bumps `version`), `test_fourth_open_thesis_refused_state_unchanged`, `test_rm_retires_open_theses`, `test_dropped_counts_toward_no_limit`, `test_units_fit_caps_at_max_config` (255 symbols, max-length CJK, every unit validated and ≤ 4 MiB), `test_unit_ids_injective`, kernel `invest_recipe_slots_resolve`. Mutations: accept any key of S → {`superseded_key_refused`}; accept any current key regardless of symbol → {`attestation_requires_portfolio_creator_and_current_key`}; drop the 120-minute bind window → {`never_seen_key_goes_stale`}; use the default 8 instead of the configured `lease_days` → {`lease_expiry_goes_stale`}; drop the assessment prefix → {`board_shows_assessment_change`}; drop the thesis cap → {`fourth_open_thesis_refused_state_unchanged`}; skip retiring on rm → {`rm_retires_open_theses`} |
| P3 | `series_show` on the Longbridge SDK (~600); remove `plugins/market` | L1 | Tests: `test_series_contract_matches_market_series`, `test_series_refuses_agent_caller`, kernel `chart_series_resolves_through_invest` (the real resolver, `resolver.rs:573-575`). Mutation: accept an agent caller → {`series_refuses_agent_caller`} |
| C | cut-over (§3.8) | ops | check C |

L2 means two independent review channels, re-run fresh after every fix (AGENTS.md).

## 5. Risks

- **Wake cost:** up to `max_held + max_watched` weekly research wakes, bounded by the limits and cap.
- **Overlay churn:** portfolio units republish every tick (#1995); research units refresh only on
  research calls and show their snapshot time. **Rule drift:** rules live only in the plugin.
- **Planner mistakes:** a first creation under the current key with other arguments *is* attested
  (provenance cannot see arguments); renewal recovers it (§3.3).
- **Silent superseded Tracks** hold cap slots until the owner closes them (§3.3, runbook step 8).

## 6. Rejected alternatives

The child-track route (F12) and `managed_track_identities`; a CLI-only `neige track add`; any
stored binding, handshake or listing-based existence check (§7); widening the unit contracts or
capping config at contract numbers; a kernel thesis object; links inside units.

## 7. Review findings

| Round | Finding | Resolution |
|---|---|---|
| 1 | Bound creator; fan-out; runbook order; quiescence; CLI row | Scope `All`; depth 1 with both halves; reset step; steps 1–3 and check C; owner 3 |
| 1–3 | Research lifecycle (binding, generations, lost-Track checks): blockers in every round | **Restructured (r4):** provenance + one issued key + lease. Deleted `coverage_add`/`coverage_rm`, the `coverages` table, the `reserved`/`checked`/`refused` states, the listing check and K1's `closed_at` |
| 1–3 | Unit contracts, churn, caps, ids, conservation | Aggregation at any config; only counted states; rm retires theses; input bounds; `VENUE.CODE`; CJK byte test; `test_projections_conserve_value` |
| 2 | Background calls carry a Track | F4 corrected; `series_show` keys on the absent caller |
| 3 | Simplifications | Deleted `thesis_ls`, `instrument_ls`, `sources`, `reviews` |
| 4 | A-B1/B-2 silent superseded Tracks eat the cap | Promise narrowed (§3.3): renewal restores coverage, not cleanup. The cap -32409 lists `{track_id, title}`; the Planner sends `neige_user_notify`; runbook step 8; test `cap_refusal_lists_open_tracks` |
| 4 | A-B2/B-3 predictions that cannot go red | K2 fixture re-derived (1,990 B texts: 2,099 vs 4,133 B); the role gate is only the in-transaction gate, and `refuses_worker` is a regression test; P2 has a cross-symbol case and two separate mutations |
| 4 | B-4 bootstrap | Step 6 sends a first message; check C requires `US:SPY` live and seen, plus calendar rows on both Tracks |
| 4 | B-1 wrong-argument Track under the right key | False promise deleted. Attestation is exactly "portfolio-created, current key, `live`". A mismatched creation is a Planner mistake, recovered by renewal (the fingerprint refuses that key forever) |
| 4 | Nits | Decision-log badges and disclosures; upper-case symbols and case-kept keys; `live` required; re-add issues n+1; `last_seen_at` does not bump `version`; one typed `_meta` builder; the view exception added to `agent-commands.md` §3; superseded folded into -32409; constant bind window; holdings branch deleted; fixed K2 notice; F23 cites both trees |
| 5 | B-1 assessment missing from the board | Sections are labeled `<assessment> · <title>` (title ≤ 110); test `board_shows_assessment_change` |
| 5 | B-2 replays bypass the role gate | The handler authorizes before both arms; the in-transaction gate is the mint backstop; test `refuses_worker_replay`; drop-check mutation → {`refuses_worker_replay`} |
| 5 | A-B1/B-3 drop-count red set | {`refuses_past_open_cap`, `cap_refusal_lists_open_tracks`} |
| 5 | Nits | Refusal lists `{track_id, creator_key}`, title `· <n>` deleted; non-default `lease_days` fixture; K2 mutation names the report-reference predicate; `decision_gate.rs:46-107` cited; V-class pointer in `agent-commands.md`; `cards.rs:173`; one spare slot (sum ≤ 255); research Worker drops `instrument_status` |
