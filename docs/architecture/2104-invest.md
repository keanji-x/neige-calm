# Invest: a multi-instrument portfolio with long-lived research Tracks

Baseline: `origin/main` 2dfb69d44. Every `file:line` below was read on that tree. Facts
marked **4140** come from one read-only query of the production database
(`calm.db?mode=ro`, 2026-10-04, migration 138).
Status: design, revision 1. Issue: #2104. Review tier: **L2** (authority, persistence and a
broker writer). Docs only; no code changes.
Owner direction (2026-10-04, not reopened here): one Python plugin `invest` merges
`plugins/paper-trading` and `plugins/market`; one portfolio Track is the only broker writer;
each covered instrument gets an ordinary top-level research Track; two linked recipes; kernel
gaps `neige_track_add` and plugin standing instructions; names per
`docs/conventions/agent-commands.md`; land after #2087 B0 and B5; compatibility covers 4140 only.

## 1. Problem, goals, non-goals

**Today.** The paper plugin is a single-symbol SPY/cash allocator. `SPY.US` is fixed in the
order sizer (`plugins/paper-trading/paper_trading/allocation.py:152`), in the SDK position,
quote and preflight checks (`paper_trading/sdk_bridge.py:82,113,169`), and in the one tool
parameter `target_spy_bps` (`plugins/paper-trading/manifest.json:41-45`). One Track owns it
by configuration (`allocation.py:44-45`). Research lives in that Track's prose sections
(`spy-recipe.md:60-80`), so the AI cannot add an instrument or follow a view over time.

**Goals.**
1. Target weights over several instruments, bounded by operator config (markets, maximum
   instrument count, per-instrument weight cap). The AI adds instruments inside the bounds.
2. One portfolio Track writes to the broker. Every covered instrument has its own long-lived
   research Track with its own calendar and recipe. Research Tracks never trade.
3. Views raised in the portfolio report are followed in the instrument Track, and their
   status flows back to the portfolio report.
4. Kernel stays generic: a Planner can open a Track from a recipe, and an external plugin can
   give Planners standing instructions read at Planner start. No plugin identity in the kernel.

**Non-goals.** Real money, other brokers, a Rust SDK. Multi-currency settlement (§3.7).
A general ledger-migration framework. Changing the report renderer or data-unit grammar.

## 2. Verified facts the design relies on

