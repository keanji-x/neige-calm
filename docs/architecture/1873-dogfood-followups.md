# #1873 — #1870 dogfood follow-ups

**Owner rules.** (1) Simple first, pain points only; hypotheticals are one-line KNOWN GAPS.
(2) Compatibility means the 4140 database only. (3) Child environments come from allowlists,
configuration from typed inputs.

**Outcome.** Seven small fixes found by the Claude Planner of track `676b3df5…` (#1870, PR #1871).
The kernel edge that `track_idle` guards (item 4) and the Planner sandbox (items 5 and 6) do not
change: item 4 is an open owner decision, and the owner chose option B for items 5 and 6.

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
| F20 | `calm.ratify.request` requires `working`. The FSM has `working → blocked` (planner) and `blocked → working` (user, planner) but no reviewing ↔ blocked edge. The template tells the Planner to flip `reviewing → working` before the request and `working → reviewing` after the grant | `review.rs:251`; `track_lifecycle.rs:203-229`; `issue-development.md:102-106`, `:148-155` | read |
| F21 | `blocked` drives notifications through the newest `track.lifecycle_changed` into `blocked`, so the request must keep parking the track there | `track_activity/sql.rs:261-280` | read |

## 2. Decisions

1. **Ratify message: carry it verbatim.** `ratify.resolved` gains `message: Option<String>`
   (`serde(default, skip_serializing_if)`, the `agent_message` shape): 4140 rows have none, and a
   user may send none. An empty or whitespace-only message becomes `None`. The observation gains
   the same field, and the wake adds `The user's message, verbatim:` followed by the text, for
   grant and deny alike. Rationale: the owner already sends instructions this way (F3), and no
   in-repo client calls the route, so this is the one place to fix. The lifecycle event keeps its
   `agent_message`.
2. **Squash trailer: fix it at the source.** Add `"attribution": {"commit": "", "pr": ""}` to
   `build_claude_settings_json` (worker, chat card and restart share it) and to the Planner's
   `settings_json`. `gh.pr.merge` is unchanged. Rationale: the one observed need was the trailer,
   the owner's standing rule is no Co-Authored-By, and the squash body is the PR title/body the
   Planner writes at publish.
3. **publish `url` is the PR's web URL.** Both publish printers use `--json number,headRefOid,url`,
   so live stdout and the recovered stdout stay the same command. The publish forge event extracts
   `url` from `/url`, and `forge.pr.opened` drops the extra field at deserialization, so the event
   schema is unchanged. The tool returns `event["url"]`. Rationale: gh already knows the URL, and
   parsing remotes (ssh, GHE) is fragile.
4. **Read-only tasks in parallel: not a small change → OWNER DECISION 1.** Two readers in the
   shared tree need four changes: a `read_only` predicate, a `track_idle` reader/writer split
   across its three terms, a migration (the active-path unique index, F10), and skipping the
   delivery. The delivery `git add -A` takes `index.lock` and would race (F10), and a post-release
   clean-tree check would also be needed. That is more than a predicate plus one check.
5. **Forge reads return their result (owner: option B, decided).** Flip `gh.pr.diff` and
   `gh.pr.checks` to `parked: false`. The call then waits (the 300 s non-parked deadline) and
   returns `result.event`: the diff's `artifact_path`, which the Planner reads, and the checks'
   `conclusion`. `parked` is outside the idempotency hash (F7), so the change is hash-neutral.
   The template gains one line: do not use the gh CLI for PR reads; use the git-forge tools. The
   sandbox network is unchanged. Result before this fix: not lost (F13), only never surfaced.
6. **Loopback (owner: option B, decided).** No sandbox change. Workers already reach loopback
   (F19), so the template says preview self-checks (curl or Playwright against the dev stack)
   run in a worker task, not in the Planner's Bash. This is a prompt change only; #1869 gets a
   note that the Claude-Planner proxy failure is `~/.zshenv` inside the netns (F17).
7. **Ratify round-trip: request from `reviewing`, grant returns there.** `calm.ratify.request`
   accepts `working` or `reviewing`. The FSM gains `reviewing → blocked` (planner) and
   `blocked → reviewing` (user). A grant moves a still-`blocked` track back to the `from` of the
   newest `track.lifecycle_changed` into `blocked`. That edge is the request's own edge, derived
   from events as `ratify_request_pending_tx` is, so there is no new field. A track that is no
   longer `blocked` is left alone. A deny still changes nothing. Pending 4140 requests came from
   `working` and return there, as today.

## 3. Changes by file

| File | Change |
|---|---|
| `calm-types/src/event.rs`, `observation.rs` | `message` on `RatifyResolved`; wake text (item 1) |
| `calm-server/src/routes/cards.rs` | pass the trimmed message to the event; grant target = origin when `blocked` (1, 7) |
| `calm-server/src/dispatcher/mod.rs` | map `message` into the observation (1) |
| `calm-server/src/ratify_state.rs` | `ratify_origin_tx`: `from` of the newest edge into `blocked` (7) |
| `calm-types/src/track_lifecycle.rs` | two edges plus the test mirror (7) |
| `calm-server/src/mcp_server/tools/review.rs`, `prompts/tools/calm.ratify.request.md` | accept `reviewing`; doc (7) |
| `calm-server/src/routes/claude_cards.rs`, `claude_planner/spawn.rs` | `attribution` (2) |
| `calm-types/src/forge_git.rs`, `mcp_server/tools/track_publish.rs`, `prompts/tools/calm.track.publish.md` | `url` (3) |
| `plugins/git-forge/main.rs`, `manifest.json` | diff/checks `parked: false`; descriptions say the call waits (5) |
| `templates/builtin/issue-development.md` | no manual flips (7); no gh CLI for PR reads (5); preview checks in a worker (6) |
| Generated / pinned | `fe/core/api/generated/wire.ts`, `fe/core/api/schemas.ts` (`message` optional), `goldens/events/ratify.resolved.json` (plus a with-message golden), `goldens/mcp_tool_registry.json`, the template prompt golden, `openapi.json` if the field doc changes; `tests/support/gh_shim.rs` answers `number,headRefOid,url` |

No migration. No `SYNC_EVENT_VERSION` or `WEB_COMPAT_VERSION` bump: the field is additive and the
zod schema is not strict.

## 4. Test plan (must-red)

Each row: test → production line → the single mutation that turns it red → predicted red set.

| Item | Test | Mutation (production only) | Predicted red |
|---|---|---|---|
| 1 | `dispatcher::tests::ratify_resolved_wake_carries_the_user_message_verbatim` (event → `harness_observation_from_event` → `to_turn_text`) | `dispatcher/mod.rs` mapping sets `message: None` | that test |
| 1 | `review_ratify::ratify_grant_and_deny_carry_the_message_on_resolved` (route, both decisions) | `cards.rs` pushes `RatifyResolved { message: None, … }` | that test |
| 2 | `claude_cards::tests::claude_settings_hide_attribution` | drop `attribution` from `build_claude_settings_json` | that test |
| 2 | `spawn_tests::the_sandbox_settings_are_exactly_the_owner_decision` (expected JSON + `attribution`) | drop it from `settings_json` | that test |
| 3 | `track_publish::the_publish_result_url_is_the_pr_url` (replaces the `:435` origin assertion; D2 keeps its ref checks) | `track_publish.rs:331` back to `dest.url` | that test |
| 5 | `git_forge_happy_path_persists_ordered_template_events`: the diff response has `parked:false` and `result.event.artifact_path` equal to the event row's; the checks response has `result.event.conclusion` | `lower_gh_pr_diff` back to `parked: true` | that test + `lowers_gh_pr_diff` |
| 7 | `review_ratify::ratify_from_reviewing_grant_returns_to_reviewing` (reviewing → request → blocked → grant → reviewing, no Planner flip) | `cards.rs` grant target back to `TrackLifecycle::Working` | that test |
| 7 | same test | delete the `(Reviewing, Blocked)` edge | that test + the `track_lifecycle` legal-edge mirror |

Kept green: `ratify_route_grant_emits_resolved_and_flips_blocked_to_working` (origin `working`) and
`ratify_request_rejects_*` (the message still contains "not in `working`"). Gates:
`-p calm-server` (review_ratify, track_publish, forge_template_e2e, dispatcher, claude_cards,
claude_planner), `-p calm-types`, the plugin unit tests, `local-rust-gates.sh --quick` (OpenAPI),
the fe lint/build/test for the schema, and `scripts/local-ratchet-gates.sh`.

## 5. KNOWN GAPS

- The ratify bypass already exists: both Planner backends can run `gh pr merge` or `git push`
  with the owner's `hosts.yml` token (F18). Option B keeps the network as is; it does not close this.
- A publish key first used before the deploy now hashes differently and is refused as reused; call again with a new key.
- `gh.pr.checks` still wakes the Planner after its synchronous result (as publish does with
  `forge.pr.opened`); that is one redundant turn per read.
- The Claude Planner's own Bash reaches neither loopback nor, without the `$ALL_PROXY` workaround, GitHub (`~/.zshenv`, netns). By owner decision it uses forge tools and worker tasks.
- `gh.pr.merge` cannot set the squash subject or body. That is hash-neutral to add later (F7).
- The Claude Planner writes the PR body itself, so `attribution.pr = ""` removes only Claude's automatic PR line.
- A grant after the user has already resumed the track (no longer `blocked`) leaves the lifecycle alone, where today it forced `working`.

## 6. OWNER DECISIONS

**Decided — items 5 and 6: option B.** The sandbox network stays as is: no loopback and no proxy
for the Planner. Forge reads return results (item 5). Preview checks move to a worker task
(item 6).

**Open — OWNER DECISION 1 (item 4, the S2 fence).** Pick one:
- **(a) Won't fix.** Reviews stay serial (about 7 min per track, F11). No change.
- **(b) Prompt only.** One review task per round runs channel a and channel b as two parallel
  subagents inside one Claude worker, and reports each verdict verbatim. There is no kernel change
  and the fence is untouched. Cost: one worker process hosts both channels, so they are
  independent contexts but not independent processes. Codex workers have no subagents.
- **(c) Kernel reader/writer fence.** The four changes in item 4, including a migration. Not
  recommended.

Recommendation: **(b)** if context independence is enough for the two-channel rule; otherwise (a).
