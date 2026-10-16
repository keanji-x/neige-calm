# Issue #2666 S1 workspace extraction

Updated: 2026-10-16. S1 is implemented and has passed two independent Codex L2
code reviews. [PR #2667](https://github.com/keanji-x/neige-calm/pull/2667) records
the final delivery checks and merge status. The framework issue remains open
for S2a → S3 → S2b → S4 → S5; this extraction does not provide a view backend.

## Implemented boundary

`crates/workspace` owns existing filesystem/Git mechanisms: command environment,
managed path/ownership materialization, Git path/registration/HEAD probes, common
metadata locking, worktree ensure/teardown/ignored cleanup, physical recycling,
restore and trash collection. It has no task, truth, template or plugin policy.

`calm-server::workspace_service` owns attached/managed selection and admission,
actual baseline/upstream selection, blocking bridges, recovery admission, area
compensation and postcommit sweep. Production callers include track creation and
retry, repoint/today/child, worker admission, track/area deletion, boot/planner/
hibernation recovery, attachments and Git/file consumers. Truth consumes only
pure shared path constructors. Old implementations and compatibility re-exports
are absent.

Ensure retains refresh → metadata lock → registration/ref probes → new-branch
base/upstream preparation → worktree add. Refusal/error text, branch config,
wire/events, seven-day trash retention and ten-minute ignored cleanup remain
unchanged. No migration, ledger, archive policy or later-slice API is added.

The Git environment is an intentional closed set owned by `workspace::command`
(PATH/HOME/XDG_CONFIG_HOME plus the C locale), equivalent to the former helper
expansion; network additions remain kernel policy.

## Verification and review

The continuation ran these checks successfully:

```sh
scripts/local-ratchet-gates.sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  scripts/local-contract-gates.sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  scripts/local-rust-gates.sh --quick --base origin/main
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked --no-tests=fail -p workspace -p calm-server \
  -E 'package(workspace) | test(/^operation::workspace_lease::|workspace_recycle|workspace_materialize|track_worktree|worktree_clean|workspace_service/)' \
  --test-threads 8
```

Contract checks passed 195 + 178 + 13 tests; the expanded focused selection
passed 164 tests. Initial contract/lint failures exposed a nonstandard test
module alias, its sibling import, formatting and orphaned comments; all were
corrected. Tests remain registered with their standard parent modules, and
comments follow the moved tests. No permanent scan exception was added.

The original implementation's owner-marker mutation evidence was preserved:
relaxing equality to nonempty-marker acceptance failed exactly the workspace
foreign-marker test and the real track-delete foreign-marker test (133 passed,
2 failed). The restored production file was compared byte-for-byte with its
pre-mutation copy; the continuation's focused run is green.

Both independent reviews explicitly checked abstraction boundaries, duplicate
logic, hardcoded application assumptions, external contract facts and permanent
maintenance cost, plus the real ensure/delete/recovery/area compensation paths.
Mechanical fixes were rechecked by both channels without changing their full
production verdicts. Stack E2E and remote CI outcomes belong to the PR record;
no real Codex E2E or broad local workspace suite is claimed here.

## Continuation constraints

Follow [the framework design](../architecture/track-workspace.md). S2a must
consolidate persisted workspace identity, typed launch/content capabilities and
supervisor tickets without selecting a view backend. S3 requires its separately
reviewed delivery protocol implementation design before implementation. Never
reuse an already reported Neige worker attempt; desktop continuation here used
its own Git delivery branch rather than resuming that failed attempt.
