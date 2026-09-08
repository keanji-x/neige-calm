# Lease settlement repair — focused implementation handoff

Base: 14a4252bb38434c47242dca1a8ccef52ebd33e91. Branch: codex/1501-candidate-lease-repair. Exclusive original Euler worktree, existing physical neige1501-candidate-target, flock /tmp/neige-1501-cargo.lock. No work in the obsolete lease-liveness tree.

## One proven mechanism

OperationRuntime::wait polls at 25ms and invokes steady parked recovery; production candidate submission itself waits, and the fixture adds another waiter. Recovery claimed a retained exited leader's lease, spent a median 58ms checking/cleaning its stopped group, returned LeaveParked and released it. The observer's 2s retry repeatedly missed. Single diagnostic correlated eight denied UUIDs with those no-op recovery UUIDs. The historical 30s timeout was NOT reproduced; diagnostic passed 25.602s and was never called a fix.

## Approved smallest fix

ProviderAdapter gets a default-allow read-only eligibility hint. The shared steady sweep checks it after missing-deadline validation and before its pre/deadline split. Only CandidateVerifyAdapter defers, with exact boot/PID/start-time, matching pgrp, retained Z/X leader and group_stopped=Ok(true). Missing/uncertain proof allows original recovery. No kind-string dispatch, no state storage, no new process manager. Boot force claim/release, cancel, compensation, completion lease fence, ProcessIdentity, group proof, actual kernel wait status and exit-file rules remain intact. A1 design documents full callers and stale allow/defer races. Existing oversized core trait/driver files gain only 8/9 lines respectively to keep the framework change at its existing boundary; new files remain below 800 lines.

## Deterministic acceptance

Completion fixture hook pauses after real WNOWAIT + group cleanup. Fixture-local SQLite trigger audits actual production lease acquisition. Two tests cover zero steady claims pre/deadline; both were RED at that exact assertion before the fix. Read-hint hook pauses after computing a real hint; tests pin allow->exit and defer->settled/reaped races. Scheduler's own waiter is handshaken before audit baseline, avoiding a stale prior sweep contaminating results. Every new success path checks actual exit 0 despite forged gate.exit=7, releases capacity, and waits for leader reaping/group removal; cancellation checks Failed and cleanup. RAII resumes barriers and identity-gated group cleanup on failure.

Focused GREEN: 8/8 in 12.702s (`lease-green.log`): four new lease tests, original held-lease test, live overdue kill/timeout, missing-leader live-group capacity, interrupted snapshot/policy recovery. No broader candidate-suite or workspace GREEN claim.

Mutation verification PASS: final fixture version, exactly the preregistered two tests RED at zero-claim assertions, restored source SHA256 equal to original fixed source, then 2/2 GREEN. `lease-mutation-result.json` records exact sets and hashes; the script restores in finally. Scoped feature-enabled Clippy PASS with `-D warnings`; fmt PASS. Default-feature library check PASS (45.83s). Parent owns fresh independent full reviews/integration as agreed; no independent full-review claim from this worktree.

## Exact commands and source evidence

Every Cargo command used `flock /tmp/neige-1501-cargo.lock env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 CARGO_TARGET_DIR=/mnt/data2/kenji/.build/neige1501-candidate-target`.

- Initial RED and final mutation selection: `cargo nextest run --locked -p calm-server --test mcp_integration_suite candidate_lease_defers --test-threads 8 --no-fail-fast`. Initial test version appears in lease-red.log; final handshaken fixture is pinned by mutation RED/restore GREEN.
- Focused GREEN: `cargo nextest run --locked -p calm-server --test mcp_integration_suite -E 'test(candidate_lease_) | test(candidate_verification_live_observer_respects) | test(candidate_verification_owned_deadline_kills) | test(candidate_verification_recovery_retains_capacity_when_leader_missing_group_live) | test(candidate_verification_recovery_reexecutes_same_snapshot)' --test-threads 8 --no-fail-fast`.
- Mutation: `python3 .codex-local/lease-mutation.py` under the same prefix. Plan file was written before execution.
- Lint: `cargo clippy --locked -p calm-server --lib --test mcp_integration_suite --features calm-server/codex-e2e -- -D warnings`. This compiles the feature combination; no real Codex E2E ran.
- Default build surface: `cargo check --locked -p calm-server --lib`.
- Formatting: `cargo fmt --all --check`; final diff: `git diff --check`.

Call path in this patch: operation/driver.rs:290 wait, :314 25ms timer, :318/:356 steady enforcement, :860 hint before both claim branches. scheduler/file_delivery.rs:187 is the production candidate waiter. operation/mod.rs:762 defaults the hint to true. file_delivery/candidate_verify.rs:432/563 computes candidate eligibility; :197 remains the parked-lease denial; :421 remains the 2s completion retry; :469/470 retains deadline recovery's exited-leader/group cleanup rule. Both SQL claim implementations and owned_parked::reconcile are untouched.

Owner proof: lease-owner-evidence.log contains exact numbered excerpts; lease-owner-observations.json correlates all eight denials and records full diagnostic log SHA256. The complete log and diagnostic script remain in this worktree's .codex-local/lease-owner-diagnostic.log and lease-owner-diagnostic.py. No original-timeout reproduction, full workspace suite, release build, OpenAPI generation, deployment, or independent full-review completion is claimed. The scoped checks honor the user's prohibition on workspace builds. No wire/schema/generated artifact changed.

Changed source scope: five production/fixture-hook files (candidate_verify.rs, file_delivery/mod.rs, test_hooks.rs, operation/driver.rs, operation/mod.rs), review test module registration, new candidate_verification_lease.rs, and the existing A1 design document. candidate_authoring.rs, fixtures, plugin_host/child_process.rs, every other worktree, and historical untracked evidence are untouched.
