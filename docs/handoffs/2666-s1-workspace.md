# Issue #2666 S1 workspace extraction — handoff

Updated: 2026-10-16. Work stopped at the user's request to continue in a new conversation. Implementation is in the working tree; it is **not delivered or gate-verified**. No PR was created and no manual commit was made.

## Workspace and task identity

- Checkout: `/mnt/data2/kenji/neige-calm/.claude/worktrees/track-cbb14d8380634641a19cf58282b2feed`
- Track: `cbb14d8380634641a19cf58282b2feed`
- Original attempt: `cbb14d8380634641a19cf58282b2feed:s1-workspace-framework`
- Upstream starting commit: `6b5010b2e05b13fd59b49fd272b42c5eaa8c01ba`.
- Read root `AGENTS.md`, `CONTRIBUTING.md`, and [the design](../architecture/track-workspace.md), especially §§3.1, 4.2, 5.2 and the S1 row in §6. There were no more specific `AGENTS.md` files under the affected crates.
- Review tier: **L2**, because teardown/recovery and filesystem ownership move. The Planner arranges two independent formal review channels; this worker did not launch reviewers.
- The original worker is reporting this attempt as incomplete on handoff. A new worker must obtain its own valid task/attempt if continuing through Neige; do not reuse a reported attempt ID.

## Scope and hard constraints

Implement only S1: a mechanism-only package named `workspace`, plus one kernel workspace service in `calm-server`. Preserve cwd, baseline selection timing, refusal codes/text, branch config, wire/events/OpenAPI, logging meaning, 7-day trash retention and 10-minute ignored cleanup. No schema/migration, ledger, archive policy, LaunchHandle, overlay, or later-slice mechanisms. Every new public API needs a production caller. Remove old implementations without compatibility re-exports. Keep handwritten files at or below 800 lines. Keep `NEIGE_CODEX_BIN` unset, cap builds at 6 jobs and tests at 8 threads; do not run broad suites or real Codex E2E.

The requested carry-forward diff (`22b530cce38a61f9efd078c4e059e5389a8e70dc` → `658782955d9e2160fc13d9bf44cf56884aa5b7bb`) was applied with `git apply --reject`. All four files applied cleanly, so no merge-file resolution was needed. No `.rej` files remain. The carried files are:

- [lazy-track-worktree.md](../architecture/lazy-track-worktree.md)
- [research-track-workspace-cow-optional.md](../architecture/research-track-workspace-cow-optional.md)
- [track-cow-view.md](../architecture/track-cow-view.md)
- [track-workspace.md](../architecture/track-workspace.md)

## Current implementation

New package: `crates/workspace` (registered in root Cargo members, Cargo.lock, and server/truth dependencies). Its runtime dependencies are only `thiserror`, `libc`, and `tracing`; tests use `tempfile`. No truth/model/task/template/plugin dependency.

Mechanism modules:

- `command.rs`: original Git command environment construction. Isolated Git keeps exactly PATH/HOME/XDG_CONFIG_HOME and C locale; the ordinary scrubbed Git command retains its original behavior.
- `materialize.rs`: managed path construction, ownership claim/init/repair, internal excludes and ownership-marker probe. `.neige/` exclude protocol constant moved here; attachment storage consumes it directly.
- `base.rs`: pure Git path/HEAD/common-dir/registration/checkout probes formerly mixed into lease base code. Lease row types and serialization remain in server.
- `git.rs`, `metadata_lock.rs`, `worktree.rs`, `teardown.rs`: original path validation, registration, common metadata lock, ensure/remove and ignored cleanup.
- `recycle.rs`: physical containment/marker checks, atomic trash rename, rollback rename, empty-area rmdir and GC with a caller-supplied retention duration.
- `error.rs`: mechanism `Error::Internal` maps explicitly to server `CalmError::Internal`; display prefix remains `internal:`.

Kernel policy: `crates/calm-server/src/workspace_service/`:

- `workspace_materialize.rs`: managed/attached selection, attached admission, operation-key digest and managed worker admission bridge.
- `track_worktree.rs`: existing fetch scheduling, baseline choice and upstream config policy, blocking bridge and best-effort teardown warning.
- `worktree_clean.rs`: unchanged 10-minute timer, truth/session/occupancy admission and per-close deduplication.
- `workspace_recycle.rs`: managed/system-area policy, original refusal tags/logging, 7-day GC policy, batch compensation and runtime recovery gate.
- `sweep.rs`: transactional sweep capture and postcommit track/area worktree/candidate-ref teardown.

Ensure preserves its precise ordering: refresh upstream first; acquire metadata lock; check registration and existing branch; invoke the kernel callback only for a new branch to choose base and write upstream config; then `git worktree add`. Lock filenames, fixture pause points and Git flags remain unchanged. Clean passes the same contention observer through the shared lock.

Production callers migrated include track create/first-message retry/repoint, today, child track, worker admission, scheduler digest, track/area delete and compensation/finalization, harness boot recovery, planner recovery, hibernation, state cleanup startup, and Git/FS/attachment consumers of the moved primitives. Truth uses the single workspace path constructors, including managed paths. Old materialize/recycle/track_worktree/clean/teardown/metadata-lock implementations are deleted. No compatibility re-export was added.

