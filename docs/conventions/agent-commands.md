# Agent command and MCP naming convention

Status: normative for every new or changed kernel tool, `neige` command and native plugin tool.
Appendices A–B are the plan that brings the current surface into line; Appendix C records the
owner's decisions (2026-10-04). Builds on #2003 (`docs/architecture/2003-cli-mcp-naming.md`: brand, grammar,
mechanical CLI) and #2053 (Worker report actions). Verified at `24102733a` (2026-10-04). Issue: #2087.

## 1. Principles

1. **One meaning, one word.** A verb or parameter name means the same thing on every tool, and one
   meaning is never spelled two ways.
2. **The verb tells the effect.** From the action alone a caller knows whether the call only shows
   something (view), records a write anchor (anchored read), changes state (write), starts or ends
   something (lifecycle) or maintains storage (maintenance). The effect is never hidden in a
   parameter such as `cancelled: true` or `status`.
3. **Unix/git first.** When Unix or git has a verb with this meaning, use it (`ls`, `cat`, `log`,
   `diff`, `rm`). Otherwise use a plain imperative English verb. Never use a noun as an action.
4. **The object is the thing acted on,** as one singular lowercase noun (`track`, `task`, `report`).
5. **Names derive mechanically.** Tool name ↔ CLI command ↔ CLI options ↔ kernel client callable
   are computed from each other. There are no aliases and no hand-written spellings.
6. **Fewest words.** Add a verb or parameter name only when no listed word has the meaning.
7. **Errors teach.** Every refusal names the tool, the cause and the valid choices.

## 2. Grammar

```
kernel tool   := "neige." object "." action           object ∈ [a-z]+, action ∈ vocabulary (§3)
CLI command   := "neige" SP object SP action           action "_" → "-"
CLI option    := "--" key, "_" → "-"                   key = the input_schema key
CLI positional:= a required key listed in the command; also accepted as "--" key
plugin tool   := "plugin." plugin_id "_" tool           tool := segment ("." segment)*, §6
callable      := kernel tool with "." → "_"            e.g. neige_track_cat (Codex and Claude)
```

- The action is one vocabulary word. `_` inside an action exists only for the two #2053 Worker
  outcome actions (`report_success`, `report_failure`). **No new compound actions:** a new
  meaning gets a vocabulary word or a parameter.
- Only render views that never reach the tool use a single dash (`ls -l`). `--json` and `--force`
  are global. A command gets `--force` only when its tool is destructive.
- The kernel owns raw names only. A client callable for a **plugin** tool is client-minted (Codex
  hashes long names, Claude keeps `-`). Agents use the names that discovery returns and never
  derive plugin callables.
