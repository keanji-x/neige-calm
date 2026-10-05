# Commit message ownership (#2139 R1)

Review tier: L2. The change adds a migration on `task_git_deliveries` and widens the worker
report contract (`neige_task_done`), which is an authority boundary.

Status: design only. Code facts were checked at `origin/main` `47ffdfc2a`.

## Owner's standing rules

1. Do not expand without limit. Use the fewest mechanisms that cover the **observed** symptoms.
   Record each hypothetical case as a one-line KNOWN GAP.
2. Compatibility means only the 4140 production DB. There is no general compatibility story.
3. No hardcoded application assumptions in the kernel. The kernel never knows about
   `OWNERSHIP-CHANGE`, DCO or conventional commits. Repository policy stays in the repository.
4. Model required fields as required types. A missing value must have a domain meaning, not hide
   behind `Option`.

## 1. Observed symptom and evidence

Track `028f636e…`, PR #2133 (#2129). The commit evidence comes from the candidate refs in this
repository, `git for-each-ref refs/neige/candidates/028f636ee14544b49761938e3178798c`:

| Delivery | Commit | Subject | Readonly `fe/` paths |
|---|---|---|---|
| `5db130e6` | `0e337d38f1` | `neige: attempt …:implement-ci-wake completed (delivery …)` | 3 (`schemas.ts`, `schemas.test.ts`, `generated/wire.ts`) |
| `f8ba15d3` | `24f470503c` | `neige: attempt …:fix-check-locators completed …` | 0 |
| `c096e99a` | `01c30272c7` | `neige: attempt …:fix-golden-coverage completed …` | 0 |
| `be3b8d90` | `04db7a6b31` | the terminal `commit-tree` rewrite, pinned by the no-op Codex task | 3, with trailers |
| `4a92633a` | `dacaff270d` | `neige: attempt …:fix-checks-upgrade-identity completed …` | 0 |
| `d9659f64` | `be05cf9145` | `neige: attempt …:fix-test-module-registration completed …` | 0 |

- Only one attempt touched readonly paths, and it **completed**. Its worker had the three trailers
  (event 97056) but had no way to put them into its commit.
- The PR body did carry the trailers. The Planner wrote them through `gh api`, which slice C
  replaces with `neige_dev_publish` updating the body. Only the per-commit rule failed.
- No attempt in this track ended other than `completed`. Across all 76 candidate refs in this
  repository, 16 point at a kernel commit, and all 16 subjects say `completed`.

## 2. Verified facts, with one correction to the brief

- `delivery_message` (`git_candidate/delivery.rs:152-161`) is derived from the row alone. It is
  `$1` of `GIT_DELIVERY_SCRIPT` (`calm-types/src/forge_git.rs:70-91`), which runs
  `git add -A; git diff --cached --quiet || git commit -q -m "$1"`.
- **Correction:** the commit message is **not** in the semantic hash.
  `SemanticForgePayload` (`mcp_server/transport.rs:769-778`) hashes the idempotency key, the
  event extraction table, `subject`, `context` and `probe`, and excludes `argv` on purpose. The
  message still has to live on the row for a different reason. After a crash between the report
  transaction and the submission, the scheduler rebuilds the argv from the row
  (`scheduler/git_delivery.rs:210-231`). The MCP arguments are gone by then. Once an operation
  exists, its `payload_json` freezes the argv.
- Path: `emit.rs:59-86` builds `Event::TaskCompleted`. Then
  `CardDecisionSink::commit_worker_task_report` (`decision_sink.rs:60-214`) runs one transaction:
  admit, task CAS, and `release_workspace_lease_for_card_tx(…, ReleaseDelivery::Commit(Completed))`.
  The release calls `insert_initial_delivery_tx` (`delivery.rs:281-330`). After the transaction,
  `submit_reported_delivery` runs.
- Every attempt end inserts a delivery row through `ReleaseDelivery`. That covers failed (sink and
  reaper), spawn-failed (both adapters), interrupted (card delete, plugin callback, boot reclaim)
  and `CommitAsTaskEnded`. Of these, only the done report carries anything from the worker.
- `Event::TaskCompleted` (`calm-types/src/event.rs:517`) has three other production producers
  (`scheduler/mod.rs:730,2195`, `mcp_server/tools/track_state.rs:180`), none of which has a message.
