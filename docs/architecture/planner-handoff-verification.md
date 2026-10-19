# Planner identity cutover verification

L2: persisted identity and authority change. The Planner owns the two fresh independent reviews.
The worker owns the complete focused selection, real API generation, formatting and these single-factor production mutations.
The kernel owns text/quick/contracts, focused replay, truth authorization and FE consumer checks.
No provider start, real Codex or Tier 2 test is part of this verification.

The exclusive worktree has one writer and no concurrent reviewer. Each prediction, source hash and exact patch below is written before its mutation.
Each mutation runs the entire task-specified focused selection, restores the original bytes in `finally`, then runs the same selection green.

## health mutation: prediction before write

Production file: `crates/calm-server/src/operation/handoff_cutover_sql.rs`.

Original SHA-256: `76c2ff7c2a6b099d67e46d57494279a90c49c4a3981bfb3d513a567de8cb04c4`.
Mutated SHA-256: `1808acdb9136bef6ef59946cc90e20b7d4d5ed09e2622c0a4eecf7e4a9b776e7`.

Complete predicted red set:

- `calm-server::harness::run_loop::handoff_cutover_tests::handoff_cutover_revalidates_fault_and_claim_without_any_writes`

```diff
--- crates/calm-server/src/operation/handoff_cutover_sql.rs
+++ crates/calm-server/src/operation/handoff_cutover_sql.rs
@@ -74,10 +74,7 @@
             return Err(conflict());
         }
     }
-    if snapshot.handoff_pause
-        != (PauseState::Paused {
-            receipt: receipt.clone(),
-        })
+    if snapshot.handoff_pause.receipt() != Some(receipt)
         || !snapshot.handoff_pause.valid_checkpoint(&snapshot)
         || snapshot
             .pending_entries()
```

Result after restoration:

```json
{
  "kind": "health",
  "expected": [
    "calm-server::harness::run_loop::handoff_cutover_tests::handoff_cutover_revalidates_fault_and_claim_without_any_writes"
  ],
  "actual": [
    "calm-server::harness::run_loop::handoff_cutover_tests::handoff_cutover_revalidates_fault_and_claim_without_any_writes"
  ],
  "red_exit": 100,
  "restored_exit": 0,
  "source_restored_sha256": "76c2ff7c2a6b099d67e46d57494279a90c49c4a3981bfb3d513a567de8cb04c4",
  "original_sha256": "76c2ff7c2a6b099d67e46d57494279a90c49c4a3981bfb3d513a567de8cb04c4",
  "red_log": "/tmp/handoff-mutation-health-red.log",
  "restored_log": "/tmp/handoff-mutation-health-restored.log"
}
```

## Lifecycle regression and scope

The first complete selection passed 211/211 after narrowing transcript insertion to its current owner:
Failed current owners may still finish a terminal projection; superseded and pending identities cannot write.

Health mutation ran that entire 211-test selection, produced exactly its predicted one-test red set,
and restored to 211/211 with the identical production SHA-256 above.

Three later lifecycle tests extend the complete selection to 214 tests: all registered provider wrappers,
existing suffix-reader ownership, and read-only history versus parent Track deletion. Their first run
reported two failures: the Track deletion trigger regression, and a fixture attempting to append the
suffix in the retired source card's scope. The latter is now emitted in the target card's scope; the
production history authorization fence is retained. The migration limits history's update trigger to
content/identity fields and uses the existing owner-presence pattern for deletion, permitting FK cleanup
and parent deletion. The healthy-receipt/queue production adapter bytes are unchanged by that fix.

The existing terminology-baseline generator was run after the schema and fixture sweep. Its only
change is 298 → 306 for the existing transcript storage vocabulary: three physical-table guards
and five test-only references to that table or its existing repository API. No scan rule or exclusion
changed. A later ordinary test needs no new test inventory; SQL references to the frozen storage name
continue to update the owning counted baseline. Migration 0173 is added to the existing head-schema fixture.

## queue mutation: prediction before write

Production file: `crates/calm-server/src/operation/handoff_cutover_sql.rs`.

Original SHA-256: `76c2ff7c2a6b099d67e46d57494279a90c49c4a3981bfb3d513a567de8cb04c4`.
Mutated SHA-256: `d7124a5e7d3609f7bfc52912bd3a10a7f1f4873e3024e777a58dedb982cc72c9`.

Complete predicted red set:

- `calm-server::harness::run_loop::handoff_cutover_lifecycle_tests::handoff_cutover_preserves_queue_floor_then_reads_only_suffix_from_existing_owner`
- `calm-server::harness::run_loop::handoff_cutover_tests::handoff_cutover_transfers_complete_queue_and_revokes_both_old_sessions`

