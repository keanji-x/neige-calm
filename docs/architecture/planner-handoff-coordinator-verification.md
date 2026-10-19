# Durable coordinator verification

Review tier: L2. This record covers the internal coordinator described in
[the design and recovery matrix](planner-handoff-coordinator.md). Formal independent
reviews belong to the Planner. No external REST/MCP/AI/UI entry is delivered.

## Production boundary and reproduction

The initial production-registry regression was red: nextest run
`3814b736-7587-4a05-b3c4-caf6257e1b56`, one selected test, failed
`handoff_coordinator_is_declared_in_the_production_operation_contract` because the
accepted base had no coordinator. The completed regression uses the actual production
adapter registry; composition tests invoke the actual typed admission, OperationRuntime,
SQLite store, real pause owners, keyed cutover and activation adapter with native stubs.
They do not seed successful pause/cutover/activation receipts.

Coverage includes exact authorization/generation, same/competing keys, queued input,
busy driver refusal, source/target pause intent and receipt boundaries, cancellation,
dropped answer, serial absence proof, health fault/unconfirmed release, storage failure,
actual SQLite file reopen, explicit generation retry, committed cutover lost progress,
activation hold/Unknown and system observation delivery at handoff boundaries. The existing
primitive, dispatcher, recovery and transaction suites retain their owning assertions.

## Worker selection

The worker and mutations use this complete requested selection, never an individual
predicted failing test:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked -p calm-server --lib \
  --test planner_harness_suite --test planner_card_reset \
  -E 'test(/handoff_coordinator|handoff_activation|handoff_cutover|handoff_pause|durable_observation|dispatcher::tests|planner_preserving_recovery::observation_recovery|planner_harness_boot_recovery|reset_planner_card_preserves_runtime_pending_queue_and_push_watermark|public_snapshot_persistence_waits_for_issuance_checkpoint_owner|harness::tests::|harness::snapshot::|interrupt|watchdog|timeout|system_error/)' \
  --no-tests fail --no-fail-fast --test-threads 8
```

The package lint command is:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo clippy --locked -p calm-server --lib --tests --features codex-e2e -- -D warnings
```

## Mutation predictions (recorded before mutation)

Only the production coordinator adapter is mutated, in this exclusive worktree without
concurrent review or kernel gate. Each mutation retains an exact byte before-image,
SHA-256 and single-factor diff; the complete actual red set is compared to this prediction.
The before-image is restored safely without checkout/reset; restored hashes and green
selection establish absence of mutation residue. These names are historical verification
evidence, not a maintained test registry.

M1 removes the first source intent checkpoint before invoking the real pause. That
checkpoint also validates the captured generation atomically. Predicted complete red set:

- `harness::run_loop::handoff_coordinator_tests::handoff_coordinator_cancel_at_each_precommit_checkpoint_compensates_original_claims`
- `harness::run_loop::handoff_coordinator_tests::handoff_coordinator_intents_are_durable_before_any_real_pause`
- `harness::run_loop::handoff_coordinator_tests::handoff_coordinator_dropped_pause_answer_recovers_original_claim_without_repause`
- `harness::run_loop::handoff_coordinator_recovery_tests::handoff_coordinator_interrupted_intent_before_send_uses_serial_absence_barrier`
- `harness::run_loop::handoff_coordinator_recovery_tests::handoff_coordinator_pointer_aba_rejects_original_approval_before_any_pause`

M2 discards a positive original cutover query result after a missing postcommit checkpoint,
misclassifying the operation as absent and entering precommit compensation. Predicted
complete red set:

- `harness::run_loop::handoff_coordinator_tests::handoff_coordinator_cutover_commit_reply_failure_queries_success_without_release`

## Verification ownership and remaining boundaries

The kernel owns the six task gate steps: text gates, quick Rust, Rust contracts, complete
focused replay, `calm-truth --lib decision_gate::tests`, and FE install/lint/build/tests.
The final gate result is reported from the kernel log; this document is not edited after
a successful gate merely to append its status. CI owns broad suites. No real Codex/Tier 2
E2E, external messages, self-commit, self-arranged formal review or visible UI work occurs.