- Publish (`builtin_plugins/dev/publish.rs`, `publish_scripts.rs`) takes `title` and `body` from
  the Planner. It pushes the done candidate's sha as-is and refuses any other tip (`:92-124`).
- Merge (`git_actions.rs:401`) runs `gh pr merge --squash` without `--body`. CONTRIBUTING (:104-116)
  says the PR body becomes the default squash body.
- The ownership CI rules are in `fe/tools/ownership/validator.ts`:
  - **R-commit** (`:131-148`): each commit in base..head that changes a readonly path needs a
    matching `OWNERSHIP-CHANGE: <path> — … (#N)` line in its own message. The check is line-based,
    not a git-trailer parse.
  - **R-body** (`:151-168`): every such line in any commit must appear verbatim in the PR body.
  - **R-squash** (`:204-249`): after the push to main, the original PR commits are audited again,
    and the squash message must preserve every line.
  - The workflow also runs on `pull_request: edited`, so a corrected body re-runs the audit.

## 3. Decision 1: the carrier

| Option | Covers R-commit for the observed case | Cost |
|---|---|---|
| **(a) worker message in `neige_task_done`**, persisted on the delivery row | yes: the attempt that changes the paths writes the lines | 1 migration column, 1 optional tool field, 1 CLI option |
| (b) Planner message at publish; the kernel squashes base..tip into one commit | yes, and failed-attempt commits leave the PR | publish pushes a sha that is not a candidate. That breaks D3 (`publish-not-a-candidate`), the probe's `headRefOid == $1`, the own-commit set behind exit 22 (`$7`), and the checks wake's head identity. The squash sha also needs fixed dates so retries reproduce it. That is four mechanisms touched to fix one symptom |
| (c1) let the worker commit | yes | it contradicts the worker contract ("do not `git commit`"). The Codex sandbox cannot write the shared gitdir. Failed attempts still get kernel commits |
| (c2) the Planner gives the message at task spawn | partly | the Planner does not know which paths the worker will touch |
| (c3) the repository drops R-commit, since PRs are always squashed | yes, for this repository only | it weakens an authority audit to fit the platform. DCO and per-commit lint stay broken for other repositories |

**Pick (a), as one raw message string.** The issue suggested structured `commit_message` /
`trailers`. A raw string is one field with no kernel-side trailer model. It matches `git commit -m`
and the #1727 rule "no trailer, no parsing" (`1727-s4-candidate-binding.md:11,289`). The kernel
never interprets the text. The repository's CI is the only judge of its policy, and its R-commit
rule is line-based anyway, so a kernel-side git-trailer validator would disagree with it in both
directions.

Contract:

- `neige_task_done` gains an optional `commit_message: string`. The CLI mirror is
  `neige task done … [--commit-message <text>]`. The `every_option_is_its_schema_key` test pins it.
- In Rust, the value is `DeliveryMessage` in `git_candidate`:
  `enum DeliveryMessage { Kernel, Worker(CommitMessage) }`. `Kernel` means "no message supplied;
  the kernel writes its fixed text". That is a domain state, not a hidden missing value. Failed and
  other non-completed attempts, pre-migration rows, and done reports without the field all map to
  it. `CommitMessage` is a validated newtype (§4).
- `emit.rs` parses the field **before** the sink. An invalid message refuses the whole report with
  `invalid_params`, and nothing is written. The worker corrects the message and reports again. The
  attempt is still running, so the retry is admitted.
- The `DeliveryMessage` travels as a parameter:
  `commit_worker_task_report(identity, event, message)`, then
  `ReleaseDelivery::Commit(outcome, message)`, then
  `insert_initial_delivery_tx(…, outcome, message, now)`. It does **not** go on `task.completed`.
  That event has three other producers without a message, so the field would have to be
  optional there.
  The delivery row owns the commit, and the commit itself is the readable record.
- Every other `ReleaseDelivery::Commit` caller passes `DeliveryMessage::Kernel`. The 0144 CHECK
  refuses a supplied message with any other outcome (§6), so a wrong combination fails loudly.
- `delivery_message(row)`: `Worker(m)` returns `m` verbatim. `Kernel` returns today's
  `neige: attempt {a} {outcome} (delivery {d})`, byte for byte.
