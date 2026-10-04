# #1830 S3 — publish the verified candidate

**Owner rules.** (1) Simple first, pain points only; hypotheticals are one-line KNOWN GAPS.
(2) Compatibility means the 4140 database only. (3) S1, S2, S2b and S3 land before deploy.
(4) Credentials: child environments come from allowlists, configuration from typed inputs.

**Outcome.** The Planner of an attached track calls one kernel tool, `calm.track.publish`. The
kernel pushes `neige/track-<id>` to the checkout's upstream URL only when the branch tip is the
commit of a `done` attempt of this track. It then opens the PR, or reuses the open one, and
checks that the PR head is that commit. The run is one forge operation, and its git commands
never see a GitHub token. The same fix applies to every kernel git script that runs in the
track's checkout.

**Scope (owner, 2026-09-29).** S3 is publish only. The track worktree, its branch and its
candidate refs are removed when the track or its area is deleted (S1 D7), not by a separate
reclaim. **Acceptance change:** "removed after merge" becomes "removed when the track is
deleted". The follow-up #1868 removes `gh.pr.create` from git-forge; the MCP publication
entry point is now `neige.dev.publish`.

## 1. Facts

Verified at c3886d776 by reading the code, or by the query or command shown (4140 `calm.db`,
2026-09-29).

| # | Claim | Where | Verified |
|---|---|---|---|
| H1 | Plugin forge tools are stateless lowerings from a tool name and its arguments to argv. The plugin reads no database | `plugins/git-forge/main.rs:123-136` | read |
| H2 | A kernel-built forge action needs no running plugin: a delivery submits its own argv under `GIT_FORGE_PLUGIN_ID` | `git_candidate/delivery.rs:413-432`; `mcp_server/transport.rs:833-878` | read |
| H3 | 4140: git-forge is not installed. All 94 forge-action ops are kernel commits, and there are 0 `forge.pr.*` events. There are 27 tracks | `plugins`, `operations`, `events`, `tracks` | query |
| H4 | Every forge child (action and probe) gets `env_clear` plus PATH, HOME, LANG, LC_ALL, TERM, the configured proxies and GH_TOKEN, GITHUB_TOKEN, GH_ENTERPRISE_TOKEN, GITHUB_ENTERPRISE_TOKEN, SSH_AUTH_SOCK, GIT_SSH_COMMAND, GH_HOST, NO_PROXY | `forge_action_adapter/mod.rs:53-67`, `:360-375`, `:1025` | read |
| H5 | Git runs repository-selected code with its own env: hooks, fsmonitor, filters, `credential.helper`, `core.sshCommand`. The kernel's boundary for such runs is an allowlist env with no credential variable (HOME kept) | `workspace_materialize.rs:64-86` | read |
| H6 | Since S2, the delivery script's `git add -A` and `git commit` run in the tree the worker wrote, under H4's env. The Planner's `git.commit` does the same in the same tree | `calm-types/src/forge_git.rs:10`, `:65-66`; `transport.rs:958-982` | read |
| H7 | The semantic payload hash covers `probe` but not `argv`: changing an action script is safe, but a changed probe makes a persisted op resubmitted after deploy hit `idempotency_payload_conflict`. The task-verify sampler reuses `GIT_LEASE_PROVENANCE_SCRIPT` on its own | `transport.rs:788-797`; `task_verify_adapter/target.rs:382` | read |
| H8 | The adapter parses the whole of stdout as one JSON value for the event fields. `gh pr create` has no `--json` and prints a URL | `forge_action_adapter/mod.rs:462-476`; `gh pr create --help` (2.74.2) | read + command |
| H9 | A keyed operation row is permanent: a resubmit with the same key returns the old op, even a failed one | `operation/driver.rs:129-139` | read |
| H10 | `parked` decides only whether the MCP call waits. Forge event kinds are all success events, so a parked failure wakes no one | `transport.rs:919-936`; `dispatcher/mod.rs:71-75` | read |
| H11 | `task_candidates` (`track_id`, `producer_attempt_id` = `tasks.id`, `commit_sha`) is immutable. Only a passed gate, or an ungated report, writes `done` | `migrations/0113…:53-69`; `0097…:107-108`; `git_candidate/verification.rs:40-41` | read |
| H12 | The delivery commits only a non-empty index, so an attempt that changed nothing pins its predecessor's commit | `forge_git.rs:66` | read |
| H13 | `head_upstream(repo_root)` gives `remote`, `merge` and `url`, the effective **fetch** URL. `git push <remote>` would use the remote's pushurl instead | `workspace_lease/upstream.rs:36-60`, `:89` | read |
| H14 | `gh -R` accepts https and scp/ssh URLs but not a filesystem path | `gh pr list -R https://…`, `-R git@…:a/b.git` (both reached the host), `-R /tmp/x.git` (refused) | command |
| H15 | This host pushes over https through the global helper `!/usr/bin/gh auth git-credential`, and gh's token is in `~/.config/gh/hosts.yml`. The kernel service sets no GH_* variable | `git config --global --get-regexp credential`; `grep -c oauth_token hosts.yml` = 5; `neige-next.service{,.d}` | command |
| H16 | A managed directory is `git init` on `main` with no remote | `workspace_materialize.rs:305-340` | read |
| H17 | The FE labels every `calm.track.*` tool except `rename` as a read ("Reading the track") | `fe/core/domain/conversation.ts:994-1000` | read |
| H18 | The test `gh` shim treats `--repo` as a bare git dir. `pr create` returns an existing PR by head, and a new PR's `headRefOid` is the remote's branch tip | `tests/support/gh_shim.rs:103-205` | read |
| H19 | A forge child gets proxies only from settings. 4140 has none, and this host reaches github.com directly (`git ls-remote https://github.com/keanji-x/neige-calm.git` with every proxy variable unset returned HEAD) | `terminal_adapter.rs:972-980` | read + command |

