# Explicit forward Planner activation verification

## Explicit forward activation attempt

Attempt `7e93be68c1b94d6dac678f815f5602d9:handoff-forward-activation` adds only an
internal explicit activation operation. This section extends the earlier cutover evidence;
it does not claim external API/UI, a two-pause coordinator, native history continuity,
real provider inference or Tier 2 coverage. L2 review remains the Planner's responsibility.

The first real OperationRuntime submission reproduction failed because the activation kind
was absent: `/tmp/handoff-activation-red.log` records its one selected failing test.
The new operation reuses the start adapter's resource and installation owner, the existing
provider readiness/authentication owner, Report/task/transcript repositories and queue
eligible-prefix/source-CAS recovery. No private provider file or copied VFS session format
is used. Codex thread/start without a durable response receipt remains typed Unknown after
an unconfirmed request; explicit retry never blindly reissues it. Local backends retain their
existing first-turn native resource and credential contract. Backend wrappers are exercised
through registered metadata; no real native inference is executed.

The worker runs the task's full focused selection and formats code. The kernel owns text,
Rust quick, contract scans, full focused replay, truth authorization and FE consumers on its
frozen target. No API schema or generated API binding changes are introduced by this slice.
The actual terminology baseline generator updates its existing counted vocabulary only.

Mutation runs below use the complete `test(/handoff_activation/)` observer selection.
The two initial authority runs used `--lib --test planner_harness_suite --test planner_card_reset`.
Nextest confirms that all 16 activation observers reside in the unit binary, so subsequent
mutations select `--lib` only; each run records its exact command. They use `--locked`,
`--no-tests fail --no-fail-fast --test-threads 8`, `NEIGE_CODEX_BIN` unset,
`RUSTC_WRAPPER=` and `CARGO_BUILD_JOBS=6`. Predictions, complete diffs and source hashes
are saved before production writes, and restoration is byte-exact in `finally` before the
same selection runs green. No reviewer or other writer observes transient production code.


### Activation authority mutation: prediction before production write

Production file: `crates/calm-server/src/operation/handoff_activation_sql.rs`.

Original SHA-256: `6ba4a4c5c3897fc053ff02797c68d63b8a44ea0ff4026245ef688d0deb0bfe0d`.
Mutated SHA-256: `55fe335af8d7fef8de92fb1a8617c3f74cb69a20edc3c43bc14206821d79dee4`.

Complete predicted red set:

- `calm-server::harness::run_loop::handoff_activation_tests::handoff_activation_rejects_wrong_cutover_closed_track_and_changed_configuration`

```diff
--- crates/calm-server/src/operation/handoff_activation_sql.rs
+++ crates/calm-server/src/operation/handoff_activation_sql.rs
@@ -81,7 +81,7 @@
     let Some((None, None, Some(root), template_id)) = track else {
         return Err(conflict());
     };
-    if root != scope.new_session_id || template_id != plan.configuration.template_id {
+    if false || template_id != plan.configuration.template_id {
         return Err(conflict());
     }
     let card: crate::db::rows::CardRow = sqlx::query_as(
```

Result after byte-exact restoration:

```json
{
  "kind": "authority",
  "expected": [
    "harness::run_loop::handoff_activation_tests::handoff_activation_rejects_wrong_cutover_closed_track_and_changed_configuration"
  ],
  "actual": [],
  "red_exit": 0,
  "restored_exit": 0,
  "source_restored_sha256": "6ba4a4c5c3897fc053ff02797c68d63b8a44ea0ff4026245ef688d0deb0bfe0d",
  "valid": false,
  "red_log": "/tmp/handoff-activation-mutation-authority-red.log",
  "restored_log": "/tmp/handoff-activation-mutation-authority-restored.log"
}
```


The first authority mutation above is **invalid evidence**, retained deliberately: it
produced no failures, and its restoration passed 15/15. The test used NULL root and was
refused by the earlier required-root pattern, masking exact non-null root comparison.
The regression now distinguishes missing root from a different existing session, and also
attempts the forbidden SQL transition after a blocked claim exists. A post-installation
checkpoint failure regression now ACKs the initial input, delivers a new suffix through the
live owner and explicitly retries the same operation; it checks that neither an ACKed
briefing nor older prefix input is reinserted. The reruns below must match their complete
predictions; the invalid run is not counted as successful mutation verification.


### Activation authority_exact mutation: prediction before production write

Production file: `crates/calm-server/src/operation/handoff_activation_sql.rs`.

Original SHA-256: `6ba4a4c5c3897fc053ff02797c68d63b8a44ea0ff4026245ef688d0deb0bfe0d`.
Mutated SHA-256: `55fe335af8d7fef8de92fb1a8617c3f74cb69a20edc3c43bc14206821d79dee4`.

Complete predicted red set:

- `calm-server::harness::run_loop::handoff_activation_tests::handoff_activation_rejects_wrong_cutover_closed_track_and_changed_configuration`