| # | Fact | Evidence |
|---|---|---|
| F1 | One plugin may publish overlays onto any Track: the permission is per `entity_kind`, never per entity id. | `plugin_host/perms.rs:11-13`; `plugin_host/callbacks.rs:192-241` |
| F2 | A live slot resolves `(report's own track, plugin, kind)`; the same kind on two Tracks is two independent units. | `mcp_server/tools/track_report_hydrate.rs:79,142-160` |
| F3 | A data unit is one `Component` cell; no component has a link field. | `calm-types/src/report_blocks/native_view/model.rs:105-109,215-233` |
| F4 | A plugin tool learns the calling Track and the caller's role from host `_meta`, never from arguments; kernel-initiated calls carry no Track. | `plugin_host/mcp.rs:184-198,414-429`; `tests/cases/mcp_plugin_tools/caller_identity.rs:11-35`; `paper_trading/rpc.py:69-74` |
| F5 | A Track with `plugin_scope` sees only its owner plugin's tools; an unbound Track sees every enabled plugin. | `mcp_server/tool_visibility.rs:13-37,75-120` |
| F6 | Template binding is limited to trusted forge plugins (built-ins by default), so an external plugin cannot bind a Track. | `forge_trust.rs:9-17` |
| F7 | The SPY recipe needs other plugins' tools (Longbridge, Wisburg), so its Track is unbound. **4140:** Track 7c0dd087… has `plugin_scope` NULL and recipe d2a8d568… rev 3. | `spy-recipe.md:12` |
| F8 | Report links use the track-link form that `neige.area.outline` documents (track id, optional block fragment), within one area only; `neige.report.backlinks` lists inbound links; a Planner reads other area reports through `area/reports/`, whose rows carry `trackId`. | `prompts/tools/neige.area.outline.md`; `neige.report.backlinks.md`; `neige.track.ls.md`; `mcp_server/tools/area_reports.rs:63-74` |
| F9 | Calendar entries belong to the Track that created them, and each timed or weekly occurrence wakes that Track's Planner. | `builtin_plugins/calendar/store.rs:21,108`; `builtin_plugins/calendar/instructions.md:4,20-24` |
| F10 | Only built-ins inject live Planner instructions, gated on `plugin_scope` or template ownership. The recipe text reaches the Planner as a creation-time card snapshot. | `operation/planner_harness_start_adapter.rs:435-456`; `routes/tracks.rs:1459`; #2098 |
| F11 | One create entry point: `create_track_structure` reads a recipe inside the write transaction and stamps `recipe_id`/`revision`. The keyed create binds `(area_id, idempotency_key)` → track in that transaction and can deliver a first message exactly once. | `routes/tracks.rs:1248,1316-1332,1352-1376`; `routes/tracks/create.rs:184-291,474-508`; migration `0088` |
| F12 | The child-track route is task-shaped: its payload is `{task_id, parent_track_id, goal, …}`, it charges the tree task budget, it inherits no template or recipe, and it sets `parent_track_id`. | `operation/child_track_adapter.rs:68-75,196-216,250-258,271` |
| F13 | A kernel-managed creation (`create_managed_track`) also calls `create_track_structure`, but only for templates and with kernel lifecycle. | `routes/tracks.rs:1677-1708`; `daily_planner.rs:49-78` |
| F14 | Recipes are human-only: no agent-facing write path exists. | `routes/track_recipes.rs:119-127` |
| F15 | Planner tool surface: 30,000 B cap, measured 29,909 B by its own comment; 2,048 B per description. Hidden tools are served through the `neige` CLI (`report tag`, `report find`). | `mcp_server/tools/mod.rs:172-176`; `report_tag.rs:49,68`; `area_reports.rs:42,60` |
| F16 | Manifest unknown fields are tolerated; a new field needs a `manifest_version` bump so an old kernel refuses the file. v4 is the latest. | `plugin_host/manifest.rs:17-22,129-136` |
| F17 | A plugin's data dir is `<plugins_data_dir>/<id>/`; the SPY ledger is `spy-cash/ledger.sqlite3` under it. | `plugin_host/process.rs:55`; `allocation.py:32`; `ledger.py:57` |
| F18 | Paper-only is enforced by the SDK identity check: the paper channel plus the account number read from a daily statement. | `sdk_bridge.py:55-77` |
| F19 | Cash is USD-only: one USD balance is required and non-USD cash is refused. | `sdk_bridge.py:92-108` |
| F20 | A network-touching plugin tool needs `openWorldHint`, which Codex cannot approve under `approval_policy: never`; writes therefore record state and wake a loop. | `plugins/market/README.md:33-46` |
| F21 | `chart.series` requires `VENUE:SYMBOL` ids and names `neige://plugin/<id>/<tool>`. | `calm-types/src/report_blocks/chart_series.rs:131-139,163-167`; `kinds.rs:115-133` |
| F22 | **4140:** `dev-neige-market` holds no `plugin_kv` rows and no overlays (no holdings anywhere). Open Tracks referencing `neige://plugin/dev-neige-market`: the SPY Track and 7d686d59…. The SPY Track has 4 calendar rows. | read-only query |

## 3. Decisions

### 3.1 Shape

```
portfolio Track (recipe invest-portfolio)        instrument Track ×N (recipe invest-instrument)
  Planner: instrument_add/set/rm, thesis_add/rm,    Planner: instrument_status, thesis_set, thesis_ls
           decision_add, neige_track_add            calendar: its own weekly/earnings entries
  Worker:  execution_add  ──► broker (paper)        never trades (plugin fence, §3.6)
            ▲ units: portfolio.*, thesis.board        ▲ units: instrument.position, thesis.records
            └──────────── invest plugin ledger (one SQLite, one writer loop) ─────────┘
```

Both Tracks are ordinary, unbound, in the same area (F5, F7, F8). The plugin fences every write
on the host-supplied Track and role (F4), as the SPY App does today (`allocation.py:43-55`).

### 3.2 Linkage: structured theses in the plugin ledger (decision D1)

A **thesis** is a plugin-owned record: `thesis_id` (caller-chosen slug), `symbol`, `stance`
(`bullish|bearish|neutral`), `title`, `body`, `source_refs`, `assessment`
(`open|holding|at_risk|broken`), `version`, and the bound instrument Track. Flow:

1. The portfolio Planner raises a view with `thesis_add` (covered symbol only).
2. The instrument Planner owning that symbol assesses it with `thesis_set` under
   `expected_version`. The plugin admits this only when `_meta` Track equals the symbol's bound
   Track (F4).
3. On each change the runtime republishes `thesis.board` (records, one per open thesis) on the
   portfolio Track and `thesis.records` on the instrument Track. The publish loop already
   republishes each tick (`runtime.py:15-27`); F1 and F2 make one plugin serve both Tracks.
4. Only the portfolio retires a thesis, with `thesis_rm` (the journal keeps it). `assessment` is
   data, not an effect: retiring is a verb, never `assessment: retired` (convention §1.2).

**Report links** are prose, not unit content (F3). The portfolio report keeps a "覆盖标的"
section that links each research report by its track id, in the F8 link form. Each research
report links back to the portfolio's thesis section. `neige_link_ls` (today
`neige.report.backlinks`) shows each Planner who points at it (F8). Recipes forbid copying an
assessment into prose: the live units are the only status, the same rule `spy-recipe.md:17`
applies to account figures.

No new kernel mechanism: data units, overlays per Track and area links already exist. Rejected:
assessment in the instrument report's prose, read back by the portfolio Planner (stale, unparsed);
a kernel "thesis" object (domain policy in the kernel); links inside units (needs a new
component field for one plugin).

### 3.3 Discovery: the plugin's symbol → Track map is the one source of truth (D2)

The plugin must already know each instrument Track's id to publish units onto it (F1, F2) and to
fence `thesis_set` (F4). That map is therefore the truth, in the `instruments` table (§3.7).
`instrument_ls` returns `{symbol, state, track_id, target_bps, weight_bps, open_theses}`. To read
a research report in depth, the portfolio Planner takes the row whose `trackId` matches from
`neige track ls area/reports/` and runs `neige track cat <path> --blocks …` (F8).
Not used as truth:
- Report tags are written by each Track's own Planner, so they cannot attest coverage
  (`report_tag.rs:1-3`).
- The kernel provenance link (§3.4) records who opened a Track. The plugin cannot read it, and
  it says nothing about which symbol.

Binding sequence, three idempotent calls in one portfolio turn:
`instrument_add {symbol, message}` reserves a slot inside the bounds. It returns `recipe_id` (the
configured instrument recipe) and touches no network (F20); the loop then checks the symbol.
Then `neige_track_add {recipe_id, title, idempotency_key: "invest-<symbol>", message}` returns
`track_id`, and `instrument_set {symbol, track_id, expected_version}` binds it. Reserving before
creating means a bounds refusal never leaves an orphan Track. A wrong `track_id` fails visibly:
that Track's `instrument_status` is refused, and the instrument Planner sees the refusal.

### 3.4 `neige_track_add` (kernel gap 1, D3)

**Contract.** Object `track`, verb `add` (§3 "add an entry to a collection").
Input `{recipe_id, title, idempotency_key, message}`, all required, closed schema.
Result `{track_id, created_at}`; a replay returns the same object.