## 2. Decisions

- **D1 A kernel tool: `calm.track.publish {idempotency_key, title, body}`**, Planner-only
  (`visible_to_roles: [Planner]` plus `require_role`). *Why:* the equality rule needs the database,
  which a plugin lowering cannot read (H1). The kernel already runs its own forge actions (H2), and
  git-forge is not on 4140 today (H3).
- **D2 Destination.** From the upstream of `neige/track-<id>` itself (H13), read by branch name
  in the track worktree (#2112: the kernel copies the checkout's upstream onto the branch when it
  makes it; the branch the main checkout is on later does not matter): `url` is both the push
  destination and gh's `--repo` (H14), so the two cannot diverge; the base is `merge` without
  `refs/heads/`. With no upstream, or remote `.`, it refuses: `refused: publish-no-upstream:
  neige/track-<id> has no upstream remote to push to; set one with git -C <worktree> branch
  --set-upstream-to and retry`.
- **D3 The equality rule.** The tip is `git rev-parse --verify refs/heads/neige/track-<id>^{commit}`
  (isolated git, in `repo_root`). It must equal the `commit_sha` of a `task_candidates` row of this
  track whose attempt has `status = 'done'` (H11). This is one SQL read and adds no new state.
  *Why "a done candidate equal to the tip" and not "the latest candidate":* since S2 every attempt
  commits, so this tip means the branch holds nothing unverified. An empty attempt re-pins the same
  commit (H12), which "latest" would refuse. Refusals, answered before any operation exists:
  - `refused: publish-not-a-candidate: neige/track-<id> is at <tip>, which no attempt of this track
    produced (latest candidate <sha>, attempt <id>, <status>). A commit made after the last attempt
    is not verified: let a task produce it, or undo it.`
  - `refused: publish-candidate-not-done: <tip> is the candidate of attempt <id>, which is <status>;
    only a done attempt's commit can be published.`
