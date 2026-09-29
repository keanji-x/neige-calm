# #1873 — #1870 dogfood follow-ups

**Owner rules.** (1) Simple first, pain points only; hypotheticals are one-line KNOWN GAPS.
(2) Compatibility means the 4140 database only. (3) Child environments come from allowlists,
configuration from typed inputs.

**Outcome.** Seven small fixes found by the Claude Planner of track `676b3df5…` (#1870, PR #1871).
The kernel edge that `track_idle` guards (item 4) and the Planner sandbox (items 5 and 6) do not
change: the owner declined item 4 (won't fix) and chose option B for items 5 and 6.

## 1. Facts

Verified at a0eb9cd6f by reading the code, or by the query or command shown (4140 `calm.db`,
2026-09-29). The #1870 Planner is `worker_sessions.provider = claude` (card `a27fe26f…`).
All 7 of its tasks ran on Claude workers.

| # | Claim | Where | Verified |
|---|---|---|---|
| F1 | The ratify route passes `message` only to the `blocked → working` lifecycle change. `ratify.resolved` has `{track_id, decision}` and a deny drops the text | `routes/cards.rs:1002`, `:1017-1040`; `event.rs:673-676` | read |
| F2 | The wake text is `Ratification was resolved with decision=…`, with no message | `observation.rs:474-481`; `dispatcher/mod.rs:1633-1636` | read |
| F3 | The owner's text is in events 73685 and 73686 only. Event 73687 is `{"decision":"grant",…}`. The wake (transcript row 25054) ends with the F2 sentence | `events`, transcript rows | query |
| F4 | `gh.pr.merge` takes `repo, pr, phase, slice_id, expected_head_sha` and runs `gh pr merge --squash --delete-branch` with no message flags | `plugins/git-forge/main.rs:441-462` | read |
| F5 | The Co-Authored-By trailer comes from the Claude worker's own `git commit`. The delivery commit message (op 486, `neige: attempt … completed`) was not used because the tree was already clean. GitHub's squash then copied the trailer to main | `git log -1 f34d2418d`, `a0eb9cd6f`; op 486 argv; `forge_git.rs:84-85` | command, query |
| F6 | Kernel-written Claude settings contain `hooks` only (worker, chat card, restart). The Planner settings have no `attribution`. Claude Code 2.1.280 documents `attribution: {commit, pr}`, where "empty string hides attribution". `includeCoAuthoredBy` is deprecated | `routes/claude_cards.rs:313-338`; `claude_planner/spawn.rs:21-32`; the CLI's settings schema | read, `grep` of the CLI bundle |
| F7 | Merge argv is outside the idempotency hash (`SemanticForgePayload` excludes argv), so `--subject/--body` would be hash-neutral. `GIT_DELIVERY_*` is a separate script family | `transport.rs:786-795`; `forge_git.rs:70-106` | read |
| F8 | publish returns `"url": dest.url`, the remote URL. Its script prints `gh pr view --json number,headRefOid`, and the output probe prints the same | `track_publish.rs:331`; `forge_git.rs:111-150` | read |
| F9 | One in-tree worker per track: `compute_ready` holds every codex/claude task that runs in the checkout while `track_idle` is false. `track_idle` has three terms: in-flight in-tree task, held lease, and unsettled delivery | `scheduler/mod.rs:149-170`; `calm-truth/.../track_idle.rs:16-58`; `task_execution.rs:8-12` | read |
| F10 | At most one held lease per path (`workspace_leases_active_path_idx`). Every release runs the delivery `git add -A` in the shared tree | migration 0115:69; `forge_git.rs:84` | read |
| F11 | Serial reviews cost about 405 s on #1870. Channel b waited behind channel a for 130 s, 126 s and 149 s | `tasks` created/finished ms | query |
| F12 | `gh.pr.diff` and `gh.pr.checks` lower with `parked: true`. For a parked payload the MCP call returns `{op_id, parked:true}` at once | `main.rs:386`, `:437`; `transport.rs:917-929` | read |
| F13 | The diff is not lost: event 73550 names `forge-results/269dc8d5….result`, 35 178 bytes, a full patch. `forge.pr.diff.read` does not wake the Planner. `forge.pr.checks` wakes it with the conclusion only | `dispatcher/mod.rs:181`, `:1145`, `:1730`; `observation.rs:433-439`; `ls` | read, command |
| F14 | A non-parked call waits and returns `result.event`; `stdout` is inlined only for `forge.issue.read` | `forge_action_adapter/mod.rs:734-748` | read |
| F15 | The Claude Planner's Bash runs in bubblewrap with `--unshare-net`. Its only egress is Claude's proxy, reached through the `localhost:3128/1080` bridge. The sandbox sets `NO_PROXY=localhost,127.0.0.1,…`, so host loopback is unreachable | CLI bundle (`t_`, proxy env builder) | `grep` of the CLI bundle |
| F16 | Transcript rows 24918/24922 (card `a27fe26f…`, the Claude Planner) show `curl 127.0.0.1:18870` and `:5870` failing with "Failed to connect … after 0 ms" | transcript rows | query |
| F17 | 24994 `gh pr checks` failed at `proxyconnect 127.0.0.1:2080`: `~/.zshenv` re-exports `HTTPS_PROXY=http://127.0.0.1:2080` over the sandbox's value. Local repro: `bwrap --unshare-net --setenv HTTPS_PROXY http://localhost:3128 -- zsh -c 'echo $HTTPS_PROXY'` prints `…127.0.0.1:2080`, and curl to `127.0.0.1:4140` inside the netns gets "Couldn't connect" | `~/.zshenv:13-14`; transcript row 24994 | command |
| F18 | gh on this host is logged in through `~/.config/gh/hosts.yml`, which is readable from the Planner sandboxes. Evidence: Codex Planner row 20568 (`gh auth status`, token `gho_…`). Before #1870, Claude Planner rows 23540/23614/23918 pushed and ran `gh pr checks/view` by setting `HTTPS_PROXY=$ALL_PROXY` (the sandbox's SOCKS bridge). Codex Planner row 20668 ran `gh pr merge 139 --squash` directly, exit 0 | transcript rows | query (file not read) |
| F19 | Workers can reach loopback. Claude workers run `claude --allow-dangerously-skip-permissions --settings <hooks-only>` in a host PTY; no `sandbox` key exists in the kernel settings, `~/.claude/settings.json` or the repo. Codex runs `workspace-write` with `network_access=true`, and Codex Planner row 24706 got HTML from `curl 127.0.0.1:4173` | `claude_adapter/mod.rs:356-378`; `shared_codex_home.rs:301-302`; transcript rows | read, query |

## 2. Decisions

1. **Ratify message: carry it verbatim.** `ratify.resolved` gains `message: Option<String>`
   (`serde(default, skip_serializing_if)`, the `agent_message` shape): 4140 rows have none, and a
   user may send none. An empty or whitespace-only message becomes `None`. The observation gains
   the same field, and the wake adds `The user's message, verbatim:` followed by the text, for
   grant and deny alike. Rationale: the owner already sends instructions this way (F3), and no
   in-repo client calls the route, so this is the one place to fix. The lifecycle event keeps its
   `agent_message`.
2. **Squash trailer: fix it at the source.** Add `"attribution": {"commit": "", "pr": ""}` to
   `build_claude_settings_json_for`, the shared base of the worker, chat card, restart and
   Planner-opened terminal settings (`terminal_hooks.rs:118`), and to the Planner's
   `settings_json`. `gh.pr.merge` is unchanged. Rationale: the one observed need was the trailer,
   the owner's standing rule is no Co-Authored-By, and the squash body is the PR title/body the
   Planner writes at publish.
3. **publish `url` is the PR's web URL.** Both publish printers use `--json number,headRefOid,url`,
   so live stdout and the recovered stdout stay the same command. The publish forge event extracts
   `url` from `/url`, and `forge.pr.opened` drops the extra field at deserialization, so the event
   schema is unchanged. The tool returns `event["url"]`. Rationale: gh already knows the URL, and
   parsing remotes (ssh, GHE) is fragile.
4. **Read-only tasks in parallel: won't fix (owner decision (a)).** Reviews stay serial (about
   405 s per round, F11). Two readers in the shared tree would need four changes: a `read_only`
   predicate, a `track_idle` reader/writer split across its three terms, a migration (the
   active-path unique index, F10), and skipping the delivery, whose `git add -A` takes
   `index.lock` and would race (F10). That cost is not worth the saving.
5. **Forge reads return their result (owner: option B, decided).** Flip `gh.pr.diff` and
   `gh.pr.checks` to `parked: false`. The call then waits (the 300 s non-parked deadline) and
   returns `result.event`: the diff's `artifact_path`, which the Planner reads, and the checks'
   `conclusion`. `parked` is outside the idempotency hash (F7), so the change is hash-neutral.
   The effective wait bound is the smaller of the kernel's non-parked deadline
   (`forge_deadline_ms(false)` = 300 s, `transport.rs:1070-1071`) and the MCP client's
   `tools/call` timeout (Codex default 120 s, `neige-mcp-stdio-shim/src/budget.rs:10`); both reads
   take seconds. `gh.pr.checks` without `attempt` has the fixed key `gh.pr.checks:{repo}:{pr}`
   (`main.rs:396-399`), and the same key and hash returns the existing operation
   (`operation/driver.rs:132-134`), so a re-read would return the first conclusion forever: its
   description says to pass a new `attempt` on each re-read. The template gains one line: do not
   use the gh CLI for PR reads; use the git-forge tools. The sandbox network is unchanged. Result
   before this fix: not lost (F13), only never surfaced.
6. **Loopback (owner: option B, decided).** No sandbox change. Workers already reach loopback
   (F19), so the template says preview self-checks (curl or Playwright against the dev stack)
   run in a worker task, not in the Planner's Bash. This is a prompt change only; #1869 gets a
   note that the Claude-Planner proxy failure is `~/.zshenv` inside the netns (F17).

Item 7 moved to #1876 (lifecycle → open/closed).

## 3. Changes by file

| File | Change |
|---|---|
| `calm-types/src/event.rs`, `observation.rs` | `message` on `RatifyResolved`; wake text (item 1) |
| `calm-server/src/routes/cards.rs` | pass the trimmed message to the event (1) |
| `calm-server/src/dispatcher/mod.rs` | map `message` into the observation (1) |
| `calm-server/src/routes/claude_cards.rs`, `claude_planner/spawn.rs`, `terminal_hooks.rs` | `attribution`; the "hooks-only" comments (2) |
| `calm-types/src/forge_git.rs`, `mcp_server/tools/track_publish.rs`, `prompts/tools/calm.track.publish.md` | `url` (3) |
| `plugins/git-forge/main.rs`, `manifest.json` | diff/checks `parked: false` and their lowering tests; descriptions say the call waits and checks re-reads pass a new `attempt` (5) |
| `templates/builtin/issue-development.md` | no gh CLI for PR reads (5); preview checks in a worker (6) |
| Generated / pinned | `fe/core/api/generated/wire.ts`, `fe/core/api/schemas.ts` (`message` optional), `goldens/events/ratify.resolved.json` (plus a with-message golden), `goldens/mcp_tool_registry.json`, the template prompt golden, `openapi.json` if the field doc changes; `tests/support/gh_shim.rs` answers `number,headRefOid,url` |

No migration. No `SYNC_EVENT_VERSION` or `WEB_COMPAT_VERSION` bump: the field is additive and the
zod schema is not strict.

## 4. Test plan (must-red)

Each row: test → production line → the single mutation that turns it red → predicted red set.

| Item | Test | Mutation (production only) | Predicted red |
|---|---|---|---|
| 1 | `dispatcher::tests::ratify_resolved_wake_carries_the_user_message_verbatim` (event → `harness_observation_from_event` → `to_turn_text`) | `dispatcher/mod.rs` mapping sets `message: None` | that test |
| 1 | `review_ratify::ratify_grant_and_deny_carry_the_message_on_resolved` (route, both decisions) | `cards.rs` pushes `RatifyResolved { message: None, … }` | that test |
| 2 | `claude_cards::tests::claude_settings_hide_attribution` | drop `attribution` from `build_claude_settings_json_for` | that test |
| 2 | `spawn_tests::the_sandbox_settings_are_exactly_the_owner_decision` (expected JSON + `attribution`) | drop it from `settings_json` | that test |
| 3 | `track_publish::the_publish_result_url_is_the_pr_url` (replaces the `:435` origin assertion; D2 keeps its ref checks) | `track_publish.rs:331` back to `dest.url` | that test |
| 5 | `git_forge_happy_path_persists_ordered_template_events`: the diff response has `parked:false` and `result.event.artifact_path` equal to the event row's; the checks response has `result.event.conclusion` | `lower_gh_pr_diff` back to `parked: true` | that test + `lowers_gh_pr_diff` |

Kept green: `ratify_route_grant_emits_resolved_and_flips_blocked_to_working` and
`ratify_route_deny_emits_resolved_and_stays_blocked`. Gates:
`-p calm-server` (review_ratify, track_publish, forge_template_e2e, dispatcher, claude_cards,
claude_planner), `-p calm-types`, the plugin unit tests, `local-rust-gates.sh --quick` (OpenAPI),
the fe lint/build/test for the schema, and `scripts/local-ratchet-gates.sh`.

## 5. KNOWN GAPS

- The ratify bypass already exists: both Planner backends can run `gh pr merge` or `git push`
  with the owner's `hosts.yml` token (F18). Option B keeps the network as is; it does not close this (#1875).
- A publish key first used before the deploy now hashes differently and is refused as reused; call again with a new key.
- `gh.pr.checks` still wakes the Planner after its synchronous result (as publish does with
  `forge.pr.opened`); that is one redundant turn per read.
- The Claude Planner's own Bash reaches neither loopback nor, without the `$ALL_PROXY` workaround, GitHub (`~/.zshenv`, netns). By owner decision it uses forge tools and worker tasks.
- `gh.pr.merge` cannot set the squash subject or body. That is hash-neutral to add later (F7).
- The Claude Planner writes the PR body itself, so `attribution.pr = ""` removes only Claude's automatic PR line.
- The worker's `claude` is unpinned (`config.rs:97`, a PATH lookup, today 2.1.280). The still-installed 2.1.220 knows only `includeCoAuthoredBy`, so pointing a worker at it brings the trailer back.

## 6. OWNER DECISIONS

**Decided — items 5 and 6: option B.** The sandbox network stays as is: no loopback and no proxy
for the Planner. Forge reads return results (item 5). Preview checks move to a worker task
(item 6).

**Decided — item 4: (a) won't fix.** Reviews stay serial (about 405 s per round, F11).