```diff
--- crates/calm-server/src/operation/handoff_activation_sql.rs
+++ crates/calm-server/src/operation/handoff_activation_sql.rs
@@ -81,7 +81,7 @@
     let Some((None, None, Some(root), template_id)) = track else {
         return Err(conflict());
     };
-    if root != scope.new_session_id || template_id != plan.configuration.template_id {
+    if false || template_id != plan.configuration.template_id {
         return Err(conflict());
     }
     let card: crate::db::rows::CardRow = sqlx::query_as(
```

Result after byte-exact restoration:

```json
{
  "kind": "authority_exact",
  "expected": [
    "harness::run_loop::handoff_activation_tests::handoff_activation_rejects_wrong_cutover_closed_track_and_changed_configuration"
  ],
  "actual": [
    "harness::run_loop::handoff_activation_tests::handoff_activation_rejects_wrong_cutover_closed_track_and_changed_configuration"
  ],
  "red_exit": 100,
  "restored_exit": 0,
  "source_restored_sha256": "6ba4a4c5c3897fc053ff02797c68d63b8a44ea0ff4026245ef688d0deb0bfe0d",
  "valid": true,
  "red_log": "/tmp/handoff-activation-mutation-authority_exact-red.log",
  "restored_log": "/tmp/handoff-activation-mutation-authority_exact-restored.log"
}
```


### Activation sql_fence mutation: prediction before production write

Production file: `crates/calm-truth/migrations/0170_planner_handoff_activation.sql`.

Command: `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --lib -E 'test(/handoff_activation/)' --no-tests fail --no-fail-fast --test-threads 8`.

Original SHA-256: `78c9e2084ffc073c4c12e3b1d41bec3c06d270c3c27a471c027763c1d44e5330`.
Mutated SHA-256: `08ff246aabc24c76a9a6c10a6af58a3a5b434bfa3204bda7c46d22a055d5c934`.

Complete predicted red set:

- `calm-server::harness::run_loop::handoff_activation_tests::handoff_activation_authhold_blocks_and_requires_explicit_original_retry`

```diff
--- crates/calm-truth/migrations/0170_planner_handoff_activation.sql
+++ crates/calm-truth/migrations/0170_planner_handoff_activation.sql
@@ -50,7 +50,8 @@
  (NEW.state != OLD.state OR NEW.thread_id IS NOT NULL OR NEW.agent_session_id IS NOT NULL
   OR NEW.mcp_token_hash IS NOT NULL OR NEW.active_turn_id IS NOT NULL OR NEW.terminal_run_id IS NOT NULL)
  AND NOT (
-    NEW.state='starting' AND NEW.contract=OLD.contract AND NEW.provider=OLD.provider
+    EXISTS(SELECT 1 FROM planner_handoff_activations bypass WHERE bypass.session_id=OLD.id)
+    OR NEW.state='starting' AND NEW.contract=OLD.contract AND NEW.provider=OLD.provider
     AND NEW.card_id IS OLD.card_id AND NEW.track_id=OLD.track_id AND NEW.id=OLD.id
     AND NEW.agent_session_id IS NULL AND NEW.active_turn_id IS NULL AND NEW.terminal_run_id IS NULL
     AND EXISTS (
```

Result after byte-exact restoration:

```json
{
  "kind": "sql_fence",
  "expected": [
    "harness::run_loop::handoff_activation_tests::handoff_activation_authhold_blocks_and_requires_explicit_original_retry"
  ],
  "actual": [
    "harness::run_loop::handoff_activation_tests::handoff_activation_authhold_blocks_and_requires_explicit_original_retry"
  ],
  "red_exit": 100,
  "restored_exit": 0,
  "source_restored_sha256": "78c9e2084ffc073c4c12e3b1d41bec3c06d270c3c27a471c027763c1d44e5330",
  "valid": true,
  "red_log": "/tmp/handoff-activation-mutation-sql_fence-red.log",
  "restored_log": "/tmp/handoff-activation-mutation-sql_fence-restored.log"
}
```


### Activation unknown mutation: prediction before production write

Production file: `crates/calm-server/src/operation/handoff_activation.rs`.

Command: `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --lib -E 'test(/handoff_activation/)' --no-tests fail --no-fail-fast --test-threads 8`.

Original SHA-256: `5e740e93698171eda10b695918fb78555ff2b7c3be561c5e9e5511fafc3e9cdc`.
Mutated SHA-256: `2d5a72faff046fae242f10f585d04e287c04d515bd2ea5bcb06f9fef201ca3fb`.

Complete predicted red set:

- `calm-server::harness::run_loop::handoff_activation_tests::handoff_activation_cancellation_startup_replay_keeps_unknown_intent_without_native_repeat`
- `calm-server::harness::run_loop::handoff_activation_tests::handoff_activation_queue_and_briefing_are_durable_before_any_resource_request`
- `calm-server::harness::run_loop::handoff_activation_tests::handoff_activation_resource_checkpoint_failure_is_unknown_and_never_remints`
- `calm-server::harness::run_loop::handoff_activation_tests::handoff_activation_unknown_resource_never_blindly_reissues`