- **D4 One credential split for every kernel git script (the defect class of H5 and H6).**
  `forge_git.rs` gains `FORGE_SHELL_PRELUDE`, which defines two functions:
  - `neige_git() { env -u GH_TOKEN -u GITHUB_TOKEN -u GH_ENTERPRISE_TOKEN -u GITHUB_ENTERPRISE_TOKEN git "$@"; }`
  - `neige_gh() { (cd / && env -u SSH_AUTH_SOCK -u GIT_SSH_COMMAND gh "$@"); }`

  The prelude goes only where a script runs hooks, filters, helpers or the network: every `git`
  in `GIT_COMMIT_SCRIPT` (the Planner commit), in `GIT_DELIVERY_SCRIPT` (the delivery commit), in
  the publish action script, and in the plugin's `git.worktree.add` (now `sh -c "<prelude>;
  neige_git worktree add …"`) becomes `neige_git`; every `gh` in the publish script becomes
  `neige_gh`. Every script of the plugin's `git.commit` kind gets the prelude too and calls
  `neige_git`: the probe's `git status` runs `core.fsmonitor` and clean filters, the output
  probe's `git log` runs `gpg.program` under `log.showSignature` (review rounds 1 and 2; 4140 has
  no git-forge, so no persisted `git.commit` op carries the old probes, H3).
  `GIT_LEASE_PROVENANCE_SCRIPT` (reused by the sampler, H7), `GIT_DELIVERY_PROBE_SCRIPT` and
  `GIT_DELIVERY_OUTPUT_PROBE_SCRIPT` run only `rev-parse`, `merge-base --is-ancestor` and
  `worktree list`, which run no repository code, and stay byte-identical. Git keeps HOME, the proxies and SSH_AUTH_SOCK (an
  ssh push needs them); a GitHub https push authenticates through the user's global helper and
  gh's `hosts.yml`, as on 4140 (H15). So whatever the repository selects (hooks, fsmonitor,
  filters, credential helpers, `core.sshCommand`) runs without a GitHub token, and gh runs from `/`,
  outside any repository, without the ssh keys.

  *Why env and not `-c` overrides:* one rule covers every helper class. Hooks keep working (S2's
  T4b holds a delivery with a pre-commit hook). It matches the materialize boundary (H5). Only
  `argv` changes and no probe does, so no persisted op conflicts (H7).
- **D5 The publish script.** `GIT_TRACK_PUBLISH_SCRIPT` takes `$1 sha $2 branch $3 url $4 base
  $5 title $6 body`:
  - `neige_git push --porcelain "$3" "$1:refs/heads/$2" >&2`. The refspec sends only that commit
    and its ancestors. It never forces; a non-fast-forward push fails with git's message.
  - If `neige_gh pr view "$2" --repo "$3" --json state` does not show `OPEN`, it runs
    `neige_gh pr create --repo "$3" --head "$2" --base "$4" --title "$5" --body "$6" >&2`.
  - `neige_gh pr view "$2" --repo "$3" --json number,headRefOid` is the **only stdout** (H8) and
    must show `headRefOid == $1` (exit 21 otherwise). It feeds `forge.pr.opened{pr_number,
    head_sha}` directly on the live path.
- **D6 The operation.** `submit_forge_action_with_key(GIT_FORGE_PLUGIN_ID, track, planner card,
  cwd = the track worktree, …)`. Idem key `track.publish:<idempotency_key>` (keys are
  permanent, H9, so a failed publish retries under a new key; the sha is in the payload, so the
  same key over a moved tip is refused `publish-key-reused` with `idempotency_payload_conflict`); `parked: false`, so the Planner
  waits (300 s) and gets success or failure inline (H10). Probe: landed iff `neige_git ls-remote
  "$3" refs/heads/$2` prints `$1` and the open PR's `headRefOid` is `$1`; output probe = D5's last line.
- **D7 Managed tracks, and attached tracks without a worktree, are out of scope** and get
  `refused: publish-needs-track-worktree: only an attached track with its own worktree can be
  published (a managed track has no remote)` (H16).
- **D8 Prompt.** `prompts/planner.md`, a new bullet after `:77`: "To deliver an attached track, when
    `neige/track-<id>` is at a done attempt's commit call `calm.track.publish` (title, body): it
    pushes that commit and opens or reuses the PR against the upstream branch, and refuses a
    commit made after the last attempt. Then merge as your template says." And
  `templates/builtin/issue-development.md:69`: "…open a PR with calm.track.publish…".
- **D9 FE:** `TRACK_PUBLISH_TOOL` in `fe/core/keys/mcp-tools.ts` and a label branch before the
  prefix fallback ("Publishing the track" / "Published the track"), as the file requires (H17).
- **Deploy note (4140):** install git-forge, for the template's `gh.pr.merge` and `gh.issue.*` (H3).