- Tool description (`prompts/tools/neige_task_done.md`) and worker contract line 3
  (`prompts/worker/head-{mcp,cli}.md`): "Optionally pass `commit_message`: the full message of the
  commit the kernel makes of your checkout. Write what the repository requires (subject, body,
  trailers). It is used only when the kernel commits your changes." The repository's own
  AGENTS/CONTRIBUTING tell the worker what that is. The kernel prompt names no repository rule.

### Oracle trace: #2129 under (a)

1. The Planner spawns `implement-ci-wake` (Codex, track worktree, kernel-delivery lease).
2. The worker edits Rust plus three readonly `fe/` paths. It reads the repository's
   AGENTS/CONTRIBUTING and calls `neige_task_done{attempt_id, result, commit_message:
   "fix(forge): include exact CI evidence in planner wakes\n\nCloses #2129\n\nOWNERSHIP-CHANGE:
   fe/core/api/schemas.ts — … (#2129)\n…(3 lines)"}`.
3. `emit.rs`: `CommitMessage::parse` accepts it. The sink transaction admits the report, flips the
   task to done, releases the lease and inserts the row with `commit_message` set.
4. After the transaction, `submit_reported_delivery` reads the row. `delivery_message` returns the
   worker text, and the script commits `0e337d38f1′` with that message and pins the candidate.
   *Crash variant:* the process dies before the submit. On boot, the scheduler finds the unsettled
   row with no operation, rebuilds the argv from the row (same message) and submits it.
5. `fix-check-locators` and `fix-golden-coverage` touch no readonly path. They may pass
   conventional messages or none (kernel text). R-commit does not look at them.
6. The Planner calls `neige_dev_publish{title, body ⊇ the 3 lines}`. The tip is a done candidate,
   so it pushes and opens PR #2133.
7. CI R-commit passes, because `0e337d38f1′` carries its own lines. R-body passes, because the
   Planner wrote the body.
8. The L2 fixes produce two more attempts, and the Planner publishes again. With slice C, the
   publish updates the body; without it, the existing body still holds the lines.
9. `gh pr merge --squash` uses the default body, which is the PR body. R-squash passes on main.

This removes the terminal `commit-tree` rewrite (and its wrong-cwd failure), the no-op Codex
re-registration task, and, together with slice C, the five `gh api` body edits.

## 4. Decision 2: validation the kernel owns

The kernel validates only what carriage needs. The bytes must go safely through argv, SQLite and
`git commit`, and git must always accept them. It never checks what the text says.

| Input | Action | Why |
|---|---|---|
| field absent | `DeliveryMessage::Kernel` | defined default |
| not a JSON string | refuse `invalid_params` | type |
| empty, or only whitespace | refuse | `git commit` aborts on an empty cleaned message, so the delivery would fail as `commit_failed` and the work would stay unpinned |
| contains NUL | refuse | argv cannot carry NUL, and git messages cannot either |
| other C0 controls (except `\t`, `\n`, `\r`) or DEL | refuse | a display text that the UI, terminal and GitHub render. Refusing beats silently stripping |
| more than 16 384 bytes (UTF-8) | refuse | bounded argv (`MAX_ARG_STRLEN` is 128 KiB), DB row and operation payload. Generous next to the trailer case (~100 bytes per line) |
| CRLF, trailing spaces, leading or trailing blank lines | **stored verbatim, not normalised** | git's default `-m` cleanup (`whitespace`) normalises at commit time. Keeping the worker's bytes keeps the row trivially reproducible |
| trailer syntax, subject length, conventional form, `Signed-off-by` | not checked | repository policy. The repository's CI judges it |

- Refusal text names the rule and the limit, for example
  `task_done: commit_message has a NUL byte`. The literals live in the existing error style.
- `CommitMessage::parse` is the one validator. Row reads go through it too, so a corrupt row fails
  loudly instead of reaching argv.
- **Crash re-submit and dedup:** the message is persisted in the report transaction, before any
  operation exists. Every argv builder reads that row, so the report handler and the scheduler
  produce the same argv. The semantic hash does not cover argv, so it is unchanged either way, and
  no compatible-hash entry is needed. A repeated done report rolls back as `REPEATED`
  (`decision_sink.rs:146,209`): the first message wins, as the first `result` does.

