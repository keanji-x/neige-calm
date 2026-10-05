# Exact CI wake evidence

Review tier: L2, because the forge completion event is persisted and replayed.

The checks lowerer owns the GitHub rollup fold. Classify each entry once and use
that classification for both the aggregate conclusion and every failed check's
name and locator (CheckRun detailsUrl/id, StatusContext targetUrl/id).
The wait and deadline snapshot continue to share this production fold.

Keep the `forge.pr.checks` discriminator and historical conclusion. Add a
snapshot containing required head_sha and mergeable strings. Only the entire
snapshot may be absent: this explicitly denotes historical events and already
frozen operations, which cannot truthfully acquire evidence after the fact.
Legacy wake text must say the exact head and mergeability were not recorded.
New lowerings always extract the whole snapshot; incomplete snapshots fail
typed deserialization rather than defaulting individual required fields.

The additive optional snapshot preserves old clients' interpretation of the
existing conclusion and the frontend has no checks query consumer. Therefore
SYNC_EVENT_VERSION stays unchanged; historical rows and released migrations
are untouched. Result evidence is extended with failed_checks, while snapshot
is the only additional persisted event field. This avoids replay-time reads
of mutable PR state or inference of GitHub policy in the dispatcher.

Verify the production MCP path for mixed failed CheckRun/StatusContext output,
exact head/mergeability in the persisted event and wake, successful completion,
pending receipt reuse, head movement, deadline and one completion per wait.
Keep the historical serde golden and add current and malformed snapshot checks.
Planner will arrange both independent L2 review channels after implementation.

gh 2.74.2's `pr view` exporter drops node IDs, so its URL-less checks cannot
satisfy the locator contract. The dev lowerer now reads the supported GraphQL
`statusCheckRollup.contexts` connection with node IDs and 100-node pagination.
Each page carries PR head/mergeability and the selected head commit OID. Before
classification, reject any page whose PR head or commit differs from the first
head; after pagination, re-read the current PR head and reject stale evidence.
The snapshot's head and mergeability come from the same first GraphQL response.
The wait retries failed reads at the existing interval; the deadline probe uses
the same reader and fold once, and cannot emit evidence for a mixed head.
The reader requires `jq` on the forge subprocess PATH: gh 2.74.2 refuses
`--slurp` combined with `--jq`. The isolated E2E image installs it explicitly.
Failed checks prefer a non-empty URL, then the real GraphQL node ID. Missing
both is a read error, never a fabricated or null locator. Regression seeds use
the real lossy export shape; only the API shim models the source node IDs.

The user authorized the core/api contract extension in #2129. The platform's
commit and later PR must preserve these ownership trailers:

```
OWNERSHIP-CHANGE: fe/core/api/schemas.ts — add exact CI snapshot contract (#2129)
OWNERSHIP-CHANGE: fe/core/api/schemas.test.ts — verify exact CI snapshot contract (#2129)
OWNERSHIP-CHANGE: fe/core/api/generated/wire.ts — generate exact CI snapshot bindings (#2129)
```

## Verification

### Locator repair after review B

The real gh 2.74.2 export regression removed the fixture-only `databaseId`/`id`
fields from URL-less CheckRun and StatusContext seeds. The targeted
`gh_pr_checks_reports_no_checks_and_mergeability` test failed for exactly those
two cases, both returning `id:null` (`/tmp/check-locators-red.log`). The repaired
regression also checks failures at positions 101/102, cross-page head movement,
commit/head mismatch and a stale final head. The production API shim only
returns IDs for the GraphQL query that actually requests them.

Read-only live probes extracted the production query, reader and fold from
`git_actions.rs` and ran against PR #2133 with gh 2.74.2. Both the normal
100-node page size and a forced one-node page size returned all five failed
checks with real URLs at head `0e337d38f15a85a95fe58f916a141ee16f6d3079`.
Mergeability is preserved from each read's first response, so independent reads
can differ as GitHub recomputes it (`/tmp/check-locators-probe.log`).

Mutation verification executed the production reader/fold extracted from source
with controlled GraphQL responses. Before mutation, all four named probes were
green. Replacing only the commit-OID equality check with `false` made exactly
`wrong-commit` red; `all-pages-locators`, `mixed-head` and `stale-head` remained
green. Restoring that expression made all four green again. Evidence:
`/tmp/check-locators-mutation-{baseline,red,restored}.log`. No Rust build or
other writer ran during the transient production mutation.

The worker's discovered `neige.task.report_success` input schema contains only
`attempt_id`, `result` and `artifacts`; its CLI help likewise exposes no commit
metadata option. This repair provides no platform commit-metadata entry point
and does not repair the missing trailers on the prior delivery commit. The
three canonical trailers above remain available for Planner's delivery fix.