## 3. Change list (about 250 production lines, no migration)

- `calm-types/src/forge_git.rs`: `FORGE_SHELL_PRELUDE`, prepended to the action scripts D4 names, and
  `GIT_TRACK_PUBLISH_SCRIPT` with its probe and output probe (D5, D6).
- The prelude is carried by the plugin's `git.commit` and `git.worktree.add` lowerings
  (`plugins/git-forge/main.rs`) and by `delivery_argv` (not its probes) (`git_candidate/delivery.rs`).
- `mcp_server/tools/track_publish.rs` (new, about 180 lines): descriptor, role check, the D2, D3
  and D7 refusals, payload and submission. `prompts/tools/calm.track.publish.md`. Registered in
  `tools/mod.rs`.
- Prompt and template (D8). FE (D9).

## 4. Gates and registries

- `tests/goldens/mcp_tool_registry.json`, `mcp_tools_list_role_filter.rs:38-44`,
  `mcp_assistant_tool_gate.rs:26-83`, `issue_development_planner_prompt.txt`
  (`REGEN_PLANNER_PROMPT_GOLDEN=1`), and the plugin's argv tests (`lowers_git_commit`,
  `lowers_git_worktree_add`, `git_commit_lowering_uses_shared_scripts_as_drift_lock`), and
  `delivery_argv_joins_provenance_and_delivery_scripts` (`git_candidate/tests.rs:1113`), whose
  argv gains the prelude.
- FE: the `conversation.test.ts` label; `(cd fe && npm ci && npm run lint && npm run build && npm test)`.
- `tests/support/gh_shim.rs`: `pr view --json state` by head; for an open PR, `pr view` and
  `pr create` read `headRefOid` live (`git --git-dir "$repo" rev-parse refs/heads/$head`), and a
  merged PR keeps its stored value; an argv log per invocation, and whether `GH_TOKEN` was set.
  Then `scripts/local-ratchet-gates.sh`.
- Not triggered: migrations, OpenAPI, `wire.ts`,
  `scripts/ci/ratchets/*`, the event-version lockstep.
- S2b edits `planner.md:76` in parallel, so rebase after it and regenerate the golden.

## 5. Tests

Fixtures: a local bare origin and a clone (`support::git_helpers`); the track worktree from the
production `ensure_track_worktree` (`test_seams::attach_track_worktree_for_test`); candidates from
the real delivery with the test-played worker of `track_worker_cwd.rs`; the `gh` shim on PATH under
`support::forge_env::{FORGE_ENV_LOCK, EnvGuard}`, with `--repo` = the bare origin's path (H18).
No real repository and no network. New file `tests/cases/track_publish.rs` in `mcp_integration_suite`.

| Test | Pins | Mutation (one production line) |
|---|---|---|
| P1 `publish_pushes_the_candidate_and_opens_its_pr`: a done attempt gives candidate C; the result has `pr_number` and `head_sha == C`; origin `neige/track-<id>` == C; the shim PR's `headRefOid` == C; one `forge.pr.opened{head_sha: C}`; the shim log has no probe invocation (the live stdout path completed the op); the clone is unchanged | D5, D6 | M1: the script's push line is dropped |
| P2 `publish_refuses_a_commit_made_after_the_last_attempt`: after C, the Planner's `git.commit` adds D. The refusal is `publish-not-a-candidate` and names D and C; origin has no track branch; no forge op row | D3 | M2: the candidate query ignores `commit_sha` |
| P3 `publish_refuses_a_failed_attempts_candidate`: the only attempt writes a file and calls `calm.task.fail`. The refusal is `publish-candidate-not-done` and names the attempt and `failed` | D3 | M3: the status test accepts `failed` |
| C1 `publish_git_never_sees_a_github_token`: `GH_TOKEN=sentinel` in the kernel env, and a `pre-push` hook writes `${GH_TOKEN-unset}` to a file. After P1's flow the file says `unset`, and the shim log shows gh got the token | D4 | M4: `neige_git` drops `-u GH_TOKEN` |
| C2 `a_delivery_commit_hook_never_sees_a_github_token`: the same, with a `pre-commit` hook, through a real delivery | D4 | M5: the delivery script's commit line calls `git`, not `neige_git` |
| C3 `planner_git_commit_and_its_probe_never_show_repository_code_a_github_token` (mcp_git_forge_plugin): a failing `pre-commit` hook makes the Planner's `git.commit` fail, so its probe runs `git status`; a `core.fsmonitor` script appends `${GH_TOKEN-unset}`, and every line says `unset` | D4 | M6: the commit probe's `status` calls `git`, not `neige_git` |
| C4 `planner_git_commit_output_probe_never_shows_gpg_a_github_token` (mcp_git_forge_plugin): a `pre-commit` hook stashes the change and fails, so the probe finds a clean tree (landed) and the output probe runs `git log` on a signed HEAD under `log.showSignature`; a `gpg.program` script appends `${GH_TOKEN-unset}`, and every line says `unset` | D4 | M7: the output probe's `log` calls `git`, not `neige_git` |

