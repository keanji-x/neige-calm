# SpawnFailed verification evidence (#2353)

Tier L2: durable lifecycle and deletion authorization. Formal independent review
is owned by the Planner; this execution did not launch reviewers or commit.

## Focused commands actually run

All nextest commands use the following prefix and suffix:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked --no-tests fail <selection> --test-threads 8
```

Selections and actual results:

- `-p calm-server --lib -E 'test(operation::terminal_adapter::tests::launch_cleanup_tests::spawn_failed_definite_ack_compensates_worker_rows)'`: baseline exit 100, 1 failed (1525 skipped). Actual phase Stuck, expected Failed; error `worker launch cleanup is unverified`. Production was unchanged at this red run.
- `-p calm-server --lib -E 'test(operation::terminal_adapter::tests::launch_cleanup_tests::) | test(operation::terminal_adapter::tests::disposal_tests::) | test(operation::terminal_adapter::tests::pty_cwd_tests::) | test(operation::codex_adapter::tests::viewer_cleanup_tests::)'`: first fix exit 0, 26/26 passed. Added restart fixtures initially failed (2 failures) because injection allowed drive's fallback Stuck write; fixture corrected to block both writes. An intermediate compile failure was a missing WorkerSessionProjectionRepo import, corrected.
- `-p calm-server --lib -E 'test(operation::terminal_adapter::tests::launch_cleanup_tests::)' --no-fail-fast`: final worker run exit 0, 9/9 passed (1522 skipped). The kernel gate replays the full requested lib group against the final frozen tree.
- `-p calm-server --test runtime_suite -E 'test(kernel_process_suite::reconcile_supervisor_on_boot::) | test(no_double_spawn::terminal_create_recovery_spawn_failure_clears_stale_pid_before_compensation)'`: exit 0, 7/7 passed (601 skipped).
- `-p calm-proc-supervisor --test integration_suite -E 'test(pty_cwd::)'`: exit 0, 6/6 passed (26 skipped).
- `-p calm-session --lib -E 'test(control::spawn_failed_tests::)'`: exit 0, 2/2 passed (11 skipped). Actual bincode frames roundtrip both required dispositions; exact v1 SpawnFailed shape fails decoding, as does an unknown JSON disposition.
- `cargo fmt --all`, `cargo fmt --all --check`, `git diff --check`: exit 0.

No broad suites, tier 2 or real Codex E2E were run. Kernel owns ratchet, contract,
quick Rust gates and final replay. Gate results are in the attempt's gate log.

## Production mutation

Exclusive worktree, no parallel writer/reviewer. For each mutation, predicted
complete red set before changing production, asserted the single replacement
was applied, ran the entire launch_cleanup_tests filter with `--no-fail-fast`,
compared the complete actual red set, restored original bytes in `finally`,
then ran the same filter green. Tests/fixtures were never mutated.

Every name below has prefix
`operation::terminal_adapter::tests::launch_cleanup_tests::`.

| Single-factor mutation | Predicted = actual red set | Actual output | After restore |
| --- | --- | --- | --- |
| Disable NoChildCreated receipt persistence in renderer | spawn_failed_definite_ack_compensates_worker_rows; spawn_failed_ack_restart_consumes_rejection_before_boot_exit; spawn_failed_compensation_restart_is_idempotent | exit 100; 9 tests: 6 passed, 3 failed | exit 0; 9/9 |
| Remove only negative CAS socket equality | spawn_failed_negative_cas_fences_retain_ownership | exit 100; 9 tests: 8 passed, 1 failed | exit 0; 9/9 |
| Remove only business veto from rejected cleanup branch | spawn_failed_ack_restart_consumes_rejection_before_boot_exit | exit 100; 9 tests: 8 passed, 1 failed | exit 0; 9/9 |

Final byte comparison with each saved production source succeeded; formatter and
diff checks found no mutation residue. The mutation runner printed
`ALL MUTATIONS VERIFIED; all production bytes restored`.

## Compatibility and scope

Required classification is appended to bincode SpawnFailed fields: old receipts
cannot accidentally become NoChildCreated. Control version is 2, consumed by
existing version/package metadata and upgrade compatibility preflight. Deploy
server and supervisor together. Old/missing/unknown durable checkpoints stay
conservative, no migration/backfill. Generic driver is unchanged.

The regression uses actual claimed report tasks, TerminalWorkerAdapter,
OperationRuntime, real supervisor and a proxy forwarding its real reply. Restart
cases reopen file SQLite, rebuild runtime/state, execute actual boot reconciliation
and recovery, and repeat recovery. Injection interrupts either after durable ack
or after row cleanup before completion persistence. It verifies synthetic -1
cannot win over rejection, no second Ensure, missing prepared runtime/card/terminal,
workspace and sibling preservation, successful fast exit, business veto, and
unknown/disconnected/invalid-CAS/write-failure retention.

Pre-existing follow-up: worker_cleanup's rollback error branch only logs and
returns Deleted; terminal/Claude callers ignore the outcome. This existed before
this change and was not expanded or repaired here. See
`crates/calm-server/src/operation/worker_cleanup.rs` rollback error branch and
`crates/calm-truth/src/db/sqlite/card_composite.rs` rollback transaction.

First gate stopped at text checks: retiring-vocabulary count and seven long SQL
literals. Replaced the test's legacy output-key access with a direct zero-session
row-count assertion, and split SQL source literals using Rust line continuation
without changing SQL semantics. Both reported text gates then passed locally;
no baseline changes. Final kernel replay revalidates this delta.
