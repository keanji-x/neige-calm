# #2058 S2 — a checks call that waits for the head's CI

**Owner rules.** (1) Simple first, pain points only; hypotheticals are one-line KNOWN GAPS.
(2) Compatibility means the 4140 database only. (3) Only hazards this change introduces get
mechanism. (4) One consistent agent-facing surface. (5) The kernel makes causes findable; it does
not classify them for the Planner.

**Outcome.** `gh.pr.checks` (MCP name `plugin.dev.neige.git-forge_gh.pr.checks`) stops being a
one-shot read. It returns the standard parked-forge receipt at once, and the kernel wakes the
Planner with `forge.pr.checks` when the PR's checks for the current head finish (success or
failure), when the PR is conflicting, when the head moves, or at the parked deadline. The result
tells `no_checks` apart from `pending` and carries `head_sha` and `mergeable`. No new mechanism:
the forge-action operation already parks; only the lowering, one wake sentence, the tool
description and one template line change.

## 1. Problem (from #2058, track `1022c771…`, PR #2042)

- The fold maps "no checks at all" to `pending` (`git_actions.rs:237`, comment at `:226`).
- PR #2042 conflicted with main, so GitHub ran no `pull_request` workflow. The Planner reported
  "CI pending" three times and ended its turn; nothing would ever wake it again.
- The Planner twice declared Codex tasks (`observe-ci-*`) only to wait for CI: a worker as a timer.
- Each read of an already-known state woke the Planner once more, for a turn that read the
  state and ended.
- No forge tool exposes mergeability, so it fell back to `gh run list` and `gh api pulls/2042`.

## 2. Facts

Verified at fd26e2267 by reading the code, or by the command shown (gh 2.74.2, 2026-10-04).

