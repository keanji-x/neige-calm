# Agent command and MCP naming convention

Status: normative for every new or changed kernel tool, `neige` command and native plugin tool.
Appendices A–B are the plan that brings the current surface into line; Appendix C records the
owner's decisions (2026-10-04; revised the same day to the `_` separator, C9; B5 descoped
2026-10-05, C10). Builds on #2003 (`docs/architecture/2003-cli-mcp-naming.md`: brand, grammar,
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
5. **Names derive mechanically.** Tool name ↔ CLI command ↔ CLI options are computed from each
   other, and the name a model sees is the tool name itself. There are no aliases and no
   hand-written spellings.
6. **Fewest words.** Add a verb or parameter name only when no listed word has the meaning.
7. **Errors teach.** Every refusal names the tool, the cause and the valid choices.

## 2. Grammar

```
word          := [a-z0-9]+                             one word; never contains "_"
kernel tool   := "neige_" object "_" action            object: a word; action: a §3 verb
CLI command   := "neige" SP object SP action           the tool name split at "_"
CLI option    := "--" key, "_" → "-"                   key = the input_schema key
CLI positional:= a required key listed in the command; also accepted as "--" key
plugin tool   := "plugin_" plugin_id "_" tool          plugin_id: a word; tool: §6
model sees    := "mcp__" server_key "__" tool name     e.g. mcp__neige__neige_track_cat
```

- **`_` separates and nothing else.** Every segment is one word, so a name splits at `_` without
  ambiguity and the CLI command is that split. **No compound actions:** a new meaning gets a
  vocabulary word or a parameter.
- **The tool name is provider-safe.** Codex replaces every character outside `[A-Za-z0-9_]` with
  `_`, and the Claude and OpenAI APIs refuse `.` in a tool name. A name made of words and `_` passes
  through both unchanged, so prompts, docs, the CLI help and the model's tool list all show the
  same name.
- Schema keys keep their own `_` (`track_id`); the grammar splits only tool names.
- Only render views that never reach the tool use a single dash (`ls -l`). `--json` and `--force`
  are global. A command gets `--force` only when its tool is destructive.
- The kernel mints every name it serves, plugin tools included (§6), in this alphabet. No adapter
  respells a name; the only provider rule left is Codex's 128-byte cap on
  `mcp__<server>__<name>` (§9).
