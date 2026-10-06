# #2003 — One public naming system for the agent-facing CLI and MCP tools

Status: implemented (2026-10-04): #2015 (S1+S6), #2036 (S2+S3+0134), #2073 (S4), PR-4 (S5). Issue: #2003.
Related: #1801 (kernel-served CLI), #1668 / #1686 (Codex sanitized names), #1877 / #1883 (report
read anchors), #1893 (Planner tool byte budget).

## 0. Standing rules (owner) and decisions already made

1. **Do not expand without limit.** Fix the observed pain points first. Prefer the simplest design
   and the fewest mechanisms. A hypothetical case gets a one-line KNOWN GAP, not new mechanism.
2. **Compatibility covers only the production 4140 database**
   (`sqlite3 -readonly ~/.local/share/neige-next/data/calm.db`). There are no general compat
   windows and no old-client matrices. A persisted old name is counted with the command shown
   (§6.2). Anything that is not persisted is dropped without an alias.
3. **The agent-facing command surface is one consistent system:**
   - An option applies to every path of the same kind.
   - The CLI spelling mirrors the MCP name mechanically.
   - Both go through one authoritative handler and one shared renderer.
   - Errors list the valid choices.
4. **Visibility is never authorization.** The kernel owns lifecycle and authorization. Adapters own
   the mapping to client names.

**DECIDED (owner, 2026-10-04):**

- The public brand is `neige` everywhere on the public surface: the MCP server key, the kernel tool
  prefix and the CLI. The internal `calm-*` crates, modules and the `calm.db` file keep their names.
- There is one naming grammar: `neige.<object>.<action>`, applied mechanically to every kernel tool.
  The CLI spelling is derived from it (§4).
- The owner approved implementation directly after this design. §5 is the implementation plan.

## 1. Observed pain (from #2003, re-verified at `d37db34f2`)

| # | Pain | Evidence |
|---|---|---|
| P1 | One capability has 3 or 4 names, for example `neige task-completed`, `calm.task.complete`, `mcp__calm__calm_task_complete` and the alias `calm.task_completed`. | cli/commands.rs:189, tools/emit.rs:21,28 |
| P2 | The vocabulary has no grammar. CLI names may have no object (`state`, `tag`), use object-verb (`track-close`) or use past tense (`task-completed`). MCP names have 2 or 3 levels and use `_` inside segments (`admin.track_gc`, `report.blocks.kinds`, `track.cat_at`). | cli/commands.rs:79-259; golden |
| P3 | The split between CLI and MCP has no stated rule. "CLI reads, MCP writes" is false: the CLI also tags, completes tasks, closes tracks and runs gc/vacuum. | cli/commands.rs:173-258 |
| P4 | `neige tools` lists only `tools/list`-visible names. It cannot show the CLI-covered hidden tools. | cli/catalog.rs:332, transport/catalog.rs:27 |
| P5 | `neige cat report.md` and `calm.report.read` look alike but anchor differently. | track_file.rs:300, track_report.rs:164 |
| P6 | The Codex name reversal is string-guessing with an unverified cap. | transport/plugin_tool_names.rs:96-127 |

Paths are under `crates/calm-server/src/mcp_server/` unless they are rooted. "golden" means
`crates/calm-server/tests/goldens/mcp_tool_registry.json`.

## 2. Fact table (A)

### 2.1 Registry mechanics

- `ToolRegistry` is a name → (descriptor, handler) map (registry.rs:298). `register_default_tools`
  at tools/mod.rs:27 is the only population entry point. Built-in plugin natives are added at
  `builtin_plugins/mod.rs:129`. Each native is wrapped in `require_bound` (:146), which checks
  that the plugin is running, trusted and in the track's scope.
- `visible_to_roles` filters only `tools/list` (registry.rs:264, 330). `tools/call` routes by name
  regardless of it. The path is transport.rs:493 → transport/call.rs:8-19 → handler. The handler's
  `require_role` (registry.rs:108) or `require_role_any` (:123) is a UX gate. The real boundary is
  `role_gate::enforce_role` inside every eventized write (registry.rs:105-107).
- `neige/cli` (transport.rs:373 → cli/mod.rs:78) parses argv with the COMMANDS table
  (cli/commands.rs:79). It then calls the **same** `call_registered_tool` (cli/mod.rs:129) and
  renders through `cli/render.rs`. There is no second handler.
- Plugin manifest tools are minted as `plugin.<id>_<tool>` (transport.rs:436-466). Their inverse
  is `plugin_tool_route` (:660). They are listed only to `PLUGIN_TOOL_ROLES`.
- Aliases come from `register_deprecated_alias` (registry.rs:353). They are hidden, log a warning
  and delegate to the real handler.

### 2.2 Every registered tool (45 golden rows at `d37db34f2`)

Columns:

- **L** — listed in `tools/list`: P = planner, A = assistant, W = worker, — = hidden.
- **CLI** — the current spelling, if any.
- **Roles** — the roles the handler admits at call time.
- **Scope** — what the call can reach.
- **Effect** — V = view-read, AR = anchor-read, W = write, LC = lifecycle, M = maintenance.