```diff
--- crates/calm-server/src/operation/handoff_activation.rs
+++ crates/calm-server/src/operation/handoff_activation.rs
@@ -183,10 +183,7 @@
     ) -> Result<AppServerInteractOutcome> {
         let state: ActivationResult = serde_json::from_value(output.result.clone())?;
         // A persisted sending intent with no returned resource is not an idempotency receipt.
-        if matches!(
-            state,
-            ActivationResult::ResourceRequestRecorded | ActivationResult::Unknown { .. }
-        ) {
+        if false {
             self.checkpoint(output, op, ActivationResult::Unknown {
                 reason: "Native resource result unconfirmed; explicit evidence-based recovery required".into(),
             }).await?;
```

Result after byte-exact restoration:

```json
{
  "kind": "unknown",
  "expected": [
    "harness::run_loop::handoff_activation_tests::handoff_activation_cancellation_startup_replay_keeps_unknown_intent_without_native_repeat",
    "harness::run_loop::handoff_activation_tests::handoff_activation_queue_and_briefing_are_durable_before_any_resource_request",
    "harness::run_loop::handoff_activation_tests::handoff_activation_resource_checkpoint_failure_is_unknown_and_never_remints",
    "harness::run_loop::handoff_activation_tests::handoff_activation_unknown_resource_never_blindly_reissues"
  ],
  "actual": [
    "harness::run_loop::handoff_activation_tests::handoff_activation_cancellation_startup_replay_keeps_unknown_intent_without_native_repeat",
    "harness::run_loop::handoff_activation_tests::handoff_activation_queue_and_briefing_are_durable_before_any_resource_request",
    "harness::run_loop::handoff_activation_tests::handoff_activation_resource_checkpoint_failure_is_unknown_and_never_remints",
    "harness::run_loop::handoff_activation_tests::handoff_activation_unknown_resource_never_blindly_reissues"
  ],
  "red_exit": 100,
  "restored_exit": 0,
  "source_restored_sha256": "5e740e93698171eda10b695918fb78555ff2b7c3be561c5e9e5511fafc3e9cdc",
  "valid": true,
  "red_log": "/tmp/handoff-activation-mutation-unknown-red.log",
  "restored_log": "/tmp/handoff-activation-mutation-unknown-restored.log"
}
```


### Activation briefing mutation: prediction before production write

Production file: `crates/calm-server/src/operation/handoff_activation_sql.rs`.

Command: `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --lib -E 'test(/handoff_activation/)' --no-tests fail --no-fail-fast --test-threads 8`.

Original SHA-256: `6ba4a4c5c3897fc053ff02797c68d63b8a44ea0ff4026245ef688d0deb0bfe0d`.
Mutated SHA-256: `dc91b22bf1f49ed21802220175708c24809bfd7a6e04e2f200881bedf5e8ba1b`.

Complete predicted red set:

- `calm-server::harness::run_loop::handoff_activation_recovery_tests::handoff_activation_post_install_failure_preserves_ack_and_pending_suffix_on_retry`
- `calm-server::harness::run_loop::handoff_activation_tests::handoff_activation_authhold_blocks_and_requires_explicit_original_retry`
- `calm-server::harness::run_loop::handoff_activation_tests::handoff_activation_cancellation_startup_replay_keeps_unknown_intent_without_native_repeat`
- `calm-server::harness::run_loop::handoff_activation_tests::handoff_activation_original_unknown_survives_lost_reply_and_actual_file_reopen`
- `calm-server::harness::run_loop::handoff_activation_tests::handoff_activation_queue_and_briefing_are_durable_before_any_resource_request`
- `calm-server::harness::run_loop::handoff_activation_tests::handoff_activation_suffix_and_briefing_reach_first_input_once_without_losing_floor_queue`

```diff
--- crates/calm-server/src/operation/handoff_activation_sql.rs
+++ crates/calm-server/src/operation/handoff_activation_sql.rs
@@ -205,7 +205,7 @@
         0,
         QueueEntry::system(Observation::SystemContext { text: briefing }, None)?,
     );
-    snapshot.set_pending_entries(entries);
+    snapshot.set_pending_entries(facts.snapshot.pending_entries());
     let changed = sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2 AND state='pending_activation' AND handle_state_json=?3")
         .bind(serde_json::to_value(&snapshot)?.to_string()).bind(&scope.new_session_id)
         .bind(&facts.source_text).execute(&mut **tx).await?;
```

Result after byte-exact restoration:

```json
{
  "kind": "briefing",
  "expected": [
    "harness::run_loop::handoff_activation_recovery_tests::handoff_activation_post_install_failure_preserves_ack_and_pending_suffix_on_retry",
    "harness::run_loop::handoff_activation_tests::handoff_activation_authhold_blocks_and_requires_explicit_original_retry",
    "harness::run_loop::handoff_activation_tests::handoff_activation_cancellation_startup_replay_keeps_unknown_intent_without_native_repeat",
    "harness::run_loop::handoff_activation_tests::handoff_activation_original_unknown_survives_lost_reply_and_actual_file_reopen",
    "harness::run_loop::handoff_activation_tests::handoff_activation_queue_and_briefing_are_durable_before_any_resource_request",
    "harness::run_loop::handoff_activation_tests::handoff_activation_suffix_and_briefing_reach_first_input_once_without_losing_floor_queue"
  ],
  "actual": [
    "harness::run_loop::handoff_activation_recovery_tests::handoff_activation_post_install_failure_preserves_ack_and_pending_suffix_on_retry",
    "harness::run_loop::handoff_activation_tests::handoff_activation_authhold_blocks_and_requires_explicit_original_retry",
    "harness::run_loop::handoff_activation_tests::handoff_activation_cancellation_startup_replay_keeps_unknown_intent_without_native_repeat",
    "harness::run_loop::handoff_activation_tests::handoff_activation_original_unknown_survives_lost_reply_and_actual_file_reopen",
    "harness::run_loop::handoff_activation_tests::handoff_activation_queue_and_briefing_are_durable_before_any_resource_request",
    "harness::run_loop::handoff_activation_tests::handoff_activation_suffix_and_briefing_reach_first_input_once_without_losing_floor_queue"
  ],
  "red_exit": 100,
  "restored_exit": 0,
  "source_restored_sha256": "6ba4a4c5c3897fc053ff02797c68d63b8a44ea0ff4026245ef688d0deb0bfe0d",
  "valid": true,
  "red_log": "/tmp/handoff-activation-mutation-briefing-red.log",
  "restored_log": "/tmp/handoff-activation-mutation-briefing-restored.log"
}
```


### Final worker evidence and kernel boundary

The final complete task-specified focused selection passed **229/229**, exit 0, in
27.580 seconds; raw output: `/tmp/handoff-activation-full-focused3.log`. This includes
the real post-installation failure/ACK/suffix retry regression and all registered backend
metadata cases. A subsequent mechanical delta only removes its unused import.

Four valid production mutations have complete predicted/actual red sets of **1, 1, 4, 6**
tests respectively: exact non-null root, SQL claim-only bypass, unknown-effect reissue,
and durable briefing removal. Every restoration passes **16/16** with the exact original
production SHA-256. The first NULL-root mutation remains documented as invalid and is
not counted. All final production hashes were compared again after the last restoration.
The existing snapshot writer inventory classifies both activation writes as row-owned;
the head-schema fixture includes migration 0170. Migration 0169 remains byte-identical.

The shared start owner supplies fresh Codex resources and permissions; local provider
wrappers keep their existing deferred first-turn native session/credential contract.
Readiness and metadata tests use registered stubs, including unconfigured backend refusal;
no real native inference is claimed. Existing caller-supplied SideConversation and activity
briefings cannot supply a trusted handoff reconstruction: this domain builder therefore
reads the existing Report/task/transcript owners, bounds and labels their evidence, and
never uses caller context JSON or copies VFS session rendering.

The operation result records its original action, not a current provider health promise.
Unknown effects require evidence-based forward recovery; no native request is blindly
reissued and no old role/token/history/worker identity is compensated. There is no external
REST/MCP/AI/UI entry or automatic two-pause coordinator in this slice. No frozen FE source
or generated API artifact changes, so no FE ownership-change trailer applies.

The kernel alone runs the six declared steps on this attempt's frozen commit. The final
report must cite its returned actual verdict and gate log; this paragraph is the verification
plan, not a claim that the gate has passed. No documentation is edited after a successful
gate. Planner owns the independent L2 reviews; CI owns broad suites.


### First kernel gate and byte-preserving text fix

Kernel run r1 on `c76186392dad91caf5615da48d889843c00d8a7f` stopped at the prose
ratchet: ten new SQL literals raised `long_literal` from 148 to 158. The other four
text checks passed. The exact log is
`runs/7e93be68c1b94d6dac678f815f5602d9:handoff-forward-activation/gates/1.log`.

The ten SQL literals now use the existing `concat!` spelling. Agent-facing briefing
fragments now live in prompts, compiled with `include_str!`. The one-shot extraction
manifest and comparison are `/tmp/handoff-activation-literal-extraction.json` and
`/tmp/handoff-activation-extraction-proof.json`: every SQL and prompt byte is identical,
and expanding only those literal macros (plus their explicit same-name format arguments)
makes all five edited Rust files identical apart from whitespace. No control flow, binding,
queue, checkpoint, permission or side-effect condition changed. The critical mutation
logic and verdicts above therefore remain applicable; the full focused selection is rerun
for this mechanical delta and the kernel will verify the new frozen target.