## 5. Decision 3: failed, canceled, interrupted and spawn-failed attempts

Their commits carry the kernel text, exactly as today. `neige_task_fail` gains nothing, and the
0144 CHECK allows a message only on `outcome = 'completed'`.

- Can the ownership check still fail on them? Yes, when a non-completed attempt changed a readonly
  path and left the change. Not observed: 0 of 16 kernel commits in this repository's candidate
  refs, and 0 in the #2129 track.
- The remedy needs no new mechanism. A `start:"upstream"` catch-up task replays the last done
  commit as one single-parent commit on upstream (`2058-s1-track-catch-up.md:13,128-134`). Its
  worker passes `commit_message` with every needed line, and publish replaces the track's own
  branch. The PR then holds one commit.
- Adding `commit_message` to `neige_task_fail` would cover only the worker-reported subset. Reaped,
  canceled and interrupted attempts have no report. It is left out under rule 1.

## 6. Decision 4: the PR body and the squash body

- The Planner owns the PR text. `neige_dev_publish` already requires `title` and `body`, and slice
  C makes a re-publish update an open PR's title and body. The Planner copies the required lines
  from the worker's result or from the candidate commit, as it did in #2129.
- The kernel does **not** append trailers to the body. Appending "all trailers of base..tip" would
  be a new kernel mechanism that parses messages. The observed run did not need it, because the
  Planner-written body already passed R-body.
- Squash body: the merge keeps the GitHub default (the PR body), so R-squash follows from R-body.
  The kernel's merge lowering does not change.
- If the body misses a line, CI's R-body names it. The Planner publishes again with the corrected
  body, and the `edited` event re-runs the audit. That is one round, with no history rewrite.

## 7. Decision 5: migration, triggers and 4140 compatibility

`crates/calm-truth/migrations/0144_task_git_delivery_commit_message.sql`. The number is assigned
last, at rebase time.

```sql
-- #2139 R1 — the commit message a worker supplied with neige_task_done; NULL = the kernel's own
-- one-line text (every non-completed attempt, a done report without one, every row before this
-- migration). Written once by the release that ends the attempt.
ALTER TABLE task_git_deliveries ADD COLUMN commit_message TEXT NULL
  CHECK (commit_message IS NULL OR (outcome = 'completed'
    AND length(CAST(commit_message AS BLOB)) BETWEEN 1 AND 16384));

-- The 0113 immutability trigger names its columns, so it does not cover this one.
CREATE TRIGGER task_git_delivery_commit_message_immutable
BEFORE UPDATE OF commit_message ON task_git_deliveries
BEGIN SELECT RAISE(ABORT, 'git delivery commit message is immutable'); END;
```

- **Settled-once trigger (0113:38):** it lists columns explicitly, and a new column is not in the
  list, so the new trigger above covers it. The 0122 outcome trigger set the precedent.
- **Settlement UPDATEs** (`settle_candidate_tx`, `settle_failed_tx`) never name `commit_message`,
  so `UPDATE OF commit_message` does not fire for them. A test pins this.
- **Outcome-immutable trigger (0122):** unaffected. The CHECK reads `outcome` only at INSERT, or at
  an UPDATE that the triggers already forbid.
- A column CHECK that references `outcome` in `ADD COLUMN` was checked on SQLite 3.40.1: existing
  rows pass, and a message with `outcome='failed'` is refused. The migration test re-checks this on
  the bundled `libsqlite3-sys`.
- **4140 rows** read as NULL, which is `Kernel`, which produces today's exact text. An unsettled
  pre-upgrade row with no operation rebuilds byte-identical argv. One with an operation uses its
  frozen payload. No backfill and no compatibility branch: NULL means the same thing on old rows
  as on new non-completed rows.
- `head_schema_fixture.rs:83` gains the new file name.

**DB check for the orchestrator** (read-only, on the 4140 DB, `sqlite3 -readonly <db>`):