| MCP raw name | L | CLI | Roles (handler) | Scope | Effect | Source |
|---|---|---|---|---|---|---|
| calm.admin.track_gc | — | `track-gc` (+`--force` unless `--dry-run`) | Planner (admin.rs:79) | own bound track (admin.rs:86-95) | M | admin.rs:44 |
| calm.admin.vacuum | — | `vacuum` (+`--force`) | Planner (admin.rs:139) | whole DB | M | admin.rs:64 |
| calm.area.outline | P | — | Planner (report_links.rs:72) | area | V | report_links.rs:45 |
| calm.calendar.create | P,A | — | Planner, Assistant (calendar/tools.rs:15) after require_bound | own track, plugin `dev.neige.calendar` | W | builtin_plugins/calendar/tools.rs:55 |
| calm.calendar.list | P,A | — | same | same | V | same |
| calm.calendar.update | P,A | — | same | same | W | same |
| calm.dispatch_request | — | — | Planner (emit.rs:77) | none | retired shim (#644) | emit.rs:50 |
| calm.plan.cancel | P | — | Planner (plan.rs:455) | own track | LC | plan.rs:420 |
| calm.plan.list | P | — | Planner (plan.rs:627) | own track | V | plan.rs:608 |
| calm.plan.upsert | — | — | Planner (plan.rs:407) | none | retired shim (#985) | plan.rs:329 |
| calm.preview.register | P | — | Planner (preview.rs:101) | own track preview key | W | preview.rs:54 |
| calm.preview.unregister | P | — | Planner | same | W | preview.rs:85 |
| calm.ratify.request | P | — | Planner (review.rs:66) | own track | LC | tools/review.rs:40 |
| calm.report.blocks.kinds | P,A | — | Planner, Assistant (track_report_blocks.rs:83) | static | V | track_report_blocks/contracts.rs:13 |
| calm.report.commit | P,A | — | Planner, Assistant (:168) | own report | W, needs anchors | contracts.rs:471 |
| calm.report.find | — | `find <area/reports/> -name/-tag` | Planner (area_reports.rs:156-157) | own area | V | area_reports.rs:45 |
| calm.report.links.backlinks | P | — | Planner (report_links.rs:288) | area → own report | V | report_links.rs:57 |
| calm.report.read | P | — | Planner, Assistant (track_report.rs:127) | own report | AR (index: V) | track_report.rs:81 |
| calm.report.tag | — | `tag <report.md> --add/--remove` | Worker reads; add/remove is Planner only (report_tag.rs:77,100) | own report | V / W | report_tag.rs:53 |
| calm.report.write_markdown | P,A | — | Planner, Assistant (:92) | own report | W, needs a whole anchor | contracts.rs:407 |
| calm.review.round | P | — | Planner (dev/review.rs:105) after require_bound | git-forge-bound track | W | builtin_plugins/dev/review.rs:48 |
| calm.source.capture | P | — | Planner (source.rs:135) | own track | W | source.rs:65 |
| calm.source.list | P | — | Planner (source.rs:540) | own track | V | source.rs:109 |
| calm.task.complete | W | `task-completed --attempt-id --result --artifact…` | Worker (emit.rs:114) | own session/attempt | LC | emit.rs:90 |
| calm.task.fail | W | `task-failed --attempt-id --reason` | Worker (emit.rs:199) | own session/attempt | LC | emit.rs:176 |
| calm.task.verdict | P | — | Planner (track_state.rs:161) | own track attempts | LC | track_state.rs:137 |
| calm.terminal.control | P | — | Planner (terminal.rs:305) | own track terminals | W | terminal.rs:44 |
| calm.terminal.input | P | — | Planner | same | W | terminal.rs:50 |
| calm.terminal.observe | P | — | Planner | same | V | terminal.rs:38 |
| calm.terminal.open | P | — | Planner | same | W | terminal.rs:32 |
| calm.terminal.resolve | P | — | Planner | same | V | terminal.rs:26 |
| calm.track.cat | — | `cat <path> [--blocks/--sections]` | Planner, Worker (track_file.rs:122); area reports are Planner only | own track, own report, area reports | V (no anchor) | track_file.rs:66 |
| calm.track.cat_at | — | `cat-at <commit> <path>` | Planner, Worker (track_history.rs:145) | own track VCS | V | track_history.rs:69 |
| calm.track.close | P | `track-close --message` | Planner (track_state.rs:239) | own track | LC | track_state.rs:217 |
| calm.track.diff | — | `diff <from> [to] [path]` (+`--to`/`--path`) | Planner, Worker (:110) | own track VCS | V | track_history.rs:49 |
| calm.track.log | — | `log [path] --limit --include-empty` | Planner, Worker (:166) | own track VCS | V | track_history.rs:88 |
| calm.track.ls | — | `ls [path] [-l]` | Planner, Worker (track_file.rs:99) | own track; `area/reports/` is Planner only | V | track_file.rs:49 |
| calm.track.publish | P | — | Planner (dev/publish.rs:274) after require_bound | git-forge-bound attached track | LC | builtin_plugins/dev/publish.rs:57 |
| calm.track.rename | P | — | Planner (track_rename.rs:79) | own track, empty title only | W | track_rename.rs:44 |
| calm.track.state | — | `state` | Planner, Worker (track_state.rs:68) | own track | V | track_state.rs:50 |
| calm.user.notify | P | — | Planner (user_notify.rs:88) | own track → user | W | user_notify.rs:39 |
| calm.get_track_state | — | — | alias → track.state | — | alias | track_state.rs:29 |
| calm.update_task_meta | — | — | alias → task.verdict | — | alias | track_state.rs:30 |
| calm.task_completed | — | — | alias → task.complete | — | alias | emit.rs:28 |
| calm.task_failed | — | — | alias → task.fail | — | alias | emit.rs:29 |

The meta command `neige tools names|describe` (cli/catalog.rs:171) and `neige help`
(cli/help.rs) are CLI-only. They are not tools.

Observations:

- The 11 hidden non-alias tools are exactly the CLI-covered views and the maintenance tools, plus
  the two retired shims. Hiding them is a byte-budget choice (#1893), not a permission.
- The `calm` MCP server key is written in these places:
  - `shared_codex_home.rs:17` (`EXPECTED_MCP_SERVERS`) and `:321` (`[mcp_servers.calm]`)
  - `wiring.rs:53` (the Planner's per-thread terminal approvals)
  - `claude_planner/spawn.rs:37-50` (the `mcpServers.calm` config) and `:76` (the allowed-tools
    entry `mcp__calm`)
  - `claude_planner/translate.rs:28` (`mcp__calm__`) and `:442` (`"server":"calm"`)
  - `claude_planner/driver.rs:139` (the `server.name == "calm"` check)

### 2.3 Where the names appear (sweep at `d37db34f2`)

Pattern: `calm\.(track|report|task|plan|terminal|source|admin|area|calendar|preview|ratify|review|user|…aliases)`.

| Place | Files / occurrences | Notes |
|---|---|---|
| `crates/calm-server/prompts/**` | 26 / 64 | `prompts/tools/<name>.md`: 41 stems, one per non-alias tool, pinned by tools/mod.rs:240. CLI spellings `neige <cmd>`: 19 files / 50 (cat 20, state 12, ls 6, find 4, tag 3, tools 2, others 1). worker/head-cli.md and head-mcp.md differ only in lines 7-9. |
| Rust, non-test (all crates) | 69 / 295 | Mostly tools/*.rs, planner_card.rs, wiring.rs, cli/render/listing.rs, calm-types/src/observation.rs, calm-truth/src/track_fs_view/mod.rs:1442. |
| Rust tests (src + tests/) | 96 / 749 | Heaviest: terminal_wait_and_drift.rs 60, mcp_assistant_tool_gate.rs 52, mcp_tools_list_role_filter.rs 45. 12 test files hold `mcp__calm`, including 14 `tests/fixtures/claude_planner_stream/*.ndjson`. |
| Goldens | mcp_tool_registry.json 47; issue_development_planner_prompt.txt 8; assistant_prompt*.txt 6+6; worker_prompt_mcp.txt 3; worker_prompt_cli.txt 1 | Regenerate with `REGEN_MCP_TOOL_REGISTRY_GOLDEN=1`, `REGEN_PROMPT_GOLDENS=1` and `REGEN_PLANNER_PROMPT_GOLDEN=1`. |
| Frontend `fe/` (there is no root `web/` tree) | 12 / 39 | `fe/core/keys/mcp-tools.ts:4-37` classifies writes and reads by tool name. `fe/core/domain/conversation.ts:784,993-1000` treats `calm.user.notify` as speech and the `calm.track.*` prefix as looks. Two tests use `server:'calm'`. |
| Plugins | 2 / 5 | `plugins/git-forge/manifest.json:7` description; `plugins/paper-trading/spy-recipe.md:5,12,13`. Plugin tools themselves are `plugin.<id>_<tool>`, max 45 bytes today, no sanitize collisions (32 tools). |
| Built-in text | builtin_plugins/{calendar,dev}/instructions.md; templates/builtin/{investment-research,issue-development}.md; calm-types/src/observation/task-acceptance.md | |
| docs/ | 36 / 372 (`neige <cmd>`: 11 / 36) | Living docs: `docs/using-neige-calm.md` (shipped by `scripts/release/build-alpha.sh:102`). Design records are history; they are not rewritten. |
| e2e/ | test_planner_claude_ux.py 64, planner_claude_ux.py 11, planner_claude_ux_metrics.py 11 | |
| scripts/, .github/ | 0 | |
| Released migrations | 4 SQL files (calm-truth 0109, 0114, 0116, 0128) | Byte-frozen. They are not touched. |

## 3. Codex name mapping, as deployed (B)

### 3.1 Which Codex production runs

- `neige-next.service` loads `EnvironmentFile=~/.local/share/neige-next/env`. That file sets
  `CALM_CODEX_BIN=/home/kenji/.codex/packages/standalone/current/bin/codex`. Isolated Workers use
  `isolated-codex.json` `codex_binary`, which is the same path.
- `current` is a symlink to `releases/0.159.2-x86_64-unknown-linux-musl`. Running
  `… --version` prints `codex-cli 0.159.2`. A live production `codex` process resolves to that
  release (checked with `/proc/<pid>/exe`).
- The production `data/codex-home/config.toml` has `[mcp_servers.calm]` → `neige-mcp-stdio-shim`.
  It has no `[features]` table, so `non_prefixed_mcp_tool_names` is off (default false). That
  means prefix mode applies (Codex `core/src/config/mod.rs:1444`).
- `external/codex` is at `5a440c0` (2026-06-07). It is **not** the deployed source. Its
  `codex-mcp/src/tools.rs:261` has `MAX_TOOL_NAME_LENGTH = 64`. The deployed 0.159.2 behaves
  differently, as measured below.

### 3.2 Smallest reproduction (black box: the deployed binary, mock endpoint, no model)

The scratch files are in the session scratchpad `…/scratchpad/d2003/`, not in the repo:

- `fake_mcp.py` is a stdio MCP server with fixed raw names.
- `mock_responses.py` is a localhost `/v1/responses` server. It records the request body and
  answers 400.
- `codex_home/config.toml` is a throwaway CODEX_HOME. Its `model_provider` points at the mock.
- `run_probe.sh` runs `codex exec` under `env -i` with no credentials. No production state and no
  real model is involved.
- `keyrepro/` compiles the **real** `transport/plugin_tool_names.rs` through `#[path]`, with only
  its `super::*` imports stubbed. It feeds each captured callable name, bare and as
  `mcp__calm__<name>`, to `model_tool_key`, matched against the raw list.

Actual output (abridged; annotations in parentheses are mine; verbatim output is `keyrepro.out` in the scratchpad):

```
codex-cli 0.159.2
namespace=mcp__calm
 18  calm_task_complete                         ok -> calm.task.complete
 29  mcp__calm__calm_task_complete              ok -> calm.task.complete
 34  plugin_dev_neige_git_forge_wf_tool         ok -> plugin.dev.neige.git-forge_wf.tool
 72  plugin_dev_neige_git_forge_…(72, name abridged)   ok -> …
100  plugin_dev_neige_git_forge_xxx…(100)       ok -> …
117  plugin_dev_neige_git_forge_xxx…(117, raw 117 bytes)   ok -> …
117  plugin_dev_neige_git_forge_xxx…_013580773a32   (raw 118..200 bytes, 7 names)   MISS (unknown tool)
128  mcp__calm__plugin_dev_neige_git_forge_xxx…_013580773a32                          MISS (unknown tool)
 29  plugin_dev_x_y_t_7a37e287c14a   (raw plugin.dev.x-y_t)   MISS (unknown tool)
 29  plugin_dev_x_y_t_ec721b457ce9   (raw plugin.dev.x.y_t)   MISS (unknown tool)
```

### 3.2a What the deployed binary does

- It sends one Responses `namespace` tool named `mcp__<server key>`. The functions inside it
  are the raw names with every character outside `[A-Za-z0-9_]` replaced by `_`.
- The cap is **128 bytes for `mcp__<server>__<name>`**, so a callable name gets 117 bytes under
  `mcp__calm`.
  - A longer name is truncated to 104 bytes, then `_` and 12 hex characters of SHA-1 are added.
  - Two raw names that sanitize alike **both** get a hash suffix.
- The existing comment's "128-char cap" (plugin_tool_names.rs:97) is right for the deployed
  binary. The issue's 64-byte figure comes from the stale `external/codex`.
- On `tools/call`, Codex sends the **raw** `tool.name` back to the server (`external/codex …
  tools.rs:50` "Raw MCP tool definition; `tool.name` is sent back"). Normal kernel and plugin
  calls therefore never depend on our reversal.
- The only kernel input that accepts a model spelling is `source.capture`'s `call.tool`
  (source.rs:273-325). Exact raw names win there. If there is no exact match, the
  `model_tool_key` match must be unique.

**Conclusion:**

- The reversal fails for hashed names (longer than 117 bytes, or sanitize collisions).
- It fails **explicitly**: the error `UNKNOWN_TOOL_NAME` lists the recorded raw names. It never
  routes to the wrong tool.
- No production tool triggers it today. The longest plugin tool is 45 bytes, and there are no
  collisions. So this stays a KNOWN GAP pinned by a test (S6), not new mechanism.

### 3.3 Claude adapter

- `claude_planner/translate.rs:36-56` builds an **explicit** map from the card's visible
  `tools/list` names to Claude's spelling (`[A-Za-z0-9_-]`, :59). It restores a name only when
  exactly one tool has that spelling. Otherwise it keeps the raw name and logs a warning.
- That is the pattern we want: a map built from the actual list, not a reverse parse.
- Kernel names under the grammar contain neither `-` nor `_`, so the Claude and Codex spellings are
  identical.

## 4. Proposal (C, D)

### 4.1 Report reads: view vs index vs anchored text (contract)

The concurrency contract is unchanged. The rename only renames it, and help and prompts state it
in one sentence.

| Read | Returns text | Records an anchor (`ReadLedger`) | Enables |
|---|---|---|---|
| `neige track cat report.md` / `neige.track.cat` (also with `--blocks`/`--sections`) | yes | **no**: "A view only: it anchors no report write" (track_file.rs:300-301) | nothing |
| `neige.report.read {select:"index"}` | no (ids and headings) | **no** (track_report.rs:152; test `select_index_returns_anchors_without_text`, mcp_track_report.rs:1442) | nothing |
| `neige.report.read` full (with or without markers) | yes | the whole doc at its docRev (track_report.rs:160-174) | `report.write` (needs whole) and every `report.commit` op |
| `neige.report.read {select:{blocks|sections}}` | yes, the selection | exactly the rendered ids | `report.commit` ops on those blocks/sections only |
| own `neige.report.write` / `neige.report.commit` | — | the post-write doc as a whole read / only what the commit authored (report_read_ledger.rs:1-5) | the next write without a re-read |

- The ledger lives in process memory with a 2 h TTL. A kernel restart forgets it, and the next
  write fails closed (report_read_ledger.rs:6-9).
- These refusals name the read to make (track_report_blocks.rs:113, anchors.rs:61,
  track_report/sections.rs:46-48). They stay as they are, with the new names.
- **Contract:** a view never anchors. Only a `report.read` that returns text anchors, and it
  anchors exactly what it rendered. Every report write needs an anchor. Its refusal names the
  `report.read` call to make.
- Help keeps the line at cli/help.rs:89-90, reworded as "`track cat` is a view; a report write
  needs `neige.report.read` first."
- Regression tests that must stay green: `a_cat_read_anchors_no_commit`
  (tests/cases/mcp_report_sections.rs:493), `select_index_returns_anchors_without_text`, and the
  write-refusal cases in mcp_track_report.rs.

### 4.2 Grammar (DECIDED)

#2053 extends actions to underscore-separated lowercase words for
`neige.task.report_success` / `neige.task.report_failure`. Objects remain one
word and names retain exactly three segments. The existing flat CLI currently
spells these `task-report-success` and `task-report-failure`; the global
hierarchical CLI work is separate.

```
kernel tool  := "neige." object "." action        object ∈ [a-z]+; action ∈ [a-z]+(_[a-z]+)*
CLI command  := "neige" SP object SP action        (action "_" → "-")
CLI option   := "--" key with "_" → "-"            (the schema key, mechanically)
plugin tool  := "plugin." <plugin-id> "_" <tool>   (unchanged; plugin-owned identity, §4.6)
```

- The object is the thing acted on (track, report, task, plan, terminal, …). The action is the
  operation or view, as the tool already names it. Imperative verbs stay as they are.
- Past-tense CLI forms disappear because the CLI is derived from the tool name.
- Objects contain no underscores, so sanitizing (`.`→`_`) remains **injective and reversible**
  for kernel tools: the first two underscores delimit the prefix and object; the remainder
  is the action. The longest name is `neige.task.report_failure` (25 B), well under the 116 B that Codex
  allows under `mcp__neige`. Codex therefore never hashes a kernel tool, and the Codex and Claude
  callable ids are both `neige_<object>_<action>`. A test pins this (S6).
- Rejected alternative (one line): dropping the tool prefix (raw `task.complete` under server
  `neige`) would avoid the doubled `mcp__neige__neige_…`. The owner chose the `neige.` prefix, and
  6 bytes per call do not matter.

### 4.3 Rename table (old → new), every tool

Renames beyond the brand happen **only** where the old name breaks the structure: a third
segment, `_` inside an object, or no object. #2053 separately changes the outcome verbs
to explicit report actions.

| Old | New | CLI (derived) |
|---|---|---|
| calm.admin.track_gc | neige.admin.gc | `neige admin gc --track-id … [--keep] [--dry-run] --force` |
| calm.admin.vacuum | neige.admin.vacuum | `neige admin vacuum --force` |
| calm.area.outline | neige.area.outline | — |
| calm.calendar.create / list / update | neige.calendar.create / list / update | — |
| calm.plan.cancel / list | neige.plan.cancel / list | — |
| calm.preview.register / unregister | neige.preview.register / unregister | — |
| calm.ratify.request | neige.ratify.request | — |
| calm.report.blocks.kinds | neige.report.kinds | — |
| calm.report.commit | neige.report.commit | — |
| calm.report.find | neige.report.find | `neige report find <area/reports/> [--name] [--tag]` |
| calm.report.links.backlinks | neige.report.backlinks | — |
| calm.report.read | neige.report.read | — |
| calm.report.tag | neige.report.tag | `neige report tag <report.md> [--add …] [--remove …]` |
| calm.report.write_markdown | neige.report.write | — |
| calm.review.round | removed upstream by #2017 | — |
| calm.source.capture / list | neige.source.capture / list | — |
| calm.task.complete | neige.task.report_success | `neige task report-success --attempt-id … [--result …] [--artifacts …]` |
| calm.task.fail | neige.task.report_failure | `neige task report-failure --attempt-id … --reason …` |
| calm.task.verdict | neige.task.verdict | — |
| calm.terminal.control / input / observe / open / resolve | neige.terminal.* | — |
| calm.track.cat | neige.track.cat | `neige track cat <path> [--blocks] [--sections]` |
| calm.track.cat_at | neige.track.show | `neige track show <commit> <path>` |
| calm.track.close | neige.track.close | `neige track close --message …` |
| calm.track.diff | neige.track.diff | `neige track diff <from> [to] [path]` |
| calm.track.log | neige.track.log | `neige track log [path] [--limit] [--include-empty]` |
| calm.track.ls | neige.track.ls | `neige track ls [path] [-l]` |
| calm.track.publish / rename | neige.track.publish / rename | — |
| calm.track.state | neige.track.status | `neige track status` |
| calm.user.notify | neige.user.notify | — |
| calm.dispatch_request, calm.plan.upsert | **removed** (retired shims) | — |
| calm.get_track_state, calm.update_task_meta, calm.task_completed, calm.task_failed | **removed** (aliases) | — |
| MCP server key `calm` | `neige` | Codex sees `mcp__neige`; Claude sees `mcp__neige__neige_…` |
| `neige tools names|describe` | `neige tool ls|describe` | CLI-only meta command, like `help` |

The result is 38 tools (39 minus `calm.review.round`, which #2017 removed), all of the form `neige.[a-z]+.[a-z]+(_[a-z]+)*`, plus the plugin manifest tools.

### 4.4 What the CLI is for, and what MCP is for

- **MCP is the complete surface.** Every operation is a registered tool. The CLI never has an
  operation that MCP lacks.
- **Listed or hidden in `tools/list`** is a context-budget choice (#1893). A tool is hidden when
  shell use serves it better. It is never a permission (rule 4).
- **The CLI is an argv front-end** over `call_registered_tool` and the shared renderer. A tool gets
  a CLI command, through a row in `COMMANDS`, only for one of three reasons:
  1. a shell-native view (`track ls/cat/show/diff/log/status`, `report find/tag`);
  2. a Worker report in CLI mode (`task report-success/report-failure`);
  3. lifecycle or maintenance that needs a `--force` confirm (`track close`, `admin gc/vacuum`).
- **Mechanics (rule 3):**
  - `Command` loses its `name` field. The spelling is computed from `tool`.
  - Every option is `--<key>`. Single-dash options are only render views that never reach the tool
    (`-l`).
  - Every positional key is also accepted as its `--<key>` option, because diff already does this.
  - Unknown object, action or option errors list the valid choices.
  - `--json` and `--force` are global.

### 4.5 Discovery

- `neige tool ls (--prefix P | --all) [--after N]` returns rows `{name, cli, listed}`.
  - The rows are the session's `tools/list` set **plus** every CLI-covered tool.
  - `cli` is the derived command or null. `listed` says whether `tools/list` shows the tool to
    this session.
- `neige tool describe --name N` returns the MCP descriptor plus `cli` and `listed`.
- The output footer states: "listing is not a grant; the tool's role gate decides."
- Discovery does **not** print a client callable id. The client mints it (Codex) or the adapter
  maps it (Claude), and the kernel never receives it on `tools/call` (§3.2a). The doc and help
  give the one rule that applies to kernel tools: the callable id is the raw name with `.`→`_`.

### 4.6 Who owns callable-id ↔ raw-name mapping

- **Kernel:** it owns raw names only. `tools/call` resolves exact raw names (transport.rs:493-505).
- **Adapters:**
  - The Claude adapter keeps its explicit map (§3.3).
  - The Codex spelling helpers `codex_sanitized` / `model_tool_key` move from the generic
    `mcp_server/transport/plugin_tool_names.rs` into a Codex-adapter-owned module
    (`crate::codex_appserver::tool_names`). Their comment states the measured 0.159.2 rule.
  - `source.capture` keeps its resolution, which is an explicit map built from the recorded-tool
    list (source.rs:273): an exact match, else a unique key, else an explicit error with the raw
    choices.
  - There is no new reverse parser. The Codex hash cannot be inverted without copying Codex
    internals, so a hashed name stays an explicit failure (KNOWN GAP K1).
- **Plugin names** keep `plugin.<id>_<tool>`. Plugin ids exclude `_` (transport.rs:455), and
  routing uses exact `(id, tool)` lookup (transport.rs:734). Renaming them gives no observed
  benefit and touches routing.

## 5. Slices (E) — ordered, each about 1k lines or less

Every slice has these common gates:

- `scripts/local-ratchet-gates.sh`
- `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server <filter> --test-threads 8`
- For tool, prompt or SQL-reading changes, the **whole** `-p calm-server` run, because the
  source-scan invariant suites need it.
- `scripts/local-rust-gates.sh --quick` (OpenAPI and clippy).
- `(cd fe && npm ci && npm run lint && npm run build && npm test)` when `fe/` changes.

| # | Slice | Files (main) | Acceptance | Must go red first (prove by single-factor mutation) |
|---|---|---|---|---|
| S1 | Remove the 4 aliases, the 2 retired shims and `register_deprecated_alias`. An unknown tool name lists the session's tools. | tools/emit.rs, tools/plan.rs, tools/track_state.rs, registry.rs (alias fn, `deprecated_aliases`, its tests), tools/mod.rs:240-246 (drop the "expected to carry aliases" assertion), prompts/tools/calm.{dispatch_request,plan.upsert}.md deleted, transport.rs:543 `unknown_tool` message, golden regen, tests that call aliases or shims | Golden has 39 rows. `tools/call calm.task_completed` → -32601 whose message lists the visible names. | New `unknown_tool_error_lists_the_sessions_tools` (tests/cases/mcp_tools_list_role_filter.rs). Mutation: drop the list from the message → only this test is red. |
| S2a | Brand and grammar: `terminal.*` | tools/terminal.rs (+schema_tests), wiring.rs:53 approvals, prompts/tools/calm.terminal.*.md → `git mv` neige.terminal.*.md, prompts/guides/terminal.md, terminal test cases (≈180 lines), goldens | `tools/list` (Planner) shows `neige.terminal.*`. Approvals still skip prompts for open, control and input. | The stale-name sweep invariant (S2d) goes red if one prompt, observation or refusal keeps `calm.terminal.open`. Verify by reverting one line in `prompts/guides/terminal.md`. |
| S2b | `report.*` and `area.*` (write_markdown→write, blocks.kinds→kinds, links.backlinks→backlinks) | track_report*.rs, report_links.rs, area_reports.rs, report_tag.rs, refusal texts (track_report_blocks.rs:113, anchors.rs:61, sections.rs:46), cli/help.rs:83-90, prompts/tools + guides/report.md, report.rs, report_read_ledger.rs docs, fe/core/keys/mcp-tools.ts (report constants), tests | The anchor contract (§4.1) is unchanged. The refusals name `neige.report.read`. | `a_cat_read_anchors_no_commit` stays green. The refusal-text assertions in mcp_track_report.rs go red if the message keeps `calm.report.read`. |
| S2c | `track.*` (cat_at→show) | track_file.rs, track_history.rs, track_state.rs, track_rename.rs, dev/publish.rs, calm-truth track_fs_view/mod.rs:1442, cli/render/listing.rs, prompts, fe TRACK_* constants, the §6.3 migration + its test, tests | Planner and Worker view tools are renamed. CLI rows point at the new tools (the old CLI spelling stays until S4). | `track_history_drill_ins_are_hidden_but_registered` (registry.rs:675) uses the constants and stays green. A migration test seeds `"tool":"calm.track.cat_at"` and `calm.user.notify` transcript rows and asserts they read back as `neige.track.show` / `neige.user.notify`; it goes red if the migration's rename table drops a row. |
| S2d | task, plan, source, admin (track_gc→gc), calendar, preview, ratify, review, user. Add the grammar invariant and the stale-name sweep invariant (below). | emit.rs, plan.rs, source.rs, admin.rs, calendar/tools.rs:55, preview.rs, review.rs, dev/review.rs, user_notify.rs, worker/head-{cli,mcp}.md, observation/task-acceptance.md, builtin instructions and templates, plugins/git-forge/manifest.json:7, plugins/paper-trading/spy-recipe.md, fe constants, tests, goldens | No `calm\.` token is left in prompts, registry or fe constants. | New `kernel_tool_names_follow_the_grammar` (tools/mod.rs): every non-`plugin.` registry name matches `^neige\.[a-z]+\.[a-z]+(?:_[a-z]+)*$`. Mutation: rename one constant back to `calm.track.cat_at` → red: this test, the golden, the stale-name sweep and every direct-call test using that literal (list them by name before mutating). |
| S3 | MCP server key `calm` → `neige` | shared_codex_home.rs:17,321 (+tests/cases/shared_codex_home.rs), wiring.rs:53, claude_planner/{spawn.rs:37-76, translate.rs:28,442, driver.rs:139, catalog_fetch.rs}, neige-mcp-stdio-shim (2 refs), claude_planner tests + 14 ndjson fixtures, fe tests `server:'calm'` | On boot, an existing `[mcp_servers.calm]` is removed (`sanitize_unexpected_mcp_servers`, state.rs:1071) and `[mcp_servers.neige]` is written (state.rs:1134). The Claude Planner connects to `neige`. | New `boot_replaces_a_stale_calm_server_key` (tests/cases/shared_codex_home.rs): seed a home with `[mcp_servers.calm]`, boot, assert only `neige` remains. Mutation: leave `calm` in `EXPECTED_MCP_SERVERS` → red. |
| S4 | Mechanical CLI: two-word commands, `--key` options, positional keys also accepted as options, error choices, `tool list|describe` with `{name, cli, listed}` | cli/commands.rs (drop `name`, derive it, two-token parse), cli/help.rs (root, object and command help), cli/catalog.rs (rows + CLI-covered tools), cli/render.rs (keyed by tool, unchanged), prompts `neige <cmd>` (≈150 lines), worker/head-cli.md, goldens worker_prompt_cli.txt and the planner and assistant goldens, cli/commands/tests.rs, tests/cases/neige_cli_*.rs | `neige track cat report.md` works. `neige cat` → usage error listing the objects. `neige tool list --all` as a Planner shows `neige.track.cat` with `cli: "neige track cat"` and `listed: false`. | New `every_option_is_its_schema_key` (commands/tests.rs): every non-View `Opt.flag == "--" + key.replace('_','-')` and every key exists in the tool's `input_schema`. Mutation: restore `--artifact` → red. New `tool_list_includes_cli_covered_hidden_tools`. |
| S5 | Living docs and e2e scripts | docs/using-neige-calm.md, docs/architecture/1801-kernel-served-cli.md (pointer note only), e2e/{test_,}planner_claude_ux*.py, docker/Dockerfile.server:9 | `git grep -nE 'calm\.(track\|report\|task)\|neige (ls\|cat\|state)\b' -- docs/using-neige-calm.md e2e` is empty. | — (text only; ratchet gates) |
| S6 | Codex adapter ownership + measured regression | move `codex_sanitized`/`model_tool_key` → `crate::codex_appserver::tool_names` (callers: source.rs:15, plugin_tool_names.rs:172), fix the comment to the 0.159.2 rule, add unit tests with the literal names captured in §3.2 | Bare and `mcp__neige__`-qualified names resolve. Hashed and colliding names → `source.capture` error `UNKNOWN_TOOL_NAME` listing raw names. Every kernel tool sanitizes injectively to ≤ 116 B. | New `hashed_codex_callables_fail_explicitly` + `kernel_tool_callables_are_injective_and_unhashed`. Mutation: make the key strip a trailing `_[0-9a-f]{12}` → the first test is red. |

**Stale-name sweep invariant (PR-2, replaces hand-made file lists).** A test
`no_retired_tool_names_remain` scans every tracked file under `crates/`, `fe/` and `plugins/` and
`docs/using-neige-calm.md` (there is no root `templates/`; the built-in templates live under
`crates/calm-server/templates/`) (its tool-name lines therefore move into PR-2; PR-4 keeps the CLI-spelling and e2e text) for
`\bcalm\.(admin|area|calendar|dispatch_request|get_track_state|plan|preview|ratify|report|review|source|task|task_completed|task_failed|terminal|track|update_task_meta|user)\b`
and for `mcp__calm(?:__|\b)`. The allowlist is closed: released migrations, the §6.3 migration and its
test, and this document. A deliberate retired-name rejection input (S1's -32601 test) is exempt
per line, by a trailing `// retired-name: rejection input` marker; a whole file is never exempt. The test catches the runtime guidance that the §2.3 sweep missed, for
example `harness/run_loop.rs:2682`, `calm-types/src/observation.rs:295`, `track_report_guard.rs:15`,
`tools/write_args.rs:9` and `track_activity/sql.rs:29`. The `planner_card` prompt-token scanner then
accepts only `neige.`.

**Must-red sets** are predicted over the full `-p calm-server` run, not one test. A production
constant that is reverted to an old name also turns red the direct-call tests that use the literal
name (for example `tests/cases/neige_cli_commands.rs:98`). The prediction lists them.

Order: S1 → S2a → S2b → S2c → S2d → S3 → S4 → S5. S6 is independent and can land any time after S1.

Each of S2a–S2d is atomic for its family: its tool, prompt, golden, test and fe changes land
together, so CI is green at every merge. During S2 the brand is mixed across families, never within
one.

Size check (lines matching per family in crates/fe/e2e/plugins): terminal 476, report 245,
track 199, task 98, plan 81, the rest ≈ 200, `neige <cmd>` 150, `mcp__calm`/`"calm"` 62.

### 5.1 Source-invariant gates the implementation will hit

- `tools/mod.rs:118` `default_registry_matches_full_golden`: regenerate and hand-verify.
- `tools/mod.rs:240` `prompt_files_cover_exactly_the_registered_tools` (renamed in S1 from `…_the_non_alias_tools`): `git mv` every
  `prompts/tools/*.md`, and drop the alias branch in S1.
- `tools/mod.rs:170` `planner_tool_surface_fits_its_byte_budget` (≤ 30,000 B; 29,984 at #1967).
  The brand adds 1 B per `calm.` in listed descriptions (15 + 1 in schemas), and each `neige <cmd>`
  → `neige <object> <cmd>` in a listed description adds 6 B. **Measure before S2d/S4**, and trim
  wording instead of raising the cap.
- `planner_card.rs:489-600` prompt-token tests (`calm_tool_tokens`): every prompt token must be a
  visible tool and aliases must stay hidden.
- Prompt goldens: `planner_card.rs:4/11/18` (`REGEN_PROMPT_GOLDENS=1`,
  `REGEN_PLANNER_PROMPT_GOLDEN=1`).
- `registry.rs:675` hidden drill-ins test. `tests/cases/mcp_tools_list_role_filter.rs` and
  `mcp_assistant_tool_gate.rs` (role matrices by name).
- `tests/cases/{boot_invariants,deferred_write_tx_invariant,fork_guard_exemption_invariant,harness_turn_start_invariant}.rs`:
  scan-type suites. Run the whole `-p calm-server`.
- `tests/cases/worker_flow_{claude,codex}_golden.rs` and the event goldens (`tests/goldens/events/`)
  if a fixture carries tool names.
- `scripts/gate-1316-terminology-ratchet.sh` (in local-ratchet-gates.sh) counts terms in crates,
  fe, docs and e2e in both directions. The rename adds none of its terms.
- `scripts/gate-prose-ratchet.sh` counts Rust string literals of 120+ characters in both
  directions. `calm.`→`neige.` (+1) or `neige ls`→`neige track ls` (+6) can push a literal across
  120. Re-run it per slice and re-baseline only with a note.
- `scripts/ci/frozen-vector-gate.sh`: the vectors hold no tool names (0 hits). There is no
  expected change.
- Released migrations are byte-frozen. PR-2 adds one new data migration (§6.3), numbered last.

## 6. Migration and exit plan (rule 2: the production 4140 DB only)

### 6.1 What is persisted in production (counted 2026-10-04, read-only)

The counting script (`dbscan.py` in the scratchpad) runs a `LIKE '%<pattern>%'` count over every
text column of every table in `calm.db` (opened `mode=ro`). The results that matter:

| Where | Old names | Read back by code? | Action |
|---|---|---|---|
| Planner transcript items table (`<harness>_items`).params: `"tool":"calm.*"` in `mcpToolCall` rows | 1168 rows (2026-09-16 … 10-02), all in `$.item.tool`. Aliases: 0 in the tool field (get_track_state 3, task_completed 1, task_failed 1 occur only in other params text). Retired writers in the tool field: report.blocks.upsert 12, report.blocks.delete 8, task.replace 2. `mcp__calm__`: 34 rows, 20 of them in the tool field (5 git-forge plugin names, 0 kernel names) | **yes**: fe classifies history by name (mcp-tools.ts, conversation.ts:784,993), and activity SQL (`track_activity/sql.rs:29`) | §6.3 migration |
| worker_flow_items.payload | calm.* 438, `mcp__calm__` 159, aliases 28 | displayed only (raw tool name) | none |
| track_recipes.body | 1 row "SPY 与现金 · 每日例程": calm.calendar.list, calm.source.capture, calm.report.commit | **yes**: agents read it as instructions | §6.3 migration (with revision bump) |
| plugins.manifest (`dev.neige.git-forge`) | description: calm.track.publish, calm.review.round | read by the Planner as a description | none: every boot refreshes it from the compiled `plugins/git-forge/manifest.json` (§6.3) |
| cards.payload, tasks.goal/acceptance, operations.*, events.payload (card.*, track.report_edited, task.*), track_vcs_objects | prompt and report text, history | no: all matching tasks are terminal (`done`/`failed`) | none |
| report_sources (77 rows) | stores `plugin_id` + `tool` separately (`Origin::Plugin`) | no change | none |
| `data/codex-home/config.toml` `[mcp_servers.calm]` | 1 table | boot rewrites it (S3) | automatic |

Commands to re-count (run before S2a and again after deploy):

```
DB=~/.local/share/neige-next/data/calm.db; T=harness
sqlite3 -readonly $DB "select count(*) from ${T}_items where params like '%\"tool\":\"calm.%'"
sqlite3 -readonly $DB "select id,title from track_recipes where body like '%calm.%'"
sqlite3 -readonly $DB "select id from plugins where manifest like '%calm.%'"
sqlite3 -readonly $DB "select count(*) from tasks where status in ('running','dispatched')"
```

### 6.2 Exit plan

- **No aliases.** The 4 aliases and 2 shims are deleted in S1. In-flight model contexts that still
  say `calm.*` get -32601 with the valid names (S1). An old CLI spelling gets a usage error that
  lists the objects (S4).
- **Deploy window:** run the last command in §6.1; it must print 0. The shared Codex
  app-server needs no manual step: its adoption signature hashes `MCP_SERVER_KEY`
  (`compute_env_signature`), so the first boot adopts a pre-rename daemon only to drain it, and
  the next thread start replaces it. Until then, existing threads keep the old daemon's tool
  catalog, not just their in-flight turns; #2087 B0 now replaces such a daemon at boot, before
  harness recovery. A Claude
  Planner process is spawned per turn with the new `--mcp-config`. Use the standard production
  restart runbook; real Codex E2E is never run on this host.
- **After deploy:**
  - Check that the SPY recipe row and the git-forge manifest row no longer contain `calm.`.
  - Re-run the §6.1 counts.

### 6.3 Stored names are rewritten once (DECIDED)

- The fe carries **no** legacy-name table. A front-end map would never exit, because the 1168 rows
  are deleted only with their card (`calm-truth …/out_of_domain.rs:46`).
- One new migration (numbered last, at merge time) rewrites the persisted names that code reads
  back:
  - **Planner transcript items, the `tool` field only** (`$.item.tool` in `<harness>_items.params`).
    Its readers are the fe history, activity SQL (`track_activity/sql.rs:29`) and rewind. Rewind
    uses user input and provider anchors (`harness/rewind.rs:63`, `claude_planner/rewind.rs:62`) and
    never resends stored tool rows. Codex resumes from provider-owned history
    (`codex_appserver.rs:769`). User input, other params fields and provider session files are not
    touched.
  - The rename map is §4.3, plus the historical alias targets (`calm.get_track_state` →
    `neige.track.state`, `calm.update_task_meta` → `neige.task.verdict`, `calm.task_completed` →
    `neige.task.complete`, `calm.task_failed` → `neige.task.fail`; see track_state.rs:29 and
    emit.rs:28) and the shims under their own names (`neige.dispatch.request`, `neige.plan.upsert`)
    so that history stays legible. The implementer counts which of the 35 `mcp__calm__`
    occurrences sit in the `tool` field. Only those are normalized to the raw `neige.` name.
  - **`track_recipes.body`**, with `revision = revision + 1` and `updated_at` set for every changed
    row. Both are required by the optimistic lock at `calm-truth …/track_recipe.rs:51` and the
    track-creation stamp at `routes/tracks.rs:1289`.
- The migration test seeds every observed shape (each §4.3 row, each alias, a qualified
  `mcp__calm__` tool field, a recipe row) and asserts the read-back through the real readers: the
  activity projector for `neige.user.notify`, and the recipe revision bump.
- **Not rewritten:**
  - `plugins.manifest`. It needs no migration: `git-forge` is a compiled built-in, and every boot
    runs `PluginHost::reconcile_builtins` (state.rs → plugin_host/builtin.rs), whose
    `plugin_install` upsert (`ON CONFLICT(id) DO UPDATE SET manifest = excluded.manifest`) writes
    the compiled manifest back. No reinstall step.
  - The stored `server` field (`"calm"`), which no reader uses (the fe and the projector key on
    `tool`).
  - `worker_flow_items`, task text and events. Events are replayed (`replay.rs:145`), but
    the item-added event carries a row reference, not params (`calm-types/src/event.rs:335`). Event
    goldens and frozen vectors hold no tool names, and no hash covers the two rewritten columns.

## 7. KNOWN GAPS

- **K1:** a Codex callable that Codex hashed (raw name sanitized to more than 117 bytes under
  `mcp__calm`, or 116 under `mcp__neige`, or a sanitize collision between plugins such as
  `dev.x-y` and `dev.x.y`) is not resolved by `source.capture`. The call fails explicitly with the
  raw names listed. No production tool is affected (max 45 B, no collisions).
- **K2:** role admission is not declared. `neige tool ls` can show a CLI-covered tool that the
  caller's role is refused (for example, Worker → `neige admin gc`). The output says that listing
  is not a grant. Resolved by #2289: tools declare `roles`, and the catalog is what the role may
  call (`docs/conventions/agent-commands.md` §7).
- **K3:** the fe infers read/write from tool names (`mcp-tools.ts`), which is a UI-side policy
  inference. A declared effect on the tool-call item would remove it. That is out of scope here.
- **K4:** Claude's own tool-name length limit was not measured. Kernel names are at most 36 B as
  `mcp__neige__…`. Plugin names longer than about 64 B under Claude are unverified.
- **K5:** `external/codex` (`5a440c0`) is not the deployed 0.159.2. Read Codex behavior from the
  deployed binary (§3.2), not from that tree.
- **K7 (found in PR-1):** a built-in plugin native (`calm.calendar.*`,
  `calm.track.publish`) called outside its track scope is refused by `require_bound`
  (builtin_plugins/mod.rs) with the bare `-32601 tools/call: <name>`, without the visible-tool
  list. These names are public in the registry, so this is no existence oracle. Only the
  transport's unknown-name path lists the session's tools.
- **K6:** the ledger is process memory. A restart forces re-reads. This is unchanged and fails
  closed.
- **K8 (PR-3):** recipe text that #2056's migration 0135 rewrote to `neige task-report-success` (and any
  stored `neige cat`-style spelling) is refused by the two-word CLI. Production holds 0 such recipes
  (`select count(*) from track_recipes where body like '%neige task-%' or body like '%neige cat%'`
  → 0 of 3, 2026-10-04), so no recipe migration is added.

## 8. Decisions (2026-10-04)

1. **History display:** one data migration (§6.3), with no fe legacy map. It is the structural
   option: one value keeps one meaning and no compat code is left to exit.
2. **`cat_at` → `track.show`:** a separate tool. Folding it into `track.cat` would create
   cat-option × commit combinations (blocks, sections, area reports) that rule 3 would then require
   on every path.

## 9. PR grouping

The S-slices are review units. They land as four PRs, so the brand is never mixed on `main`:

| PR | Slices | Note |
|---|---|---|
| PR-1 | S1 + S6 (S1 includes the `deprecated_alias_names` caller at `planner_card.rs:572`) | Remove aliases and shims; Codex adapter owns spelling; measured regression tests |
| PR-2 | S2a–S2d + S3 + the §6.3 migration | The atomic brand and grammar switch; no `(calm\|neige)` scanner window |
| PR-3 | S4 | Mechanical CLI and discovery |
| PR-4 | S5 | Living docs and e2e scripts |

## 10. PR-2 implementation record

- The migration is `crates/calm-truth/migrations/0134_neige_tool_names.sql` (renumber last at
  merge). It rewrites only `$.item.tool` (`json_set`; other values unchanged, and measured byte-identical on a 4140 copy) through one map:
  §4.3, the four aliases, the two shims, the three retired writers stored on 4140
  (`calm.report.blocks.upsert` → `neige.report.upsert`, `calm.report.blocks.delete` →
  `neige.report.delete`, `calm.task.replace` → `neige.task.replace`) and the five
  Claude-qualified git-forge names → their raw `plugin.dev.neige.git-forge_gh.*` names. The 70
  stored `calm.review.round` rows become `neige.review.round`, a history-only name since #2017. Recipe
  bodies get the same rename; a changed row bumps `revision` and sets `updated_at` to
  `max(updated_at + 1, now_ms)`.
- Dry run on an in-memory copy of 4140's Planner transcript table and `track_recipes` (2026-10-04): 1188 rows
  changed (1168 `calm.*` + 20 qualified), 0 rows changed outside the tool field, 0 `calm.*` or
  `mcp__calm__` tool fields left; the SPY recipe went from revision 3 to 4.
- The fe keeps only names that 4140 history holds: `REPORT_WRITE_TOOLS` is `neige.report.write`,
  `neige.report.upsert`, `neige.report.commit`; `REPORT_DELETE_TOOL` is `neige.report.delete`. The
  never-stored `calm.report.write`, `calm.report.edit` and `calm.report.blocks.move` (and with it
  `REPORT_MOVE_TOOL`) are dropped.
- The server key has one owner, `mcp_server::wiring::MCP_SERVER_KEY`; the Codex home,
  the Planner approvals, the Claude MCP config, allowed tools, translate and driver all read it.

### 10.1 PR-2 review (L2, two channels)

- Codex (read-only) and an executing subagent both returned APPROVE with no blocking findings. The
  subagent applied 0134 to a `VACUUM INTO` copy of 4140, both with `sqlite3` and through the real
  `MIGRATOR.run`: 1198 rows changed, 0 bytes changed outside `$.item.tool`, 0 old tool fields left,
  and the SPY recipe revision went 3 → 4.
- Fixed in PR-2: the shared Codex daemon's adoption signature now hashes `MCP_SERVER_KEY`, so a
  daemon adopted from before the rename is drained at boot and replaced at the next thread start
  (`env_signature_replaces_a_daemon_from_before_the_server_key_rename`, mutation-verified).
  Existing threads kept the old catalog until a thread start; #2087 B0 now replaces such a daemon
  at boot.
- Recorded, not applied:
  - The migration test re-applies the embedded SQL after fixture boot rather than upgrading a
    pre-0134 fixture through `MIGRATOR.run`. The real migrator was exercised on the 4140 copy.
  - The sweep does not match escaped (`calm\.report`), sanitized (`calm_report_read`) or built
    (`format!("calm.{}")`) spellings, nor a bare server key. None exist today.
  - Recipe bodies are renamed by prefix without the retired-writer map. That is correct for 4140,
    whose only matching recipe holds 3 live names.
  - `e2e/planner_claude_ux*.py` still matches `calm.terminal.*` until PR-4.

## 11. PR-3 implementation record

- `Command` has no `name`; `neige <object> <action>` is `tool` minus `neige.`. Positionals have
  only `key` and `required`. Their refusals are generated: `<cmd> requires <key>`, and
  `unexpected argument …; usage: <usage line>`. Unknown object, action and option errors list the
  objects, the actions or every accepted flag. `--json` and `--force` are global; `--force` is
  accepted only by a command with a confirm.
- Option renames that the mechanics forced: `task report-success --artifacts` (was `--artifact`),
  `report find --name`/`--tag` (were `-name`/`-tag`). `ls -l` stays the only view flag. `diff`
  lost its hand-written `--to`/`--path` rows; every positional is now its `--<key>` option.
  A named `--<key>` claims its slot first, and the remaining positionals fill the unclaimed
  slots in order, so `track show --commit c report.md` and `track show report.md --commit c` are
  the same call (PR-3 review).
- `neige tool list` JSON is `{tools: [{name, cli, listed}], next_cursor}`. `describe` is the MCP
  declaration plus `cli` and `listed`. The footer "listing is not a grant; the tool's role gate
  decides" ends the text output only; JSON carries data only.
- Help: `neige help`, `neige help <object>` (its actions), `neige help <object> <action>`, and the
  same with `--help`. Help texts are keyed by tool name.
- H8 (`prompt_neige_mentions_name_served_commands`) now also checks every `-…` token inside a
  `` `neige <object> <action> …` `` span against the command's accepted flags.
- Facts this design did not record:
  - The Planner surface measured 28,870 B at `0841b3ae8` (not 29,984) and is 28,888 B after PR-3.
  - The guides have their own cap (`every_guide_fits_its_byte_budget`, 7,500 B in total). PR-3
    reached 7,550 B and trimmed wording to 7,489 B.
  - `calm-types/src/report/legacy_initial_v4.md` is sha256-frozen and keeps `neige cat`. H8
    exempts it by name. `initial_body_is_header_line_plus_legacy_v4` now pins the shipped body as
    header plus the frozen body with that one spelling renamed.
  - The `Mention` doc comment in `calm-types` reaches `fe/core/api/generated/{openapi.json,wire.ts}`.
    Those files are regenerated with `npm run gen:api` and carry `OWNERSHIP-CHANGE` trailers.
- Merged with #2056 (rebased onto `fd26e2267`): its tool names, descriptions, `report_received`
  result and migration 0135 stand. The Worker commands are now derived like every other one:
  `neige task report-success --attempt-id … [--result …] [--artifacts …]` and
  `neige task report-failure --attempt-id … --reason …`. The action's `_` is written `-` in the
  command; the flat `task-report-success`/`task-report-failure` spellings are old one-word
  spellings, a usage error listing the objects, and `task complete`/`task fail` are unknown
  actions.
  After the merge the Planner surface is 29,458 B (cap 30,000) and the guides 7,489 B (cap 7,500).
- Left as is: the `track_report_gate_guard` classifier inputs (any literal `neige` executable is
  flagged, whatever its arguments), and the `fe` conversation tests' sample shell strings.

## 12. PR-4 implementation record

- S5 renames the remaining living text: `e2e/planner_claude_ux*.py` and their test (whose metrics read `neige.terminal.*` again), `e2e/PLANNER_CLAUDE_UX.md`, `.env.example`, `docker/Dockerfile.server`, `docs/events-retention.md`, `docs/report-live-views.md` and the two oracle YAML files. The oracle files describe current behavior and nothing machine-reads them.
- Design records (`docs/architecture/*`, `docs/design-*`, `docs/_*`, `docs/archive/`) keep the names of their time.