- MCP is the complete surface. A CLI row exists only for a shell-native view, a Worker report in
  CLI mode, or an operation that needs `--force` (#2003 §4.4).

## 3. Verb vocabulary (closed)

Effect classes: **V** view (no state change, no anchor), **AR** anchored read, **W** write,
**LC** lifecycle, **M** maintenance.

| Verb | Class | Meaning | Precedent |
|---|---|---|---|
| `ls` | V | list the object's entries as rows, no bodies | `ls` |
| `cat` | V | print one entry's content; never anchors a write | `cat` |
| `show` | V | one named object's details, or a file at a revision | `git show` |
| `status` | V | current state of the bound object | `git status` |
| `log` | V | change history, one row per change | `git log` |
| `diff` | V | net change between two points | `git diff` |
| `find` | V | entries matching predicates | `find` |
| `describe` | V | the contract of a type: schemas, kinds, usage | `neige tool describe`, `kubectl describe` |
| `read` | AR | return content **and** record the anchor that a later write of the same object requires | `read(2)` (may block) |
| `write` | W | replace the whole content; needs a whole read | `write(2)` |
| `commit` | W | apply a batch of ops atomically with a `message`; needs anchors | `git commit` |
| `tag` | W/V | list tags, or add/remove them | `git tag` |
| `rename` | W | change the display name | `rename(2)` |
| `add` | W | add an entry to a collection | `git add`, `git worktree add` |
| `set` | W | replace one entry's value under `expected_version` | `git config set` |
| `rm` | W | remove an entry from its collection | `rm`, `git rm` |
| `capture` | W | store a recorded call result as an immutable source | none (domain) |
| `notify` | W | put one ask on the user's notifications | `notify-send` |
| `input` | W | send text or keys to a terminal against the latest read | `tmux send-keys` |
| `control` | W | claim, release or detach terminal control | none (domain) |
| `open` | LC | start a terminal | `open(2)` |
| `close` | LC | end the track | `close(2)` |
| `cancel` | LC | stop a pending or running task | `cancel` (lp/CUPS) |
| `publish` | LC | push the track branch and open or reuse its PR | `npm publish` |
| `request` | LC | ask a human to ratify the track | none (kept) |
| `accept` | LC | record that an attempt meets the task (Planner) | none (plain English) |
| `reject` | LC | record that an attempt does not meet the task, with a `reason` | none (plain English) |
| `report_success`, `report_failure` | LC | the Worker's outcome claim for its attempt (#2053) | none |
| `gc` | M | prune history and sweep unreferenced objects | `git gc` |
| `vacuum` | M | reclaim database pages | SQLite `VACUUM` |

Decisions, one line each:

- **`ls`, not `list`.** `track.ls` already exists and Unix spells it so; `list`, `outline`,
  `reports` and `backlinks` all become `ls` on the right object. `neige tool list` becomes
  `neige tool ls`.
- **`cat` / `show` / `read` stay three verbs** because their effects differ: `cat` is a view,
  `show` adds a revision or names one object, `read` anchors. `workspace.report` becomes `cat`.
- **`terminal.observe` is an anchored read.** `terminal.input` acts "against your latest live
  observation" and refuses without one (`terminal_interaction/operations.rs:112`, "observe first")
  or when the screen moved (`stale_observation`). That is `report.read`'s pattern, so it becomes
  `terminal.read`, and the boolean `observe` (read back after a write) becomes `read`.
- **`status`, not `state`.** `git status` is the known word; plugins already use `barra.status`
  and `spy.status`.
- **Noun actions are retired:** `outline` → `area.ls`, `backlinks` → `link.ls`, `kinds` →
  `report.describe`, `verdict` → `task.accept` / `task.reject`, `changes` → `workspace.diff`, `edits` →
  `workspace.log`, `reports`/`report` → `workspace.ls`/`workspace.cat`, `resolve` →
  `terminal.show`.
- **CRUD words retire:** `create`/`register` → `add`, `update` → `set`, `unregister` and
  cancel-by-flag → `rm`. `mv` is not used: no tool moves an entry between containers.
- **Domain verbs with no Unix equivalent stay** (`capture`, `control`, `request`, `accept`,
  `reject`, `publish`), each with one meaning.

## 4. Parameter vocabulary

| Name | Meaning (only this) |
|---|---|
| `<noun>_id` | the identifier of that noun: `track_id`, `attempt_id`, `terminal_id`, `source_id`, `entry_id`, `preview_id`, `observation_id`. A bare `id` is never a parameter. |
| `key` | the task key: the plan's stable name of a task. Nothing else is called `key` at top level. |
| `idempotency_key` | a caller-chosen replay key for a write; a replay returns the first result |
| `expected_version` | the optimistic lock of a `set` or `rm` |
| `message` | the audit note a write records: why this change (`git commit -m`) |
| `reason` | why something failed or was rejected; stored with the failure |
| `text` | verbatim text delivered to a person or typed into a terminal |
| `body` | a document's whole content (report body, PR body, comment body) |
| `summary` / `title` | one-line summary / display name |
| `path` | a track-relative view path (`report.md`, `area/reports/x.md`) |
| `cursor` / `next_cursor` | paging: pass the previous result's `next_cursor` (an opaque **string**) as `cursor` |
| `limit` | the most rows to return, where a tool offers it |
| `from` / `to` | range endpoints; a window is half-open `[from, to)` |
| `until` | the inclusive last date of a recurrence only (RFC 5545 `UNTIL`) |
| `date` / `timezone` | a `YYYY-MM-DD` day / an IANA zone name, in input and output alike |
| `blocks` / `sections` | report block ids / H1 section texts to select, top level on every tool that selects report parts |
| `detail` | how much of each selected item to return (`summary`, `full`, `index`) |

- **Casing:** snake_case keys in input and output. Exempt: the MCP envelope (`structuredContent`,
  `isError`, a fixed protocol), JSON Schema keywords, and opaque payloads (a block's `payload`, a
  plugin's result).
- **Spelling:** American English (`canceled`, `color`).
- **Flags:** a boolean names the extra effect it adds when true and defaults to false (`dry_run`,
  `include_empty`, `with_markers`, `claim`, `read`). A boolean never switches the operation; that
  is a verb. On the CLI a flag takes no value.
- **Closed input:** a kernel tool refuses an unknown top-level key with the valid keys.

## 5. Results and errors

- **Success** is a JSON object, never a bare array or scalar. A read returns the data. A write
  returns its post-state (ids, new revision, timestamps); with none, `{"ok": true}`. `ok` is never
  false: a refusal is an error.
- **Lists:** rows under a plural key. A paged list always carries `next_cursor` (`null` on the last
  page). An unpaged list over its cap is refused with a hint to narrow it and never truncated
  silently. `<field>_truncated` marks content clipped inside one row.

Agent-facing JSON-RPC codes, one meaning each:

| Code | Meaning |
|---|---|
| -32601 | unknown tool or method; the message lists the session's tools |
| -32602 | invalid arguments: missing, malformed or unknown key, bad value; lists the valid choices |
| -32403 | forbidden: role, scope or grant refuses this caller |
| -32404 | the named entity does not exist in the caller's scope |
| -32409 | conflict with current state: stale anchor or revision, wrong status, already done; re-read and retry |
| -32503 | a dependency is unavailable now: plugin disabled or not running |
| -32603 | internal error |
| -32002, -32401, -32426 | session protocol only (not initialized, unknown token, old client); never from a tool |

- The message starts with the full tool name: `neige.task.cancel: task t3 is verifying; …`.
- Error `data` carries machine fields (`refusal`, current revisions). Text and data say the same.
- **CLI exit codes:** `0` success, `1` usage (unknown object, action, option, missing `--force`),
  `4` the tool refused or its result could not be rendered. The forwarder alone uses `2` (its
  environment: missing variable or non-UTF-8 argument), `3` (kernel unreachable or protocol
  failure) and `141` (write failed). Each code has one meaning across both.

## 6. Plugins

- **Plugin id:** a lowercase hyphen slug `[a-z0-9]+(-[a-z0-9]+)*`, 2–64 bytes, for **every**
  plugin. Five of the seven installed ids already have that shape (`dev-neige-market`,
  `cli-longbridge`, `mcp-wisburg-mcp-server-49abefc5`, …). The two built-in dotted ids are renamed
  (`dev.neige.calendar` → `dev-neige-calendar`, `dev.neige.git-forge` → `dev-neige-git-forge`;
  slice B5). Ids never contain `_`, which delimits `plugin.<id>_<tool>`.
- **Native tool names** (manifest-authored) are `<object>.<verb>` with §3's verbs and §4's
  parameters. A tool that wraps a known CLI mirrors that CLI's words instead (`gh.pr.list`,
  `git.worktree.add`). A connector plugin (an external MCP server) keeps its upstream names.
- **Host callbacks** (`neige.kv.*`, `neige.overlay.*`, `neige.card.*`, `neige.event.subscribe`)
  are the plugin-to-host protocol, not tools. They keep their names. A kernel tool never uses the
  objects `kv`, `overlay`, `card` or `event`, so the two namespaces never collide.

## 7. Discovery and help

- `neige tool ls (--prefix P | --all) [--cursor C]` returns `{tools: [{name, cli, listed}],
  next_cursor}`. `neige tool describe --name N` returns the MCP declaration plus `cli` and
  `listed`.
- `listed` (shown in `tools/list`) is a context-budget choice. A tool is hidden when shell use
  serves it better. Listing is never a grant; the tool's role gate decides.
- `neige help`, `neige help <object>`, `neige help <object> <action>` (and `--help`). Command help
  shows the usage line and every option (its schema key), and for a report view, the read to make
  before a write ("`track cat` is a view; a report write needs `neige.report.read`").
- A tool description states its roles, its effect, its result shape, and the read it needs.

## 8. Checklist and enforcement

Adding or changing a tool or command:

1. Pick the object (thing acted on) and a §3 verb. A new verb needs this document changed first.
2. Name parameters from §4. A new name needs a meaning that no §4 name has.
3. Close the schema; make required fields required.
4. Return an object (§5). Refuse with §5 codes, message prefixed by the tool name, valid choices listed.
5. Add `prompts/tools/<name>.md` (roles, effect, result, the read it needs).
6. Add a CLI row only for a §2 reason. Options are schema keys.
7. Regenerate goldens and keep the Planner byte budget. Run the tests below.

Enforced by tests (existing): `kernel_tool_names_follow_the_grammar`,
`every_option_is_its_schema_key`, `prompt_neige_mentions_name_served_commands` (H8),
`no_retired_tool_names_remain`, `kernel_tool_callables_are_injective_and_unhashed`,
`unknown_tool_error_lists_the_sessions_tools`, `kernel_exit_codes_are_exactly_0_1_4`.

Proposed (cheap, registry-driven, Appendix B):

- `kernel_tool_actions_are_in_the_vocabulary`: every action is a §3 verb; the compound list is
  closed.
- `kernel_tool_params_use_the_vocabulary`: every input key, recursively, is snake_case and is not
  one of the retired names (`id`, `after`, `cancelled`, `time_zone`, `request_id`, `select`).
- `every_kernel_tool_refuses_unknown_arguments`: calls each registered kernel tool with an unknown
  key and asserts -32602 `<tool>: unknown argument …` naming the valid keys.

## 9. Known gaps

- `track ls area/reports/`, `report.find` and `area.ls` list the same reports with different fields.
- Terminal snapshots are "observations", which is also the Planner's wake-item word.
- Inside a terminal `input` step, `{type: "key", key: "Enter"}` is a keyboard key, not a task key.
- `report.write` and `track.rename` take an optional `message`; other writes require one.
- `admin` and `ratify` are not things acted on (cut, Appendix B).
- Plugin host-callback error codes (-32001/-32003/-32004) differ from §5; plugin channel only.
- Paged tools have fixed page sizes; `limit` is not offered on them.

## Appendix A — Conformance table (proposal)

Persisted counts are calls stored in the Planner transcript's `$.item.tool` on the production 4140
database, read-only, 2026-10-04. Production is at migration 133, so they are stored under the
`calm.` names; migration 0134 renames them to `neige.` on deploy, and the new migration (B1) maps
`neige.<old>` → `neige.<new>` after it. Consumers: P prompts and templates, G goldens, F `fe`,
T tests, R recipes.

| Current | Proposed | 4140 stored calls | Consumers |
|---|---|---|---|
| `plan.list` | `task.ls` | 204 | P (55 refs), G, F `PLAN_LIST_TOOL`, T |
| `plan.cancel` | `task.cancel` | 18 | P (`prompts/plan-cancel/`), T |
| `task.verdict {status}` | `task.accept` / `task.reject` (C1); the migration maps each stored call by its `status` argument | 110 | P, G, F `TASK_VERDICT_TOOL`, T |
| `terminal.observe` + flag `observe` | `terminal.read` + flag `read` | 18 | P (93 refs, guides/terminal), T |
| `terminal.resolve` | `terminal.show` | 8 | P, T |
| `report.kinds` | `report.describe` | 22 | P, G, F `REPORT_READ_TOOLS`, T |
| `report.backlinks` | `link.ls` | 2 | P, F `REPORT_READ_TOOLS`, T |
| `area.outline` | `area.ls` | 2 | P, T |
| `source.list` | `source.ls` | 8 | P, T |
| `track.state`, `neige track state` | `track.status`, `neige track status` | 0 (CLI) | P (21 CLI refs, 21 turn texts in `observation.rs`), CLI render, T |
| `calendar.create` / `list` / `update` | `calendar.add` / `ls` / `set` | 8 / 2 / 0 | P, builtin instructions, R (1 recipe: `calendar.list`), T |
| `calendar.update {cancelled: true}` | `calendar.rm {entry_id, expected_version}` | — | P, T |
| `preview.register` / `unregister` | `preview.add` / `preview.rm` | 2 / 2 | P, T |
| `workspace.reports` / `report` / `changes` / `edits` | `workspace.ls` / `cat` / `diff` / `log` | 0 (unreleased) | P, T |
| `neige tool list --after` | `neige tool ls --cursor` | 0 | P (`tool-discovery.md`), help, T |
| params: calendar `id`; preview `key` | `entry_id`; `preview_id` | params are not migrated | P, T |
| params: `after` (workspace ×3; edits takes an integer) | `cursor` (string); `workspace.log` `next_cursor` becomes a string | — | P, T, F wire types `ReportEditsPage` |
| param: calendar.list `until` | `to` | — | P, T |
| param: terminal `request_id` | `idempotency_key` | — | P, T |
| param: `ratify.request` `reason` (a question for a person) | `text` | — | P, T |
| param: `report.read` `select` | `blocks` / `sections` / `detail: full\|index` | — | P (report guides), T |
| output: `time_zone` (workspace, creation identity) | `timezone` | unreleased (0136) | P, F wire types |
| output: `docRev`, `schemaVersion`, `taskDiagnostics` (report read/write/commit) | `doc_rev`, `schema_version`, `task_diagnostics` | — | P, refusal texts, T |
| output: `trackId`, `updatedAt` (`report.find`, `track ls area/reports/`) | `track_id`, `updated_at`; wrapped as `{reports: […]}` | — | P, CLI render, T |
| error: role refusal -32602 (`require_role`) | -32403 | — | T |
| error: report revision conflict -32001 | -32409 (data unchanged) | — | P, T |
| error: plugin disabled / not running -32002 | -32503 | — | T, `routes/plugins.rs` mapping |
| error: calendar store -32000 | -32602/-32409 by cause | — | T |
| error prefixes `plan_cancel:`, `track_report:`, `task_verdict:`, … | the tool name | — | T |
| 18 schemas without closed input; shared parsers ignore unknown keys | refuse unknown keys (§4) | — | T |
| forwarder exit 4 for transport failures | 3 | — | T, `1801` doc |

## Appendix B — Implementation slices (proposal)

Common gates per slice: `scripts/local-ratchet-gates.sh`; targeted
`env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server <filter> --test-threads 8`;
the whole `-p calm-server` run for tool or prompt changes; `scripts/local-rust-gates.sh --quick`;
the `fe` gate when `fe/` changes. Measure the Planner tool budget (29,458 / 30,000 B) and the
guide budget (7,489 / 7,500 B) before and after; trim wording, never raise a cap.

| # | Slice | Pain | Acceptance | Must go red first |
|---|---|---|---|---|
| B1a | View verbs: `task.ls/cancel`, `source.ls`, `area.ls`, `link.ls`, `report.describe`, `track.status`, `workspace.ls/cat/diff/log`, `tool ls`; one new migration (numbered last) renaming the stored `$.item.tool` values and the 1 recipe (revision bump), mapping the 0134 output names | synonyms, noun actions | No old name in the registry, prompts or `fe`; the migration rewrites only the tool field | New `kernel_tool_actions_are_in_the_vocabulary`; mutation: register `neige.plan.list` again → red: that test, the golden, the retired-name sweep and the direct-call tests using the literal |
| B1b | Terminal: `observe` → `read`, `resolve` → `show`, flag `observe` → `read`, `request_id` → `idempotency_key` | anchored read named two ways | Input still refuses without a prior read; approvals unchanged | `stale_observation` and "read first" refusal tests name `neige.terminal.read`; mutation: keep `observe` in the refusal text → red |
| B1c | Calendar and preview: `add/ls/set/rm`, `entry_id`, `to`, `preview_id`; `task.accept` / `task.reject` | CRUD words, effect in a flag | `calendar.set` no longer takes `cancelled`; `rm` stops wakes | Migration test seeds accepted and rejected `task.verdict` rows and a `calendar.list` row and the recipe, and reads back the new names; mutation: drop one map row → red |
| B2 | Parameters and paging: `cursor` everywhere (string), `report.read` `blocks`/`sections`/`detail`, `ratify.request` `text`, outputs `timezone`, plus the parameter vocabulary test | `after` with two types; one selection spelled two ways | `workspace.log` pages with a string cursor; `fe` wire regenerated with `npm run gen:api` | New `kernel_tool_params_use_the_vocabulary`; mutation: rename `cursor` back to `after` on one tool → red |
| B3 | Results: snake_case report outputs; `{reports: […]}` with `track_id`/`updated_at` | `docRev` beside `updated_at` | CLI `--json` and MCP agree; refusal texts say `doc_rev` | New `report_tool_results_are_snake_case` over real `read`/`write`/`commit`/`find` calls (recursion skips `payload`); mutation: restore `docRev` → red |
| B4 | Errors and exits: transport-level unknown-key refusal for every kernel tool, codes per §5, tool-name prefixes, forwarder exit 3 | silent unknown keys; overloaded codes | Every kernel tool refuses `{"zz": 1}` with its valid keys | New `every_kernel_tool_refuses_unknown_arguments`; mutation: skip the check for one tool → red; existing `kernel_exit_codes_are_exactly_0_1_4` plus a forwarder twin |

| B5 | Plugin ids: rename `dev.neige.calendar` → `dev-neige-calendar` and `dev.neige.git-forge` → `dev-neige-git-forge` in the built-in manifests, kernel references and one migration over every table that stores a plugin id (plugins, plugin_kv, report sources, scopes and bindings, transcript `plugin.<id>_…` tool names, recipes; count each on 4140 first) | two id shapes | `plugin_ids_are_hyphen_slugs` over every registered and built-in manifest; nothing on 4140 still holds a dotted id | New `plugin_ids_are_hyphen_slugs`; mutation: restore one dotted built-in id → red |

**Approved scope (owner, 2026-10-04):** B1 (B1a–B1c, one migration) and B2 first, then B5 (L2: a
persistence migration). B3 and B4 come later. Order: B1a → B1b → B1c → B2 → B5. Each slice stays near
1k lines; B1a is the largest (≈ 260 name occurrences in crates, fe, plugins and docs).

**Cut (recorded, no observed pain):** `admin.*` → `track.gc`/`db.vacuum` (hidden, 0 stored
calls); `ratify.request` → another object; `terminal.input` → `send`; `terminal.control` split;
`source.capture` → `add` (one recipe would need a rewrite); `{"ok": true}` → `{}`; `track.rename`'s
`{ok: false, refused}` → -32409 (deliberately not an error today); calendar entry field
`cancelled` (stored in 4 kv rows and served to `fe`); plugin tool renames (`gh.issue.comments`,
`market.holdings.list`, `barra.series`, …: recipes name them); plugin `idem`/`attempt` → §4 names;
host-callback codes; `limit` on paged tools.

## Appendix C — Owner decisions (2026-10-04)

1. **`task.verdict` → `task.accept` / `task.reject`.** The effect is in the verb (principle 2), not in
   a `status` argument. `task.reject` takes the `reason`.
2. **`plan.*` → `task.*`:** `task.ls`, `task.cancel`. One object for one concept.
3. **Plugin ids: one format, hyphen slugs, for every plugin,** including the two built-ins
   ("长痛不如短痛"). Slice B5.
4. **Worker compounds:** `task.report_success` / `report_failure` (#2056) stay as the only compounds.
5. **Paging:** `cursor` / `next_cursor`, an opaque string, everywhere.
6. **`terminal.observe` → `terminal.read`:** `read` always means the anchored read.
7. **Verbs follow Unix/git** ("类unix 风格, 大众认知高, 学习成本低").
8. **Scope:** B1 + B2, then B5; B3 and B4 later.