| # | Claim | Where | Verified |
|---|---|---|---|
| F1 | git-forge is now the builtin `dev` plugin; `gh.pr.checks` lowers to one `gh pr view --json statusCheckRollup --jq PR_CHECKS_JQ`, with idem key `gh.pr.checks:{repo}:{pr}[:{attempt}]`, probe `gh pr view --json state`, output probe = the action argv, and `parked: false` | `builtin_plugins/dev/mod.rs:5`; `dev/git_actions.rs:241-290` | read |
| F2 | The jq fold: any failure → `failure`; `length == 0` or any pending → `pending`; else `success` | `git_actions.rs:223-239` | read |
| F3 | Every forge action, read or write, is an operation that returns `SpawnOutcome::Parked`; its observer waits on the child and completes from the result file | `forge_action_adapter/mod.rs:1270-1492` | read |
| F4 | The payload's `parked` only decides whether the MCP call waits: `true` returns the existing result or the pending receipt `{op_id, parked:true, status:"pending", message, completion_event}` at once; `false` waits on the op | `transport.rs:939-957`; `forge_receipt.rs:13-31`; `prompts/forge-action/pending.md` | read |
| F5 | Parked siblings today: `gh.pr.list`, `gh.pr.merge`, `gh.issue.close`, `gh.issue.comment`. Non-parked: `gh.pr.diff`, `gh.issue.view`, `gh.pr.checks` (#1873 D5 flipped checks to `false`) | `git_actions.rs:191,219,288,367,436`; `git_actions/issue/tests.rs:23`; `1873-dogfood-followups.md:64` | read |
| F6 | Deadline: `now + 900 s` when parked, 300 s otherwise (`NEIGE_FORGE_DEADLINE_SECS` overrides both) | `transport.rs:841`, `:1092-1100` | read |
| F7 | The parked sweep runs on the scheduler reconcile tick (default 300 s) and at boot. A live pid before the deadline is left alone | `scheduler/mod.rs:55`, `:1528-1535`; `driver.rs:852-854` | read |
| F8 | Past the deadline a live forge child is killed, then `recover_parked(alive=false)` runs the probe; "landed" runs the output probe and completes the op **succeeded with that output**, appending the event in the same tx | `driver.rs:897-1021`; `forge_action_adapter/mod.rs:1542`, `:1568-1586`, `:1035-1083`, `:943-998` | read |
| F9 | Boot: a live child of this boot is re-attached (pid poll every 2 s); a dead one completes from its result file, else from the probe | `forge_action_adapter/mod.rs:50`, `:1508-1566` | read |
| F10 | A failed forge op appends no event, so nothing wakes the Planner | `forge_action_adapter/mod.rs:810-837` | read |
| F11 | `forge.pr.checks` wakes the Planner (catch-up kind, push `true`); the turn text is `Forge checks for PR #N read <conclusion>. Re-read the track state.` | `dispatcher/mod.rs:71`, `:131-137`, `:1564-1571`; `observation.rs:433-439` | read |
| F12 | So today a checks call returns the conclusion inline **and** its event wakes the Planner again: the "already-known state" wake. #1873 recorded it as a KNOWN GAP | `transport.rs:953-957` + F11; `1873-dogfood-followups.md:124` | read |
| F13 | A resubmit with the same key and payload hash returns the existing op (even terminal); a different hash is `idempotency_payload_conflict`. The hash covers the event field map, context and probe, not argv | `operation/driver.rs:131-139`; `transport.rs:782-790` | read |
| F14 | `result.event` is the built payload map, so it keeps fields the `Event` struct lacks; publish already returns `url` this way while `forge.pr.opened` has no `url` | `forge_action_adapter/mod.rs:740-748`; `dev/publish.rs:254`, `:333`; `event.rs:707-711` | read |
| F15 | `Event::ForgePrChecks` is `{track_id, pr_number, conclusion: String}`; `track_vcs` decodes stored rows with `?`, so a new required field would break every track holding an old row | `event.rs:720-725`; `track_vcs/runs.rs:62`, `:451` | read |
| F16 | A parked forge op of a track makes track and area deletion return 409 "in-flight forge-action; retry after it settles" | `workspace_lease/mod.rs:87-125`; `routes/tracks.rs:3152`; `routes/areas.rs:802` | read |
| F17 | gh's `mergeable` is `MERGEABLE`, `CONFLICTING` or `UNKNOWN` (merged PR #2042: `UNKNOWN`). Open PR #2024 is `CONFLICTING` with 30 finished checks (SUCCESS/SKIPPED): a conflict does not imply an empty rollup | `gh pr list/view --json mergeable,headRefOid,statusCheckRollup` | command |
| F18 | The last 20 completed `pull_request` runs: `CI` 554-713 s, every other run 19-64 s | `gh run list --event pull_request --limit 20` | command |
| F19 | The Planner prompt already says "turn-reactive, not a polling loop … Do not poll"; the template says `gh.pr.checks` returns the conclusion and to pass a new `attempt` per re-read | `prompts/planner.md:7-9`; `templates/builtin/issue-development.md:82`, `:130` | read |

**Answers to the brief.** The checks call already runs as a parked operation (F3) but makes the
MCP call wait (F1, F4). The observer is the adapter's child wait plus the 300 s sweep, with a
900 s parked deadline and boot re-attach (F6-F9). The "already-known state" wake is F12; it
disappears with this change for every call that parks, because the receipt replaces the inline
result and the wake becomes the only delivery (no separate fix). One race remains: if the first
read settles before `transport.rs:940` looks up the op, that call returns the terminal result
inline and the event still wakes once (KNOWN GAP). A parked call returns a handle, not a blocked
turn (F4).

## 3. Decision

- **D1 Result fields.** `PR_CHECKS_JQ` reads `--json headRefOid,mergeable,statusCheckRollup` and
  prints `{conclusion, mergeable, head_sha}`. `conclusion`: `failure` if any check failed, else
  `pending` if any is unfinished, else `no_checks` for an empty or null rollup, else `success`
  (the per-check rules of F2 are unchanged). `failure` outranks unfinished checks on purpose:
  one failed check means this head cannot go green, so the call fails fast. `mergeable`: gh's value lowercased (`mergeable`,
  `conflicting`, `unknown`). `head_sha` is the name `forge.pr.opened` and `gh.pr.diff` already use.
  The event field map extracts all three; `forge.pr.checks` persists only `conclusion` (F14, F15).
- **D2 The wait.** The action argv becomes `sh -c PR_CHECKS_WAIT_SCRIPT sh <pr> <repo> <secs> <jq>`.
  The script reads once (the D1 command), records the first `head_sha`, and exits 0 printing
  that read's JSON as soon as `conclusion` is `success` or `failure` (so as soon as any check
  fails, even while others still run), or `mergeable` is
  `conflicting`, or `head_sha` differs from the first one. Otherwise, or when `gh` fails, it
  sleeps `<secs>` and reads again. `<secs>` is `PR_CHECKS_POLL_SECS = 15` in the lowering (one
  GraphQL call per 15 s per waiting call). It is written as short `concat!` pieces like
  `PR_CHECKS_JQ`, under the prose ratchet.