The affected mutation source file's hash changes only for literal spelling:
`handoff_activation_sql.rs` is now
`a3c08b2fe570cd966a06e9a9ee92ce77c4199229f0c77af2985bc32f31382db1`,
from `6ba4a4c5c3897fc053ff02797c68d63b8a44ea0ff4026245ef688d0deb0bfe0d`.
The operation and migration hashes remain `5e740e93698171eda10b695918fb78555ff2b7c3be561c5e9e5511fafc3e9cdc`
and `78c9e2084ffc073c4c12e3b1d41bec3c06d270c3c27a471c027763c1d44e5330`.
The prose gate passes at its unchanged 148 baseline; migration 0170 is not edited after r1.

Prompt assets (rendered bytes preserved, including fragment newline boundaries):

- [Header](../../crates/calm-server/prompts/handoff-activation/header.md)
- [History](../../crates/calm-server/prompts/handoff-activation/history.md)
- [Report](../../crates/calm-server/prompts/handoff-activation/report.md)
- [Absent Report](../../crates/calm-server/prompts/handoff-activation/report-absent.md)
- [Task](../../crates/calm-server/prompts/handoff-activation/task.md)
- [Tasks](../../crates/calm-server/prompts/handoff-activation/tasks.md)
- [Template](../../crates/calm-server/prompts/handoff-activation/template.md)
- [Transcript](../../crates/calm-server/prompts/handoff-activation/transcript.md)
- [Truncation](../../crates/calm-server/prompts/handoff-activation/truncation.md)


After the byte-preserving extraction, the complete focused command passes **229/229**,
exit 0, in 31.574 seconds (`/tmp/handoff-activation-full-focused-r2.log`). The prose gate
also passes with its unchanged baseline. The next kernel run verifies this frozen tree.


### Second kernel gate: mechanical clippy correction

Run r2 (`50cc025e5707b36d14feded0e0364bec5b9df62d`) passes all text checks and fmt,
then clippy rejects the legal but unnecessary unit pattern `Phase::Failed { .. }`.
Removing its braces and applying rustfmt is the only change in the operation file;
`/tmp/handoff-activation-unit-pattern-proof.json` compares the complete source after
that exact syntactic substitution. The operation hash is now
`2f1f2b573483f32614280a03593e130a123b9e8c00620e6eff274b486fbb38e8`.
The unknown-effect branch, authorization, queue and resource behavior are unchanged.

The affected-package lint fix check actually passes, exit 0, in 2m27s:
`env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo clippy --locked -p calm-server --all-targets --features calm-server/codex-e2e -- -D warnings`.
Raw output: `/tmp/handoff-activation-clippy-r3.log`. No real Codex E2E is executed.


The post-clippy activation focused command (`--lib -E 'test(/handoff_activation/)'` with
all original safety/test flags) passes **16/16**, exit 0, in 3.219 seconds;
raw output: `/tmp/handoff-activation-focused-r3.log`. The next kernel run owns the final
complete 229-test replay and all six checks on the new frozen tree.


### Third kernel gate: use the existing snapshot owner

Run r3 (`c0aa56823b2546eaeafe4bf408d8a84641e8fbc6`) passes text and Rust quick,
including OpenAPI no-drift, then reports one failed contract out of 178 server/type
contracts: the lexical writer inventory skips SQL statements on lines beginning with
`let`, so it cannot see prepare's direct UPDATE. Plugin contracts pass 195/195 and
integration contracts pass 13/13. The scanner is unchanged.

Preparation now eliminates that direct writer and reuses the existing any-runtime
snapshot primitive. Under the same BEGIN IMMEDIATE it compares the entire current
pending source, then reads back the entire prepared snapshot before commit. This
keeps one storage owner and proves the briefing/queue even if SQLite silently ignores
the UPDATE. The only new direct writer left is resource binding, still classified
row-owned. A real RAISE(IGNORE) regression requires zero native requests and a typed
NeedsRecovery result. Normal pending/history admission and migrations 0169/0170 are
unchanged. The affected durability mutation is rerun below on this implementation;
the other authorization and unknown-effect guards are unchanged.


### Activation briefing_owner mutation: prediction before production write

Production file: `crates/calm-server/src/operation/handoff_activation_sql.rs`.

Command: `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --lib -E 'test(/handoff_activation/)' --no-tests fail --no-fail-fast --test-threads 8`.

Original SHA-256: `d640df0a274ca7de993fa41cddce103e0ca12742f15ecc8b79cf81631fe95295`.
Mutated SHA-256: `a73bbe301612d9377126caf26afbbbe24274b62627364e7556f5d0c6da2da5f6`.

Complete predicted red set:

- `calm-server::harness::run_loop::handoff_activation_recovery_tests::handoff_activation_ignored_snapshot_write_cannot_issue_without_durable_briefing`
- `calm-server::harness::run_loop::handoff_activation_recovery_tests::handoff_activation_post_install_failure_preserves_ack_and_pending_suffix_on_retry`
- `calm-server::harness::run_loop::handoff_activation_tests::handoff_activation_authhold_blocks_and_requires_explicit_original_retry`
- `calm-server::harness::run_loop::handoff_activation_tests::handoff_activation_cancellation_startup_replay_keeps_unknown_intent_without_native_repeat`
- `calm-server::harness::run_loop::handoff_activation_tests::handoff_activation_original_unknown_survives_lost_reply_and_actual_file_reopen`
- `calm-server::harness::run_loop::handoff_activation_tests::handoff_activation_queue_and_briefing_are_durable_before_any_resource_request`
- `calm-server::harness::run_loop::handoff_activation_tests::handoff_activation_suffix_and_briefing_reach_first_input_once_without_losing_floor_queue`