```diff
--- crates/calm-server/src/operation/handoff_cutover_sql.rs
+++ crates/calm-server/src/operation/handoff_cutover_sql.rs
@@ -155,7 +155,7 @@
     let now = crate::model::now_ms();
     let mut fresh = HarnessSnapshot::initial(
         facts.source_snapshot.push_watermark,
-        facts.source_snapshot.pending_entries(),
+        facts.source_snapshot.pending_entries().into_iter().filter(|entry| entry.envelope_id().is_none_or(|id| id > facts.source_snapshot.push_watermark)).collect(),
     );
     fresh.last_report_body_sha256 = facts.source_snapshot.last_report_body_sha256.clone();
     for (session, snapshot) in [
```

Result after restoration:

```json
{
  "kind": "queue",
  "expected": [
    "calm-server::harness::run_loop::handoff_cutover_lifecycle_tests::handoff_cutover_preserves_queue_floor_then_reads_only_suffix_from_existing_owner",
    "calm-server::harness::run_loop::handoff_cutover_tests::handoff_cutover_transfers_complete_queue_and_revokes_both_old_sessions"
  ],
  "actual": [
    "calm-server::harness::run_loop::handoff_cutover_lifecycle_tests::handoff_cutover_preserves_queue_floor_then_reads_only_suffix_from_existing_owner",
    "calm-server::harness::run_loop::handoff_cutover_tests::handoff_cutover_transfers_complete_queue_and_revokes_both_old_sessions"
  ],
  "red_exit": 100,
  "restored_exit": 0,
  "source_restored_sha256": "76c2ff7c2a6b099d67e46d57494279a90c49c4a3981bfb3d513a567de8cb04c4",
  "original_sha256": "76c2ff7c2a6b099d67e46d57494279a90c49c4a3981bfb3d513a567de8cb04c4",
  "red_log": "/tmp/handoff-mutation-queue-red.log",
  "restored_log": "/tmp/handoff-mutation-queue-restored.log"
}
```

## Actual commands and results

The complete focused command (no empty selection, no fail-fast, eight test threads) was:

```bash
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --lib --test planner_harness_suite --test planner_card_reset -E 'test(/handoff_cutover|handoff_pause|durable_observation|dispatcher::tests|planner_preserving_recovery::observation_recovery|planner_harness_boot_recovery|reset_planner_card_preserves_runtime_pending_queue_and_push_watermark|public_snapshot_persistence_waits_for_issuance_checkpoint_owner|harness::tests::|harness::snapshot::|interrupt|watchdog|timeout|system_error/)' --no-tests fail --no-fail-fast --test-threads 8
```

The expanded selection passed 214/214 before queue mutation and 214/214 after restoring it.
The queue mutation's complete actual red set exactly matched the predicted two tests; health's
complete actual red set exactly matched its predicted one test. Both restoration hashes match
all original production bytes. The adapter still has the same SHA-256 after the lifecycle fix.

```bash
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 npm --prefix fe run gen:api
cargo fmt --all
scripts/gate-1316-terminology-ratchet.sh --update-baseline
```

Real API generation succeeded: 135 binding export tests passed and emit-openapi completed;
both generated artifacts are included. Formatting and the owning counted-baseline generator
completed successfully. The final test-only delta explicitly checks consumed receipt op/newowner
IDs and drops the reply receiver before commit, then reopens the actual SQLite file and replays
its original keyed result. The kernel gate replays the full selection on that final frozen tree.

The kernel owns the six declared verification steps. Its gate log, rather than this pre-gate
note, records their verdict. No broad workspace test run, real Codex, Tier 2, external handoff
API, provider activation or UI entry point was used by the worker.

Future activation must recheck the provider's existing hold owner, create fresh native resources
on the new current identity, and recover only forward. SQL identity commit makes no provider
readiness promise. The Planner owns the two fresh independent L2 reviews.

Kernel run 1 stopped at the text step because two test SQL literals crossed the existing prose-length
ratchet. Both are now split with `concat!`, preserving the exact SQL bytes; the prose gate and its
baseline remain unchanged. The kernel's next run owns verification of this mechanical delta.

## Final source follow-up