## Tests and evidence so far

The original 27 materialize tests, 25 recycle tests and 3 metadata-lock tests are preserved across their new owner modules. Eight pure worktree/probe tests were also moved to `workspace`. Remaining lease tests have a `workspace_service_tests` module prefix so the required gate filter selects them. New handwritten files were checked to be <=800 lines.

Relevant existing integrated tests cover ensure retries and upstream preservation, dirty track deletion, clean safety, area compensation, and the managed marker gate during boot recovery. No new artificial test copy of production policy was added.

A source-level comparison confirmed unchanged bodies (apart from error/module naming) for the five trash move/verify/rmdir/GC/timestamp functions, twelve pure Git probes, and the managed materializer's main claim/init/repair functions.

### Worker commands actually run

Several initial `cargo check`/focused builds failed on migration imports, visibility, and test registration; these were fixed. Do not treat those intermediate runs as passes.

First successful focused command:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked --no-tests=fail -p workspace -p calm-server \
  -E 'package(workspace) | test(/workspace_recycle|workspace_materialize|track_worktree|worktree_clean|workspace_service/)' \
  --test-threads 8
```

Result: **134 passed**, 5460 skipped, test phase 21.969s. Log: `/tmp/s1-focused5.log`. This run exposed that one migrated `#[test]` had been swallowed by a preceding comment; it is now registered correctly, so final selection is 135 tests.

Mutation verification completed on the exclusive checkout:

- Single production mutation in `workspace/src/recycle.rs`: replace owner equality (`contents.trim() == track_id`) with nonempty-marker acceptance.
- Predicted complete red set:
  - `workspace`: `recycle::tests::a_marker_naming_another_track_is_refused`
  - `calm-server::api_suite`: `domain_api_suite::track_workspace_recycle::a_managed_workspace_whose_marker_names_another_track_is_left_on_disk`
- Ran the **whole focused selection** with the command above plus `--no-fail-fast`.
- Result: **135 run, 133 passed, exactly the predicted 2 failed**, 5460 skipped. Log: `/tmp/s1-mutation.log`.
- Restored `recycle.rs` byte-for-byte from `/tmp/s1-recycle-before-mutation.rs`; `cmp` succeeded and owner equality is present. **No mutation is left in the tree.**

Restored-tree focused run was started with the original focused command, logging to `/tmp/s1-final-focused.log`. At the last handoff inspection it was still compiling; **no final result is claimed**. Unified exec session was `13999`; it may still be running after interruption. Check that log and avoid competing Cargo builds.

`cargo fmt --all` was run repeatedly, last output in `/tmp/s1-fmt-final.log` (empty/successful). `git diff --check` passed at handoff. Two mechanical lint cleanups were made after starting the restored-tree run: remove an identity `map_err(Into::into)` in the service and place `owned_by` before the materializer test module. The final gate must verify the resulting tree.

**No kernel gate has been invoked yet.** No ratchet/contract/quick result is claimed. No broad suite or real Codex E2E was run. No schema/OpenAPI/generated artifact was intentionally changed.

## Next steps

1. Inspect `git status`, the complete diff, this document, and `/tmp/s1-final-focused.log`; wait for any surviving build to finish before another Cargo command. Re-run focused tests as needed on the final tree. Do not restore/reset the checkout destructively.
2. Review boundary decisions, production callers, public API use, refusal/log equivalence and test registration. In particular, inspect the new mechanism error mapping, ensure callback timing and truth's path-only dependency on workspace.
3. Run the task's kernel gate (under a valid new/resumed worker attempt). Gate owns ratchet, contract, quick, and focused replay; do not duplicate these broad checks manually. Fix failures and rerun invalidated checks. CI uses `--workspace`, so new package participation is automatic; see whether contract gates require any additional registration rather than inventing a list.
4. Planner must arrange formal L2 reviews. There has been no independent formal review yet.
5. Only deliver after focused verification and gate pass. Do not claim S1 complete from this handoff.

Original gate commands, in order:

```sh
scripts/local-ratchet-gates.sh
scripts/local-contract-gates.sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  scripts/local-rust-gates.sh --quick --base origin/main
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked --no-tests=fail -p workspace -p calm-server \
  -E 'package(workspace) | test(/workspace_recycle|workspace_materialize|track_worktree|worktree_clean|workspace_service/)' \
  --test-threads 8
```

Suggested full commit message (not yet submitted):

```text
refactor(workspace): Extract workspace mechanisms and kernel policy

Implement S1 of #2666 by moving existing filesystem and Git mechanisms into
workspace and centralizing lifecycle policy in the kernel workspace service.
Preserve selection timing, refusal text, branch config, trash retention and
ignored-clean cadence without schema or wire changes.

Review tier: L2, because workspace teardown and recovery cross ownership boundaries.
Refs #2666
```

## Known remaining risks

The gate and formal L2 reviews have not run. Final compilation/clippy/contract-registration failures may remain. All test claims above are limited to the exact completed runs; the final restored tree has not yet been established green. The carried architecture documents and this handoff are intentional new Markdown files and must be linked in the Planner's Report if delivered. Do not treat the staged/untracked status as evidence of unrelated drift: new package/service files are currently untracked because the worker must not manually commit.