- **D3 The deadline is the existing one.** `parked: true` gives 900 s (F6), above the observed CI
  time (F18). Past it the sweep kills the script and completes the op from the output probe (F8),
  so the Planner is woken with a snapshot (`pending` or `no_checks`, with head and mergeable) and
  decides whether to wait again. The output probe is therefore the **one-shot** D1 read, never the
  waiting script, and the probe stays `gh pr view --json state`. No second clock in the script.
- **D4 `no_checks` + `conflicting` returns at once** (the `conflicting` rule of D2 covers it, and
  equally `success` + `conflicting`, F17). `no_checks` on a mergeable or unknown PR keeps waiting,
  because a fresh push registers its checks a little later; at the deadline it is reported as is.
- **D5 Surface.** `parked: true` is how every waiting forge tool answers (F4, F5): the call
  returns the receipt with `completion_event: "forge.pr.checks"`, the Planner ends its turn, and
  the event wakes it. Input is unchanged (`repo`, `pr`, optional `attempt`). Repeating the call with
  the same arguments returns the recorded result, `result.event = {track_id, pr_number,
  conclusion, mergeable, head_sha}` (F13). One dispatch path serves MCP and the CLI
  (`transport.rs:643-654`).
- **D6 Wording.**
  - Wake (`observation.rs:438`): `Forge checks for PR #N read <conclusion>. Repeat the same
    gh.pr.checks call to read its head_sha and mergeable.`
  - Manifest description: "Wait for a pull request's checks on its current head. Returns a pending
    receipt; the kernel wakes you with forge.pr.checks when every check has finished or as soon
    as any check fails, when the PR is conflicting, when the head moves, or after about 15-20
    minutes. Use a new attempt for each wait."
  - Template `:82`: gh.pr.checks waits for the head's CI and wakes you; do not declare tasks to
    watch CI. A `conflicting` PR gets no `pull_request` workflow run until it is synced with its
    base.

## 4. Oracle traces

**A. Push → wait → CI fails.**

| # | Actor | Step | Evidence of the step |
|---|---|---|---|
| A1 | Planner | `neige.track.publish` pushes head H; PR #N | `forge.pr.opened{head_sha:H}` |
| A2 | Planner | `gh.pr.checks {repo, pr:N, attempt:"H-1"}` | lowering D1/D2, `parked:true` |
| A3 | Kernel | submit op `…:gh.pr.checks:R:N:H-1`, deadline now+900 s; MCP returns the receipt | `transport.rs:939-951`; op `parked` |
| A4 | Planner | ends its turn | `planner.md:9` |
| A5 | Script | read 1: `{pending, mergeable, H}` → sleep 15 s; reads repeat | shim/gh log: one `pr view` per 15 s |
| A6 | GitHub | a job fails; read k: `{failure, mergeable, H}` → print, exit 0 | result file `.code` = 0 |
| A7 | Kernel | observer completes the op succeeded and appends `forge.pr.checks{N, failure}` in one tx | `forge_action_adapter/mod.rs:943-998` |
| A8 | Kernel | dispatcher pushes `ForgePrChecks`; turn text "…read failure. Repeat…" | `dispatcher/mod.rs:1060-1066` |
| A9 | Planner | repeats A2's call: the recorded `result.event` (`head_sha: H`); declares the fix task | `driver.rs:131-136` |

**B. Conflicting PR.**

| # | Actor | Step | Evidence of the step |
|---|---|---|---|
| B1 | GitHub | main moved; PR #N is `CONFLICTING`; no `pull_request` run | F17 |
| B2 | Planner | `gh.pr.checks {…, attempt:"H-2"}` → receipt; ends turn | A3 |
| B3 | Script | read 1: `{no_checks, conflicting, H}` → exit 0 within seconds, no sleep | D2, D4 |
| B4 | Kernel / Planner | `forge.pr.checks{no_checks}` wake; the repeated call shows `conflicting`; it syncs the branch (S1) | A7-A9 |