| Aspect | Decision |
|---|---|
| Who | Planner role only (`require_role`, `mcp_server/registry.rs:108-118`), on an open creator Track. A reports-only managed Planner is refused (`managed_track.rs:215-226`). Workers never. |
| Which recipes | Any stored `track_recipes` row. Recipes are human-authored by construction (F14), so the agent instantiates only what the owner saved. The plugin names its recipe in typed config (`instrument_recipe_id`), and `instrument_add` returns it. |
| Where | The creator's area (`ToolCallIdentity.area_id`, `registry.rs:66-74`), so area links and reads work (F8). Managed workspace (`cwd` omitted, `routes/tracks.rs:937-938`). Planner provider copied from the creator, the rule child tracks use (`child_track_adapter.rs:240`). Default theme. |
| Entry point | The keyed create, extracted behind one function that takes an `ActorId` and an explicit key; `POST /api/tracks` and the tool both call it. It runs `create_track_structure` with `TrackInit::Recipe` (`routes/tracks.rs:1186-1199,1248`). No second minting path. |
| Idempotency | The existing binding row (F11), with key `track-add/<creator_track_id>/<idempotency_key>` in the creator's area. The REST route refuses header keys that start with `track-add/`. The first-message fingerprint makes a replay return the same Track, and a changed `recipe_id`/`title`/`message` is -32409. |
| First message | `message` is the audit note and the new Planner's brief. It is delivered once through the first-message path (`create.rs:474-508`) as `Opened by Track <creator_track_id>: <message>`. The prefix carries the id, not the title: a rename between retries must not change the digest. |
| Provenance | Migration (numbered last): `ALTER TABLE tracks ADD COLUMN creator_track_id TEXT`. It is not a `REFERENCES`, for the reason 0085 records for `recipe_id` (`0085_track_recipe_provenance.sql:8-14`). Partial index on `(creator_track_id) WHERE creator_track_id IS NOT NULL`. `parent_track_id` stays NULL and the tree budget is untouched (F12). |
| Cap | Typed config `--track-add-max-open <u32>` (a clap arg on `Config`, `config.rs:8`, ranged like `:159-166`; 1..=64, default 16, no env var). Counted as open tracks with this `creator_track_id`, inside the create write transaction (`BEGIN IMMEDIATE`, `routes/tracks.rs:1068`), so two racing adds cannot both pass. Over the cap: -32409 naming the cap and the open count. |
| Events and UI | The ordinary `TrackUpdated`, `CardAdded`… events (`routes/tracks.rs:1567-1590`) with actor `AiPlanner(card)`. `Track` gains `creator_track_id` (OpenAPI and `fe` regenerated). The Track header shows "Opened by <title>". |
| Creator closes | Nothing cascades: created Tracks are ordinary and long-lived. A closed creator cannot call (its Planner does not run), and the cap counts only open created Tracks. A created Track is closed by its own Planner (`neige.track.close` closes only the caller's Track) or by the user. |
| Listing | Hidden from `tools/list` and served as `neige track add`, like `report tag` (F15). The Planner surface has 91 B of headroom, and only plugin-instructed Planners need the tool; the instructions of §3.5 name it. See Q2. |

Rejected: reusing the child-track route (F12: task-shaped, budgeted, recipe-less); reusing
`managed_track_identities` (F13: template-only, kernel lifecycle, a different meaning of owner).

### 3.5 Plugin standing instructions (kernel gap 2, D4)

- **Manifest:** `planner_instructions: string`, at most **2,048 bytes** (the per-description
  cap, F15; the two built-ins are 1,678 and 1,976 B). It is legal only at `manifest_version` 5, so
  an older kernel refuses the file instead of dropping the key (F16). The limit is validated
  when the manifest loads.
- **Which Tracks.** Not "bound only": invest Tracks cannot be bound (F5, F6, F7). A plugin
  instructs a Planner when the plugin is running and trusted, its tools are visible to the Track
  (`TrackPluginScope::allows_manifest`), and either
  (a) the built-in rule holds (`plugin_scope` or template owner, `planner_harness_start_adapter.rs:436-443`), or
  (b) the Track's **current report** references the plugin through a `neige://plugin/<id>/` live
  source: a view slot, a live table or `chart.series` (F2, F21).
  The template declares which plugins it composes; that declaration selects the
  documentation. This keeps the existing rule that "documentation follows the saved template; it
  never enables or authorizes tools" (`:435`). The source parser is one calm-types function next
  to `validate_live_source` (`kinds.rs:123`), and hydration's `view_slots` reuses it.
- **When.** Inside `planner_instructions` (`planner_harness_start_adapter.rs:417-457`), the one
  builder that Codex `thread/start` and the Claude session share. The text is read from the
  running manifest at every Planner start. A plugin upgrade reaches a Planner at its next thread
  start or reset; a running thread keeps its developer instructions. The text sits after the
  built-ins and before the card's template context, under `## Plugin <id>`, in plugin-id order.
- **Effect on #2098.** Recipes keep only Track-specific text: layout, schedule, sections. The
  operating rules (tool order, fences, the `neige track add` call) move into the plugin and stay
  current.
- **Trust.** The plugin is operator-installed and already gives the model its tool descriptions;
  2 KiB more is the same trust class (`docs/architecture/1413-local-plugin-trust.md`).

### 3.6 Tool table (D5)

Plugin id `invest`; served as `plugin_invest_<tool>`. Symbols use the kernel's `VENUE:CODE`
spelling everywhere (F21); only `sdk_bridge.py` converts it to Longbridge's `CODE.VENUE`.
P = portfolio Track, I = a bound instrument Track. Every role and Track check uses `_meta` (F4).

| Tool | §3 verb / class | Caller | Input (§4 names; domain keys in italics) | Replaces |
|---|---|---|---|---|
| `portfolio_status` | `status` V | P Planner/Worker | `{}` → snapshot, positions, targets, decisions, orders, fills, errors | `spy.status` |
| `decision_add` | `add` W | P Planner | `decision_id`, *`weights`* `[{symbol, bps}]`, *`rationale`*, *`source_refs`*, *`valid_until`* | `spy.plan` |
| `execution_add` | `add` W | P Worker | `decision_id` (adds an execution request; the loop submits) | `spy.execute` |
| `instrument_add` | `add` W | P Planner | *`symbol`*, `message` → `{symbol, state, version, recipe_id}` | — |
| `instrument_set` | `set` W | P Planner | *`symbol`*, `track_id`, `expected_version` | — |
| `instrument_rm` | `rm` W | P Planner | *`symbol`*, `expected_version`, `message`; refused while target or position > 0 | — |
| `instrument_ls` | `ls` V | P Planner | `{}` → rows, `slots_left`, allowed markets | — |
| `instrument_status` | `status` V | I Planner/Worker | `{}` → the calling Track's instrument, position, theses | — |
| `thesis_add` | `add` W | P Planner | `thesis_id`, *`symbol`*, *`stance`*, `title`, `body`, *`source_refs`* | — |
| `thesis_set` | `set` W | I Planner (own symbol) | `thesis_id`, *`assessment`*, `summary`, *`source_refs`*, `expected_version` | — |
| `thesis_rm` | `rm` W | P Planner | `thesis_id`, `expected_version`, `message` | — |
| `thesis_ls` | `ls` V | P or I Planner | *`symbol`* (optional) | — |
| `series_show` | `show` V | kernel `chart.series`; Planners | the `market.series` contract, unchanged keys | `market.series` |

Verb check against §3: `status`, `ls` and `show` are views; `add`, `set` and `rm` are writes, and
every `set`/`rm` takes `expected_version` (§4). There are no compound actions, no `refresh`,
`plan` or `execute`, and no effect is hidden in a parameter. Retired with no replacement:
- `spy.refresh`: the loop wakes on every write and polls every `poll_seconds`. A step waits until
  `portfolio_status.snapshot.at` is after its start.
- `market.quote` and `market.holdings.*`: research uses the Longbridge connector, and 4140 holds
  no holdings (F22).

`series_show` is the one view over several named series; `show` is the closest §3 word ("one
named object's details"), applied to a series set. Write tools never touch the network (F20).

**Overlay kinds (data units).** Portfolio: `portfolio.nav`, `portfolio.nav_history`,
`portfolio.account`, `portfolio.weights`, `portfolio.weight_history`, `portfolio.holdings`,
`portfolio.decision_log`, `portfolio.fill_log`. These are today's eight SPY units, generalized,
using #2102's layout, plus `thesis.board`. Instrument: `instrument.position` and `thesis.records`.

### 3.7 Ledger and config (D6)

Fresh `invest` ledger at `<plugins_data_dir>/invest/ledger.sqlite3`, `user_version` 1. It reuses
`Ledger`'s session, lock and journal (`ledger.py:20-107`). The SPY file is not upgraded (§3.8).

```sql
CREATE TABLE instruments (symbol TEXT PRIMARY KEY, state TEXT NOT NULL
  CHECK (state IN ('reserved','checked','covered','dropped','refused')),
  track_id TEXT UNIQUE, version INTEGER NOT NULL, body TEXT NOT NULL);
CREATE TABLE decisions (id TEXT PRIMARY KEY, body TEXT NOT NULL, state TEXT NOT NULL,
  error TEXT, created_at TEXT NOT NULL);                 -- body.weights: {symbol: bps}
CREATE TABLE orders (id TEXT PRIMARY KEY, decision_id TEXT NOT NULL REFERENCES decisions(id),
  symbol TEXT NOT NULL, request TEXT NOT NULL, state TEXT NOT NULL, broker_id TEXT UNIQUE,
  broker_status TEXT, error TEXT, UNIQUE (decision_id, symbol));
CREATE TABLE theses (id TEXT PRIMARY KEY, symbol TEXT NOT NULL REFERENCES instruments(symbol),
  assessment TEXT NOT NULL, version INTEGER NOT NULL, retired_at TEXT, body TEXT NOT NULL);
-- unchanged shapes: meta, sources, fills, valuations (body gains positions{}), journal, reviews
```

Rules carried over: one unresolved decision at a time (`allocation.py:88-89`); a decision is valid
for at most 24 h (`:86-87`); each order is limited by `max_order_bps` (`:139-140`); uncertain
submissions are never resubmitted (`:210-212`); a request is committed before the broker write
(`:201-202`). New rules:
- A decision splits into one order per symbol whose drift exceeds `drift_bps`. Sells go first;
  buys are sized from settled cash after the sells reconcile.
- The decision settles when every order is final.
- A weight > 0 needs `covered` state, so every held instrument has a research Track.
- The weights must satisfy: each `bps` ≤ `max_weight_bps`, and the sum ≤ 10000 − `cash_buffer_bps`.

Config (closed schema, `manifest_version` 5): `account_no`, `broker_home`, `oauth_client_id`,
`sdk_python_path`, `access_region` (as today), `portfolio_track_id` (was `owner_track_id`),
`instrument_recipe_id`, `markets` (non-empty subset of `["US","HK","SH","SZ"]`),
`max_instruments` (1..=32), `max_weight_bps`, `cash_buffer_bps`, `drift_bps`, `max_order_bps`,
`quote_max_age_seconds`, `poll_seconds`, and `opening_positions: [{symbol, shares}]` (generalizes
`opening_shares`, `allocation.py:115-122`; immutable per ledger).

`profile: spy_cash` is deleted. The paper fence is the SDK identity proof (F18), not a config
literal. **v1 refuses** any market whose currency is not USD, because cash handling is USD-only
(F19). The type is ready for HK; multi-currency is Q3.

### 3.8 4140 cut-over (D7): recommend a fresh start

| | Convert the SPY Track into the portfolio Track | **Fresh start (recommended)** |
|---|---|---|
| Code | ledger v1→v2 upgrade from the old data dir (F17), with tests | none beyond `opening_positions` |
| One-off 4140 ops | rewrite 8 slot sources in one report plus the recipe; reset the Planner `template_context` (the #2098 procedure); rename the Track and its 4 calendar entries (F22) | create a portfolio Track from the new recipe; close the SPY Track |
| Kept | NAV and decision history, Planner transcript | the SPY Track stays readable as the acceptance record |
| Lost | — | NAV continuity: the portfolio history starts at the cut-over day |

Fresh start removes three bespoke operations and one-shot upgrade code. Its cost is that the
NAV chart restarts. Runbook (owner-run, after the Sat 2026-10-10 verdict):
1. Disable the paper plugin: **one writer per account**. A second ledger on the same account
   would see foreign orders.
2. The user closes the SPY Track.
3. Save both invest recipes; create the portfolio Track.
4. Install `invest` with `portfolio_track_id` and `opening_positions` equal to the broker's
   holdings (checked at the first reconciliation).
5. The first portfolio turn covers `US:SPY` (§3.3), so the held position gets a research Track.
6. Uninstall `market` once nothing references it. 7d686d59… still does (Q4).

## 4. Slices

Order: #2087 B0 → … → B5, then K1 ∥ K2 → P1 → P2 → P3 → cut-over. K1 and K2 are kernel-generic
and inert until a recipe uses them, so they may ride any kernel deploy. P1–P3 and the cut-over
deploy after the verdict. P1 rebases on #2102 (same recipe layout). Gates per slice:
`scripts/local-ratchet-gates.sh`; for K1/K2 the whole `-p calm-server` run (new tool, prompt
surface); for P* `python3 -m pytest plugins/invest/tests -q`; for K1, `fe` and OpenAPI regen.

| # | Slice (≈ lines) | Tier | Acceptance | Must go red first (mutation) |
|---|---|---|---|---|
| K1 | `neige_track_add` (~1k: migration, keyed-create extraction, tool, CLI row, prompt file, config, `fe` field) | L2: authority + migration | Planner creates an unbound recipe Track in its area with `creator_track_id`, `recipe_id`/`revision`, NULL `parent_track_id`; replay returns it; the brief is delivered once; Worker → refused; cap enforced; REST create unchanged | `track_add_records_provenance_not_parent` (mutation: set `parent_track_id` → red); `track_add_refuses_past_open_cap` and `track_add_counts_only_open_tracks` (drop the count → red); `track_add_replays_and_refuses_changed_request`; `track_add_is_planner_only`; `track_add_delivers_brief_once`; migration test `creator_track_id_is_additive` |
| K2 | plugin standing instructions (~600) | L2: prompt authority and trust | A Planner whose report references `neige://plugin/<id>/` gets that plugin's text at start; others do not; an upgraded manifest takes effect at the next start | `plugin_instructions_follow_report_references` (mutation: predicate `true` → `…_skip_unreferenced_tracks` red); `…_skip_stopped_or_untrusted_plugin`; `…_are_read_live_not_snapshotted`; `manifest_v4_refuses_planner_instructions`; `manifest_refuses_instructions_over_2048_bytes` |
| P1 | `plugins/invest` core (~1k, Python): rename, ledger v1, `decision_add`/`execution_add`/`portfolio_status`, multi-symbol SDK bridge, portfolio units + recipe | L2: broker writer | Several weights execute sells-then-buys within caps on the fake broker; only the portfolio Track writes; the real App passes the caller-identity test (`caller_identity.rs:45` ported) | `test_weights_respect_universe_bounds`; `test_sells_settle_before_buys`; `test_instrument_track_cannot_trade` (mutation: drop the Track fence at the `allocation.py:44` analogue → red); `test_sdk_bridge_refuses_unconfigured_market`; `test_opening_positions_pin_first_reconciliation` |
| P2 | instruments + theses (~900): tools, instrument units on bound Tracks, instrument recipe, `planner_instructions` | L2: cross-Track writes, persistence | `instrument_add → neige_track_add → instrument_set` binds; a thesis set in I appears on P's board within one tick; recipes' slots resolve | `test_thesis_set_only_from_bound_track` (mutation: skip the `_meta` match → red); `test_add_refused_at_max_instruments`; `test_rm_refused_while_held`; kernel `invest_recipe_slots_resolve` (the `spy_recipe_slots.rs` analogue) |
| P3 | `series_show` on the Longbridge SDK (~600); retire `plugins/market` | L1: read-only data source | Same reply contract as `market.series` for US; `chart.series` blocks in both recipes render | `test_series_contract_matches_market_series` (golden from the Rust tests); `test_series_needs_no_track_meta` (F4) |
| C | 4140 cut-over (runbook §3.8, owner-run) | ops | portfolio Track reconciles `opening_positions`; `US:SPY` covered | — |

Each slice ends with the review tier above. L2 means two independent channels, re-run fresh
after every fix (AGENTS.md).

## 5. Risks

- **Wake cost.** N research Tracks with weekly and earnings entries mean N Planner turns a week.
  `max_instruments` is the operator bound, and the kernel cap is the backstop.
- **Overlay churn.** (N+1) Tracks × units republished each tick; #1995 owns publish throttling.
- **Agent-supplied `track_id`** in `instrument_set`: the plugin cannot verify it, but a wrong id
  only misroutes display units, and that Track's own calls fail visibly (§3.3).
- **Instructions drift from recipes.** Rules live in one place (the plugin). Recipes keep
  layout and schedule only; review checks recipes for rule text.

## 6. Open questions (owner)

1. **Cut-over.** Fresh start, closing the SPY Track (lose NAV continuity), or convert it (keep
   history, add upgrade code and three one-off ops)? *Recommend fresh start* (§3.8).
2. **`neige_track_add` listing.** Hidden plus `neige track add` (precedent `report tag`), or
   listed in `tools/list` after trimming ~700 B of other descriptions? *Recommend hidden:*
   91 B headroom, and only plugin-instructed Planners use it.
3. **Markets in v1.** USD markets only (`US`), or multi-currency now? *Recommend US-only v1:*
   the cash path is USD-only (F19); HK needs FX valuation and per-currency cash.
4. **`market` retirement.** Track 7d686d59… still renders `market.series`. *Recommend:* P3 moves
   its block source to `invest/series_show` (a one-off report edit like #2021's) and uninstalls
   `market`; CRYPTO charts are dropped with it (invest is Longbridge-only).
5. **Recipe authorization and cap.** Any stored recipe with default cap 16, or a per-recipe
   opt-in flag? *Recommend any stored recipe:* recipes are human-only (F14), and the cap plus
   provenance make every add visible and bounded.