```diff
--- crates/calm-server/src/operation/handoff_activation_sql.rs
+++ crates/calm-server/src/operation/handoff_activation_sql.rs
@@ -198,7 +198,7 @@
         0,
         QueueEntry::system(Observation::SystemContext { text: briefing }, None)?,
     );
-    snapshot.set_pending_entries(entries);
+    snapshot.set_pending_entries(facts.snapshot.pending_entries());
     // The caller owns BEGIN IMMEDIATE. Compare the complete source under that
     // writer lock, then use the existing snapshot writer instead of a second
     // pending-only implementation. Read-back proves durability even if a trigger
```

Result after byte-exact restoration:

```json
{
  "kind": "briefing_owner",
  "expected": [
    "harness::run_loop::handoff_activation_recovery_tests::handoff_activation_ignored_snapshot_write_cannot_issue_without_durable_briefing",
    "harness::run_loop::handoff_activation_recovery_tests::handoff_activation_post_install_failure_preserves_ack_and_pending_suffix_on_retry",
    "harness::run_loop::handoff_activation_tests::handoff_activation_authhold_blocks_and_requires_explicit_original_retry",
    "harness::run_loop::handoff_activation_tests::handoff_activation_cancellation_startup_replay_keeps_unknown_intent_without_native_repeat",
    "harness::run_loop::handoff_activation_tests::handoff_activation_original_unknown_survives_lost_reply_and_actual_file_reopen",
    "harness::run_loop::handoff_activation_tests::handoff_activation_queue_and_briefing_are_durable_before_any_resource_request",
    "harness::run_loop::handoff_activation_tests::handoff_activation_suffix_and_briefing_reach_first_input_once_without_losing_floor_queue"
  ],
  "actual": [
    "harness::run_loop::handoff_activation_recovery_tests::handoff_activation_ignored_snapshot_write_cannot_issue_without_durable_briefing",
    "harness::run_loop::handoff_activation_recovery_tests::handoff_activation_post_install_failure_preserves_ack_and_pending_suffix_on_retry",
    "harness::run_loop::handoff_activation_tests::handoff_activation_authhold_blocks_and_requires_explicit_original_retry",
    "harness::run_loop::handoff_activation_tests::handoff_activation_cancellation_startup_replay_keeps_unknown_intent_without_native_repeat",
    "harness::run_loop::handoff_activation_tests::handoff_activation_original_unknown_survives_lost_reply_and_actual_file_reopen",
    "harness::run_loop::handoff_activation_tests::handoff_activation_queue_and_briefing_are_durable_before_any_resource_request",
    "harness::run_loop::handoff_activation_tests::handoff_activation_suffix_and_briefing_reach_first_input_once_without_losing_floor_queue"
  ],
  "red_exit": 100,
  "restored_exit": 0,
  "source_restored_sha256": "d640df0a274ca7de993fa41cddce103e0ca12742f15ecc8b79cf81631fe95295",
  "valid": true,
  "red_log": "/tmp/handoff-activation-mutation-briefing_owner-red.log",
  "restored_log": "/tmp/handoff-activation-mutation-briefing_owner-restored.log"
}
```


The owner adjustment focused selection passes **20/20**, exit 0, in 7.138 seconds:
17 activation observers plus all three handle-state writer inventory tests.
Command: `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --lib --test runtime_suite -E 'test(/handoff_activation|migration_suite::handle_state_writers::/)' --no-tests fail --no-fail-fast --test-threads 8`.
Raw output: `/tmp/handoff-activation-owner-focused-r4-final.log`. The new ignored-write
case originally expected Stuck; the real operation owner classifies Conflict as Failed
from Pending, so its assertion now checks that declared refusal and typed NeedsRecovery.

The refreshed durability mutation's complete red set is **7/7 as predicted**, and its
byte-exact restoration passes **17/17** (`briefing_owner` record above). The final SQL
adapter SHA-256 is `d640df0a274ca7de993fa41cddce103e0ca12742f15ecc8b79cf81631fe95295`.
The other critical guards are unchanged; the kernel now replays 230 focused tests.


The current affected-package all-target clippy check passes, exit 0, in 2m15s
(`/tmp/handoff-activation-clippy-r4.log`), with the same command/flags recorded above.
The next kernel run owns all six checks, including the complete 230-test frozen replay.

## Full snapshot recovery follow-up from d1c865a3