- MCP is the complete surface. A CLI row exists only for a shell-native view, a Worker report in
  CLI mode, or an operation that needs `--force` (#2003 §4.4).

## 3. Verb vocabulary (closed)

Effect classes: **V** view (no state change, no anchor; access metadata excepted, see the last decision below), **AR** anchored read, **W** write,
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
| `ask` | W | put questions on the user's notifications; the answer wakes the Planner (#2209) | none (plain English) |
| `send` | W | deliver one message to another Track's Planner and wake it | `send(2)`, `sendmail` |
| `input` | W | send text or keys to a terminal against the latest read | `tmux send-keys` |
| `control` | W | claim, release or detach terminal control | none (domain) |
| `open` | LC | start a terminal | `open(2)` |
| `close` | LC | end the track | `close(2)` |
| `cancel` | LC | stop a pending or running task | `cancel` (lp/CUPS) |
| `publish` | LC | push the track branch and open or reuse its PR | `npm publish` |
| `accept` | LC | record that an attempt meets the task (Planner) | none (plain English) |
| `reject` | LC | record that an attempt does not meet the task, with a `reason` | none (plain English) |
| `done` | LC | the Worker's claim that its attempt met the task (was `report_success`, #2053) | `task <id> done` (Taskwarrior) |
| `fail` | LC | the Worker's claim that its attempt cannot meet the task, with a `reason` (was `report_failure`) | none (plain English) |
| `gc` | M | prune history and sweep unreferenced objects | `git gc` |
| `vacuum` | M | reclaim database pages | SQLite `VACUUM` |

Decisions, one line each:

- **`ls`, not `list`.** `track_ls` already exists and Unix spells it so; `list`, `outline`,
  `reports` and `backlinks` all become `ls` on the right object. `neige tool list` becomes
  `neige tool ls`.
- **`cat` / `show` / `read` stay three verbs** because their effects differ: `cat` is a view,
  `show` adds a revision or names one object, `read` anchors. `workspace.report` becomes `cat`.
- **`terminal.observe` is an anchored read.** `terminal_input` acts "against your latest live
  observation" and refuses without one (`terminal_interaction/operations.rs:112`, "observe first")
  or when the screen moved (`stale_observation`). That is `report_read`'s pattern, so it becomes
  `terminal_read`, and the boolean `observe` (read back after a write) becomes `read`.
- **`status`, not `state`.** `git status` is the known word; plugins already use `barra.status`
  and `spy.status`.
- **Noun actions are retired:** `outline` → `area_ls`, `backlinks` → `link_ls`, `kinds` →
  `report_describe`, `verdict` → `task_accept` / `task_reject`, `changes` → `workspace_diff`, `edits` →
  `workspace_log`, `reports`/`report` → `workspace_ls`/`workspace_cat`, `resolve` →
  `terminal_show`.
- **CRUD words retire:** `create`/`register` → `add`, `update` → `set`, `unregister` and
  cancel-by-flag → `rm`. `mv` is not used: no tool moves an entry between containers.
- **Domain verbs with no Unix equivalent stay** (`capture`, `control`, `ask`, `accept`,
  `reject`, `publish`), each with one meaning.
- **`done` / `fail` replace the two Worker compounds** so every action is one word. The Worker
  claims (`task_done`, `task_fail`); the Planner decides (`task_accept`, `task_reject`).
- **A view may stamp access metadata** (e.g. a last-seen time) and refresh derived projections onto
  the caller's own Track; it never changes authority or domain state (#2104).
- **`send` means mail only** (#2130): `add` would hide the wake (principle 2), `ask` is the
  user's notifications and `input` is a terminal. Appendix B's cut `terminal_input` → `send` stays
  cut.

## 4. Parameter vocabulary

| Name | Meaning (only this) |
|---|---|
| `<noun>_id` | the identifier of that noun: `track_id`, `attempt_id`, `terminal_id`, `source_id`, `entry_id`, `preview_id`, `observation_id`. A bare `id` is never a top-level parameter (nested `ops[].id` in `report_commit` is a block id, §9). |
| `key` | the task key: the plan's stable name of a task. Nothing else is called `key` at top level. |
| `idempotency_key` | a caller-chosen replay key for a write; a replay returns the first result |
| `expected_version` | the optimistic lock of a `set` or `rm` |
| `message` | the audit note a write records: why this change (`git commit -m`) |
| `reason` | why something failed or was rejected; stored with the failure |
| `text` | verbatim text delivered to a person, typed into a terminal, or mailed to a Track |
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
- **Closed input:** a kernel tool refuses an unknown top-level key with the valid keys. Every
  kernel schema declares `additionalProperties: false`, and the registry refuses the key from that
  schema's `properties` for every tool (B4), so no tool keeps its own list.

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

- The message starts with the full tool name: `neige_task_cancel: task t3 is verifying; …`. The
  registry leads every refusal of a kernel tool with its name (B4).
- **Order** of every kernel tool call (`ToolRegistry::lookup`): the session's identity, the
  managed-track grant (-32403), a built-in plugin's visibility fence (-32601 or -32503), closed
  input (-32602), then the tool's declared `roles` (-32403), and only then the handler: its
  arguments and its state. Closed input precedes the role gate for every tool alike: it names only
  the keys of the public schema (`neige tool describe`), reads no state, and the role gate still
  runs before any state read or write.
- Error `data` carries machine fields (`refusal`, current revisions). Text and data say the same.
- **CLI exit codes:** `0` success, `1` usage (unknown object, action, option, missing `--force`),
  `4` the tool refused or its result could not be rendered. The forwarder alone uses `2` (its
  environment: missing variable or non-UTF-8 argument), `3` (kernel unreachable or protocol
  failure) and `141` (write failed). Each code has one meaning across both
  (`kernel_exit_codes_are_exactly_0_1_4`, `forwarder_exit_codes_are_exactly_2_3_141`).

## 6. Plugins

- **Plugin id:** one word, `[a-z0-9]+`, 2–32 bytes. The two built-ins are words (slice B5,
  migration 0148), and the install route refuses any other id for a new plugin. The five
  installed external ids are grandfathered (Appendix C, item 10): the boot loader still accepts
  them, their rows keep reload and enable, and the install route still accepts them
  (`LEGACY_PLUGIN_IDS`, §9), so a fresh host or a reinstall keeps working.

  | Plugin | Id |
  |---|---|
  | Calendar (built-in) | `calendar` (was `dev.neige.calendar`) |
  | Development (built-in) | `gitforge` (was `dev.neige.git-forge`) |
  | Market, Barra, Paper trading | `dev-neige-market`, `dev-neige-barra`, `dev-neige-paper-trading` (kept) |
  | Longbridge, Wisburg | `cli-longbridge`, `mcp-wisburg-mcp-server-49abefc5` (kept) |

- **Minted name:** `plugin_<id>_<tool>`, where both `<id>` and `<tool>` have every character
  outside `[A-Za-z0-9_]` written `_` (`plugin_dev_neige_market_market_quote`), so Codex and Claude
  show the model the same name. `plugin_results::registry_name` is the one minting function;
  discovery, the ambiguity message, recorded results and `source_capture` use it, and migration
  0148 respelled stored transcript names the same way. Routing looks the minted name up among the
  running plugins' minted names and dispatch sends the plugin its original upstream name. Minting
  is not injective, so it is fenced: a manifest (or a connector's materialized tool set) whose
  tools mint one name is refused, and a plugin whose id mints the same `plugin_<id>_` prefix as a
  running one (`a-b`, `a.b`) is refused at spawn. A name two plugins still mint is refused at
  routing, naming both raw pairs.
- **Prefix by lifecycle, not by where the code lives.** `neige_` names a tool served to every
  Track. A tool that comes and goes with a plugin's enablement or a Track's plugin scope is a
  plugin tool, `plugin_<id>_<tool>`, minted by `registry_name`, whether its manifest declares it
  or a built-in compiles it. An always-enabled built-in is still scoped: a Track bound to another
  plugin does not see it.
- **Native tool names** (manifest-authored or compiled) are written in the minted form already:
  `<object>_<verb>` with §3's verbs and §4's parameters, so nothing is rewritten. When the
  plugin applies that verb to one object only, the object is left out, because the plugin id
  already names it (`publish`, not `track_publish`); a plugin with several objects keeps them
  (`instrument_add`, `thesis_add`). A tool that wraps a known CLI mirrors that CLI's words
  instead (`gh_pr_list`, `git_worktree_add`). A connector plugin (an external MCP server) keeps
  its upstream names; only its minted name is rewritten.
- **Host callbacks** (`neige.kv.*`, `neige.overlay.*`, `neige.card.*`, `neige.event.subscribe`)
  are JSON-RPC methods of the plugin-to-host protocol, like `tools/call`, not tools. No model sees
  them, so they keep their dotted names. Since a tool name never contains `.`, the two namespaces
  cannot collide.

## 7. Discovery and help

- **The catalog is what the role may call** (#2289): the kernel tools and running, in-scope
  compiled natives whose declared `roles` hold the session's role, the in-scope manifest plugin
  tools for a Planner or Worker, under the managed-track restriction `tools/call` applies. One
  owner (`SessionCatalog`, `transport/catalog.rs`) produces it; `tools/list` is its `listed_for`
  view.
- `neige tool ls (--prefix P | --all) [--cursor C]` returns `{tools: [{name, surfaces, cli,
  listed, plugin, kind}], next_cursor}`. `neige tool describe --name N` returns the MCP declaration
  plus `surfaces`, `cli`, `listed`, `plugin` and `kind`. `surfaces` is `["mcp"]`, or
  `["mcp","cli"]` when a `neige` command calls the tool; every tool is served over MCP. `cli` is
  that command or null. `listed` is true exactly when the session's `tools/list` shows the tool.
  `plugin` is the id of the plugin serving the tool, built-in natives such as
  `plugin_gitforge_publish` included, or null for a kernel tool; `kind` is the kind its manifest
  declares (`forge-action`) or null (#2227). A text row is
  `name  mcp|mcp+cli  cli-or-—  listed|hidden  kernel|plugin:<id>  kind-or-—`.
- **Three refusals teach apart:**
  - *No tool* (`describe`, exit 4, `kind: catalog`): an unknown name, or a plugin tool outside the
    Track's scope or not running. Plugin scope stays undiscoverable, as at `tools/call`.
  - *Wrong role* (`describe`, exit 4, `kind: role` with `roles` and `role`): a kernel tool or a
    running, in-scope native whose `roles` exclude the session's role: "`neige_admin_gc` is for
    roles [Planner]; this session is Assistant". Kernel tool names are public (#2003 K7).
  - *MCP only* (any command, exit 1, usage): `neige <object> <action>` where
    `neige_<object>_<action>` is a kernel tool with no command: "`neige_report_read` has no CLI
    command; call it as an MCP tool (`neige tool describe --name neige_report_read` shows it)".
- `listed` (shown in `tools/list`) is a context-budget choice. A tool is hidden when shell use
  serves it better. Listing is never a grant: a tool declares `roles` (who may call it) and
  `listed_for` (the subset whose `tools/list` shows it, a context-budget choice), and the
  registry gates `roles` before the handler runs.
- `neige help`, `neige help <object>`, `neige help <object> <action>` (and `--help`). Command help
  shows the usage line and every option (its schema key), and for a report view, the read to make
  before a write ("`track cat` is a view; a report write needs `neige_report_read`").
- A tool description states its roles, its effect, its result shape, and the read it needs.

## 8. Checklist and enforcement

Adding or changing a tool or command:

1. Pick the object (thing acted on) and a §3 verb. A new verb needs this document changed first.
2. Name parameters from §4. A new name needs a meaning that no §4 name has.
3. Close the schema; make required fields required.
4. Return an object (§5). Refuse with §5 codes, message prefixed by the tool name, valid choices listed.
5. Add `prompts/tools/<name>.md` (roles, effect, result, the read it needs).
6. Add a CLI row only for a §2 reason. Options are schema keys.
7. Regenerate goldens and keep the Planner byte budget. Run the tests below. One recorded
   exception (#2130, owner): `neige_mail_send` must be listed to be discoverable, so S1 trimmed
   restated schema facts from eight Planner descriptions (164 B) and raised
   `planner_tool_surface_fits_its_byte_budget`'s cap by the remaining 430 B, to 30,430 B. B5 lowered it
   to the measured 30,398 B.

Enforced by tests (existing): `kernel_tool_names_follow_the_grammar` (B0 changes it to §2's
`neige_<word>_<word>`), `every_option_is_its_schema_key`, `prompt_neige_mentions_name_served_commands` (H8),
`no_retired_tool_names_remain`, `kernel_tool_callables_are_injective_and_unhashed`,
`unknown_tool_error_lists_the_sessions_tools`, `kernel_exit_codes_are_exactly_0_1_4`.

Added by the slices (registry-driven, Appendix B):

- `served_tool_names_use_the_word_alphabet`: every name in `tools/list` matches `[A-Za-z0-9_]+`,
  kernel and plugin tools alike (B5 adds the built-ins, the repository manifests and a connector
  whose id and tools carry `.` and `-`).
- `served_tool_names_fit_the_codex_cap`: the `mcp__neige__` form of every kernel and native
  plugin tool is within 128 bytes. Connector tools are exempt (§9).
- `kernel_tool_actions_are_in_the_vocabulary`: every action is one §3 verb (added in B1c, once
  every retired verb is gone).
- `plugin_ids_are_words`: every built-in manifest id, and every id the install route accepts for
  a new plugin, matches `[a-z0-9]{2,32}` (installed external ids are grandfathered, §9).
- `minted_tool_names_refuse_collisions` and `a_connector_tool_with_a_dot_is_called_upstream_by_its_raw_name`
  (B5): colliding tools and ids are refused, and `foo.bar` is served as `plugin_<id>_foo_bar` and
  called upstream as `foo.bar`.
- `kernel_tool_params_use_the_vocabulary`: every input key, recursively, is snake_case (opaque
  payloads skipped, §4), and no top-level key is a retired name (`id`, `after`, `cancelled`,
  `time_zone`, `request_id`, `select`, `until`).
- `report_tool_results_are_snake_case` (B3): real `report_read`/`write`/`commit`/`find` and
  `track_ls area/reports/` calls through the kernel socket return objects whose keys, recursively,
  are snake_case (a block's `payload` skipped), and `neige --json` prints the same JSON;
  `no_kernel_tool_returns_a_top_level_array` calls every read-only kernel tool.
- `every_kernel_tool_refuses_unknown_arguments` (B4): an authenticated Planner session calls each
  registered kernel tool through the kernel socket with `{"zz": 1}` (a built-in's tool on a Track
  its plugin owns) and gets -32602 ``<tool>: unknown argument `zz`; valid: <schema keys>``; each
  schema declares `additionalProperties: false`.
- `forwarder_exit_codes_are_exactly_2_3_141` (B4): the forwarder's own exits, disjoint from the
  kernel's.

## 9. Known gaps

- `track ls area/reports/`, `report_find` and `area_ls` list the same reports with different fields.
- Terminal snapshots are "observations", which is also the Planner's wake-item word.
- Inside a terminal `input` step, `{type: "key", key: "Enter"}` is a keyboard key, not a task key.
- `report_write` and `track_rename` take an optional `message`; other writes require one.
- `admin` is not a thing acted on (cut, Appendix B).
- Plugin host-callback error codes (-32001/-32003/-32004) differ from §5; plugin channel only.
- KNOWN GAP (B4): the terminal tools answer every runtime failure -32403, a stale observation
  included; their errors are untyped, so §5's split (-32409 for a stale anchor) needs typed
  terminal errors first. The e2e harness matches the refusal text, not the code.
- Internal (-32603) messages of shared domain modules keep a module tag after the tool name
  (`neige_report_commit: track_report: …`); the tag names the module, not a tool.
- A nested object's unknown key (`ops[]`, `call`, `manual`, calendar `task`) is refused by its
  tool, not by the registry.
- Paged tools have fixed page sizes; `limit` is not offered on them.
- A connector tool whose `mcp__neige__plugin_<id>_<tool>` exceeds 128 bytes is still cut and
  hash-suffixed by Codex (#2003 K1); `served_tool_names_fit_the_codex_cap` covers kernel and
  native tools only.
- `report_read`'s `task_diagnostics` respells the REST `BlockVerdict` keys at the tool boundary
  (the fe reads the camelCase wire); enum values such as `pending_reason.kind: "notAdmitted"` and the
  data in `gate_result` and `message_args` keep their spelling.
- `report_commit`'s nested `ops[].id` is a bare block id; renaming it to `block_id` changes the
  report op contract without an observed pain (kept).
- KNOWN GAP (B5 descope, Appendix C item 10): the five installed external plugin ids keep `.` or
  `-` (`dev-neige-market`, `dev-neige-barra`, `dev-neige-paper-trading`, `cli-longbridge`,
  `mcp-wisburg-mcp-server-49abefc5`). Renaming one moves its directory under
  `~/.config/neige-calm/plugins` and rewrites the report CRDT cards and `report_sources` that name
  it, so each keeps its id until it is reinstalled under a word. The five are the closed
  `plugin_host::manifest::LEGACY_PLUGIN_IDS`, which the install route accepts besides a word, so
  the repository's `plugins/market`, `plugins/barra` and `plugins/paper-trading` still install on
  a fresh host; the list is never extended. Their minted names are already in the word alphabet. Recipe bodies are rewritten only for the built-in names; on 4140 no recipe
  names a minted external tool.
- A call to a minted name that two installed but not running plugins both mint answers the
  ambiguity error, naming both, before the scope and role checks (the disabled-plugin lookup in
  `dispatch_plugin_tools_call`), so it can reveal that a plugin is installed. Spawn-time refusal
  covers running plugins only, so this stays reachable, but only with a deliberately colliding id.
- Native plugin tools repeat their id's last word as the first word
  (`plugin_dev_neige_market_market_quote`, `plugin_dev_neige_barra_barra_series`). Dropping it would rename recipe-named tools (cut, Appendix B).

## Appendix A — Conformance table (proposal)

Persisted counts are calls stored in the Planner transcript's `$.item.tool` on the production 4140
database, read-only, 2026-10-04, while it was at migration 133 (`calm.` names). It has since
been deployed past 0134, so the same calls are stored under the `neige.` names of 0134. B0's migration respells every
`neige.<o>.<a>` as `neige_<o>_<a>`, and B1's migration maps the old words to the new ones. The
Current column gives today's dotted names; Proposed gives the new names without the `neige_`
prefix. Consumers: P prompts and templates, G goldens, F `fe`, T tests, R recipes.

| Current | Proposed | 4140 stored calls | Consumers |
|---|---|---|---|
| every kernel tool `neige.<o>.<a>`; `plugin.<id>_<tool>` | `neige_<o>_<a>`; `plugin_<id>_<tool>` | every stored call (count at B0 start) | P, G, F, T, R, the CLI derivation, `codex_appserver/tool_names.rs` |
| `task.report_success` / `task.report_failure` | `task_done` / `task_fail` | count at B0 start | P (Worker heads), templates, CLI `task report-success`, T |
| `plan.list` | `task_ls` | 204 | P (55 refs), G, F `PLAN_LIST_TOOL`, T |
| `plan.cancel` | `task_cancel` | 18 | P (`prompts/plan-cancel/`), T |
| `task.verdict {status}` | `task_accept` / `task_reject` (C1); the migration maps each stored call by its `status` argument | 110 | P, G, F `TASK_VERDICT_TOOL`, T |
| `terminal.observe` + flag `observe` | `terminal_read` + flag `read` | 18 | P (93 refs, guides/terminal), T |
| `terminal.resolve` | `terminal_show` | 8 | P, T |
| `report.kinds` | `report_describe` | 22 | P, G, F `REPORT_READ_TOOLS`, T |
| `report.backlinks` | `link_ls` | 2 | P, F `REPORT_READ_TOOLS`, T |
| `area.outline` | `area_ls` | 2 | P, T |
| `source.list` | `source_ls` | 8 | P, T |
| `track.state`, `neige track state` | `track_status`, `neige track status` | 0 (CLI) | P (21 CLI refs, 21 turn texts in `observation.rs`), CLI render, T |
| `calendar.create` / `list` / `update` | `calendar_add` / `ls` / `set` | 8 / 2 / 0 | P, builtin instructions, R (1 recipe: `calendar.list`), T |
| `calendar.update {cancelled: true}` | `calendar_rm {entry_id, expected_version}` | — | P, T |
| `preview.register` / `unregister` | `preview_add` / `preview_rm` | 2 / 2 | P, T |
| `workspace.reports` / `report` / `changes` / `edits` | `workspace_ls` / `cat` / `diff` / `log` | 0 (unreleased) | P, T |
| `neige tool list --after` | `neige tool ls --cursor` | 0 | P (`tool-discovery.md`), help, T |
| params: calendar `id`; preview `key` | `entry_id`; `preview_id` | params are not migrated | P, T |
| params: `after` (workspace ×3; edits takes an integer) | `cursor` (string); `workspace_log` `next_cursor` becomes a string | — | P, T, F wire types `ReportEditsPage` |
| param: calendar.list `until` | `to` | — | P, T |
| param: terminal `request_id` | `idempotency_key` | — | P, T |
| param: `ratify_request` `reason` (a question for a person) | `text`; the tool is retired by `user_ask` (#2209) | — | P, T |
| param: `report_read` `select` | `blocks` / `sections` / `detail: full\|index` | — | P (report guides), T |
| output: `time_zone` (workspace, creation identity) | `timezone` | unreleased (0136) | P, F wire types |
| output: `docRev`, `schemaVersion`, `taskDiagnostics` (report read/write/commit) | `doc_rev`, `schema_version`, `task_diagnostics` | — | P, refusal texts, T |
| output: `trackId`, `updatedAt` (`report_find`, `track ls area/reports/`) | `track_id`, `updated_at`; wrapped as `{reports: […]}` | — | P, CLI render, T |
| error: role refusal -32602 (`require_role`) | -32403 (done, B4) | — | T |
| error: report revision conflict -32001 | -32409 (data unchanged) (done, B4) | — | P, T |
| error: plugin disabled / not running -32002 | -32503 (done, B4) | — | T, `routes/plugins.rs` mapping |
| error: calendar store -32000 | -32602/-32404/-32409 by cause (done, B4) | — | T |
| error prefixes `plan_cancel:`, `track_report:`, `task_verdict:`, … | the tool name (done, B4) | — | T |
| 18 schemas without closed input; shared parsers ignore unknown keys | refuse unknown keys (§4) (done, B4) | — | T |
| forwarder exit 4 for transport failures | 3 (done, B4) | — | T, `1801` doc |
| forwarder exit 5 for a non-UTF-8 argument (`neige-cli/src/main.rs:70`) | 2 (done, B4) | — | T (`neige-cli/tests/forwarder.rs`), `1801` doc |

## Appendix B — Implementation slices (proposal)

Common gates per slice: `scripts/local-ratchet-gates.sh`; targeted
`env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server <filter> --test-threads 8`;
the whole `-p calm-server` run for tool or prompt changes; `scripts/local-rust-gates.sh --quick`;
the `fe` gate when `fe/` changes. Measure the Planner tool budget (29,458 / 30,000 B) and the
guide budget (7,489 / 7,500 B) before and after; trim wording, never raise a cap.

| # | Slice | Pain | Acceptance | Must go red first |
|---|---|---|---|---|
| B0 | Separator: every kernel tool `neige_<o>_<a>`, plugin prefix `plugin_`, `task_done` / `task_fail`; CLI split at `_`; the kernel-tool part of `tool_names.rs` goes (kernel names are no longer respelled); `source_capture` keeps resolving sanitized plugin spellings until B5; one migration (numbered last) respelling the stored `$.item.tool` values and recipe bodies (revision bump); the retired-name sweep rejects dotted kernel names (host callbacks excepted) | the model sees a different name than prompts and docs | `tools/list` names equal the prompt names byte for byte; CLI commands unchanged except `task done` / `task fail` | New `served_tool_names_use_the_word_alphabet` (kernel tools) and `served_tool_names_fit_the_codex_cap`; mutation: mint one kernel tool with `.` → red: the alphabet test, the grammar test and the sweep |
| B1a | View verbs: `task_ls`/`task_cancel`, `source_ls`, `area_ls`, `link_ls`, `report_describe`, `track_status`, `workspace_ls`/`cat`/`diff`/`log`, `tool ls`; its own migration (numbered last) maps B0's output names (`neige_plan_list` …), renaming the stored `$.item.tool` values and the 1 recipe (revision bump); its test runs the real chain 0134 → B0 → B1 on seeded rows | synonyms, noun actions | No retired view name in the registry, prompts or `fe`; the migration rewrites only the tool field | Retired-name sweep extended with the B1a names; mutation: register `neige_plan_list` again → red: the sweep, the golden and the direct-call tests using the literal |
| B1b | Terminal: `observe` → `read`, `resolve` → `show`, flag `observe` → `read`, `request_id` → `idempotency_key` | anchored read named two ways | Input still refuses without a prior read; approvals unchanged | `stale_observation` and "read first" refusal tests name `neige_terminal_read`; mutation: keep `observe` in the refusal text → red |
| B1c | Calendar and preview: `add/ls/set/rm`, `entry_id`, `to`, `preview_id`; `task_accept` / `task_reject` | CRUD words, effect in a flag | `calendar_set` no longer takes `cancelled`; `rm` stops wakes | Migration test seeds accepted and rejected `task.verdict` rows and a `calendar.list` row and the recipe, and reads back the new names; new `kernel_tool_actions_are_in_the_vocabulary`; mutation: drop one map row → red, and register `neige_task_verdict` again → red |
| B2 | Parameters and paging: `cursor` everywhere (string), `report_read` `blocks`/`sections`/`detail`, `ratify_request` `text` (retired by `user_ask`, #2209), outputs `timezone`, plus the parameter vocabulary test | `after` with two types; one selection spelled two ways | `workspace_log` pages with a string cursor; `fe` wire regenerated with `npm run gen:api` | New `kernel_tool_params_use_the_vocabulary`; mutation: rename `cursor` back to `after` on one tool → red |
| B3 | Results: snake_case report outputs; `{reports: […]}` with `track_id`/`updated_at` | `docRev` beside `updated_at` | CLI `--json` and MCP agree; refusal texts say `doc_rev` | New `report_tool_results_are_snake_case` over real `read`/`write`/`commit`/`find` calls (recursion skips `payload`); mutation: restore `docRev` → red |
| B4 | Errors and exits: transport-level unknown-key refusal for every kernel tool, codes per §5, tool-name prefixes, forwarder exits 3 (transport) and 2 (non-UTF-8 argument) | silent unknown keys; overloaded codes | Every kernel tool refuses `{"zz": 1}` with its valid keys | New `every_kernel_tool_refuses_unknown_arguments`; mutation: skip the check for one tool → red; existing `kernel_exit_codes_are_exactly_0_1_4` plus a forwarder twin |
| B5 | Plugin ids and minted names, descoped (Appendix C, item 10): the two built-in ids become words (`calendar`, `gitforge`) in the manifests, kernel references, `fe` and one migration (0148: `plugins` row copy with `plugin_kv`/`plugin_tokens`, `tracks.plugin_scope`, the `operations.idempotency_key` prefix, transcript `plugin_<id>_…` names respelled as minted, recipes); git-forge's tool names respelled `_` (`gh.pr.checks` → `gh_pr_checks`) with its `idem_key` literals unchanged; every plugin tool name minted in `[A-Za-z0-9_]` by one function for discovery, routing and recorded results (§6); colliding minted tools and ids refused; the install route takes only `[a-z0-9]{2,32}` for a new id; `source_capture` drops its sanitized-spelling fallback and keeps only stripping the `mcp__<server>__` qualifier | ids in three shapes; `-` and `.` rewritten by Codex but not by Claude | Every served plugin name passes `served_tool_names_use_the_word_alphabet`; a connector tool named `foo.bar` is served as `…_foo_bar` and called upstream as `foo.bar`; nothing on 4140 holds an old built-in id outside history | New `plugin_ids_are_words`; mutation: restore `dev.neige.calendar` as the built-in id → red; mutation: route by the raw tool name → the `foo.bar` dispatch test red |

**Approved scope (owner, 2026-10-04):** B0, B1 (B1a–B1c) and B2 first, then B5. Each slice that
renames stored names adds its own migration: a merged migration is frozen, and main deploys between
slices, so B1b and B1c cannot extend B1a's.
B0, B1 and B5 carry migrations (L2). B3 and B4 come later. Order: B0 → B1a → B1b → B1c → B2 → B5.
B0 is a mechanical respelling (≈ 1,360 occurrences of a dotted kernel name outside `docs/`, 804 of
them in `crates/calm-server/tests`). The other slices stay near 1k lines; B1a is the largest
(≈ 260 name occurrences in crates, fe, plugins and docs).

**Cut (recorded, no observed pain).** A cut item overrides the general rules above for that item
until it is taken up: `admin.*` → `track_gc`/`db_vacuum` (hidden, 0 stored
calls); `terminal_input` → `send`; `terminal_control` split;
`source_capture` → `add` (one recipe would need a rewrite); `{"ok": true}` → `{}`; `track_rename`'s
`{ok: false, refused}` → -32409 (deliberately not an error today); calendar entry field
`cancelled` (stored in 4 kv rows and served to `fe`); plugin tool word changes beyond the `_`
respelling (`gh.issue.comments`, `market.holdings.list`, `barra.series`, the repeated id word, …:
recipes name them); plugin `idem`/`attempt` → §4 names;
host-callback codes; `limit` on paged tools.

## Appendix C — Owner decisions (2026-10-04)

1. **`task.verdict` → `task_accept` / `task_reject`.** The effect is in the verb (principle 2), not in
   a `status` argument. `task_reject` takes the `reason`.
2. **`plan.*` → `task.*`:** `task_ls`, `task_cancel`. One object for one concept.
3. **Plugin ids: one format for every plugin,** including the two built-ins ("长痛不如短痛"):
   one word, per C9. Slice B5.
4. **Worker compounds:** `task.report_success` / `report_failure` (#2056) become `task_done` /
   `task_fail` (C9). No action is a compound.
5. **Paging:** `cursor` / `next_cursor`, an opaque string, everywhere.
6. **`terminal.observe` → `terminal_read`:** `read` always means the anchored read.
7. **Verbs follow Unix/git** ("类unix 风格, 大众认知高, 学习成本低").
8. **Scope:** B1 + B2, then B5; B3 and B4 later.
9. **`_` is the separator** ("按照_ 改"). The model never sees `.`: Codex rewrites it to `_` and
   the Claude and OpenAI APIs refuse it. With one-word segments and `_` between them, the name in
   prompts, docs and help is the name the model sees, and it still splits into the CLI command.
   The `neige_` prefix stays, so a kernel tool is recognizable in any text. Slice B0.
10. **B5 descope (2026-10-05).** Only the two built-in ids are renamed (`dev.neige.calendar` →
    `calendar`, `dev.neige.git-forge` → `gitforge`), and the kernel mints every plugin tool name in
    `[A-Za-z0-9_]`, so Codex and Claude show the model the same name. The five installed external
    plugins keep their ids: renaming them needs filesystem moves and a report-CRDT rewrite. New
    plugins must use one-word ids (§6, §9).
