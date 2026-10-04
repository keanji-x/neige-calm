# Invest: a multi-instrument portfolio with long-lived research Tracks

Baseline: `origin/main` 2dfb69d44. Every `file:line` below was read on that tree. Facts
marked **4140** come from one read-only query of the production database
(`calm.db?mode=ro`, 2026-10-04, migration 138).
Status: design, revision 3 (review rounds 1–2 folded in, §7). Issue: #2104. Review tier: **L2**
(authority, persistence and a broker writer). Docs only; no code changes.

**Owner direction (2026-10-04, not reopened):**
- One Python plugin `invest` merges `plugins/paper-trading` and `plugins/market`.
- One portfolio Track is the only broker writer. Each covered instrument gets an ordinary
  top-level research Track, and the two recipes are linked.
- Kernel gaps: `neige_track_add` and plugin standing instructions.
- Names follow `docs/conventions/agent-commands.md`; land after #2087 B0 and B5.
- Compatibility covers 4140 only.

**Owner decisions (on #2104):**
1. Fresh start at cut-over.
2. US market only.
3. `neige_track_add` is listed for the Planner. K1 frees the bytes by trimming existing
   descriptions and never raises the 30,000 B cap; there is no CLI row.
4. Retire `market`: the block on Track 7d686d59… moves to `invest`, and crypto charts are dropped.
5. Any stored recipe, bounded by the cap.
6. Instruments are **held** (target weight > 0 or a position) or **watched** (a research Track,
   no position), with their own limits `max_held` and `max_watched`. Watched → held is a weight
   change, not a new Track. Projections fit any configured value by aggregating. The kernel cap
   must hold held + watched.

No owner question is open.

## 1. Problem, goals, non-goals

**Today.** The paper plugin is a single-symbol SPY/cash allocator. `SPY.US` is fixed in the
order sizer (`plugins/paper-trading/paper_trading/allocation.py:152`), in the SDK checks
(`paper_trading/sdk_bridge.py:82,113,169`) and in the tool parameter `target_spy_bps`
(`plugins/paper-trading/manifest.json:41-45`). One Track owns the portfolio by config
(`allocation.py:44-45`). Research lives in that Track's prose (`spy-recipe.md:60-80`).

**Goals.**
1. Target weights over US instruments, within operator bounds (`max_held`, `max_watched`,
   per-instrument weight cap). The AI adds instruments inside the bounds.
2. One portfolio Track writes to the broker. Each covered instrument has its own long-lived
   research Track with its own calendar and recipe. Research Tracks never trade.
3. Views raised in the portfolio report are followed in the research Track, and their status
   flows back to the portfolio report.
4. A generic kernel: a Planner opens a Track from a recipe, and an external plugin gives
   Planners standing instructions. No plugin identity enters the kernel.

**Non-goals.**
- Real money, other brokers, a Rust SDK, non-US markets and crypto charts.
- A ledger-migration framework.
- Changing the renderer or the data-unit contracts.

## 2. Verified facts

| # | Fact | Evidence |
|---|---|---|
| F1 | A plugin may publish overlays onto any Track: the permission is checked per `entity_kind`, never per id. | `plugin_host/perms.rs:11-13`; `plugin_host/callbacks.rs:192-241` |
| F2 | A live slot resolves `(report's own track, plugin, kind)`. An invalid unit renders `unavailable`. | `mcp_server/tools/track_report_hydrate.rs:79,142-160,248-250` |
| F3 | A data unit is one `Component`. No component has a link field. | `calm-types/src/report_blocks/native_view/model.rs:105-109,215-233` |
| F4 | `tools/call` carries `_meta["dev.neige/track"]` and, for agent calls only, `"dev.neige/caller"` (role, card, session). The chart-series resolver passes the Track and **no caller**. | `plugin_host/mcp.rs:184-198,414-430`; `report_series/resolver.rs:573-575`; `caller_identity.rs:11-35`; `paper_trading/rpc.py:69-74` |
| F5 | A Track with `plugin_scope` sees only its owner plugin's tools; an unbound Track sees every enabled plugin. Template binding is limited to trusted forge plugins. | `mcp_server/tool_visibility.rs:13-37,57-73,75-120`; `forge_trust.rs:9-17` |
| F6 | The SPY recipe needs other plugins (Longbridge, Wisburg), so its Track is unbound. **4140:** 7c0dd087… has `plugin_scope` NULL and recipe d2a8d568… rev 3. | `spy-recipe.md:12` |
| F7 | Area links use the track-link form `neige.area.outline` documents, within one area only. `neige.report.backlinks` lists inbound links. | `prompts/tools/neige.area.outline.md`; `neige.report.backlinks.md` |
| F8 | The outline is truncated: at most 50 Tracks and 32 KiB, with omitted Tracks only counted. The `area/reports/` listing is complete or refused (> 500), includes closed Tracks (no `closed_at` filter) and lacks `closed_at`. | `report_links.rs:19-21,83,142-146,174-178`; `neige.track.ls.md`; `area_reports/store.rs:7-9,21-30` |
| F9 | Calendar entries belong to their Track, and each timed or weekly occurrence wakes that Track's Planner. | `builtin_plugins/calendar/store.rs:21,108`; `calendar/instructions.md:4,20-24` |
| F10 | Only built-ins inject live Planner instructions. Recipe text is a creation-time card snapshot. | `operation/planner_harness_start_adapter.rs:435-456`; `routes/tracks.rs:1459`; #2098 |
| F11 | One create entry point. `create_track_structure` reads a recipe in the write transaction. The keyed create binds `(area_id, key)` → track there and delivers a first message once. The REST digest covers REST fields. | `routes/tracks.rs:1248,1316-1332,1352-1376`; `routes/tracks/create.rs:184-291,334-345,474-508` |
| F12 | A binding row outlives its Track, so a deleted Track's key is dead forever. | `0088_track_create_idempotency.sql:40-48`; `create.rs:520-530` |
| F13 | The child-track route is task-shaped, charges the tree budget, inherits no recipe and sets `parent_track_id`. | `operation/child_track_adapter.rs:68-75,196-216,250-258,271` |
| F14 | `create_track` takes the area-delete lock and the Claude availability gate. MCP `AppContext` has no `RouteState`. A Planner's MCP actor is `AiPlannerSession`. A `TrackUpdated` event can carry a `message`, as `neige.track.close` does. | `routes/tracks.rs:773-787`; `mcp_server/registry.rs:78-81,155-189`; `tools/track_state.rs:294-297` |
| F15 | Recipes are human-only. | `routes/track_recipes.rs:119-127` |
| F16 | Planner tool surface: cap 30,000 B, 2,048 B per description. **Measured 29,929 B over 30 tools** (§3.4); the code comment's 29,909 is stale. | `mcp_server/tools/mod.rs:172-176`; `tests/goldens/mcp_tool_registry.json` |
| F17 | Unknown manifest fields are tolerated, so a new field needs a version bump; v4 is the latest. Plugin rows store the manifest and `enabled`. Plugins have only `kv`/`overlay`/`card`/`event` host callbacks and cannot read kernel config. | `plugin_host/manifest.rs:17-22,129-136`; `calm-truth/src/model.rs:238-250`; `callbacks.rs:124-140` |
| F18 | Data dir `<plugins_data_dir>/<id>/`. The SDK runs as a separate session (`start_new_session`) that SIGTERM to the plugin does not reach. Its CLI is `sdk_bridge.py … snapshot --request '{"since": …}'`. | `plugin_host/process.rs:56,250-258`; `allocation_broker.py:80-85,146-155`; `sdk_bridge.py:138-140,226-252` |
| F19 | Paper-only is the SDK identity proof (paper channel plus the account number from a daily statement). Cash is USD-only, and buys use settled cash. | `sdk_bridge.py:55-77,92-108` |
| F20 | Codex cannot approve an `openWorldHint` tool under `approval_policy: never`. | `plugins/market/README.md:33-46` |
| F21 | `chart.series` needs `VENUE:SYMBOL` ids and a `neige://plugin/<id>/<tool>` source. | `calm-types/src/report_blocks/chart_series.rs:131-139,163-167`; `kinds.rs:115-133` |
| F22 | Unit contracts: time series ≤ 4 datasets × 6 series × 500 points; distribution ≤ 12 slices; records ≤ 4 datasets × 100; table ≤ 32 columns × 500 rows; a record has ≤ 8 sections. | `native_view.rs:122-128,167,227`; `native_view/model.rs:228,243,441-443` |
| F23 | Reconciliation refuses unowned active orders and holdings that differ from executions. Budgets: 500 orders, 5,000 executions. | `allocation_reconcile.py:83-84,112-129`; `sdk_bridge.py:146-153` |
| F24 | Codex rebuilds instructions at thread start; a reset forces a new thread. Claude rebuilds them at every `open_session`. | `routes/cards.rs:198,1454-1455`; `harness/backend.rs:102`; `claude_planner/wiring.rs:35-46` |
| F25 | **4140:** `dev-neige-market` has no `plugin_kv` rows and no overlays. Open Tracks citing it: the SPY Track and 7d686d59…. The SPY Track has 4 calendar rows. | read-only query |

## 3. Decisions

### 3.1 Shape

```
portfolio Track (recipe invest-portfolio)        research Track ×(held+watched) (invest-instrument)
  Planner: instrument_*, coverage_rm, thesis_add/rm,  Planner: coverage_add, instrument_status,
           decision_add, neige_track_add                     thesis_set/ls; own calendar; no trading
  Worker:  execution_add ──► broker (paper)
           ▲ units portfolio.*, thesis.board           ▲ units instrument.position, thesis.records
           └──────── invest ledger (one SQLite file, one reconcile/submit loop) ────────┘
```

Both kinds of Track are ordinary, unbound and in one area (F5, F6, F7). Every write is fenced on
`_meta` (F4), as `allocation.py:43-55` does today.

### 3.2 Linkage: structured theses in the plugin ledger (D1)

A **thesis** is a plugin record: `thesis_id` (caller-chosen slug), `symbol`, `stance`
(`bullish|bearish|neutral`), `title`, `body`, `source_refs`, `assessment`
(`open|holding|at_risk|broken`) and `version`.
1. The portfolio raises it with `thesis_add`. The symbol must be live, and **at most 3 open
   theses per symbol** are allowed: the 4th is refused with no change.
2. The research Track covering the symbol assesses it with `thesis_set` under `expected_version`.
   It is admitted only if `_meta` Track equals the current coverage's `track_id` (§3.3).
3. The runtime republishes `thesis.board` (portfolio) and `thesis.records` (research) after each
   change and every tick (`runtime.py:15-27`, F1, F2).
4. The portfolio retires a thesis with `thesis_rm`. `instrument_rm` retires a symbol's open
   theses in the same transaction, so churn cannot accumulate open theses. Retiring is a verb,
   never an assessment value (convention §1.2).

**Links** are prose (F3). The portfolio's "覆盖标的" section links each research report (F7), and
each research report links back. `neige_link_ls` (today `neige.report.backlinks`) shows inbound
links. Recipes never copy an assessment into prose (the `spy-recipe.md:17` rule).

### 3.3 Discovery and coverage: generations, bound by an explicit write (D2)

**Truth.** The plugin's ledger is the one source of truth for symbol → research Track. It
drives publication (F1, F2) and the `thesis_set` fence. Tags are self-written
(`report_tag.rs:1-3`) and never count.

**Provenance in `_meta` (K1).** `_meta["dev.neige/track"]` becomes
`{id, creator_track_id, creator_key}`, filled from the Track row and `null` when absent
(`mcp.rs:425-427`). It is generic: no plugin identity.

**Coverage** is the research Track bound to a live instrument, in numbered **generations**:
- **Issuing a generation.** When an instrument becomes `checked`, the plugin mints generation
  `g` and stores the exact
  `track_add = {recipe_id, title, idempotency_key: "invest-<venue>-<code>-<g>", text, message}`.
- **Opening the Track.** The portfolio passes `track_add` verbatim to `neige_track_add`.
  - Retries are byte-identical, so a replay returns the same Track (F11).
  - The arguments do not exist before `checked`, so no Track exists before the symbol check.
  - A key is never reused, so a deleted Track's dead key (F12) cannot block re-coverage.
- **Binding (explicit write, B-1).** The research Planner's first step is `coverage_add {}`.
  The plugin admits it only if all of these hold:
  - `_meta.creator_track_id == portfolio_track_id`;
  - `creator_key` is the current generation's key;
  - that generation has no Track yet.

  It then records `track_id`. This rules out a portfolio-supplied id, a race with the
  portfolio and binding by an unrelated or cross-area Track. The portfolio has no creator, so it
  can never bind. Background chart calls carry no caller, and every write requires one.
- **Before binding.** `instrument_status` returns `{instrument: null, reason}` as a normal
  result. **Every view changes no state**, and a test proves it.
- **Ending a generation.** `coverage_rm {symbol, expected_version, message}` (portfolio) ends the
  current generation and issues `g + 1`. The old Track then sees `superseded`, and its writes are
  refused.
- **Dropping an instrument.** `instrument_rm` drops the instrument and ends coverage; that Track
  sees `dropped`.
- **Who closes a research Track.** Its own Planner closes it (`neige_track_close`) on `dropped`
  or `superseded`; the user may close it at any time.

**Lost Tracks (B-2).** The portfolio's weekly step checks each covered `track_id` against
`neige track ls area/reports/`. That listing is complete or refused, never truncated (F8). K1
adds `closed_at` to its rows (one column in `area_reports/store.rs:21-30`).
- A Track absent from a successful listing (deleted or moved) or closed while the instrument is
  live gets `coverage_rm`, and the new `track_add` follows.
- The truncated outline is never evidence (F8).
- A refused listing (> 500 reports) stops the step and reports it; nothing is superseded.

**Instrument states:** `reserved → checked | refused`, then `dropped` by `instrument_rm`. Only
`reserved` and `checked` are **live**. `refused` and `dropped` rows count toward no limit and may
be re-added; re-adding starts a new generation.

### 3.4 `neige_track_add` (kernel gap 1, D3)

**Contract.** Object `track`, verb `add` (§3). Input `{recipe_id, title, idempotency_key, text,
message}`, all required, closed schema.
- `text` (§4): the verbatim first message to the new Planner.
- `message` (§4): the audit note, carried on the creation `TrackUpdated` event the way
  `neige.track.close` carries its note (F14).

Result `{track_id, created_at}`; a replay returns the same object. Listed for the Planner
(owner 3); no CLI row.

| Aspect | Decision |
|---|---|
| Who | A Planner (a Worker gets -32403), on an open creator Track whose plugin scope is `All` (`tool_visibility.rs:57-73`); a bound or fail-closed creator would escape its fence. A reports-only managed Planner is refused (`managed_track.rs:215-226`). |
| Depth | 1. Refused when the creator has a `creator_track_id`, or separately when it has a `parent_track_id`. |
| Refusals | Role, scope, depth and cap refusals are -32403 (§5). The message names the cause; for the cap it gives `--track-add-max-open`, the cap and the open count. |
| Recipes | Any stored recipe (owner 5; recipes are human-only, F15). |
| Where | The creator's area (`registry.rs:66-74`), a managed workspace (`routes/tracks.rs:937-938`), the creator's Planner provider (`child_track_adapter.rs:240`), the default theme. Actor `AiPlannerSession` (F14). |
| Entry point | The keyed create (F11), extracted behind one function that takes an `ActorId`, a key and a request fingerprint. `POST /api/tracks` and the tool both call it, keeping the area-delete lock and the Claude gate (F14). The tool reaches it through `AppContext.track_creator: OnceCell<Arc<dyn TrackCreator>>`, set at boot like `operation_runtime` (`registry.rs:174`); the implementation holds `RouteState`. |
| Idempotency | The existing binding row, keyed `track-add/<creator_track_id>/<idempotency_key>` in the creator's area; REST refuses that prefix. The fingerprint covers only the tool's own inputs (`recipe_id`, `title`, `text`, `message`), not derived fields such as the provider (unlike `create.rs:334-345`). A different request under the key is -32409. |
| First message | `Opened by Track <creator_track_id>:` + `text`, delivered once (`create.rs:474-508`). |
| Provenance | Migration, numbered last: `tracks.creator_track_id TEXT` and `tracks.creator_key TEXT`, both or neither (a named `CHECK`, as in 0085), with no `REFERENCES` (0085's reason) and an index on `creator_track_id`. `parent_track_id` stays NULL and the tree budget is untouched (F13). Both values reach plugins through `_meta` (§3.3). The `area/reports/` rows gain `closed_at`. |
| Cap | `--track-add-max-open <u32>`, a clap arg on `Config` (`config.rs:8`, ranged like `:159-166`); 1..=256, default 16, no env var. Counts the creator's open created Tracks inside the create transaction (`routes/tracks.rs:1068`). |
| Cap vs. plugin limits | A plugin cannot read kernel config (F17), so `invest` cannot check at start that the cap ≥ `max_held + max_watched`. The runbook sets the cap to that sum plus a margin of 2 for superseded Tracks not yet closed. A mismatch shows at `neige_track_add` as the -32403 above. The instrument stays `checked` without coverage, and `portfolio_status` lists it under `uncovered`. |
| Events and UI | The ordinary create events (`routes/tracks.rs:1567-1590`), with `message` on `TrackUpdated`. No visible UI change; the generated `Track` type gains two fields. |
| Creator closes | Nothing cascades. |
| Budget | Measured 29,929 B: the 30 Planner rows of `mcp_tool_registry.json`, summing description bytes (prompt `trim_end`; `task.verdict` rendered with its guidance; every SHA-256 matched) and compact schema bytes. That leaves 71 B. K1 adds about 1.1 KB, so it trims at least that much from the largest descriptions (`report.commit` 1,535 B, `terminal.input` 1,479 B, `source.capture` 1,397 B, `plan.list` 1,362 B). It re-measures at its own base and corrects the comment. |

### 3.5 Plugin standing instructions (kernel gap 2, D4)

- **Manifest:** `planner_instructions: string`, ≤ 2,048 B, legal only at `manifest_version` 5
  (F17), validated when the manifest loads.
- **Which Tracks.** Not "bound only": invest Tracks cannot be bound (F5, F6). A plugin instructs a
  Planner when its row is enabled, its tools are visible to the Track
  (`TrackPluginScope::allows_manifest`), and either
  (a) the built-in rule holds (`planner_harness_start_adapter.rs:436-443`), or
  (b) the Track's current report references `neige://plugin/<id>/` in a view slot, a live table
  or `chart.series` (F2, F21).
  It is documentation and never authorizes anything (`:435`). One calm-types parser next to
  `validate_live_source` (`kinds.rs:123`) finds the references, and hydration's `view_slots`
  reuses it.
- **Source and timing.** The text is read from the enabled row's stored manifest (F17), not from
  the running process, so it does not race plugin-host boot. It is built in
  `planner_instructions` (`:417-457`).
  - Codex reads it at each thread start and keeps it until a reset.
  - Claude rebuilds it at every `open_session` (F24).
- **Aggregate cap: 4,096 B**, counting every appended byte: the `## Plugin <id>` headings, the
  texts and the omission markers. Plugins go in id order. A block is admitted only if, after it,
  the remaining budget still reserves 64 B for a marker line for each plugin not yet placed
  (B5 ids ≤ 32 B). Otherwise the line `## Plugin <id>: instructions omitted (budget)` is
  appended and a warning logged. Nothing is dropped silently.
- **Effect on #2098:** recipes keep only layout and schedule. Trust is the class of tool
  descriptions (`docs/architecture/1413-local-plugin-trust.md`).

### 3.6 Tool table (D5)

Plugin id `invest`; tools are served as `plugin_invest_<tool>`. Symbols use `VENUE:CODE` (F21);
only `sdk_bridge.py` converts them. P = portfolio Track, R = research Track; checks use `_meta`.

| Tool | §3 verb | Caller | Input (§4 names; domain keys in italics) | Replaces |
|---|---|---|---|---|
| `portfolio_status` | `status` V | P Planner/Worker | `{}` → snapshot, positions, targets, held/watched counts, `uncovered`, decisions, orders, fills, errors | `spy.status` |
| `decision_add` | `add` W | P Planner | `decision_id`, *`weights`* `[{symbol, bps}]`, `message`, *`source_refs`*, *`valid_until`* | `spy.plan` |
| `execution_add` | `add` W | P Worker | `decision_id` | `spy.execute` |
| `instrument_add` | `add` W | P Planner | *`symbol`*, `message` → row (with `track_add` once `checked`) | — |
| `instrument_rm` | `rm` W | P Planner | *`symbol`*, `expected_version`, `message`; refused while held | — |
| `instrument_ls` | `ls` V | P Planner | `{}` → rows, `held`, `watched`, limits | — |
| `coverage_add` | `add` W | R Planner | `{}`: symbol and generation come from `_meta` provenance (§3.3) | — |
| `coverage_rm` | `rm` W | P Planner | *`symbol`*, `expected_version`, `message` (issues g + 1) | — |
| `instrument_status` | `status` V | R Planner/Worker | `{}` → instrument, coverage, position, theses, or `{instrument: null, reason}` | — |
| `thesis_add` | `add` W | P Planner | `thesis_id`, *`symbol`*, *`stance`*, `title`, `body`, *`source_refs`* | — |
| `thesis_set` | `set` W | R Planner, own symbol | `thesis_id`, *`assessment`*, `summary`, *`source_refs`*, `expected_version` | — |
| `thesis_rm` | `rm` W | P Planner | `thesis_id`, `expected_version`, `message` | — |
| `thesis_ls` | `ls` V | P or R Planner | *`symbol`* (optional) | — |
| `series_show` | `show` V | chart resolver only | the `market.series` contract, US only | `market.series` |

Verb check against §3:
- `status`, `ls` and `show` are views and change no state. `add`, `set` and `rm` are writes.
- Every `set`/`rm` takes `expected_version`. `coverage_add` adds the caller's Track to a
  collection, with no value to lock.
- No compound actions, and no effect hidden in a parameter.
- `series_show` declares `openWorldHint: true`. It accepts a call only with Track `_meta` and
  **no** `dev.neige/caller`, which is the real resolver's shape (F4); any agent call is refused.

Retired:
- `spy.refresh`: the loop wakes on every write and polls every `poll_seconds`.
- `market.quote` and `market.holdings.*`: there are no holdings on 4140 (F25).

Unit kinds: `portfolio.{nav, nav_history, account, weights, weight_history, holdings,
decision_log, fill_log}` (#2102 layout) and `thesis.board`; research `instrument.position` and
`thesis.records`.

### 3.7 Ledger, limits, execution and projections (D6)

Fresh ledger `<plugins_data_dir>/invest/ledger.sqlite3`, `user_version` 1. It reuses `Ledger`'s
session, lock and journal (`ledger.py:20-107`).

```sql
CREATE TABLE instruments (symbol TEXT PRIMARY KEY, state TEXT NOT NULL CHECK (state IN
  ('reserved','checked','refused','dropped')), version INTEGER NOT NULL, body TEXT NOT NULL);
CREATE TABLE coverages (symbol TEXT NOT NULL REFERENCES instruments(symbol),
  generation INTEGER NOT NULL, track_id TEXT UNIQUE, ended_at TEXT, body TEXT NOT NULL,
  PRIMARY KEY (symbol, generation));                    -- body.track_add
CREATE TABLE decisions (id TEXT PRIMARY KEY, body TEXT NOT NULL, state TEXT NOT NULL,
  error TEXT, created_at TEXT NOT NULL);                 -- body.weights: {symbol: bps}
CREATE TABLE orders (id TEXT PRIMARY KEY, decision_id TEXT NOT NULL REFERENCES decisions(id),
  symbol TEXT NOT NULL, request TEXT NOT NULL, state TEXT NOT NULL, broker_id TEXT UNIQUE,
  broker_status TEXT, error TEXT, UNIQUE (decision_id, symbol));
CREATE TABLE theses (id TEXT PRIMARY KEY, symbol TEXT NOT NULL REFERENCES instruments(symbol),
  assessment TEXT NOT NULL, version INTEGER NOT NULL, retired_at TEXT, body TEXT NOT NULL);
-- unchanged shapes: meta, sources, fills, valuations (body gains positions{}), journal, reviews
```

**Held and watched (owner 6).**
- A live instrument is **held** when the latest decision gives it weight > 0 or it has a
  position; otherwise it is **watched**.
- `instrument_add` needs watched < `max_watched`. `decision_add` needs held-after ≤ `max_held`.
  Watched → held is only a weight change.
- When held becomes watched (sold) and that pushes watched over its limit, adds are blocked
  until the count is back under; `instrument_ls` shows it. No forced action follows.
- At ledger creation each `opening_positions` symbol becomes a `reserved` instrument (held by
  position) and goes through the normal check-and-coverage flow, so a held but uncovered symbol
  is ordinary. Config refuses `len(opening_positions) > max_held`.
- A weight > 0 needs `checked`. Coverage follows within the same portfolio turn, and
  `portfolio_status` lists any `uncovered` live symbol.

**Execution.**
- Carried over: one unresolved decision (`allocation.py:88-89`); validity ≤ 24 h (`:86-87`);
  each order ≤ `max_order_bps` (`:139-140`); commit before the broker write (`:201-202`); no
  resubmission of uncertain orders (`:210-212`); the unowned-active-order and holdings-equality
  refusals, per symbol (F23).
- New: one order per symbol beyond `drift_bps`, sells first. Each `bps` ≤ `max_weight_bps`, and
  the sum ≤ 10000 − `cash_buffer_bps`.
- **T+1:** buys use settled cash only (F19). An unfunded buy leg ends `noop` at `valid_until`,
  and the next decision continues; rebalancing converges over days.
- **Identity:** remark `nc-inv-` + `digest({account, decision, symbol})[:32]` (the 39-character
  check, `sdk_bridge.py:175`), with the same digest for `client_request_id`.
- **Budgets:** one SDK quote call covers all symbols; the F23 budgets stay account-wide.

**Projections fit any configured limits by aggregating (B-3, owner 6).** Held symbols include
uncovered `opening_positions`, and are ranked by market value:

| Unit | Contract (F22) | Projection |
|---|---|---|
| `portfolio.weights` | 12 slices | top 10 held + 其他 (sum of the rest, if any) + 现金 ≤ 12 |
| `portfolio.weight_history` | 6 series per dataset | top 4 held now + 其他 + 现金 = 6 |
| `portfolio.holdings` | 500 rows | top 499 held + 其他 |
| `thesis.board` | 100 records, 8 sections | one record per live symbol, with its ≤ 3 open theses as sections; held by weight then watched by symbol; top 99 + one 其他 record (counts by assessment) |
| `thesis.records` (research) | 100 records | ≤ 3 open + the 20 latest retired |
| `nav_history`, `decision_log`, `fill_log` | 500 points, 100 records, 500 rows | 260 points, 50 records, the latest 500 fills |

**Config** (closed schema, `manifest_version` 5): `account_no`, `broker_home`, `oauth_client_id`,
`sdk_python_path`, `access_region`; `portfolio_track_id`; `instrument_recipe_id`;
`max_held` and `max_watched` (each ≥ 1, no contract-derived ceiling); `max_weight_bps`,
`cash_buffer_bps`, `drift_bps`, `max_order_bps`, `quote_max_age_seconds`, `poll_seconds`;
`opening_positions: [{symbol, shares}]` (generalizes `opening_shares`, `allocation.py:115-122`;
immutable per ledger). The market is US by code (owner 2). `profile: spy_cash` is deleted; the
paper fence is the identity proof (F19).

### 3.8 4140 cut-over: fresh start (D7, owner 1)

The NAV history restarts, and the SPY Track stays readable. Owner-run after the
Sat 2026-10-10 verdict, on the deployed K1, K2 and P1–P3:
1. **Stop new SPY decisions.** Cancel the SPY Track's 4 calendar entries (F25), then close the
   Track.
2. **Quiesce (B-4).** Wait until every paper decision is final (none `queued`, `requested`,
   `submitting`, `working` or `unknown`). Disable the paper plugin. Require `pgrep -f
   sdk_bridge.py` to find nothing (F18).
3. **Snapshot by hand** with the paper config (F18), from the paper install dir:
   `env -i PATH="$PATH" HOME=<broker_home> <sdk_python_path> -I paper_trading/sdk_bridge.py
   --access-region <access_region> --client-id <oauth_client_id> --account <account_no> snapshot
   --request '{"since": null}'`.
   Require no order in an active status. Its `shares` (SPY) is the holding recorded for step 5.
4. Save both invest recipes. Create the portfolio Track from `invest-portfolio` with no first
   message.
5. Install and enable `invest` with `portfolio_track_id`, `instrument_recipe_id`,
   `opening_positions = [{symbol: "US:SPY", shares: <step 3>}]`, `max_held`, `max_watched`. Set
   `--track-add-max-open` ≥ `max_held + max_watched + 2` (§3.4).
6. **Reset the portfolio Planner** (`POST /api/cards/<planner>/planner/reset`, F24). Its first
   thread predates `invest`.
7. Its first turn covers `US:SPY` (§3.3).
8. Edit 7d686d59…'s `chart.series` source to `neige://plugin/invest/series_show`, then uninstall
   `market` (owner 4).

**Acceptance check C:** the first `invest` reconciliation succeeds with holdings equal to
`opening_positions` and no unowned active order. An extra active order on the fake broker must
fail it (`test_cutover_refuses_unquiesced_account`).

## 4. Slices

Order: #2087 B0 → … → B5, then K1 ∥ K2 → P1 → P2 → P3 → C. K1 and K2 are inert until used and
may ride any kernel deploy. P1–P3 and C deploy after the verdict, and P1 rebases on #2102.

Gates:
- every slice: `scripts/local-ratchet-gates.sh`;
- K1/K2: the whole `-p calm-server` run and `scripts/local-rust-gates.sh --quick`; K1 also
  regenerates OpenAPI and the `fe` types (no visible UI, so no browser gate);
- P*: `python3 -m pytest plugins/invest/tests -q`.

Each mutation is single-factor in production code, and `→ {…}` is the complete predicted red set.

| # | Slice (≈ lines) | Tier | Acceptance | Must go red first |
|---|---|---|---|---|
| K1 | `neige_track_add`, provenance in `_meta`, `closed_at` on `area/reports/` rows, description trims (~1.1k) | L2 | §3.4 holds, and the surface stays ≤ 30,000 B | Tests: `track_add_records_provenance_not_parent`, `…_refuses_past_open_cap`, `…_counts_only_open_tracks`, `…_refuses_worker`, `…_refuses_bound_creator`, `…_refuses_created_creator`, `…_refuses_child_creator`, `…_replays_and_refuses_changed_request`, `…_fingerprint_ignores_provider`, `…_delivers_text_once`, `plugin_track_meta_carries_provenance`, `area_reports_rows_carry_closed_at`. Mutations: count closed Tracks → {`counts_only_open_tracks`}; drop the count → {`refuses_past_open_cap`}; drop the role gate → {`refuses_worker`}; drop the scope check → {`refuses_bound_creator`}; drop the `creator_track_id` half → {`refuses_created_creator`}; drop the `parent_track_id` half → {`refuses_child_creator`}; set `parent_track_id` → {`records_provenance_not_parent`}; omit `creator_key` → {`plugin_track_meta_carries_provenance`} |
| K2 | standing instructions (~600) | L2 | §3.5 holds for Codex and Claude | `plugin_instructions_follow_report_references`, `…_skip_unreferenced_tracks`, `…_skip_disabled_plugin`, `…_read_from_row_before_host_boot`, `…_aggregate_counts_headings_and_markers`, `manifest_v4_refuses_planner_instructions`, `manifest_refuses_instructions_over_2048_bytes`. Mutations: predicate `true` → {`skip_unreferenced_tracks`}; read from the running host → {`read_from_row_before_host_boot`}; exclude markers from the sum → {`aggregate_counts_headings_and_markers`} |
| P1 | `plugins/invest` core (~1k, Python): ledger, decisions, executions, multi-symbol bridge, portfolio units and recipe | L2 | Weights execute sells-then-buys within caps on the fake broker; only P writes; the ported `caller_identity.rs:45` test passes | `test_weights_respect_bounds`, `test_held_limit_counts_positions`, `test_sells_before_buys_settled_cash_only`, `test_research_track_cannot_trade`, `test_leg_remarks_are_unique`, `test_unowned_active_order_blocks`, `test_opening_positions_pin_first_reconciliation`, `test_cutover_refuses_unquiesced_account`. Mutation: drop the Track fence → {`research_track_cannot_trade`} |
| P2 | instruments, coverage, theses, research units and recipe, `planner_instructions` (~1k) | L2 | §3.3 end to end; a thesis set in R shows on P's board within one tick; units validate against the exported schema at any config | `test_coverage_add_requires_attested_creator_and_key`, `test_portfolio_never_binds`, `test_coverage_rm_supersedes_old_track`, `test_track_add_args_byte_identical`, `test_views_change_no_state`, `test_fourth_open_thesis_refused_state_unchanged`, `test_rm_retires_open_theses`, `test_refused_and_dropped_count_toward_no_limit`, `test_lost_track_needs_complete_listing` (research Track absent from a 60-Track outline but present in the listing: not superseded), `test_units_fit_contracts_at_any_config` (`max_held` 40, `max_watched` 150, 3 theses each, 500 add/rm churn cycles), kernel `invest_recipe_slots_resolve`. Mutations: accept any creator → {`coverage_add_requires_attested_creator_and_key`}; bind inside `instrument_status` → {`views_change_no_state`}; drop the thesis cap → {`fourth_open_thesis_refused_state_unchanged`}; skip retiring on rm → {`rm_retires_open_theses`}; drop 其他 in weights → {`units_fit_contracts_at_any_config`} |
| P3 | `series_show` on the Longbridge SDK (~600); remove `plugins/market` | L1 | US reply contract equals `market.series`; both recipes' charts render | `test_series_contract_matches_market_series`, `test_series_refuses_agent_caller`, kernel `chart_series_resolves_through_invest` (real resolver, `resolver.rs:573-575`). Mutation: accept an agent caller → {`series_refuses_agent_caller`} |
| C | 4140 cut-over (§3.8) | ops | check C | — |

L2 means two independent review channels, re-run fresh after every fix (AGENTS.md).

## 5. Risks

- **Wake cost:** up to `max_held + max_watched` research Planners wake on their calendars. The
  limits and the kernel cap bound them.
- **Overlay churn:** every tick republishes all units; #1995 owns throttling.
- **Rule drift:** rules live only in the plugin, and review checks that recipes hold none.
- **Orphans:** a Track created from other arguments never binds. It stays open inside the cap
  until the user closes it.

## 6. Rejected alternatives

- The child-track route (F13) and `managed_track_identities` (template-only).
- A CLI-only `neige track add`.
- A portfolio-supplied `track_id`, and binding inside a view.
- The outline as an existence check (F8).
- Widening native-view limits, or capping config at contract numbers (owner 6).
- A kernel thesis object, or links inside units.

## 7. Review findings

| Round | Finding | Resolution |
|---|---|---|
| 1 | A-B1 bound creator escapes its fence | Fixed: scope must be `All` (§3.4) |
| 1 | A-B2 recursive fan-out | Narrowed to depth 1; both halves tested separately (r2 B-4) |
| 1 | A-B3, B-1, B-2 binding sequence | Fixed: generations plus kernel provenance (§3.3); `instrument_set` deleted |
| 1 | A-B4 runbook order | Fixed: reset step 6; K2 reads enabled rows |
| 1 | B-3 unit contracts | Fixed by aggregation at any config (r3, owner 6) |
| 1 | B-4 broker quiescence | Fixed: steps 1–3, check C |
| 1 | B-5 CLI row | Owner 3: listed |
| 2 | A-B1 churn breaks the board | Fixed: only live states count; `instrument_rm` retires theses in one transaction; refused/dropped re-addable; churn case in the boundary test |
| 2 | B-1 binding inside a view | Fixed: explicit `coverage_add` by the research Track; re-coverage is `coverage_rm`; views-change-no-state test |
| 2 | B-2 truncated outline | Fixed: complete `area/reports/` listing plus `closed_at`; truncation test |
| 2 | B-3 background calls carry a Track | Fixed: F4 corrected; `series_show` keys on the absent caller; the real resolver is tested |
| 2 | B-4 tests that cannot go red | Fixed: 4th-thesis refusal test; separate depth halves; Worker test |
| 2 | owner 6 held/watched | Applied: §3.7 limits and projections, §3.4 cap mismatch |
| 2 | nits | Fingerprint over tool inputs; `AiPlannerSession`; `decision_add` `message`; `text`/`message` split; calendar first; snapshot command; Claude vs Codex timing; aggregate counts markers |