Ordinary tests: managed-track and no-upstream refusals; a second candidate is pushed
fast-forward and reuses the PR; a non-fast-forward push fails with git's message; a failed publish
is retried under a new key; the push goes to `url` even when the remote has another `pushurl`.

Predicted red sets over `track_publish` and the plugin's unit tests:

| Mutation | Red |
|---|---|
| M1 | P1, C1, fast-forward reuse, non-fast-forward message, retry under a new key, pushurl. Without the push the remote branch never reaches the candidate, so the shim's live `headRefOid` (read from the remote's `refs/heads/<branch>`) is missing or stale, the exit-21 check fails the publish, and each test's pushed-branch or PR-head assertion fails; C1's pre-push hook never runs |
| M2 | P2 |
| M3 | P3 |
| M4 | C1, C2, C3, C4 (they share the prelude) |
| M5 | C2 |
| M6 | C3 |
| M7 | C4 |

## 6. KNOWN GAPS

- A finished but undeleted track keeps its checkout and branch (about 48 MB each; 4140 has 27
  tracks) until the user deletes the track.
- HOME stays in git's env (the global helper needs it), so a hostile repository helper can still
  read `~/.config/gh/hosts.yml`. This is the same boundary as `isolated_git_command` (H5).
- An operator whose `git push` authenticates only through a GH_* variable gets an inline push
  failure. A rewritten branch cannot be published (no force push).
- The PR base is the checkout's upstream at publish time, not at track creation. Fixed by #2112:
  it is the track branch's own upstream, recorded at track creation.
- A `url.*.pushInsteadOf` can rewrite the direct push URL to another repository. The exit-21
  PR-head check then fails the publish; nothing refuses it ahead of time.
- A Planner can still push or open a PR by hand in a terminal. #1868 removes the plugin's
  `gh.pr.create`; MCP publication uses `neige.dev.publish`.
- The plugin's other `gh.*` lowerings still run `gh` in the track worktree with the full forge env.
- The tip can move between the D3 check and the push; the pushed sha was a done candidate and the
  tip when checked, and a later commit is published by the next call.

## 7. Implementation notes (where the code differs from the text above)

- D5 "fails with git's message": the forge adapter sends the action's stderr to `/dev/null` and a
  failed action is settled by its probe, so a rejected push reaches the Planner as `-32409
  publish-failed: operation <id>: forge action process dead and probe reports not landed …`.
  Surfacing stderr would change the adapter for every forge action; the test pins the failure
  and the untouched remote instead.
- D3 is one SQL read of the track's candidates joined with their attempt's status; the tip is
  matched against them in Rust (`check_tip`), so M2 mutates that comparison, not the SQL.
- D6: the probe's gh read is `pr view --json headRefOid,state`, distinct from the output probe,
  so P1 can prove from the shim log that no probe ran.
- §4 shim: `pr merge` also reads an open PR's head live and records it as the merged head, so
  view, create and merge agree.
- §4: `track_write_point_registry` is triggered after all: the D7 refusal test nulls
  `workspace_worktree_path`, as S2's T5 does, so it is listed with a reason.
- D5's PR-head check re-reads `pr view` up to five times two seconds apart before exit 21
  (GitHub moves an open PR's head asynchronously after a push). Untested: the shim reads the head
  live, so it cannot model the lag.