The final focused nextest command passed all 10 selected tests, including the
production MCP wait/deadline paths and URL-less/pagination regressions:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 CARGO_TARGET_DIR=/tmp/ci-wake-2129-build CARGO_PROFILE_DEV_DEBUG=0 cargo nextest run --locked -p calm-server --lib --test forge_template_e2e -E 'test(pr_checks::) | test(lowers_gh_pr_checks) | test(gh_pr_checks_wait_)' --test-threads 8
```

`cargo fmt --all --check`, `git diff --check` and all five steps of
`scripts/local-ratchet-gates.sh` passed. The scoped default-feature Clippy
command also passed:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 CARGO_TARGET_DIR=/tmp/ci-wake-2129-build CARGO_PROFILE_DEV_DEBUG=0 cargo clippy --locked -p calm-server --lib --test forge_template_e2e -- -D warnings
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 CARGO_TARGET_DIR=/tmp/ci-wake-2129-build CARGO_PROFILE_DEV_DEBUG=0 cargo clippy --locked -p calm-server --lib --test forge_template_e2e --test codex_forge_e2e --features calm-server/codex-e2e -- -D warnings
```

Evidence logs: `/tmp/check-locators-final.log`,
`/tmp/check-locators-clippy.log`, `/tmp/check-locators-final-clippy.log`,
`/tmp/check-locators-ratchet.log`. The feature Clippy check compiled/linted the
shared Codex fixture only and passed.
No real Codex E2E, workspace-wide tests/full Rust gates or port 4140 restart ran.

### Original snapshot implementation

All Rust commands below used this exact prefix (the shared target is read-only
in the worker sandbox):

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 CARGO_TARGET_DIR=/tmp/ci-wake-2129-build CARGO_PROFILE_DEV_DEBUG=0
```

Before the fix, these commands each failed exactly one regression:

```sh
cargo nextest run --locked -p calm-server --test forge_template_e2e a_checks_wait_parks_until_ci_fails --test-threads 8
cargo nextest run --locked -p calm-server --lib checks_wake_includes_exact_snapshot_for_success_and_failure --test-threads 8
```

The first failed because the persisted snapshot head was null instead of the
actual PR head. The second rendered the old conclusion-only wake text.

After the fix and mutation restoration, this command passed all 17 selected
tests, including mixed failures, URL/ID folds, successful completion, receipt
reuse, head movement, deadlines, historical/current goldens, observation
mapping, push predicates and catch-up registration:

```sh
cargo nextest run --locked -p calm-server --lib --test forge_template_e2e --test replay_event_suite -E 'test(pr_checks::) | test(checks_wake_) | test(lowers_gh_pr_checks) | test(gh_pr_checks_wait_) | test(event_serde_goldens::forge_pr_checks) | test(harness_observation_from_event_mapping_pin) | test(planner_push_predicate_and_observation_mapping_agree) | test(planner_catch_up_kinds_equal_the_push_capable_kinds)' --test-threads 8
```

The required-field regression lives in calm-types so it can be mutation-tested
without compiling the server. This command was green, then red only for
`event::checks_tests::forge_pr_checks_snapshot_is_complete_or_explicitly_historical`
under the single production mutation `#[serde(default)]` on snapshot.mergeable,
then green after removing the mutation. The actual red set matched the prediction:

```sh
cargo nextest run --locked -p calm-types --lib forge_pr_checks_snapshot_is_complete_or_explicitly_historical --test-threads 8
```

These commands also passed with the same Rust prefix; the generator exported
106 bindings. The Codex fixture was compiled/linted only, never executed:

```sh
cargo test --locked -p calm-types export_bindings_
cargo clippy --locked -p calm-server -p calm-types --lib --test codex_forge_e2e --features calm-server/codex-e2e -- -D warnings
```

Other green checks (frontend commands ran in fe/):

```sh
env npm_config_cache=/tmp/ci-wake-2129-npm npm ci --ignore-scripts
./node_modules/.bin/vitest run --project platform-independent core/api/schemas.test.ts
./node_modules/.bin/eslint core/api/schemas.ts core/api/schemas.test.ts --max-warnings=0
npm run build
```

Vitest passed 2 tests; the build passed TypeScript and Vite. `cargo fmt --all
--check` passed. `env GIT_INDEX_FILE=/tmp/ci-wake-2129-index
scripts/local-ratchet-gates.sh` passed all 5 gates. A temporary index/object
directory was needed to mark new files intent-to-add because this worker
cannot write the platform-owned git metadata. No main worktree index was changed.

Final diff review checked domain ownership (the lowerer owns GitHub policy),
duplication (one verdict per entry feeds both aggregate and failure detail), and
application assumptions (the dispatcher only copies the typed snapshot; no new
plugin-specific kernel dispatch). No blocking implementation finding remained.
The independent L2 reviews are still for Planner to arrange.