```sql
SELECT MAX(version) FROM _sqlx_migrations;                  -- expect 143
SELECT sqlite_version();
SELECT name FROM pragma_table_info('task_git_deliveries')
 WHERE name = 'commit_message';                             -- expect no row
SELECT COALESCE(outcome,'<null>') AS outcome, COALESCE(settlement,'<unsettled>') AS settlement,
       COUNT(*) AS n
  FROM task_git_deliveries GROUP BY 1, 2 ORDER BY 1, 2;     -- how common non-completed commits are
SELECT d.delivery_id, d.outcome, d.created_at_ms, o.id AS operation_id, o.phase
  FROM task_git_deliveries d
  LEFT JOIN operations o ON o.operation_key = d.operation_key AND o.kind = 'forge-action'
 WHERE d.settlement IS NULL;                                -- rows whose argv a scheduler may rebuild
SELECT d.delivery_id, d.outcome, d.settlement, c.commit_sha
  FROM task_git_deliveries d LEFT JOIN task_candidates c ON c.candidate_id = d.delivery_id
 WHERE d.track_id = '028f636ee14544b49761938e3178798c' ORDER BY d.created_at_ms;  -- §1 table
```

The answers that would change the design: a non-empty unsettled set with `operation_id IS NULL`
(still safe, but name it in the PR), or many `failed` rows settled as `candidate` (that would
re-weigh §5).

## 8. What does not change

- `task.completed` and its goldens, `fe/core/api/generated/wire.ts`, and `SYNC_EVENT_VERSION`.
  `gate-sync-event-version-lockstep.sh` stays green with no bump, which is evidence that no event
  changed.
- `GIT_DELIVERY_SCRIPT` and both probes. No script text changes, so no payload of any kind moves.
- Publish, merge, candidate binding, `task_candidates`, and the Planner and `dev` instructions.
- `neige_task_fail`.

## 9. Slices

| Slice | Content | Size | Tier |
|---|---|---|---|
| **D1: carry and persist** | 0144 migration and `head_schema_fixture` entry. New `git_candidate/commit_message.rs` (`CommitMessage::parse`, `DeliveryMessage`). `DeliveryRow.commit_message` read and write. `insert_initial_delivery_tx(…, message, …)`. `delivery_message` match. `ReleaseDelivery::Commit(outcome, message)` swept across every caller (production callers pass `Kernel`) | ~450 lines, about half tests | L2 (migration) |
| **D2: worker surface** | `neige_task_done` schema property and parse in `emit.rs` before the sink. `commit_worker_task_report(…, message)`. CLI `--commit-message` and the help usage line. `neige_task_done.md` and `worker/head-{mcp,cli}.md`. Goldens: `mcp_tool_registry.json`, `worker_prompt_{mcp,cli}.txt`. Integration tests | ~350 lines | L2 (worker authority contract) |

D2 depends on D1. Neither depends on slices A, B or C. For the PR-body path, C is complementary
but not required.

**Must-red tests.** Each must fail on the pre-change production code, or under the named mutation
where the test cannot compile without the new type.

D1 (`git_candidate/tests.rs`, migration cases):
- `delivery_message_is_the_stored_worker_message`: a row with `Worker(m)` gives argv `$1 == m`.
- `kernel_message_is_byte_equal_to_the_pre_2139_text`: a literal pin of the `Kernel` text. It
  guards the 4140 rows.
- `delivery_payload_semantic_hash_is_stable` (`:1860`), extended: a row with a worker message
  builds equal argv and an equal hash twice.
- `commit_message_is_completed_only_and_immutable`: the INSERT with `failed` is refused, an
  `UPDATE … SET commit_message` aborts, and both settlement UPDATEs on a messaged row succeed.
- `pre_2139_unsettled_row_rebuilds_identical_argv`: a 0143-schema row is migrated, read back as
  `Kernel`, and gives argv equal to the pre-upgrade builder's.
- `commit_message_parse_boundaries`: 16 384 bytes accepted and 16 385 refused; NUL, ESC, DEL and
  whitespace-only refused; `\t\n\r` and multi-byte UTF-8 accepted.

D2 (`tests/cases/git_delivery.rs` over the real MCP socket via `support/done_delivery.rs`;
`cli/commands/tests.rs`):
- `worker_commit_message_is_the_candidate_commit_message`: `git log -1 --format=%B <candidate>`
  equals the message, including its `OWNERSHIP-CHANGE`-shaped lines. The kernel treats them as
  plain text.
- `crash_before_submission_commits_the_worker_message`: the shape of
  `delivery_row_survives_crash_before_submission`, with a message. The scheduler-built commit
  carries it.
