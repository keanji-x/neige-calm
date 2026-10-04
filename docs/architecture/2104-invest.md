# Invest: a multi-instrument portfolio with long-lived research Tracks

**Baseline:** `origin/main` 549be6f1d. The design was first read at 2dfb69d44.
`git diff --stat 2dfb69d44 549be6f1d` touches two cited files: `plugins/paper-trading/spy-recipe.md`
(#2102) and `routes/cards.rs`. Both are re-read below. Every other cited file is unchanged, and so
are the prompt files and the registry golden that feed the F16 measurement.
Facts marked **4140** come from one read-only query of the production database (2026-10-04,
migration 138).

**Status:** design, revision 4. Review rounds 1–3 are folded in (§7). The research lifecycle is
rebuilt on provenance, one issued key and a lease. Issue: #2104. Review tier: **L2**. Docs only.

**Owner direction (not reopened):**
- One Python plugin `invest` merges `plugins/paper-trading` and `plugins/market`.
- One portfolio Track is the only broker writer.
- Each covered instrument gets an ordinary top-level research Track; the two recipes are linked.
- Kernel gaps: `neige_track_add` and plugin standing instructions.
- Names follow `docs/conventions/agent-commands.md`; this lands after #2087 B0 and B5.
- Compatibility covers 4140 only.

**Owner decisions (on #2104):**
1. Fresh start at cut-over.
2. US market only.
3. `neige_track_add` is listed for the Planner. K1 trims existing descriptions to make room and
   never raises the 30,000 B cap; there is no CLI row.
4. Retire `market`: the chart block on Track 7d686d59… moves to `invest`, and crypto charts go.
5. Any stored recipe may be used, bounded by the cap.
6. Each instrument is **held** (target weight > 0 or a position) or **watched** (a research Track,
   no position), with separate limits `max_held` and `max_watched`. Moving watched → held is a
   weight change. Projections fit any configured value by aggregating. The kernel cap must cover
   held + watched.

No owner question is open.

## 1. Problem, goals, non-goals

**Today.** The paper plugin allocates one symbol, SPY, against cash.
- `SPY.US` is hard-coded in the sizer (`plugins/paper-trading/paper_trading/allocation.py:152`), in
  the SDK checks (`paper_trading/sdk_bridge.py:82,113,169`) and in the tool parameter
  `target_spy_bps` (`plugins/paper-trading/manifest.json:41-45`).
- One Track owns it through config (`allocation.py:44-45`).
- Since #2102 the report is a three-view dashboard, and research lives only in the conversation
  (`spy-recipe.md:17`). No view is tracked over time.

**Goals.**
1. Target weights over US instruments, within operator bounds. The AI adds instruments inside
   those bounds.
2. One portfolio Track writes to the broker. Each covered instrument has a long-lived research
   Track with its own calendar and recipe. Research Tracks never trade.
3. Views raised in the portfolio are followed in the research Track, and their status flows
   back.
4. A generic kernel: a Planner opens a Track from a recipe, and an external plugin gives
   Planners standing instructions.

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
| F23 | A Codex Planner gets new instructions only at thread start; reset forces a new thread. Claude re-reads them at every `open_session`. | `routes/cards.rs:198,1199-1200` (re-read at 549be6f1d); `harness/backend.rs:102`; `claude_planner/wiring.rs:35-46` |
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
- `title` (≤ 120 chars), `summary` (≤ 500) and `body` (≤ 6,000), all bounded at input;
- `source_refs`;
- `assessment` (`open|holding|at_risk|broken`);
- `version`.

Lifecycle:
1. **Raise.** The portfolio raises a thesis with `thesis_add` on a live symbol. A symbol has at
   most 3 open theses; a 4th is refused and nothing changes.
2. **Assess.** The research Track holding the symbol's current key assesses it with `thesis_set`
   under `expected_version` (§3.3).
3. **Retire.** `thesis_rm` retires one thesis. `instrument_rm` retires every open thesis of its
   symbol in the same transaction. Retiring is a verb, never an assessment value.
4. **Flow back.** `thesis.board` on the portfolio Track is republished every tick
   (`runtime.py:15-27`, F1, F2), so a status change appears there within one tick. It projects
   only `title` and `summary`.

The two reports link each other in prose (F3, F7), and `neige_link_ls` shows inbound links.
Recipes never copy an assessment into prose (the rule at `spy-recipe.md:17`).

### 3.3 Research lifecycle: provenance, one issued key, a lease (D2)

**Kernel provenance (K1).** The kernel puts provenance in `_meta["dev.neige/track"]`:
`{id, creator_track_id, creator_key}`. These are copied from the Track row (`null` when absent);
`creator_key` is the raw `idempotency_key` the creator passed to `neige_track_add`. Nothing in it
names a plugin.

**One issued key per live symbol.** The ledger is the one source of truth.
- Each live symbol S carries a counter `n` that never goes back down, and the current key
  `invest-<venue>-<code>-<n>` (lowercase). It also stores the exact
  `track_add = {recipe_id, title, idempotency_key, text, message}` for that key.
- Every retry of `neige_track_add` within `n` is byte-identical and replays the same Track (F10).
- A new `n` never reuses a key, so a dead key (F11) cannot block coverage.

**Write authority.** A call is *attested for S* when `creator_track_id == portfolio_track_id` and
`creator_key == ` S's current key. Nothing is stored when the research Track first binds.
- Attested calls may `thesis_set` on S.
- An attested call also republishes S's research units onto the caller. This is stateless: the
  caller's Track id is used as the overlay target and is not stored.
- A call whose provenance names the portfolio and an older key of S, or a dropped S, gets
  `{state: "superseded", action: "close this Track"}` from `instrument_status`. Every write from
  it is refused (-32409).
- Any other caller is refused (-32403). That includes the portfolio Track itself, since it has
  no creator.
- A repeat call from the current Track is idempotent.

**Lease.** Every attested call sets `last_seen_at` for S. This is **access metadata**, like the
kernel's `last_activity_ms`: it never changes authority and is not domain state.

`portfolio_status` marks S **stale** when either holds:
- the current key was never seen within `bind_minutes` (default 120) of being issued — for
  example, the Track was never created or its first turn failed;
- it has not been seen for `lease_days` (default 8). The research recipe has a weekly calendar
  entry, and every step starts with `instrument_status`.

Stale covers a never-bound key, a deleted or closed Track, and a dead Planner, without any area
listing. The portfolio renews a stale symbol with **`instrument_set {symbol, expected_version,
message}`**. Under §3 `set` ("replace one entry's value under `expected_version`"), the value
replaced is S's issued key: n+1. The call returns the new `track_add`, which the Planner passes
verbatim to `neige_track_add`. The old Track, if still alive, is told "superseded" on its next
call and closes itself (`neige_track_close`).

**States.**
- `pending`: added, symbol not yet verified. No key exists.
- `live`: verified, key `n` issued.
- `dropped`: removed by `instrument_rm`, or refused by verification (with a reason).

Only `pending` and `live` count toward limits. A dropped symbol may be re-added; `n` continues
from where it stopped. Verification runs in the loop (F19), so no Track is created before the
symbol is checked.

### 3.4 `neige_track_add` (kernel gap 1, D3)

**Input** `{recipe_id, title, idempotency_key, text, message}`, all required, closed schema:
- `text` (§4) is the verbatim first message to the new Planner.
- `message` (§4) is the audit note, carried on the creation `TrackUpdated` (F13).

**Result** `{track_id, created_at}`; a replay returns the same result. The tool is listed for the
Planner (owner 3).

| Aspect | Decision |
|---|---|
| Who | A Planner on an open creator Track whose plugin scope is `All` (`tool_visibility.rs:57-73`). A bound or fail-closed creator would otherwise escape its fence. A reports-only managed Planner is refused (`managed_track.rs:215-226`). |
| Depth | 1. Refused when the creator has a `creator_track_id`, and separately when it has a `parent_track_id`. |
| Errors | Role, scope and depth refusals are -32403, raised with `RpcError::custom` as `managed_track.rs:221` does, not via `require_role*` (F13). The cap is state, so its refusal is -32409 (§5). The message names `--track-add-max-open`, the cap and the open count. |
| Recipes | Any stored recipe (owner 5; recipes are human-only, F14). |
| Where | The creator's area (`registry.rs:66-74`); a managed workspace (`routes/tracks.rs:937-938`); the creator's Planner provider (`child_track_adapter.rs:240`); the default theme; actor `AiPlannerSession` (F13). |
| Entry point | The keyed create (F10), extracted behind one function that takes an `ActorId`, a key and a fingerprint. REST and the tool both call it, keeping the area-delete lock and the Claude gate (F13). The tool reaches it through `AppContext.track_creator: OnceCell<Arc<dyn TrackCreator>>`, set at boot like `operation_runtime` (`registry.rs:174`); its implementation holds `RouteState`. |
| Idempotency | The existing binding row, keyed `track-add/<creator_track_id>/<idempotency_key>`. REST refuses that prefix. The fingerprint covers the tool's own five inputs only, not derived fields such as the provider (contrast `create.rs:334-345`). A different request under the same key is -32409. |
| Provenance | Migration, numbered last: `tracks.creator_track_id` and `tracks.creator_key` (the raw key), both or neither (a named `CHECK`), no `REFERENCES` (0085's reason), and an index on `creator_track_id`. `parent_track_id` stays NULL and the tree budget is untouched (F12). The provenance reaches plugins via `_meta` (`mcp.rs:425-427`). |
| Cap | `--track-add-max-open <u32>`, a clap arg on `Config` (`config.rs:8`, with a range like `:159-166`): 1..=256, default 16, no env var. It counts the creator's open created Tracks inside the create transaction's closure (`routes/tracks.rs:1281`). |
| Cap and plugin limits | The plugin cannot read the cap (F15). It checks statically that `max_held + max_watched ≤ 254` (the cap range minus a margin of 2). The runbook sets the cap to at least `max_held + max_watched + 2`. A mismatch shows up as the -32409 above; S stays stale, and `portfolio_status` shows it. |
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

**Source and timing.** The text is read from the stored manifest of the enabled row (F15), so it
does not race plugin-host boot. It is assembled in `planner_instructions` (`:417-457`). Codex
picks it up at thread start or reset; Claude at every `open_session` (F23).

**Aggregate cap: 4,096 B over every appended byte.**
- 160 B are always reserved, so instruction blocks (each heading `## Plugin <id>\n` plus its
  text) are admitted in id order only while they fit in 3,936 B.
- If any plugin is left out, one line is appended:
  `## Plugin instructions omitted (budget): <id>, <id>, …`. It is cut at an ASCII boundary to at
  most 160 B including its newline, ending with `…` when cut.
- Each omitted plugin also gets a warning log line.
- The total therefore never exceeds 4,096 B.

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
| `instrument_status` | `status` V | research Planner/Worker | `{}` → its symbol, position, theses; or `superseded` | — |
| `thesis_add` | `add` W | portfolio Planner | `thesis_id`, *`symbol`*, *`stance`*, `title`, `summary`, `body`, *`source_refs`* | — |
| `thesis_set` | `set` W | attested research Planner | `thesis_id`, *`assessment`*, `summary`, *`source_refs`*, `expected_version` | — |
| `thesis_rm` | `rm` W | portfolio Planner | `thesis_id`, `expected_version`, `message` | — |
| `series_show` | `show` V | chart resolver only | the `market.series` contract, US only | `market.series` |

**Verb rules.**
- Views change no domain state. The only things a view touches are the access metadata
  `last_seen_at` and the projection refresh (§3.3).
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
| `portfolio.holdings` | 500 rows | top 499 + 其他 (value sum) |
| `portfolio.decision_log` | 50 records × 12 facts | top 11 weights + 其他 per record |
| `thesis.board` | 100 records × 8 sections | one record per counted symbol (held by weight, then watched): top 99 + one 其他 record with counts by assessment; sections are the ≤ 3 open theses (`title` → label, `summary` → body) |
| `thesis.records` (research) | 100 records | ≤ 3 open + the 20 latest retired |
| `nav_history`, `fill_log` | 500 points, 500 rows | 260 points, latest 500 fills |

**Byte budget.** At the maximum config (254 symbols) with maximum-length CJK text, the board is
about 100 × (200 + 3 × (120 + 500)) chars × 3 B ≈ 0.6 MB, well under the 4 MiB cap (F2). A test
asserts this.

**Config** (closed schema, `manifest_version` 5):
- broker: `account_no`, `broker_home`, `oauth_client_id`, `sdk_python_path`, `access_region`;
- Tracks: `portfolio_track_id`, `instrument_recipe_id`;
- limits: `max_held`, `max_watched` (each ≥ 1, sum ≤ 254), `max_weight_bps`;
- trading: `cash_buffer_bps`, `drift_bps`, `max_order_bps`, `quote_max_age_seconds`,
  `poll_seconds`;
- lease: `bind_minutes`, `lease_days`;
- `opening_positions: [{symbol, shares}]`, immutable per ledger (it generalizes
  `allocation.py:115-122`).

The market is US by code. `profile: spy_cash` is deleted; the paper fence is the identity proof
(F18).

### 3.8 4140 cut-over: fresh start (D7, owner 1)

NAV history restarts; the SPY Track stays readable. The kernel cap is set during the K1 deploy
(a start-script argument, applied by the restart that deploy needs anyway) to at least
`max_held + max_watched + 2`. The rest is run by the owner after the Sat 2026-10-10 verdict,
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
6. Reset the portfolio Planner (`POST /api/cards/<planner>/planner/reset`, F23); its first thread
   predates `invest`. On its first turn it covers `US:SPY`.
7. Edit Track 7d686d59…'s `chart.series` source to `neige://plugin/invest/series_show`, deleting
   its CRYPTO series (owner 4), then uninstall `market`. K2 will then inject invest's
   instructions into 7d686d59…, so those instructions open by naming the two invest Track kinds
   and say to ignore the rest elsewhere.

**Acceptance check C:** the first `invest` reconciliation succeeds, with holdings equal to
`opening_positions` and no unowned active order. An extra active order on the fake broker must
fail it.

## 4. Slices

**Order:** #2087 B0 → … → B5, then K1 ∥ K2 → P1 → P2 → P3 → C. K1 and K2 are inert until used. P1
through C deploy after the verdict.

**Gates:**
- every slice: `scripts/local-ratchet-gates.sh`;
- K1/K2: the whole `-p calm-server` run and `scripts/local-rust-gates.sh --quick`, plus the
  OpenAPI and `fe` type regeneration for K1 (no visible UI, so no browser gate);
- P*: `python3 -m pytest plugins/invest/tests -q`.

Each mutation is single-factor, and `→ {…}` lists the complete set of tests predicted to go red.

| # | Slice (≈ lines) | Tier | Must go red first |
|---|---|---|---|
| K1 | `neige_track_add`, provenance in `_meta`, description trims (~1k) | L2 | Tests: `track_add_records_provenance_not_parent`; `…_refuses_past_open_cap` (cap 2: two adds, the third refused, nothing closed); `…_counts_only_open_tracks` (cap 2: two adds, close one, the third admitted); `…_refuses_worker`; `…_refuses_bound_creator`; `…_refuses_created_creator`; `…_refuses_child_creator`; `…_replays_and_refuses_changed_request`; `…_fingerprint_ignores_provider`; `…_delivers_text_once`; `plugin_track_meta_carries_provenance`. Mutations: drop the count → {`refuses_past_open_cap`}; also count closed Tracks → {`counts_only_open_tracks`}; drop the role gate → {`refuses_worker`}; drop the scope check → {`refuses_bound_creator`}; drop the `creator_track_id` half → {`refuses_created_creator`}; drop the `parent_track_id` half → {`refuses_child_creator`}; omit `creator_key` → {`plugin_track_meta_carries_provenance`} |
| K2 | standing instructions (~500) | L2 | Tests: `plugin_instructions_follow_report_references`, `…_skip_unreferenced_tracks`, `…_skip_disabled_plugin`, `…_read_from_row_before_host_boot`, `…_aggregate_never_exceeds_cap` (five plugins with 32-byte ids and 2,048 B texts), `manifest_v4_refuses_planner_instructions`. Mutations: predicate always `true` → {`skip_unreferenced_tracks`}; skip the 160 B reservation → {`aggregate_never_exceeds_cap`} |
| P1 | invest core (~1k, Python): ledger, decisions, executions, multi-symbol bridge, portfolio units, recipe | L2 | Tests: `test_weights_respect_bounds`, `test_held_after_counts_positions`, `test_sells_before_buys_settled_cash_only`, `test_research_track_cannot_trade`, `test_leg_remarks_are_unique`, `test_unowned_active_order_blocks`, `test_opening_positions_pin_first_reconciliation`, `test_cutover_refuses_unquiesced_account`, `test_projections_conserve_value` (40 held: slices and series sum to the total, 其他 equals the omitted sum), and a port of `caller_identity.rs:45`. Mutations: drop the Track fence → {`research_track_cannot_trade`}; drop 其他 from weights → {`projections_conserve_value`} |
| P2 | instruments, keys, lease, theses, research units and recipe, `planner_instructions` (~800) | L2 | Tests: `test_attestation_requires_portfolio_creator_and_current_key`, `test_superseded_key_refused`, `test_never_seen_key_goes_stale`, `test_lease_expiry_goes_stale`, `test_set_issues_next_key_with_byte_identical_args`, `test_views_change_no_domain_state`, `test_last_seen_never_changes_authority`, `test_fourth_open_thesis_refused_state_unchanged`, `test_rm_retires_open_theses`, `test_dropped_counts_toward_no_limit`, `test_units_fit_caps_at_max_config` (254 symbols, max-length CJK, every unit validated and ≤ 4 MiB), `test_unit_ids_injective`, kernel `invest_recipe_slots_resolve`. Mutations: accept any key of the symbol → {`superseded_key_refused`}; drop the bind window → {`never_seen_key_goes_stale`}; ignore `lease_days` → {`lease_expiry_goes_stale`}; drop the thesis cap → {`fourth_open_thesis_refused_state_unchanged`}; skip retiring on rm → {`rm_retires_open_theses`} |
| P3 | `series_show` on the Longbridge SDK (~600); remove `plugins/market` | L1 | Tests: `test_series_contract_matches_market_series`, `test_series_refuses_agent_caller`, kernel `chart_series_resolves_through_invest` (the real resolver, `resolver.rs:573-575`). Mutation: accept an agent caller → {`series_refuses_agent_caller`} |
| C | cut-over (§3.8) | ops | check C |

L2 means two independent review channels, re-run fresh after every fix (AGENTS.md).

## 5. Risks

- **Wake cost.** Up to `max_held + max_watched` research Planners wake weekly, bounded by the
  limits and the kernel cap.
- **Overlay churn.** Portfolio units are republished every tick; #1995 owns that. Research units
  change only on research calls, so between wakes they show their snapshot time.
- **Rule drift.** The rules live only in the plugin, and review checks the recipes.
- **Wrong-argument Tracks.** A Track created with arguments other than the issued `track_add` is
  never attested. It cannot write, it stays inside the cap until closed, and its symbol goes
  stale, which leads to renewal.

## 6. Rejected alternatives

- The child-track route (F12) and `managed_track_identities`.
- A CLI-only `neige track add`.
- Any stored binding, any handshake, and any listing-based check that a Track still exists (§7).
- Widening the unit contracts, or capping config at contract numbers.
- A kernel thesis object, and links inside units.

## 7. Review findings

| Round | Finding | Resolution |
|---|---|---|
| 1 | A-B1 bound creator; A-B2 fan-out | Fixed: scope `All`; depth 1 with both halves tested |
| 1 | A-B3/B-1/B-2 binding sequence | Superseded by r4 (§3.3) |
| 1 | A-B4 runbook order; B-4 quiescence | Fixed: reset step 6; steps 1–3 and check C |
| 1 | B-3 unit contracts | Fixed by aggregation at any config, with conservation |
| 1 | B-5 CLI row | Owner 3 |
| 2 | A-B1 churn; B-4 tests that cannot go red | Fixed: only counted states; rm retires theses; 4th-thesis test |
| 2 | B-1 bind in a view; B-2 truncated outline; B-3 background Track | Superseded by r4: no bind, no listing; F4 corrected; `series_show` keys on the missing caller |
| 3 | lifecycle blockers in every round | **Restructured (r4):** provenance + one issued key + lease. Deleted: `coverage_add`, `coverage_rm`, the `coverages` table, the `reserved`/`checked`/`refused` states, the `area/reports/` existence check and K1's `closed_at`. `instrument_set` renews the key |
| 3 | A-B3 unit and field caps, ids | Fixed: input bounds, board projected from title and summary, decision weights top 11 + 其他, `VENUE.CODE` ids, max-config CJK byte test |
| 3 | A-B4/B-1 K2 marker overflow | Fixed: one summary line within a fixed 160 B reservation; per-plugin warnings; 32-byte-id test |
| 3 | B-2 conservation | Fixed: `test_projections_conserve_value` |
| 3 | S3, S4, B nit tables | Deleted `thesis_ls` and `instrument_ls`; dropped `sources` and `reviews` |
| 3 | nits | Raw `creator_key`, idempotent repeats; held-after and the 254 sum; the count site is `:1281`; -32403 via `RpcError::custom`, the cap -32409; risks wording and test shapes; the cap is set at the K1 deploy; crypto series deleted; K2 notice for 7d686d59…; new baseline |