Migration 0175 is the only migration changed in this slice; 0174 and earlier remain byte
frozen. Internal SQLite tables do not change public wire schemas, generated consumers or
FE ownership paths; no API generator or browser preview is needed. Existing provider
Unknown recovery still requires explicit evidence and never retries native allocation
blindly. All new responsibility/test files remain below 800 lines.

## Worker baseline and before-image

Final pre-mutation selection: **254 passed**, zero failed. Package lint: exit 0.
The production adapter before-image SHA-256, recorded before M1, is
`dc13992bc09134b48bbbd05e8fc6041d99e695851d507024e1e834789c444f31`. M1/M2 mutate this file only; the before-image is retained
at `/tmp/handoff-coordinator-adapter.original`.

M1 completed: **254 selected, 249 passed, exactly the 5 predicted failures**;
complete actual red set matched the prediction. Mutated SHA-256:
`cb68db4fd8f78a409e2619635a503ff2ff24e44cd908786e081c797a47b79e6a`.
Patch and complete log: `/tmp/handoff-m1.patch`, `/tmp/handoff-m1.log`.
Restored production bytes match the recorded before-image SHA-256.

M1 restoration selection: **254 passed**, zero failed. M2 before-image is the
same restored SHA-256 `dc13992bc09134b48bbbd05e8fc6041d99e695851d507024e1e834789c444f31`; its one-test
red prediction above is fixed before applying M2.

M2 completed: **254 selected, 253 passed, exactly the 1 predicted failure**;
complete actual red set matched the prediction. Mutated SHA-256:
`1e75b8adcf204d9103c9879152f38d56ac0c7abe00cf70dac6055ad1ef76bb71`.
Patch and complete log: `/tmp/handoff-m2.patch`, `/tmp/handoff-m2.log`.
Restored production bytes again match the recorded before-image SHA-256.

M2 restoration selection: **254 passed**, zero failed. Final production file equals
the exact before-image, with no mutation residue. The final diff/status inspection found
only intended coordinator, primitive composition, migration inventory, tests and docs.
The existing head-schema filename inventory now includes 0175; no released migration
or FE/generated file changes.

| Selection | Nextest run | Actual result |
| --- | --- | --- |
| baseline | `4568af48-a83f-4038-8c05-965aea1db2f9` | 254 tests run: 254 passed, 2066 skipped |
| M1 red | `a23be45d-79f2-44ce-b90f-0d92ec10591f` | 254 tests run: 249 passed, 5 failed, 2066 skipped |
| M1 restored | `db7e9bd5-6869-4082-b09f-25f1f4d99e8f` | 254 tests run: 254 passed, 2066 skipped |
| M2 red | `7a9a706b-70f4-4aad-8bcf-52d39b30e94d` | 254 tests run: 253 passed, 1 failed, 2066 skipped |
| M2 restored | `af0149c4-f523-42fc-a552-be4f6b6273c7` | 254 tests run: 254 passed, 2066 skipped |

Package lint exit 0 is recorded in `/tmp/handoff-coordinator-clippy-v3.log`.
The kernel gate is the remaining frozen-target verification owner; its actual outcome
and full log are linked by the delivery report without changing this document afterward.

## Gate iteration and architecture clarification

Kernel run 1 stopped at the prose ratchet: ten new long SQL literals, rather than
agent-facing prose, exceeded its existing count. SQL was reformatted as readable raw
multiline literals. A lexical check preserved every SQL token and quoted value; the
long-literal count is again exactly 148. No gate policy, baseline, allowlist or migration
bytes were relaxed. The request fixture builder moved to existing test support to keep
new test files below 800 lines. Production reservation SQL and the unused shared driver
helper were narrowed in visibility; there is no new production entry or retry mechanism.

The coordinator adapter that owns both mutation factors remains byte-identical to its
recorded before-image (`dc13992bc09134b48bbbd05e8fc6041d99e695851d507024e1e834789c444f31`).
The kernel replays the full selection on the corrected frozen target. The unique-owner
table and runtime necessity/removal discussion in the design address Planner message
`b256c8aea7104a37a893ae24010f6b76`; overall simplification review remains Planner-owned.