Kernel run 2 passed every text gate and stopped at Clippy's SQL-row tuple type-complexity lint.
A type alias replaces only that tuple spelling; the SQL and validation expressions are identical.
A further production-owner regression reproduced with 214 tests: 213 passed and the complete
cutover test alone failed when asking the retired card for its original frozen Planner profile.
The shared profile owner now refuses both pending-activation and history metadata before resolving
any carried profile. This prevents a pre-cutover start replay from allocating native resources.
Both critical production mutations are rerun below on the final source, superseding the earlier
pre-lint hashes. Their recorded profile hash also binds this final startup fence.
## Final frozen source: health mutation: prediction before write

Production file: `crates/calm-server/src/operation/handoff_cutover_sql.rs`.

Original SHA-256: `86f01a9268f9d5da224bfe22d68a91ad8ec5429bb03c7663aa6744236b2877f6`.
Mutated SHA-256: `45523414730c4a9c62778e392170c3db605523de4a3b1f429f7f0b6929de81b7`.

Complete predicted red set:

- `calm-server::harness::run_loop::handoff_cutover_tests::handoff_cutover_revalidates_fault_and_claim_without_any_writes`

```diff
--- crates/calm-server/src/operation/handoff_cutover_sql.rs
+++ crates/calm-server/src/operation/handoff_cutover_sql.rs
@@ -76,10 +76,7 @@
             return Err(conflict());
         }
     }
-    if snapshot.handoff_pause
-        != (PauseState::Paused {
-            receipt: receipt.clone(),
-        })
+    if snapshot.handoff_pause.receipt() != Some(receipt)
         || !snapshot.handoff_pause.valid_checkpoint(&snapshot)
         || snapshot
             .pending_entries()
```

Result after restoration:

```json
{
  "kind": "health",
  "expected": [
    "calm-server::harness::run_loop::handoff_cutover_tests::handoff_cutover_revalidates_fault_and_claim_without_any_writes"
  ],
  "actual": [
    "calm-server::harness::run_loop::handoff_cutover_tests::handoff_cutover_revalidates_fault_and_claim_without_any_writes"
  ],
  "red_exit": 100,
  "restored_exit": 0,
  "source_restored_sha256": "86f01a9268f9d5da224bfe22d68a91ad8ec5429bb03c7663aa6744236b2877f6",
  "original_sha256": "86f01a9268f9d5da224bfe22d68a91ad8ec5429bb03c7663aa6744236b2877f6",
  "profile_sha256": "ca57e97cb337fb91aa74b6c7a93662b1d635e2961d6df41d143aeb2f03473ed1",
  "red_log": "/tmp/handoff-mutation-final-health-red.log",
  "restored_log": "/tmp/handoff-mutation-final-health-restored.log"
}
```

## Final frozen source: queue mutation: prediction before write

Production file: `crates/calm-server/src/operation/handoff_cutover_sql.rs`.

Original SHA-256: `86f01a9268f9d5da224bfe22d68a91ad8ec5429bb03c7663aa6744236b2877f6`.
Mutated SHA-256: `76c3b8dbc532f4a33169d42b1e92225e39539c174ce7bf9c8cefa2aaf8389677`.

Complete predicted red set:

- `calm-server::harness::run_loop::handoff_cutover_lifecycle_tests::handoff_cutover_preserves_queue_floor_then_reads_only_suffix_from_existing_owner`
- `calm-server::harness::run_loop::handoff_cutover_tests::handoff_cutover_transfers_complete_queue_and_revokes_both_old_sessions`

```diff
--- crates/calm-server/src/operation/handoff_cutover_sql.rs
+++ crates/calm-server/src/operation/handoff_cutover_sql.rs
@@ -156,7 +156,7 @@
     let now = crate::model::now_ms();
     let mut fresh = HarnessSnapshot::initial(
         facts.source_snapshot.push_watermark,
-        facts.source_snapshot.pending_entries(),
+        facts.source_snapshot.pending_entries().into_iter().filter(|entry| entry.envelope_id().is_none_or(|id| id > facts.source_snapshot.push_watermark)).collect(),
     );
     fresh.last_report_body_sha256 = facts.source_snapshot.last_report_body_sha256.clone();
     for (session, snapshot) in [
```

Result after restoration:

```json
{
  "kind": "queue",
  "expected": [
    "calm-server::harness::run_loop::handoff_cutover_lifecycle_tests::handoff_cutover_preserves_queue_floor_then_reads_only_suffix_from_existing_owner",
    "calm-server::harness::run_loop::handoff_cutover_tests::handoff_cutover_transfers_complete_queue_and_revokes_both_old_sessions"
  ],
  "actual": [
    "calm-server::harness::run_loop::handoff_cutover_lifecycle_tests::handoff_cutover_preserves_queue_floor_then_reads_only_suffix_from_existing_owner",
    "calm-server::harness::run_loop::handoff_cutover_tests::handoff_cutover_transfers_complete_queue_and_revokes_both_old_sessions"
  ],
  "red_exit": 100,
  "restored_exit": 0,
  "source_restored_sha256": "86f01a9268f9d5da224bfe22d68a91ad8ec5429bb03c7663aa6744236b2877f6",
  "original_sha256": "86f01a9268f9d5da224bfe22d68a91ad8ec5429bb03c7663aa6744236b2877f6",
  "profile_sha256": "ca57e97cb337fb91aa74b6c7a93662b1d635e2961d6df41d143aeb2f03473ed1",
  "red_log": "/tmp/handoff-mutation-final-queue-red.log",
  "restored_log": "/tmp/handoff-mutation-final-queue-restored.log"
}
```

Final health and queue runs both restore to 214/214. Their original/restored SQL SHA-256 is
`86f01a9268f9d5da224bfe22d68a91ad8ec5429bb03c7663aa6744236b2877f6`; the bound profile SHA-256 is
`ca57e97cb337fb91aa74b6c7a93662b1d635e2961d6df41d143aeb2f03473ed1`. The exact final red sets match
one health test and two queue tests, respectively. The cutover SQL/profile bytes remain unchanged after these runs.

Kernel run 3 reached integration-test Clippy checks and found two fixture consumers outside the
focused selection: the report-origin role matrix lacked the history case, and payload validation
still pinned the old nine server-owned keys. The role matrix now includes history with a forbidden
write verdict; the payload fixture includes the three server-owned lifecycle keys. This is a
fixture-only enum/key sweep. Final production mutation hashes remain unchanged.

The two affected fixture targets passed the necessary lint-fix check (exit 0):

```bash
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 RUSTFLAGS='-D warnings' cargo clippy --locked -p calm-server --test track_suite --test api_suite --features calm-server/codex-e2e -- -D warnings
```

## Prior attempt kernel gate boundary

The kernel subsequently returned run 5/5, stopped at Rust contracts. Runs 4 and 5 have the same
single failing server contract: CardPatch documentation did not name the new server-owned keys.
All text checks and Rust quick passed; the server/type contract group passed 177/178, and the
integration contract group passed 13/13. The three missing keys are now added to the owning
CardPatch description and real gen:api completed successfully again (135 binding export tests
and emit-openapi, exit 0), updating the generated OpenAPI artifact. The emitted description
contains all twelve keys. This documentation/generation delta changes no cutover runtime bytes.

The attempt's five-run kernel quota is exhausted. The final full gate has NOT passed; the kernel
focused replay, truth authorization step and FE consumer step were not reached. The Planner needs
a new attempt for the declared gate on this retained implementation plus the final doc fix.
The worker does not substitute local results for that missing frozen-target gate verdict.

## Gate completion attempt