**C. Deadline.** No settle by now+900 s → the next sweep (≤ 300 s later) kills the script, the probe
says landed, the output probe reads `{pending, …}`, the op succeeds and the wake says `pending` (F8).

## 5. Rejected alternatives

- A subscription or webhook system: the parked operation already waits and wakes (issue).
- Keep the read synchronous with a longer wait: CI takes 9-12 min (F18); the MCP call is bounded at 300 s (F6).
- Calendar or worker tasks as timers: extra turns and tasks, the observed waste.
- `head_sha`/`mergeable` on `Event::ForgePrChecks`: needs `Option` or a 4140 backfill (F15); decision 1.
- A separate mergeability tool: mergeability decides whether checks will ever run, so it belongs in the same answer.
- A script budget shorter than the deadline: a second clock; F8 already ends the wait with a snapshot.
- A per-tool deadline field: new knob; 900 s covers the observed CI.

## 6. Hazards

Introduced by this change:

- **I1 A failed wait wakes no one** (F10); before, the failure came back inline. The script never
  exits on a `gh` failure (D2), so only a GitHub or auth outage that also fails the probe at the
  deadline loses the wake. Mechanism: none beyond D2; KNOWN GAP, follow-up issue (decision 2).
- **I2 Delete fence.** A waiting call blocks track and area deletion with 409 for up to about
  20 min (F16; the old 300 s bound was never reached by a seconds-long read). Accepted: bounded,
  and the 409 says to retry (decision 3).
- **I3 Hash change.** The new probe argv in the hash means a pre-deploy `(repo, pr, attempt)` call
  repeated afterwards gets `idempotency_payload_conflict`; a new `attempt` works (F13).
- **I4 Worker callers** get the receipt; the event wakes the Planner, not the worker.

