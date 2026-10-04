# Invest: a multi-instrument portfolio with long-lived research Tracks

Baseline: `origin/main` 2dfb69d44. Every `file:line` below was read on that tree. Facts
marked **4140** come from one read-only query of the production database
(`calm.db?mode=ro`, 2026-10-04, migration 138).
Status: design, revision 2 (review round 1 folded in, §7). Issue: #2104. Review tier: **L2**
(authority, persistence and a broker writer). Docs only; no code changes.
Owner direction (2026-10-04, not reopened here): one Python plugin `invest` merges
`plugins/paper-trading` and `plugins/market`; one portfolio Track is the only broker writer;
each covered instrument gets an ordinary top-level research Track; two linked recipes; kernel
gaps `neige_track_add` and plugin standing instructions; names per
`docs/conventions/agent-commands.md`; land after #2087 B0 and B5; compatibility covers 4140 only.
**Owner decisions (2026-10-04, on #2104):** (1) fresh start at cut-over; (2) US market only;
(3) `neige_track_add` is listed in the Planner `tools/list`, and K1 frees the bytes by trimming
existing descriptions, never by raising the 30,000 B cap; no CLI row; (4) retire `market`: the
block on Track 7d686d59… moves to `invest`, and crypto charts are dropped; (5) any stored recipe,
bounded by the cap. No owner question remains open.

## 1. Problem, goals, non-goals

**Today.** The paper plugin is a single-symbol SPY/cash allocator. `SPY.US` is fixed in the
order sizer (`plugins/paper-trading/paper_trading/allocation.py:152`), in the SDK position,
quote and preflight checks (`paper_trading/sdk_bridge.py:82,113,169`), and in the one tool
parameter `target_spy_bps` (`plugins/paper-trading/manifest.json:41-45`). One Track owns it
by configuration (`allocation.py:44-45`). Research lives in that Track's prose
(`spy-recipe.md:60-80`), so the AI can neither add an instrument nor follow a view over time.

**Goals.**
1. Target weights over several US instruments, bounded by operator config (maximum instrument
   count, per-instrument weight cap). The AI adds instruments inside the bounds.
2. One portfolio Track writes to the broker. Every covered instrument has its own long-lived
   research Track with its own calendar and recipe. Research Tracks never trade.
3. Views raised in the portfolio report are followed in the research Track, and their status
   flows back to the portfolio report.
4. The kernel stays generic. A Planner can open a Track from a recipe, and an external plugin can
   give Planners standing instructions read at Planner start. No plugin identity in the kernel.

**Non-goals.** Real money, other brokers, a Rust SDK, markets other than US, crypto charts.
A general ledger-migration framework. Changing the report renderer or the data-unit contracts.

## 2. Verified facts the design relies on

| # | Fact | Evidence |
|---|---|---|
| F1 | A plugin may publish overlays onto any Track: the permission is checked per `entity_kind`, never per entity id. | `plugin_host/perms.rs:11-13`; `plugin_host/callbacks.rs:192-241` |
| F2 | A live slot resolves `(report's own track, plugin, kind)`, so the same kind on two Tracks is two units. An invalid unit renders `unavailable`. | `mcp_server/tools/track_report_hydrate.rs:79,142-160,248-250` |
| F3 | A data unit is one `Component` cell. No component has a link field. | `calm-types/src/report_blocks/native_view/model.rs:105-109,215-233` |
| F4 | A plugin tool learns the calling Track and the caller's role from host `_meta`, never from arguments. Kernel-initiated calls carry no Track. | `plugin_host/mcp.rs:184-198,414-429`; `tests/cases/mcp_plugin_tools/caller_identity.rs:11-35`; `paper_trading/rpc.py:69-74` |
| F5 | A Track with `plugin_scope` sees only its owner plugin's tools. An unbound Track sees every enabled plugin. | `mcp_server/tool_visibility.rs:13-37,75-120` |
| F6 | Template binding is limited to trusted forge plugins (built-ins by default). | `forge_trust.rs:9-17` |
| F7 | The SPY recipe needs other plugins' tools (Longbridge, Wisburg), so its Track is unbound. **4140:** Track 7c0dd087… has `plugin_scope` NULL and recipe d2a8d568… rev 3. | `spy-recipe.md:12` |
| F8 | Report links use the track-link form `neige.area.outline` documents, within one area only. `neige.report.backlinks` lists inbound links. `area/reports/` rows carry the track id. The outline lists each area Track with `closed_at`; a deleted Track is absent. | `prompts/tools/neige.area.outline.md`; `neige.report.backlinks.md`; `area_reports/store.rs:7-9`; `mcp_server/tools/report_links.rs:109-114` |
| F9 | Calendar entries belong to the Track that created them. Each timed or weekly occurrence wakes that Track's Planner. | `builtin_plugins/calendar/store.rs:21,108`; `builtin_plugins/calendar/instructions.md:4,20-24` |
| F10 | Only built-ins inject live Planner instructions, gated on `plugin_scope` or template ownership. Recipe text reaches the Planner as a creation-time card snapshot. | `operation/planner_harness_start_adapter.rs:435-456`; `routes/tracks.rs:1459`; #2098 |
| F11 | One create entry point. `create_track_structure` reads a recipe inside the write transaction and stamps `recipe_id`/`revision`. The keyed create binds `(area_id, key)` → track there and delivers a first message once. | `routes/tracks.rs:1248,1316-1332,1352-1376`; `routes/tracks/create.rs:184-291,474-508` |
| F12 | A binding row outlives its Track. Replaying a key whose Track was deleted is refused, so that key is dead forever. | `0088_track_create_idempotency.sql:40-48`; `create.rs:520-530` |
| F13 | The child-track route is task-shaped: payload `{task_id, parent_track_id, goal, …}`, it charges the tree task budget, inherits no recipe and sets `parent_track_id`. | `operation/child_track_adapter.rs:68-75,196-216,250-258,271` |
| F14 | `create_track` takes the area-delete lock and the Claude availability gate before minting. The MCP `AppContext` has no `RouteState`, only late-bound `OnceCell` handles. | `routes/tracks.rs:773-787`; `mcp_server/registry.rs:155-189` |
| F15 | Recipes are human-only: no agent-facing write path exists. | `routes/track_recipes.rs:119-127` |
| F16 | The Planner tool surface (description plus compact schema of each Planner-visible tool) has a 30,000 B cap, with 2,048 B per description. **Measured 29,929 B over 30 tools** (method in §3.4); the code comment's 29,909 is stale. | `mcp_server/tools/mod.rs:172-176`; `tests/goldens/mcp_tool_registry.json` |
| F17 | Manifest unknown fields are tolerated; a new field needs a `manifest_version` bump. v4 is the latest. Plugin rows store the manifest and an `enabled` flag. | `plugin_host/manifest.rs:17-22,129-136`; `calm-truth/src/model.rs:238-250` |
| F18 | A plugin's data dir is `<plugins_data_dir>/<id>/`. The SDK runs in a separate session that is not the plugin's process. | `plugin_host/process.rs:56,250-258`; `allocation_broker.py:80-85` |
| F19 | Paper-only is enforced by the SDK identity proof: the paper channel, plus the account number read from a daily statement. Cash is USD-only. | `sdk_bridge.py:55-77,92-108` |
| F20 | Codex cannot approve a network-touching (`openWorldHint`) tool under `approval_policy: never`. Writes therefore record state and wake a loop. | `plugins/market/README.md:33-46` |
| F21 | `chart.series` requires `VENUE:SYMBOL` ids and a `neige://plugin/<id>/<tool>` source. | `calm-types/src/report_blocks/chart_series.rs:131-139,163-167`; `kinds.rs:115-133` |
| F22 | Unit contracts: time series ≤ 4 datasets × 6 series × 500 points; distribution ≤ 12 slices; records ≤ 4 datasets × 100 records; table ≤ 32 columns × 500 rows. | `native_view.rs:122-128,167,227`; `native_view/model.rs:243,441-443` |
| F23 | Reconciliation refuses unowned active broker orders, and refuses holdings that differ from owned executions. Budgets: ≤ 500 orders, ≤ 5,000 executions. | `allocation_reconcile.py:83-84,112-129`; `sdk_bridge.py:146-153` |
| F24 | A Planner reset starts a new thread (`force_new_thread`), so it rebuilds the instructions. | `routes/cards.rs:198,1454-1455` |
| F25 | **4140:** `dev-neige-market` has no `plugin_kv` rows and no overlays. Open Tracks citing `neige://plugin/dev-neige-market`: the SPY Track and 7d686d59…. The SPY Track has 4 calendar rows. | read-only query |

## 3. Decisions

### 3.1 Shape

```
portfolio Track (recipe invest-portfolio)       research Track ×N (recipe invest-instrument)
  Planner: instrument_add/set/rm/ls, thesis_add/rm,  Planner: instrument_status, thesis_set/ls
           decision_add, neige_track_add            calendar: its own entries; never trades
  Worker:  execution_add ──► broker (paper)
           ▲ units portfolio.*, thesis.board          ▲ units instrument.position, thesis.records
           └────────── invest ledger (one SQLite file, one reconcile/submit loop) ──────────┘
```

Both kinds of Track are ordinary, unbound and in one area (F5, F7, F8). Every write is fenced on
the host-supplied Track and role (F4), as `allocation.py:43-55` does today.

### 3.2 Linkage: structured theses in the plugin ledger (D1)

A **thesis** is a plugin record: `thesis_id` (caller-chosen slug), `symbol`, `stance`
(`bullish|bearish|neutral`), `title`, `body`, `source_refs`, `assessment`
(`open|holding|at_risk|broken`) and `version`. Each covered instrument holds **at most 3 open
theses** (§3.7, F22).

1. The portfolio Planner raises a view with `thesis_add` (covered symbol only).
2. The research Track bound to that symbol assesses it with `thesis_set` under
   `expected_version`. Binding is kernel-attested (§3.3), so no other Track passes this fence.
3. After each change the runtime republishes `thesis.board` (portfolio) and `thesis.records`
   (research Track). The loop already republishes every tick (`runtime.py:15-27`), and F1/F2
   let one plugin serve both Tracks.
4. Only the portfolio retires a thesis, with `thesis_rm`; the journal keeps it. Retiring is a
   verb, never `assessment: retired` (convention §1.2).

**Report links** are prose (F3). The portfolio report's "覆盖标的" section links each research
report in the F8 form. Each research report links back to the thesis section, and
`neige_link_ls` (today `neige.report.backlinks`) shows inbound links. The recipes forbid copying
an assessment into prose; the units are the only status (the `spy-recipe.md:17` rule). No new
kernel mechanism is needed.

### 3.3 Discovery and binding: generations, attested by the kernel (D2)

**Truth.** The plugin's `instruments` table is the one source of truth for symbol → research
Track. It is what publication (F1, F2) and the `thesis_set` fence use. Tags are self-written
(`report_tag.rs:1-3`) and never count. The portfolio reads a report in depth by matching
`instrument_ls`'s `track_id` against `neige track ls area/reports/` (F8; the field is `trackId`
until #2087 B3).

**Binding without an agent-supplied id.** K1 adds generic provenance to every plugin call:
`_meta["dev.neige/track"]` becomes `{id, creator_track_id, creator_key}`, filled from the Track
row (`null` when absent), next to the existing id (`mcp.rs:184-187,426`). Each coverage episode
is a **generation**. The plugin binds Track T to `(symbol, gen)` the first time T calls any
`invest` tool with `creator_track_id == portfolio_track_id` and `creator_key` equal to that
generation's key. This removes the race, the unverified id and `instrument_set {track_id}`:
- The binding needs no portfolio call, so it cannot race the research Planner's first turn.
- The portfolio Track itself has no creator, so it can never bind.
- An unrelated or cross-area Track cannot match a kernel-written creator and key.

Recording T on first sight is the one write inside a view. It caches a kernel-attested fact and
changes no domain state; the alternative is a second agent call that can be forgotten.

**Lifecycle** (`instruments.state`):

| From | Event | To | Effect |
|---|---|---|---|
| — | `instrument_add {symbol}` (bounds: `max_instruments`, US, unknown symbol) | `reserved` | gen g = 1, or previous g + 1; no network (F20) |
| `reserved` | loop's SDK symbol check | `checked` or `refused` | `checked` exposes `track_add` = exact `{recipe_id, title, idempotency_key: "invest-<venue>-<code>-<g>", text}` |
| `checked` | portfolio `neige_track_add(track_add)`, then T's first call | `covered` | `track_id` = T |
| `covered`/`checked` | `instrument_set {symbol, expected_version}` (Track lost) | `checked` | g + 1, new `track_add`; the old Track now sees `superseded` |
| `covered`/`checked` | `instrument_rm` (target 0 and no position) | `dropped` | units on T stop; T sees `dropped` |

- **Retries are byte-identical** because the plugin stores `track_add` and the Planner passes it
  verbatim. A replay returns the same Track (F11).
- **A key is never reused across generations,** so a deleted Track's dead key (F12) cannot block
  re-coverage.
- **No Track exists before `checked`:** the arguments do not exist until then.
- **Before binding,** `instrument_status` from T returns `{instrument: null, reason}` as a normal
  result. A Track whose key names an older generation gets `state: superseded`; a dropped one
  gets `dropped`.
- **Who closes:** the research Planner closes its own Track (`neige_track_close`) when its status
  is `dropped` or `superseded`. The user may close it at any time.
- **Detecting a lost Track:** the portfolio's weekly step compares `instrument_ls` with
  `neige_area_ls` (F8). A covered Track that is closed or absent gets `instrument_set` (a new
  generation) while a position is held, otherwise `instrument_rm`.

### 3.4 `neige_track_add` (kernel gap 1, D3)

**Contract.** Object `track`, verb `add` (§3). Input `{recipe_id, title, idempotency_key, text}`,
all required, closed schema. `text` is the verbatim first message to the new Planner (§4 `text`);
it doubles as the audit note, and the kernel records it as that Track's first turn. Result
`{track_id, created_at}`; a replay returns the same object. **Listed** for the Planner (owner 3);
no CLI row.

| Aspect | Decision |
|---|---|
| Who | Planner only, on an open creator Track whose plugin scope is `All` (`tool_visibility.rs:57-73`). A bound or fail-closed Track would otherwise mint unbound Tracks and escape its fence (A-B1). Refused for a reports-only managed Planner (`managed_track.rs:215-226`). Every role or scope refusal is -32403 (§5). |
| Depth | 1. Refused when the creator has a `creator_track_id` or a `parent_track_id` (A-B2), so fan-out is at most the cap. |
| Recipes | Any stored recipe (owner 5); recipes are human-only (F15). |
| Where | The creator's area (`registry.rs:66-74`), with a managed workspace (`routes/tracks.rs:937-938`), the creator's Planner provider (`child_track_adapter.rs:240`) and the default theme. |
| Entry point | The keyed create (F11), extracted behind one function that takes an `ActorId` and a key. `POST /api/tracks` and the tool both call it; it keeps the area-delete lock and the Claude gate (F14). The tool reaches it through a new `AppContext` handle `OnceCell<Arc<dyn TrackCreator>>`, set at boot like `operation_runtime` (`registry.rs:174`), whose implementation holds `RouteState`. No second minting path. |
| Idempotency | The existing binding row, keyed `track-add/<creator_track_id>/<idempotency_key>` in the creator's area. REST refuses header keys with that prefix. The fingerprint makes a replay return the same Track; a different request under the key is -32409. |
| First message | `Opened by Track <creator_track_id>:` + `text`, delivered once (`create.rs:474-508`). It names the id, not the title, so a rename between retries keeps the digest. |
| Provenance | Migration, numbered last: `tracks.creator_track_id TEXT` and `tracks.creator_key TEXT`, both or neither (a named `CHECK`, as in 0085). No `REFERENCES` (0085's reason). Index on `creator_track_id`. `parent_track_id` stays NULL and the tree budget is untouched (F13). Both values reach plugins through `_meta` (§3.3). |
| Cap | `--track-add-max-open <u32>`, a clap arg on `Config` (`config.rs:8`, ranged like `:159-166`); 1..=64, default 16, no env var. Counts open Tracks with this creator inside the create transaction (`BEGIN IMMEDIATE`, `routes/tracks.rs:1068`). Over the cap: -32409. |
| Events and UI | The ordinary `TrackUpdated`/`CardAdded` events (`routes/tracks.rs:1567-1590`), actor `AiPlanner(card)`. No visible UI change: the generated `Track` type gains the two fields. |
| Creator closes | Nothing cascades. A closed creator cannot call, and the cap counts only open created Tracks. |
| Budget | Measured 29,929 B by summing the 30 Planner rows of `mcp_tool_registry.json`: description bytes (prompt file `trim_end`, `task.verdict` rendered with its guidance; every SHA-256 matched the golden) plus compact schema bytes. Headroom: 71 B. K1 adds about 1 KB, so it trims at least that much from the largest descriptions (`terminal.input` 1,479 B, `report.commit` 1,535 B, `plan.list` 1,362 B, `source.capture` 1,397 B). It re-measures at its own base and updates the stale comment. |

### 3.5 Plugin standing instructions (kernel gap 2, D4)

- **Manifest:** `planner_instructions: string`, at most 2,048 B (the per-description cap, F16).
  Legal only at `manifest_version` 5 (F17). Validated when the manifest loads.
- **Which Tracks.** Not "bound only": invest Tracks cannot be bound (F5, F6, F7). A plugin
  instructs a Planner when its plugin row is enabled, its tools are visible to the Track
  (`TrackPluginScope::allows_manifest`), and either
  (a) the built-in rule holds (`planner_harness_start_adapter.rs:436-443`), or
  (b) the Track's current report references `neige://plugin/<id>/` in a view slot, a live table
  or `chart.series` (F2, F21).
  This keeps "documentation follows the saved template; it never enables or authorizes tools"
  (`:435`). One calm-types parser next to `validate_live_source` (`kinds.rs:123`) finds the
  references, and hydration's `view_slots` reuses it.
- **Source and timing.** The text comes from the enabled plugin row's stored manifest (F17), not
  from the running process. So a Claude session that opens before the plugin host has booted
  still gets it. It is read in `planner_instructions` (`:417-457`), shared by Codex
  `thread/start` and the Claude session, at every thread start. A running thread keeps its
  instructions until reset (F24).
- **Aggregate cap** of 4,096 B per Planner, in plugin-id order. A plugin that would exceed it is
  replaced by the line `## Plugin <id>: instructions omitted (budget)` and a warning log, never
  dropped silently.
- **Effect on #2098:** recipes keep layout and schedule only; the operating rules live in the
  plugin and stay current. Trust is the same class as tool descriptions
  (`docs/architecture/1413-local-plugin-trust.md`).

### 3.6 Tool table (D5)

Plugin id `invest`; tools are served as `plugin_invest_<tool>`. Symbols use the kernel's
`VENUE:CODE` (F21); only `sdk_bridge.py` converts to Longbridge's `CODE.VENUE`.
P = portfolio Track, R = a bound research Track. Every role and Track check uses `_meta` (F4).

| Tool | §3 verb | Caller | Input (§4 names; domain keys in italics) | Replaces |
|---|---|---|---|---|
| `portfolio_status` | `status` V | P Planner/Worker | `{}` → snapshot, positions, targets, decisions, orders, fills, errors | `spy.status` |
| `decision_add` | `add` W | P Planner | `decision_id`, *`weights`* `[{symbol, bps}]`, *`rationale`*, *`source_refs`*, *`valid_until`* | `spy.plan` |
| `execution_add` | `add` W | P Worker | `decision_id` (an execution request; the loop submits) | `spy.execute` |
| `instrument_add` | `add` W | P Planner | *`symbol`*, `message` → row (incl. `track_add` once `checked`) | — |
| `instrument_set` | `set` W | P Planner | *`symbol`*, `expected_version`, `message` (new generation, §3.3) | — |
| `instrument_rm` | `rm` W | P Planner | *`symbol`*, `expected_version`, `message`; refused while target or position > 0 | — |
| `instrument_ls` | `ls` V | P Planner | `{}` → rows, `slots_left` | — |
| `instrument_status` | `status` V | R Planner/Worker | `{}` → this Track's instrument, position and theses, or `{instrument: null, reason}` | — |
| `thesis_add` | `add` W | P Planner | `thesis_id`, *`symbol`*, *`stance`*, `title`, `body`, *`source_refs`* | — |
| `thesis_set` | `set` W | R Planner, own symbol | `thesis_id`, *`assessment`*, `summary`, *`source_refs`*, `expected_version` | — |
| `thesis_rm` | `rm` W | P Planner | `thesis_id`, `expected_version`, `message` | — |
| `thesis_ls` | `ls` V | P or R Planner | *`symbol`* (optional) | — |
| `series_show` | `show` V | the kernel's `chart.series` only | the `market.series` contract, US only | `market.series` |

Verb check against §3:
- `status`, `ls` and `show` are views; `add`, `set` and `rm` are writes, and every `set`/`rm`
  takes `expected_version`.
- No compound actions, and no effect hidden in a parameter.
- `show` is the closest word for one named series set.
- `series_show` touches the network, so it declares `openWorldHint: true`. Codex Planners
  therefore cannot use it (F20). Its only caller is the kernel's background `chart.series`
  resolution, which carries no Track (F4), and research uses the Longbridge connector instead.

Retired:
- `spy.refresh`: the loop wakes on every write and polls every `poll_seconds`.
- `market.quote` and `market.holdings.*`: no holdings exist on 4140 (F25).

Unit kinds: portfolio `portfolio.{nav, nav_history, account, weights, weight_history, holdings,
decision_log, fill_log}` (#2102's layout) and `thesis.board`; research `instrument.position` and
`thesis.records`.

### 3.7 Ledger, execution and bounded projections (D6)

Fresh `invest` ledger at `<plugins_data_dir>/invest/ledger.sqlite3`, `user_version` 1. It reuses
`Ledger`'s session, lock and journal (`ledger.py:20-107`).

```sql
CREATE TABLE instruments (symbol TEXT PRIMARY KEY, state TEXT NOT NULL CHECK (state IN
  ('reserved','checked','covered','dropped','refused')), generation INTEGER NOT NULL,
  track_id TEXT UNIQUE, version INTEGER NOT NULL, body TEXT NOT NULL);  -- body.track_add
CREATE TABLE decisions (id TEXT PRIMARY KEY, body TEXT NOT NULL, state TEXT NOT NULL,
  error TEXT, created_at TEXT NOT NULL);                 -- body.weights: {symbol: bps}
CREATE TABLE orders (id TEXT PRIMARY KEY, decision_id TEXT NOT NULL REFERENCES decisions(id),
  symbol TEXT NOT NULL, request TEXT NOT NULL, state TEXT NOT NULL, broker_id TEXT UNIQUE,
  broker_status TEXT, error TEXT, UNIQUE (decision_id, symbol));
CREATE TABLE theses (id TEXT PRIMARY KEY, symbol TEXT NOT NULL REFERENCES instruments(symbol),
  assessment TEXT NOT NULL, version INTEGER NOT NULL, retired_at TEXT, body TEXT NOT NULL);
-- unchanged shapes: meta, sources, fills, valuations (body gains positions{}), journal, reviews
```

**Execution.**
- Carried over: one unresolved decision at a time (`allocation.py:88-89`); validity ≤ 24 h
  (`:86-87`); each order ≤ `max_order_bps` (`:139-140`); the request is committed before the
  broker write (`:201-202`); uncertain submissions are never resubmitted (`:210-212`); the
  unowned-active-order and holdings-equality refusals, per symbol (F23).
- New: a decision splits into one order per symbol beyond `drift_bps`, sells first. A weight > 0
  needs `covered`. Each `bps` ≤ `max_weight_bps`, and the sum ≤ 10000 − `cash_buffer_bps`.
- **T+1:** buys are sized only from settled cash (`sdk_bridge.py:105-107`). A buy leg that cannot
  be funded before `valid_until` ends `noop`, and the next day's decision continues. One decision
  never waits for settlement; rebalancing converges over days.
- **Order identity:** remark `nc-inv-` + `digest({account, decision, symbol})[:32]`, which keeps
  the bridge's 39-character check (`sdk_bridge.py:175`). `client_request_id` uses the same
  digest, so two legs of one decision never collide.
- **Budgets:** quotes for all symbols are one SDK call. The order and execution budgets (F23)
  stay account-wide, and `max_instruments` ≤ 10 keeps them sufficient.

**Bounded projections (B-3)** inside the F22 contracts, never by widening them:
- `max_instruments` ≤ 10, so `portfolio.weights` is ≤ 10 slices + cash ≤ 12.
- `portfolio.weight_history` shows the 4 largest current weights + 其他 + 现金 = 6 series.
- `thesis.board` is one records dataset of ≤ 10 × 3 = 30 open theses.
- `thesis.records` is ≤ 3 open + the 20 latest retired.
- `nav_history` is 260 points; `decision_log` 50 records; `fill_log` ≤ 500 rows.

**Config** (closed schema, `manifest_version` 5): `account_no`, `broker_home`, `oauth_client_id`,
`sdk_python_path`, `access_region` (as today); `portfolio_track_id` (was `owner_track_id`);
`instrument_recipe_id`; `max_instruments` (1..=10); `max_weight_bps`, `cash_buffer_bps`,
`drift_bps`, `max_order_bps`, `quote_max_age_seconds`, `poll_seconds`;
`opening_positions: [{symbol, shares}]` (generalizes `opening_shares`, `allocation.py:115-122`;
immutable per ledger). Market is US by code (owner 2), and the USD cash path holds (F19).
`profile: spy_cash` is deleted: the paper fence is the SDK identity proof (F19).

### 3.8 4140 cut-over: fresh start (D7, owner 1)

The NAV history restarts. The SPY Track stays readable as the acceptance record.
Owner-run, after the Sat 2026-10-10 verdict, on the deployed K1, K2 and P1–P3:
1. **Quiesce the old writer (B-4).** On the paper plugin, wait until every decision is final,
   with none `queued`, `requested`, `submitting`, `working` or `unknown`. Disable the plugin.
   Then verify with `pgrep -f sdk_bridge.py` that no SDK session survives (F18).
2. **Prove the account is quiet.** Run one SDK snapshot by hand (`sdk_bridge.py snapshot`) and
   require no order in an active state and holdings equal to the planned `opening_positions`.
   Record it in the runbook log.
3. The user closes the SPY Track. Save both invest recipes. Create the portfolio Track from
   `invest-portfolio` without a first message.
4. Install and enable `invest` with `portfolio_track_id`, `instrument_recipe_id` and
   `opening_positions` from step 2.
5. **Reset the portfolio Planner (A-B4)** (`POST /api/cards/<planner>/planner/reset`, F24). Its
   first thread started before `invest` existed and lacks the standing instructions.
6. The first portfolio turn covers `US:SPY` (§3.3). Until it is `covered`, no decision may weight
   it, and the opening position just sits there. Reconciliation admits `opening_positions`.
7. Edit 7d686d59…'s `chart.series` source to `neige://plugin/invest/series_show` (a one-off owner
   edit, like #2021's), then uninstall `market` (owner 4).

**Acceptance check (C):** the first `invest` reconciliation succeeds with holdings equal to
`opening_positions` and no unowned active order. Injecting an extra active order into the fake
broker must fail it (`test_cutover_refuses_unquiesced_account`).

## 4. Slices

Order: #2087 B0 → … → B5, then K1 ∥ K2 → P1 → P2 → P3 → C. K1 and K2 are inert until a recipe
uses them and may ride any kernel deploy. P1–P3 and C deploy after the verdict, and P1 rebases
on #2102.

Gates:
- every slice: `scripts/local-ratchet-gates.sh`;
- K1/K2: the whole `-p calm-server` run (new tool and prompt surface) and
  `scripts/local-rust-gates.sh --quick`; K1 also regenerates OpenAPI and the `fe` types
  (no visible UI change, so no browser gate);
- P*: `python3 -m pytest plugins/invest/tests -q`.

| # | Slice (≈ lines) | Tier | Acceptance | Must go red first (single-factor mutation → predicted red set) |
|---|---|---|---|---|
| K1 | `neige_track_add` + provenance in `_meta` (~1k: migration, keyed-create extraction, `TrackCreator` handle, tool, prompt, config, description trims) | L2 | An unbound Planner creates a recipe Track in its area with `creator_track_id`/`creator_key`, NULL `parent_track_id`; replay returns it; the brief is delivered once; Worker, bound, created and child creators get -32403; cap enforced; plugin `_meta` carries provenance; surface ≤ 30,000 B | Tests: `track_add_records_provenance_not_parent`, `track_add_refuses_past_open_cap`, `track_add_counts_only_open_tracks`, `track_add_refuses_bound_creator`, `track_add_refuses_created_creator`, `track_add_replays_and_refuses_changed_request`, `track_add_delivers_text_once`, `plugin_track_meta_carries_provenance`. Mutations: count closed Tracks too → {`counts_only_open_tracks`}; drop the count → {`refuses_past_open_cap`}; skip the scope check → {`refuses_bound_creator`}; skip the depth check → {`refuses_created_creator`}; set `parent_track_id` → {`records_provenance_not_parent`}; omit `creator_key` from `_meta` → {`plugin_track_meta_carries_provenance`} |
| K2 | standing instructions (~600) | L2 | A Planner whose report references a plugin gets its text from the enabled row; others do not; the 4,096 B aggregate cap is visible | Tests: `plugin_instructions_follow_report_references`, `plugin_instructions_skip_unreferenced_tracks`, `plugin_instructions_skip_disabled_plugin`, `plugin_instructions_read_from_row_before_host_boot`, `plugin_instructions_aggregate_cap_is_explicit`, `manifest_v4_refuses_planner_instructions`, `manifest_refuses_instructions_over_2048_bytes`. Mutations: predicate always true → {`skip_unreferenced_tracks`}; read from the running host → {`read_from_row_before_host_boot`} |
| P1 | `plugins/invest` core (~1k, Python): ledger, `decision_add`/`execution_add`/`portfolio_status`, multi-symbol SDK bridge, portfolio units and recipe | L2 | Several weights execute sells-then-buys within caps on the fake broker; only P writes; the ported caller-identity test passes (`caller_identity.rs:45`) | Tests: `test_weights_respect_bounds`, `test_sells_before_buys_and_settled_cash_only`, `test_research_track_cannot_trade`, `test_leg_remarks_are_unique`, `test_unowned_active_order_blocks`, `test_opening_positions_pin_first_reconciliation`, `test_cutover_refuses_unquiesced_account`. Mutation: drop the Track fence → {`research_track_cannot_trade`} |
| P2 | instruments, generations, theses, research units and recipe, `planner_instructions` (~900) | L2 | §3.3 lifecycle end to end; a thesis set in R shows on P's board within one tick; every unit at the bounds (10 instruments × 3 open theses) validates against the exported schema | Tests: `test_binding_requires_attested_creator_and_key`, `test_portfolio_track_never_binds`, `test_new_generation_supersedes_old_track`, `test_track_add_args_byte_identical`, `test_unbound_status_is_a_result`, `test_units_fit_contracts_at_bounds`, kernel `invest_recipe_slots_resolve`. Mutations: accept any creator → {`binding_requires_attested_creator_and_key`}; drop the open-thesis cap → {`units_fit_contracts_at_bounds`} |
| P3 | `series_show` on the Longbridge SDK (~600); remove `plugins/market` | L1 | Same reply contract as `market.series` for US; both recipes' charts render | `test_series_contract_matches_market_series`, `test_series_needs_no_track_meta` |
| C | 4140 cut-over (§3.8) | ops | Acceptance check C | — |

L2 means two independent review channels, re-run fresh after every fix (AGENTS.md).

## 5. Risks

- **Wake cost.** Up to 10 research Tracks, each with weekly and earnings entries, mean that many
  Planner turns. `max_instruments` bounds them, and the kernel cap is the backstop.
- **Overlay churn.** 11 Tracks' units are republished every tick; #1995 owns throttling.
- **Rules drift between plugin and recipe.** The rules live only in the plugin; review checks
  that recipes carry no rule text.
- **Orphans.** A Track created with arguments other than `track_add` never binds. It stays open
  inside the cap until the user closes it.

## 6. Rejected alternatives

- The child-track route (F13) and `managed_track_identities` (template-only, kernel lifecycle).
- A CLI-only `neige track add` (§2 allows CLI rows only for views, Worker reports and `--force`).
- A portfolio-supplied `track_id` with a pending state. It is unverifiable and races the new
  Planner's first turn (§3.3).
- Widening the native-view limits for one plugin (§3.7).
- A kernel thesis object, or links inside units.

## 7. Review round 1 (revision 2)

| Finding | Resolution |
|---|---|
| A-B1 bound creator escapes its fence | Fixed: scope must be `All` (§3.4); test `refuses_bound_creator` |
| A-B2 recursive fan-out | Fixed by narrowing: depth 1 (§3.4); test `refuses_created_creator` |
| A-B3, B-1, B-2 binding | Fixed and simplified: kernel-attested provenance in `_meta` plus plugin generations (§3.3); `instrument_set {track_id}` deleted |
| A-B4 runbook order | Fixed: reset step 5 (§3.8); K2 reads enabled rows, not the running host |
| B-3 unit contracts | Fixed: bounded projections, `max_instruments` ≤ 10, 3 open theses (§3.7); boundary test |
| B-4 broker quiescence | Fixed: steps 1–2 and acceptance check C (§3.8) |
| B-5 CLI row | Resolved by owner decision 3: the tool is listed |
| Nits | `text` for the brief; -32403; `series_show` callers; `trackId` → `track_id` after B3; F8/F18 anchors; full mutation sets; no visible UI change, so no browser gate; `TrackCreator` handle keeps the lock and gate; T+1; per-leg remarks; reconcile checks; held-uncovered SPY; budgets; aggregate cap; boot race |