Attempt `7e93be68c1b94d6dac678f815f5602d9:handoff-activation-full-snapshot-recovery`
starts exactly at `d1c865a309a32da8d17edb320cabc474799f1402`, without syncing main.
Both original independent reviews (`handoff-activation-review-a`, completed 417936;
`handoff-activation-review-b`, completed 418022) identify the same P1: the complete
current snapshot passed catch-up CAS but was discarded in favor of the operation's
initial Idle capture plus row queue fields. The older 230-test gate does not disprove it.
The [design](planner-conversation-handoff.md#start-capture-and-same-session-restoration-ownership)
now distinguishes start capture from same-session recovery, including phase and unknown
native ownership. Review tier remains L2; the Planner arranges fresh complete reviews.

### Actual initial reproduction

Command: `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --lib -E 'test(/handoff_activation_post_install_retry/)' --no-tests fail --no-fail-fast --test-threads 8`.
Raw output: `/tmp/activation-full-old-red.log`, run
`a1ca9135-4f90-4560-ab13-d7a37b80527e`: **2 selected, 2 failed**, exit 100.
An earlier compile-only failure was a missing test trait import and is not reproduction evidence.

- `handoff_activation_post_install_retry_preserves_completed_usage_and_replace_rewind`:
  real cutover/OperationRuntime/SQL trigger rejects the final Activated checkpoint after
  registry installation. The installed owner completes its briefing turn, issues a real
  user turn, receives its user echo, usage and completion notifications, then commits
  `replace_turn_durable`. Reopening the file database and rebuilding OperationRuntime
  with the same registry retries the original operation. Expected `thread/revert` before
  replacement input was absent (`[]` versus the actual thread/second-turn identity with
  two preceding turn starts). No snapshot field or private runtime field is seeded.
- `handoff_activation_post_install_retry_running_interrupt_failure_restores_resumed`:
  after actual issuance and usage notification, a second durable user entry waits while
  the original turn runs. Retry produced **IssuingTurn**, not Resumed: it had already
  reopened issuance from the stale Idle capture. The initial red stops in the ordinary
  shutdown case; the final green additionally exercises injected interrupt failure.

The first briefing turn is deliberately not replaced: production rejects replacement of
a turn containing system observations. The test completes it and replaces a later real
user turn instead. Provider notifications use the existing completed-handler barrier;
a fixture-only barrier after the real replace command answer prevents a timed race with
next issuance. Stable committed phase barriers use the public snapshot owner, not sleeps.
The pre-existing ACK/suffix regression remains in the full selection.

### Owner change and unchanged boundaries

`HarnessInstallation` explicitly selects StartCapture versus ActivationRecovery.
Activation reads the complete row after predecessor shutdown/reservation, checks its
resource/thread identity, and shares `harness::recovery::prepare_quiesced` with boot/lazy
replacement. ACP receipts, pause health invalidation and eligible suffix replay remain in
their existing owners. The complete-source CAS accepts the candidate which is passed
directly to installation; activation never calls partial queue adoption. Snapshot phase
restoration still uses `state_from_snapshot` (running/interrupt -> Resumed), and shutdown
interrupt failure does not create terminal evidence. Ordinary start retains capture
phase/thread decisions and its existing ACP/queue/pause replay branches. Its existing
`durable_observation_start_replay_preserves_row_progress` now also pins phase/turn isolation.
The included boot suite exercises actual redriven start, Assistant recovery and queue races.

No new migration, API binding, provider lifecycle, SQL permission, growing provider/test
list or CI pass is added. Existing large owning files are extended; the new recovery
responsibility file is 50 lines. Ordinary tests change only their module; provider variants
extend the existing backend owners. The kernel owns the six declared gate steps once on
the frozen target, including one focused replay. Worker owns focused iteration and fmt;
CI owns broad suites. No real native inference, real Codex E2E or Tier 2 is run. Configured
local deferred-native activation remains outside this stub coverage; registered unconfigured
provider tests prove readiness refusal and metadata only.

### Retained mutation evidence binding

`/tmp/activation-full-retained-mutation-proof.json` records byte comparison with d1c865a3.
The activation SQL remains SHA-256
`d640df0a274ca7de993fa41cddce103e0ca12742f15ecc8b79cf81631fe95295`;
0170 remains `78c9e2084ffc073c4c12e3b1d41bec3c06d270c3c27a471c027763c1d44e5330`;
0169 remains `711590b2c96fb665bb15c33a066bd945c2e0a4c61bfc22576e525c9691e8ca77`.
The old exact-root/SQL-fence/briefing-owner mutations retain their unchanged production
invariants and recorded valid red/restored sets (1/1/7). The Unknown guard is byte-identical
(SHA-256 `89f912067147235b83044d1d46dc90a953200b458ec16bf876d5f9e463c211ca`);
the entire operation diff changes only the installer import and typed dispatch argument,
after that guard. The previous valid four-red Unknown result remains evidence for that
unchanged guard, not for the changed installation semantics. No historical full-file hash
is presented as the new operation file hash. New phase isolation mutation evidence follows.

### Final phase isolation mutation and complete restoration

The initial complete follow-up run was 231/232: its running test sampled the before-row
before the usage notification, because the generic notification hook could observe an older
queued frame. The barrier now additionally waits for the notification's actual public durable
usage/terminal/transcript fact. This strengthens scheduling control without seeding fields.
The complete context selection then passed 232/232 (`/tmp/activation-full-fixed-focused3.log`,
exit 0, 28.753 seconds). Compile-only type/import/ownership fixes are not counted as red tests.

Ordinary start now passes a required, decoded capture and profile in its typed variant,
so invalid output still fails before predecessor shutdown. Activation's variant fixes Planner
profile and never decodes an operation snapshot as restoration input. A preliminary mutation
was rerun after this interface tightening; only the final source-bound evidence below is used.

Both mutation and restoration run this **entire context selection**, including all three
selected binaries, not only the predicted failing test:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --lib --test planner_harness_suite --test planner_card_reset -E 'test(/handoff_activation|handoff_cutover|handoff_pause|durable_observation|dispatcher::tests|planner_preserving_recovery::observation_recovery|planner_harness_boot_recovery|reset_planner_card_preserves_runtime_pending_queue_and_push_watermark|public_snapshot_persistence_waits_for_issuance_checkpoint_owner|harness::tests::|harness::snapshot::|interrupt|watchdog|timeout|system_error/)' --no-tests fail --no-fail-fast --test-threads 8
```

Production file: `crates/calm-server/src/operation/planner_harness_start_adapter.rs`.
Original SHA-256: `c7d5984a368885721d26142902be3f240f365f81d7c9efa8288a559a4a347e6f`.
Mutated SHA-256: `5d7dfb03bf3ba7655f70da934590fc4ec78339d7595d250599fd347ff0f0dce0`.

Complete predicted red set (saved before writing production):

- `calm-server harness::run_loop::handoff_activation_recovery_tests::handoff_activation_post_install_retry_running_interrupt_failure_restores_resumed`

```diff
--- crates/calm-server/src/operation/planner_harness_start_adapter.rs
+++ crates/calm-server/src/operation/planner_harness_start_adapter.rs
@@ -236,7 +236,9 @@
         };
         let snapshot = match installation {
             HarnessInstallation::ActivationRecovery => {
-                self.activation_catch_up(output, &binding).await?
+                let mut snapshot = self.activation_catch_up(output, &binding).await?;
+                snapshot.phase = HarnessPhaseTag::Idle;
+                snapshot
             }
             HarnessInstallation::StartCapture { snapshot, .. } => {
                 let mut snapshot = *snapshot;
```

The source write is confirmed against the mutated hash before execution. The script's
`finally` restores the exact saved production bytes before the green replay; no tests are
mutated, no destructive checkout/reset is used, and no concurrent writer/reviewer observes
transient code. Prediction, full diff, original source backup, red/restored output and result
are `/tmp/activation-full-phase-mutation-{prediction.json,diff,original.rs,red.log,restored.log,result.json}`.
Final mutation: **232 run, 231 passed, exactly 1 predicted failure**, exit 100, 35.969 seconds.
Restoration: **232/232 passed**, exit 0, 48.326 seconds. Exact comparison result:

```json
{
  "expected": [
    "calm-server harness::run_loop::handoff_activation_recovery_tests::handoff_activation_post_install_retry_running_interrupt_failure_restores_resumed"
  ],
  "actual": [
    "calm-server harness::run_loop::handoff_activation_recovery_tests::handoff_activation_post_install_retry_running_interrupt_failure_restores_resumed"
  ],
  "red_exit": 100,
  "restored_exit": 0,
  "source_restored_sha256": "c7d5984a368885721d26142902be3f240f365f81d7c9efa8288a559a4a347e6f",
  "valid": true
}
```

The final running test compares the complete persisted snapshot, allowing only the original
TurnRunning -> Resumed recovery phase conversion. It checks both successful and failed
shutdown interruption without repeated turn issuance. The replacement test verifies actual
`thread/revert` before the replacement input, retained lifetime usage/invalidation/deduplication,
and a single resource mint and briefing across file reopen/retry. Existing full-selection
ACK/suffix, pause health, original start capture, provider binding and source-CAS conflict tests
remain green. This does not claim real native readiness or eventual provider reconciliation.

The final tree is frozen for the six-step kernel gate. Its actual verdict/log belongs in the
completion report; this document is not edited after a passing gate.

### Kernel r1: regenerate the existing terminology count

Kernel r1 stops at the text gate: the new notification barrier's one call to the existing
transcript repository list method raises the crates legacy-symbol count from 308 to 309.
The other four text checks pass. The existing generator
`scripts/gate-1316-terminology-ratchet.sh --update-baseline` regenerates that counted TSV;
no pattern, exclusion, scan policy or provider/test roster changes. This is one test-only
reference to an existing contract, not a new domain identifier. Production and mutation
source hashes are unchanged. The raw kernel log is
`runs/7e93be68c1b94d6dac678f815f5602d9:handoff-activation-full-snapshot-recovery/gates/1.log`.
The next kernel run verifies the complete frozen tree again.
