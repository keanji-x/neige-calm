# Settled task worker reaping

Outcome: end the PTY session owned by a settled task attempt within one sweep
once 60 seconds have elapsed since `tasks.finished_at_ms`, on open or closed
tracks. Retain the existing IMMEDIATE claim, Codex interrupt and PTY reap.

Review tier: L2, because ending a process crosses session lifecycle authority.
The Planner arranges independent review channels.

Use `tasks`, not `current_tasks`: retries bind new cards and leave historical
attempts owning their old cards. Require Worker card role and an owning settled
attempt (`done`, `failed`, `canceled`) with a recorded finish timestamp. Exclude
any card with a dispatched, running or verifying attempt. Include terminals
with persisted exit evidence so resumable running sessions converge too.
Track closure must not authorize reaping a task-less terminal.

The 60-second grace allows the report response and provider turn to finish;
with the existing 30-second tick, normal cleanup is 60–90 seconds after finish.
Measure persisted wall-clock milliseconds, like the task writers, not terminal
creation or session activity. Missing finish timestamps fail closed. Recheck
this same predicate inside the claiming transaction, including a racing regate.

Source checks: `scheduler/git_delivery.rs::drive_git_delivery` operates on lease,
branch and persisted delivery, and `operation/workspace_lease/release.rs`
records delivery in the transaction settling the attempt. Gates and regates
use frozen candidates (`operation/task_verify_adapter/regate.rs`) and persisted
workspace/operation cwd (`task_verify_adapter/mod.rs`); no worker process is
needed. Terminal input's `terminal_interaction/target.rs::lifecycle_refusal`
refuses settled workers. Result receipts and accept/reject use persisted task,
report and delivery state (`mcp_server/tools/track_state.rs`); reviews are new
attempts. No post-settlement dependency on a live worker was found.

Acceptance: production sweep tests cover open/closed settled states, every
in-flight state, retry on another card, grace/missing timestamp, exited PTY,
task-less terminal/Planner exclusion, claim recheck and Codex interruption.
Mutation verification removes the owning-settled-attempt predicate and must
fail exactly the grace and task-less terminal protection tests. No new permanent
registry, migration, or CI check: ordinary later changes edit the owning code
and relevant tests; the existing focused replay runs once in this task's gate.

Implementation self-check: the sweeper owns lifecycle cleanup; adapters retain
no duplicate eligibility rules. Candidate enumeration and IMMEDIATE claim share
one SQL predicate; interrupt and reap retain their existing owners. Card role,
task status and task/card IDs are owned protocol facts, with no provider/plugin
identity added to generic policy. No external private session layout is inferred.
Existing test helpers mint PTYs through the production terminal-open operation;
the tests call the production sweep and claiming entry points. No released
migration or generated artifact changes are required.

## Verification evidence

Carry replay used:

```sh
git diff b42b799896341477bbfe649483b5752a52feec9b e59189142318143b606a49e495b102a862215ae6 > /tmp/worker-reap-carry.patch
git apply --reject /tmp/worker-reap-carry.patch
```

For each rejected file, a Python loop obtained base/theirs via
`git show <revision>:<path>`, restored that file's original upstream content via
`git show HEAD:<path>`, and ran
`git merge-file <path> /tmp/worker-reap-base /tmp/worker-reap-theirs`.
Three conflicts retained upstream's binding check, fake start-failure handling,
and lower terminology baseline. A duplicate already-upstream `hibernate`
method from an offset-applied hunk was removed. The final carry diff is empty;
all reject files were removed.

Initial package-wide focused command compiled with two fixture ownership errors
(consuming `Harness::stop`); these were corrected before the baseline:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --no-tests=fail --test-threads 8 worker_reap
```

An attempted narrower command used `--test terminal_ws_suite`; Cargo reported
that this is not a test target. The actual target is `api_suite`, which includes
that module. This was a worker command, not a declared gate step.

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --test terminal_ws_suite --no-tests=fail --test-threads 8 worker_reap
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --test api_suite --no-tests=fail --test-threads 8 worker_reap
```

The latter ran eight tests before fail-fast; the retry fixture then needed the
complete persisted recovery origin. The complete baseline command below ran
nine tests: eight failed for the old eligibility, and in-flight protection passed.
After implementation, it ran ten tests and all passed (the tenth retains the
existing shared Planner harness exclusion coverage).

```sh
cargo fmt --all
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --test api_suite --no-tests=fail --no-fail-fast --test-threads 8 worker_reap
git diff --check
```

The same complete-filter nextest command ran the owner-predicate mutation:
production-only removal of the owning settled-attempt EXISTS block. Predicted
and actual red sets were exactly
`worker_reap_user_terminal_and_planner_untouched` and
`worker_reap_within_grace_or_missing_finish_not_reaped`; eight others passed.
Restoration uses the saved production file, byte equality checked, without
checkout/reset. The in-flight-guard mutation predicts exactly
`worker_reap_in_flight_not_reaped_even_with_settled_attempt_on_card` and
`worker_reap_claim_rechecks_in_flight_before_write`.

The kernel gate owns text/contract checks, Rust quick checks and the declared
focused replay; CI owns broad suites. Worker commands do not replace the gate's
frozen-target verification.

The first in-flight mutation was invalid: only the in-flight test failed.
The claim test changed its sole settled owner to verifying, so the independent
owner EXISTS already blocked the claim. After safe restoration, the claim test
was strengthened to insert a second verifying attempt on the same card at the
before-write seam, leaving the settled owner in place. This pins the in-flight
exclusion at the transaction boundary independently. The complete-filter command
is rerun green before repeating the production-only guard removal.

The eight addition collisions in the carry patch were compared against upstream:
seven are byte-identical; upstream's Planner hibernation failure fixture adds
the required managed Assistant provider/contract fields and retains the carried
behavior. No carried addition is missing.

With the strengthened claim test, the repeated in-flight mutation produced
exactly the two predicted failures; eight other tests passed. Production was
restored byte-for-byte and the complete-filter green command rerun. Both
mutations changed production SQL only, with no concurrent writer/reviewer, and
no mutation residue remains.