Pre-existing, not addressed: every parked forge failure is silent (F10, #1830 H10); publish still
returns inline and wakes; a fast workflow can read `success` before slower ones register; other
gh lowerings run `gh` in the track worktree with the full forge env (#1830 KNOWN GAPS); without
`attempt` the key is fixed (#1873 D5).

## 7. Slice (one PR) — review tier L1

L1: no migration, no event or wire change, no new authority, credential env or operation phase;
it reuses the forge-action contract (decision 1 keeps the event unchanged).

Change list: `dev/git_actions.rs` (jq, wait script, lowering, `parked: true`);
`plugins/git-forge/manifest.json` (description); `calm-types/src/observation.rs` (D6);
`templates/builtin/issue-development.md:82` (D6); tests: `git_actions/tests.rs`
(`lowers_gh_pr_checks`), `tests/cases/forge_pr_checks.rs`, `forge_template_e2e.rs:610-630` (receipt,
then the event), `support/gh_shim.rs` (the three-field view; `mergeable` seeded per PR, default
`MERGEABLE`), and the expectations in `codex_forge_e2e*` (compiled, not run on this host).

Acceptance: A, B and C hold through the production MCP path with the `gh` shim; every forge tool's
receipt has the same shape; `scripts/local-ratchet-gates.sh` green.

| Test | Pins | Mutation (production only) |
|---|---|---|
| T1 `gh_pr_checks_reports_no_checks_and_mergeability`: the F2 table (including the mixed `failed-run` row, `forge_pr_checks.rs:83`) plus empty and null rollups, through the lowered output probe argv | D1 | M1: `length == 0` maps to `pending` |
| T2 `a_conflicting_pr_returns_at_once`: empty rollup, `CONFLICTING`; receipt, then the event `no_checks` within 10 s (under one interval); the repeated call shows `conflicting` | D4 | M2: drop the `conflicting` exit |
| T3 `a_checks_wait_parks_until_ci_fails`: pending rollup; receipt with `completion_event`; wait until the shim log shows the first read, then 2 s more (under one interval): the op is parked and no event exists; reseed `[COMPLETED FAILURE]`; event `failure` | D2, D5 | M3: the settle test also accepts `pending` |
| T4 `a_moved_head_ends_the_wait`: default deadline (900 s); pending; wait for the first read in the shim log, then 2 s more: parked, no event; only then commit to the PR branch; the event must arrive within 35 s (two intervals, far below deadline recovery); the repeated call's `result.event.head_sha` is the new commit (the persisted event has no `head_sha`, `event.rs:721`) | D2 | M4: drop the head comparison |
| T5 `a_wait_past_its_deadline_wakes_with_the_current_state`: `NEIGE_FORGE_DEADLINE_SECS=1` under the env lock; pending; wait for the first read, sleep 2 s; the op is still parked and no event exists **before** `sweep_parked()` runs; after it, op succeeded and event `pending` | D3 | M5: the payload carries no probe |
| T6 `a_failed_check_ends_the_wait_while_others_run`: the mixed rollup of `forge_pr_checks.rs:83` (`shard` IN_PROGRESS, `test` COMPLETED FAILURE); receipt, then the event `failure` within 10 s | D1, D2 | M6: the jq fold tests `pending` before `failure` |

Predicted red: M1 → T1, T2 (T2 reads `pending`). M2 → T2 (no event within 10 s). M3 → T3, T4,
T5 (each op completes right after its first read, so the pre-trigger "parked, no event" assertion
fails). M4 → T4 (no completion within 35 s; deadline recovery cannot run before 900 s). M5 → T5
(past the deadline with no probe the op fails and appends no event). M6 → T1 (`failed-run` reads
`pending`), T6 (first read is `pending`, so no event within 10 s). T3's reseed has no unfinished
check, so M6 leaves it green. Ordinary tests: the wake text, the
`gh` failure retry, the lowering's argv.

Source-invariant gates checked: `gate-prose-ratchet.sh` (the script in short literals),
`gate-1316-terminology-ratchet.sh` (template and docs prose), `gate-sync-event-version-lockstep.sh`
and `gate-web-compat-version-lockstep.sh` (not triggered: no event change),
`tests/goldens/events/forge_pr_checks.json` (unchanged), `issue_development_planner_prompt.txt`
(no checks text today; re-run to confirm), `mcp_git_forge_plugin.rs:95-112` (tool list unchanged),
`tests/cases/*invariant*` and `scripts/ci/ratchets/*` (no matching surface).

## 8. KNOWN GAPS

- I1: an outage that spans the deadline leaves the wait failed and the Planner asleep.
- I2: deleting a track while its wait is parked returns 409 until it settles.
- A repository with no CI waits the full deadline before `no_checks` arrives.
- A PR closed while the call waits is reported at the deadline.
- CI longer than about 15 min costs one extra wake and call per deadline.
- Repeated calls with new attempts run concurrent waits and wake once each.
- A failure wakes the Planner while other checks may still be running; the next call shows them.
- If the first read settles before the transport looks up the op, the result comes back inline
  and the event still wakes once.

## 9. Decisions (owner, review round 1)

1. **Head and mergeability travel in `result.event` only** (F14, the publish `url` precedent); the
   wake tells the Planner to repeat the call. `Event::ForgePrChecks` is unchanged, so the slice
   stays L1.
2. **Silent failures of parked forge ops** (I1 and the pre-existing class): one follow-up issue,
   filed by the orchestrator; not in this PR.
3. **Delete fence** (I2): accepted as is.
4. **Fail-fast on `failure`** is intended (D1, D2, T6).

## 10. TO-VERIFY (orchestrator, 4140)

- Existing rows and their conclusions (the event shape does not change, so this is evidence only):
  `sqlite3 -readonly ~/.local/share/neige-next/data/calm.db "SELECT json_extract(payload,'$.conclusion'), COUNT(*) FROM events WHERE kind='forge.pr.checks' GROUP BY 1;"`
- The #2042 symptom (three `pending` reads and their wakes):
  `sqlite3 -readonly ~/.local/share/neige-next/data/calm.db "SELECT id, at, json_extract(payload,'$.conclusion') FROM events WHERE kind='forge.pr.checks' AND json_extract(payload,'$.track_id')='1022c77156b84072b02b97da348ff6ac' ORDER BY id;"`
- No checks op in flight at deploy (I3 needs none):
  `sqlite3 -readonly ~/.local/share/neige-next/data/calm.db "SELECT id, phase FROM operations WHERE kind='forge-action' AND idempotency_key LIKE '%:gh.pr.checks:%' AND phase NOT IN ('succeeded','failed');"`
- No deadline override on the service (D3 assumes 900 s): `systemctl --user cat neige-next.service | grep -i FORGE_DEADLINE`.
