# Single workflow redesign groundwork

Status: incomplete. The single-owner implementation and acceptance gates are not
delivered by these groundwork changes. The existing three-operation runtime remains.
See [the proposed ownership/state/deletion matrix](planner-handoff-single-workflow.md).

The input evidence is the original overall reviews A (completed event 422185) and
B (completed event 422132), read from their own-track run JSON, and the actual
coordinator, cutover, activation, runtime, pause and resource/recovery callers.
Released migrations through 0171 and FE files have no changes.

## Reproduction boundary

The new `handoff_workflow_regression_tests` module invokes the real typed `begin`,
OperationRuntime, SQLite, registered production adapters and existing native stub.
It never seeds successful pause, cutover or activation receipts. Two fixture-only
pause seams select production boundaries: after admission commits but before drive,
and after Activated/reservation commit but before the adapter returns to the runner.
They add no release-build behavior or alternative implementation of the invariant.

The Reserved test aborts original begin without cancellation, performs a real durable
user-queue enqueue/delete on the same original session, reopens the SQLite file and
applies production boot recovery. Original claims and operation ID must remain fixed;
roles must remain Planner/Assistant, pause state Unpaused and native count zero.

The prepare-storage test installs an ABORT trigger only on activation's pending output
write. Identity commit and real pause consumption precede this fault. It checks retired
claims, roles, zero native resources and close/delete exclusion, removes the trigger
through a reopened file pool and invokes explicit retry on the original key/generation.
The required eventual result is Activated, one native resource and terminal exclusion
release. It does not manually alter operation phases or fabricate a recovery receipt.

The terminal test interrupts begin after actual Activated and active=0 commit while
the runner still has AppServerInteract. It retains exact consumed claims and installed
owner identity, adds a real provider authentication hold, reopens SQLite, applies boot
recovery and explicitly retries. Query must retain historical Activated, the native
count must stay one and the owner must not be reinstalled. This is a committed-success
reentry test, not the old branch test that rejects the Activated write itself.

Successful paths also check reservation release and real Track close/delete. Existing
legacy regression assertions are not weakened or removed.

## Command and evidence

Both worker runs use the task's complete focused selection:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked -p calm-server --lib \
  --test planner_harness_suite --test planner_card_reset \
  -E 'test(/handoff_workflow|handoff_coordinator|handoff_activation|handoff_cutover|handoff_pause|durable_observation|dispatcher::tests|planner_preserving_recovery::observation_recovery|planner_harness_boot_recovery|reset_planner_card_preserves_runtime_pending_queue_and_push_watermark|public_snapshot_persistence_waits_for_issuance_checkpoint_owner|harness::tests::|harness::snapshot::|interrupt|watchdog|timeout|system_error/)' \
  --no-tests fail --no-fail-fast --test-threads 8
```

Run 1, `/tmp/handoff-workflow-original-red.log`: 257 selected, 254 passed and three
failed. Only Reserved was a valid defect reproduction: after recovery the actual roles
were planner_history/planner instead of planner/assistant. The other two failures hit
the old fixture projection helper's inability to read superseded sessions; they are
not claimed as defect reproductions. The helper was replaced with the existing
full session-row snapshot reader, and the nested mutation-command result is explicitly
checked. No business assertion was weakened. The test module was rustfmt formatted.

Run 2, `/tmp/handoff-workflow-original-red-2.log`, nextest run ID
`b73f0617-b980-49c4-8ce5-b15b8eabe56b`: compilation completed without the earlier
unused-result warning; 257 selected, 254 passed, three failed, 2066 skipped.
All three are now actual production-boundary defect reproductions:

| Regression suffix in handoff_workflow_regression_tests | Observed failure |
| --- | --- |
| handoff_workflow_reserved_restart_without_cancel_never_reacquires_live_approval | After file reopen and boot recovery, actual roles are planner_history/planner, contrary to planner/assistant. |
| handoff_workflow_committed_prepare_abort_reopens_and_explicitly_forwards_original_key | Original activation is Stuck with no prepared output/native call; after removing ABORT, reopening the file and explicitly retrying its original generation, the required Activated result is still unreachable. |
| handoff_workflow_activated_lost_answer_remains_absorbing_after_hold_and_recovery | Activated and inactive reservation really committed first. After missing the answer, reopening and recovering under a later hold, original query returns Blocked with resources_checkpointed=true. Claims remain consumed, reservation inactive and native count one. |

The entire actual red set is exactly those three tests. The prior 254 tests pass;
that fact does not establish a corrected workflow. No mutation has been performed.
The tested regression source SHA-256 is
`f79e713b14ddc9fe69a308160f49829ad4b93e922a9f8e8e7852de5f9e146a24`.
`rustfmt --edition 2024 --check` on that module and `git diff --check` pass.
Separate runner-tail failure, later legitimate handoff, the other precommit crash
phases and complete legacy adoption cases remain untested in these new regressions.

## Unfinished acceptance

No new domain sum-state, nonserializable live capability, atomic single-owner identity
checkpoint, atomic absorbing terminal reader, resource proof migration or legacy
adoption fencing has been implemented. No old normal registration/service/mechanism or
persisted duplicate has been deleted. The coordinator adapter remains byte-identical
to the rejected baseline (SHA-256
`dc13992bc09134b48bbbd05e8fc6041d99e695851d507024e1e834789c444f31`).
Normal coordinator interaction still performs eight full output checkpoints plus its
initial prepare; cutover and activation still have their own rows/checkpoints. There
is no measured simplified after-count to report.

No critical mutation has been run because no corrected implementation exists yet.
No kernel gate, real API generator, provider inference, Tier 2, browser or broad suite
has been run. Formatter and `git diff --check` are the only non-focused checks here;
they do not establish semantic correctness. The six kernel gates and fresh Planner L2
reviews remain required for a completed implementation.