Attempt `7e93be68c1b94d6dac678f815f5602d9:handoff-cutover-gate-completion` continues
the retained `1ca99ef083eddcb415660940cc4e30714d412d57` checkout without upstream replay.
The [design](planner-conversation-handoff.md#provider-admission-belongs-to-activation-and-issuance)
records the settled PendingActivation admission boundary. This attempt changes only this
verification record; the CardPatch source/generated repair was already retained in that checkout.

The prior run's sole failing contract now passes through the production OpenAPI entry point:

```bash
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --test api_suite -E 'test(=domain_api_suite::openapi::card_patch_documents_every_server_owned_payload_key)' --no-tests fail --test-threads 8
```

Actual result: 1 selected, 1 passed, 1216 skipped (exit 0). The generated and owning CardPatch
descriptions contain all twelve server-owned keys. No contract assertion or allowlist changed.
The SQL and profile SHA-256 values were rechecked against the final mutation records above;
both match. The four final raw mutation logs retain exactly the predicted one/two failing
tests and restored 214/214 selections. No production behavior changed, so those records remain
applicable without another worker focused or mutation run.

The final frozen-target verdict belongs to this attempt's kernel gate, not the exhausted prior
attempt. Its six ordered steps are text, Rust quick, Rust contracts, the complete 214-test
focused replay, truth decision-gate authorization tests, and FE install/lint/build/tests.
Read the actual verdict and complete step output from this attempt's `runs/<attempt_id>.json`
and `runs/<attempt_id>/gates/<N>.log` track views; a passing exact contract alone is not that
verdict. The completion report records the returned final run and frozen commit. No edits
follow a successful gate. CI broad suites, real-provider activation, fresh native resources,
repeated activated handoffs, external API/UI and real Codex/Tier 2 remain outside this attempt.
The Planner still owns both fresh L2 reviews.

## Compliant delivery replay attempt

Attempt `7e93be68c1b94d6dac678f815f5602d9:handoff-compliant-delivery-replay` starts
from normal upstream `e5a2e4f1793e93af5451c7b8ef05339f648367a1`, removing the failed
delivery ancestry that lacked same-commit ownership authorization. The previous completion
attempt stopped at FE lint for ancestor `1ca99ef083eddcb415660940cc4e30714d412d57` missing
an OpenAPI trailer; its earlier five green steps do not replace this attempt's gate.

Actual replay commands are `git diff ceb5802ea6a24c3393dfd6ebeabbf9ee07ba9de0 c5827e16ee0cff56229fdcba1103e2d16abc1202`
and `git diff c5827e16ee0cff56229fdcba1103e2d16abc1202 5699bcdae1342552c7047d0a033c05bda33e7ad8`,
each saved to a temporary patch and applied with `git apply --reject`. Rejected paths were
merged with `git merge-file -p` from saved pre-patch ours and `git show` base/theirs versions.
All reject files were removed after resolution. The second patch includes upstream changes
already present; their merges retain identical upstream bytes. The complete working tree was
compared against the old candidate, including untracked new Track files: no differences before
these two documentation additions and no unexpected untracked paths. There is no new runtime
semantic difference from old 5699, no migration collision and no released-migration edit.

The SQL/profile hashes are still respectively
`86f01a9268f9d5da224bfe22d68a91ad8ec5429bb03c7663aa6744236b2877f6` and
`ca57e97cb337fb91aa74b6c7a93662b1d635e2961d6df41d143aeb2f03473ed1`.
The four final raw mutation logs above were read again: health has exactly one predicted red,
queue exactly two, and both restored selections pass 214/214. Production bytes and generated
API artifacts are unchanged, so the final mutation and real-generation evidence remains valid;
no routine mutation, generator or duplicate worker focused run is needed for this pure replay.

The kernel alone runs the six declared checks on this new frozen head: text, Rust quick,
contracts, focused selection (rejecting empty selection), truth authorization, and FE
install/lint/build/tests. This paragraph records a plan, not a passing verdict. The final report
must cite the returned exact `runs/7e93be68c1b94d6dac678f815f5602d9:handoff-compliant-delivery-replay/gates/<N>.log`
and frozen commit with actual results. Do not edit this record after a successful gate.
Broad local suites, real Codex, Tier 2, future native activation, repeated activated handoffs,
and external API/UI are not tested here. Planner owns both fresh independent L2 reviews.

### FE consumer failure diagnosis before final gate

Runs r1 and r2 pass the first five steps and FE lint/build, including the ownership audit.
They fail FE tests: r1 reports 4 failed files, 6 failed tests, 5631 passed and one unexpected
worker exit; r2 reports 5 failed files, 8 failed tests and 5630 passed. Most failures are the
unchanged 5000ms timeout in upstream route/architecture tests; the exact failing set varies.
The sidebar keyboard assertion also fails under this parallel load. Raw evidence is retained
in `/home/kenji/.local/share/neige-next/data/gate-logs/7e93be68c1b94d6dac678f815f5602d9:handoff-compliant-delivery-replay-r{1,2}.log`.

The minimum diagnostic command, from `fe`, is:

```bash
npm test -- --maxWorkers=2 web/src/app/shell/sidebar.test.tsx web/src/app/router/first-message-continuity.test.tsx web/src/app/router/first-message-queue-writes.test.tsx tools/architecture/architecture.test.ts -t 'keeps Show recovery|keeps INV-DUP|keeps exactly one first sentence|retires provisional words|actual acknowledged first edit|same-revision acknowledged Delete|actual edited ACK|lost create reply'
```

It passes 8/8 selected tests in four files (147 skipped), exit 0, in 14.73s;
raw output is `/tmp/handoff-fe-failure-repro.log`. The host exposes 48 processors (`nproc`).
The owning Vitest configuration now bounds fork concurrency at two, keeping all projects,
all tests, their existing timeouts, assertions and audit policy. This is the only non-document
change beyond the byte-identical replay; it changes test scheduling, not runtime behavior.
It requires no growing test roster and adds no CI pass. Final full gate results must validate
that scheduling fix; the diagnostic selection alone is not acceptance evidence.

## Explicit forward activation

The activation attempt and its focused, mutation and kernel evidence are recorded in
[Planner handoff activation verification](planner-handoff-activation-verification.md).