- `duplicate_completion_keeps_the_first_commit_message`.
- `invalid_commit_message_refuses_the_report_before_any_write`: the task is still running, with no
  delivery row and no `task.completed` event. A corrected retry is then admitted.
- `failed_attempt_commits_with_the_kernel_message`: non-completed attempts are unchanged.
- `task_done_maps_commit_message`, plus the existing `every_option_is_its_schema_key`.

**Mutation verification** (exclusive worktree, production code only):
- D1-M1: `delivery_message` ignores `Worker`. Predicted red: exactly
  `delivery_message_is_the_stored_worker_message`.
- D1-M2: drop the NUL refusal. Predicted red: exactly `commit_message_parse_boundaries`.
- D2-M3: `emit.rs` passes `DeliveryMessage::Kernel`. Predicted red: exactly the three D2
  message-carrying integration tests.
- D2-M4: parse after the sink transaction instead of before it. Predicted red: exactly
  `invalid_commit_message_refuses_the_report_before_any_write`.

**Gates for each slice:**
- `scripts/local-ratchet-gates.sh`, after `git add -N` for new files. It includes the prose,
  terminology and event-version lockstep gates.
- Focused runs:
  `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server <filter> --test-threads 8`
  with filters `git_candidate`, `git_delivery`, `head_schema`, `commands::tests`, `emit`.
- One whole `-p calm-server` nextest run before delivery. New SQL reads and a tool-schema change
  hit source-scan suites that a filter misses: `deferred_write_tx_invariant`,
  `no_retired_tool_names`, the `mcp_tool_registry.json` golden, the worker prompt goldens,
  `head_schema_fixture`, `every_option_is_its_schema_key`, and the prompt byte budgets.
- `scripts/local-contract-gates.sh` as well, if slice B has landed.
- `scripts/local-rust-gates.sh --quick` for compile, clippy and the OpenAPI preflight.
- No `fe/` change, so no fe gates. No real Codex E2E.

Post-deploy acceptance on 4140: the next Planner track whose worker touches a readonly path
passes `pr-ownership` on its first publish, with no terminal rewrite and no re-registration task.

## 10. KNOWN GAPs

- A non-completed attempt that changes a readonly path leaves a commit without the line, and
  R-commit fails. The remedy is a `start:"upstream"` catch-up with `commit_message` (§5). Not
  observed.
- A catch-up's single commit needs every line of the work it replays. The Planner passes them in
  the task goal; the kernel does not carry earlier messages forward.
- Nothing in the kernel checks that the PR body holds the commit lines. CI R-body does, and the fix
  is a re-publish with a corrected body (slice C).
- The kernel does not surface the commit message in Planner wakes. The Planner reads it from the
  worker's result or the commit.
- Repository hooks and config (`commit-msg`, `prepare-commit-msg`, `commit.cleanup`) may rewrite or
  refuse the message. A refusal settles as `commit_failed` through the existing path.
- A message on an attempt with no kernel-delivery lease, or with no changes (no commit), has no
  effect. The tool description says so.
- A repeated done report with a different message is ignored: the first one wins.
- The PR still lists every attempt commit, including those with kernel text. `main` gets one squash
  commit.
- Author and committer identity stay the checkout's git config. A DCO `Signed-off-by` line written
  by the worker must match it.

## 11. Decisions (orchestrator, 2026-10-05)

1. **A raw string, not structured `subject/body/trailers`.** A git message is already
   text with trailers. The kernel validates only text-level limits (§4) and carries no
   trailer knowledge.
2. **Failed-attempt commits stay a KNOWN GAP**, with the catch-up remedy (§5, §10).
   `neige_task_fail` is unchanged. Evidence: every 4140 delivery that touched a readonly
   path completed (§1).
3. **One L2 PR carrying both slices as two commits (D1, then D2).** The total is about
   800 lines, within the ~1k PR target; splitting it would ship D1's column with no
   writer.

4140 check, run read-only on 2026-10-05:
- `_sqlx_migrations` max is 140; SQLite is 3.40.1.
- There is no `commit_message` column yet.
- Deliveries: 45 rows have a NULL outcome, 31 are completed and 1 failed. All 77 are settled as `candidate`, and none is unsettled.
- All six deliveries of track `028f636e…` completed.
