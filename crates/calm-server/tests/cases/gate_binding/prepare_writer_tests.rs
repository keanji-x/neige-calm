//! Slow Git sampling must not hold SQLite's writer slot.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_prepare_allows_unrelated_writer_before_git_filter_release() {
    use calm_server::db::sqlite::begin_immediate_tx;

    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let (_, task, lease, _) = settled_gated_task(&fx, "writer-barrier", gated("true")).await;
    let hook_dir = tempfile::tempdir().unwrap();
    let hook = hook_dir.path().join("clean-filter.sh");
    let entered = hook_dir.path().join("entered");
    let release = hook_dir.path().join("release");
    write_executable(
        &hook,
        &format!(
            "#!/bin/sh\ntouch '{}'\nwhile [ ! -f '{}' ]; do sleep 0.02; done\ncat\n",
            entered.display(),
            release.display(),
        ),
    );
    git(
        &lease.path,
        &["config", "filter.barrier.clean", hook.to_str().unwrap()],
    );
    std::fs::write(
        lease.path.join(".gitattributes"),
        "worker.txt filter=barrier\n",
    )
    .unwrap();
    let worker_txt = lease.path.join("worker.txt");
    let content = std::fs::read(&worker_txt).unwrap();
    // Match the existing timeout regression's stat-cache invalidation, without altering content.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    std::fs::write(&worker_txt, content).unwrap();
    sqlx::query("CREATE TABLE unrelated_writer_probe (value INTEGER NOT NULL)")
        .execute(&fx.pool())
        .await
        .unwrap();

    let drive = spawn_drive(fx.scheduler(), task.clone());
    tokio::time::timeout(WAIT, async {
        while !entered.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("real scheduler sampling reached the Git filter barrier");

    let pool = fx.pool();
    let mut writer = tokio::spawn(async move {
        let mut tx = begin_immediate_tx(&pool).await?;
        sqlx::query("INSERT INTO unrelated_writer_probe(value) VALUES (1)")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok::<_, calm_server::error::CalmError>(())
    });
    // The explicit release barrier is the ordering oracle. This bound is below SAMPLE_TIMEOUT;
    // otherwise the old writer-held sampling could pass after the filter was killed on timeout.
    let committed_before_release = tokio::time::timeout(Duration::from_secs(1), &mut writer).await;
    let filter_still_running = !live_processes_naming(&hook.display().to_string()).is_empty();
    assert!(!release.exists());
    // Always release before reporting the assertion so a red run drains its real subprocesses.
    std::fs::write(&release, "release").unwrap();
    tokio::time::timeout(Duration::from_secs(30), drive)
        .await
        .expect("gate driver completed after releasing the filter")
        .unwrap();
    if committed_before_release.is_err() {
        tokio::time::timeout(Duration::from_secs(10), writer)
            .await
            .expect("writer completed after releasing the filter")
            .unwrap()
            .unwrap();
    }
    git(&lease.path, &["config", "--unset", "filter.barrier.clean"]);
    let _ = std::fs::remove_file(lease.path.join(".gitattributes"));
    assert!(
        matches!(committed_before_release, Ok(Ok(Ok(())))),
        "unrelated writer must commit while the Git filter is still blocked: {committed_before_release:?}",
    );
    assert!(
        filter_still_running,
        "writer completion must precede the sampling timeout"
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM unrelated_writer_probe")
        .fetch_one(&fx.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1);
}

struct FilterBarrier {
    _directory: tempfile::TempDir,
    entered: PathBuf,
    release: PathBuf,
    worktree: PathBuf,
}

impl FilterBarrier {
    async fn install(worktree: &Path, only_first: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let hook = directory.path().join("clean-filter.sh");
        let entered = directory.path().join("entered");
        let release = directory.path().join("release");
        let once = if only_first {
            format!(
                "if ! mkdir '{}'; then cat; exit; fi\n",
                directory.path().join("once").display()
            )
        } else {
            String::new()
        };
        write_executable(
            &hook,
            &format!(
                "#!/bin/sh\n{once}touch '{}'\nwhile [ ! -f '{}' ]; do sleep 0.02; done\ncat\n",
                entered.display(),
                release.display(),
            ),
        );
        git(
            worktree,
            &["config", "filter.barrier.clean", hook.to_str().unwrap()],
        );
        let tracked = worktree.join("worker.txt");
        let bytes = std::fs::read(&tracked).unwrap();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        std::fs::write(tracked, bytes).unwrap();
        Self {
            _directory: directory,
            entered,
            release,
            worktree: worktree.to_owned(),
        }
    }
    async fn wait_entered(&self) {
        tokio::time::timeout(WAIT, async {
            while !self.entered.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("sampling entered the real Git clean filter");
    }
    fn release(&self) {
        std::fs::write(&self.release, "release").unwrap();
    }
}
impl Drop for FilterBarrier {
    fn drop(&mut self) {
        // Also unblock an invocation if an assertion fails before the normal release.
        let _ = std::fs::write(&self.release, "release");
        git(
            &self.worktree,
            &["config", "--unset", "filter.barrier.clean"],
        );
    }
}

async fn candidate_with_filter(
    fx: &Fx,
    name: &str,
) -> (
    Task,
    calm_server::test_seams::KernelWorkspaceLease,
    CandidateRowView,
) {
    let (_, task, lease, candidate) = settled_gated_task_with(
        fx,
        name,
        gated("true"),
        &[(".gitattributes", "worker.txt filter=barrier\n")],
    )
    .await;
    (task, lease, candidate)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_prepare_discards_sample_for_changed_provenance_and_workspace() {
    for change in ["provenance", "workspace", "gate"] {
        let fx = fixture().await;
        fx.dispatcher.abort_event_listener_for_test();
        let (task, lease, _) = candidate_with_filter(&fx, change).await;
        let barrier = FilterBarrier::install(&lease.path, false).await;
        let drive = spawn_drive(fx.scheduler(), task.clone());
        barrier.wait_entered().await;
        let alternate_cwd = barrier._directory.path().join("lease-alias");
        match change {
            "provenance" => {
                // Unlike immutable candidate rows, the lease tuple permits this CHECK-valid
                // metadata change. The old sample's provenance_holds=true was computed against
                // the original canonical path and must not authorize this different expectation.
                sqlx::query("UPDATE workspace_leases SET canonical_path = ?1 WHERE lease_id = ?2")
                    .bind(barrier._directory.path().to_str().unwrap())
                    .bind(&lease.lease_id)
                    .execute(&fx.pool())
                    .await
                    .unwrap();
            }
            "workspace" => {
                std::os::unix::fs::symlink(&lease.path, &alternate_cwd).unwrap();
                sqlx::query("UPDATE workspace_leases SET path = ?1 WHERE lease_id = ?2")
                    .bind(alternate_cwd.to_str().unwrap())
                    .bind(&lease.lease_id)
                    .execute(&fx.pool())
                    .await
                    .unwrap();
            }
            "gate" => {
                sqlx::query("UPDATE tasks SET gate_json = ?1 WHERE id = ?2")
                    .bind(gated("false")["gate"].to_string())
                    .bind(&task.id)
                    .execute(&fx.pool())
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert_eq!(current(&fx.boot, change).await.gate_attempt, 0);
        assert!(gate_result_events(&fx, &task.id).await.is_empty());
        barrier.release();
        tokio::time::timeout(WAIT, drive)
            .await
            .expect("fresh facts were re-prepared")
            .unwrap();
        let op = gate_op(&fx, &task.id).await.unwrap();
        assert_ne!(op.phase.tag(), PhaseTag::Stuck, "{op:?}");
        if change == "provenance" {
            assert!(
                op.spawn_artifacts.is_none(),
                "prepare refusal must never spawn: {op:?}"
            );
        }
        let output = op.tx_output.unwrap();
        match change {
            "provenance" => {
                assert_eq!(
                    output.data["target"]["canonical_path"],
                    barrier._directory.path().to_str().unwrap()
                );
                assert_eq!(
                    output.data["target"]["refused"], true,
                    "a second provenance observation must refuse BEFORE spawning a gate"
                );
                assert!(
                    !fx.boot
                        .ctx
                        .gate_logs_dir
                        .join(format!("{}-g1.sh", task.id))
                        .exists(),
                    "old provenance evidence must not authorize even a temporary gate process"
                );
            }
            "workspace" => assert_eq!(output.data["cwd"], alternate_cwd.to_str().unwrap()),
            "gate" => assert_eq!(output.data["gate"]["steps"][0]["cmd"], "false"),
            _ => unreachable!(),
        }
        assert_eq!(current(&fx.boot, change).await.gate_attempt, 1);
        let events = gate_result_events(&fx, &task.id).await;
        assert_eq!(events.len(), 1);
        if change == "provenance" {
            assert!(!events[0].passed);
            assert!(
                matches!(events[0].candidate().3,
                VerifyTargetEvidence::Refused { reasons, .. } if reasons.contains(&MismatchReason::Provenance)),
                "fresh prepare must refuse; a later finalize mismatch cannot substitute: {:?}",
                events[0]
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_prepare_discards_obsolete_sampling_error_before_repreparing() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let (task, lease, _) = candidate_with_filter(&fx, "stale-timeout").await;
    // Only the first sample blocks. It times out after the facts change; its replacement can run.
    let barrier = FilterBarrier::install(&lease.path, true).await;
    let drive = spawn_drive(fx.scheduler(), task.clone());
    barrier.wait_entered().await;
    sqlx::query("UPDATE workspace_leases SET canonical_path = ?1 WHERE lease_id = ?2")
        .bind(barrier._directory.path().to_str().unwrap())
        .bind(&lease.lease_id)
        .execute(&fx.pool())
        .await
        .unwrap();
    tokio::time::timeout(WAIT, drive)
        .await
        .expect("obsolete timeout was discarded and preparation retried")
        .unwrap();
    let op = gate_op(&fx, &task.id).await.unwrap();
    assert_eq!(
        op.phase.tag(),
        PhaseTag::Succeeded,
        "refusal is committed through NoOp, not Stuck: {op:?}"
    );
    assert!(
        op.spawn_artifacts.is_none(),
        "fresh preparation refusal must spawn nothing: {op:?}"
    );
    assert_eq!(op.tx_output.unwrap().data["target"]["refused"], true);
    assert!(
        !fx.boot
            .ctx
            .gate_logs_dir
            .join(format!("{}-g1.sh", task.id))
            .exists()
    );
    let events = gate_result_events(&fx, &task.id).await;
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].status_detail.as_deref(),
        Some("gate-target-mismatch")
    );
    assert!(matches!(events[0].candidate().3,
        VerifyTargetEvidence::Refused { reasons, .. } if reasons.contains(&MismatchReason::Provenance)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_prepare_cannot_commit_under_replaced_claim_and_wait_recovers_after_expiry() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let (task, lease, _) = candidate_with_filter(&fx, "claim-replaced").await;
    let barrier = FilterBarrier::install(&lease.path, false).await;
    let drive = spawn_drive(fx.scheduler(), task.clone());
    barrier.wait_entered().await;
    let op = gate_op(&fx, &task.id).await.unwrap();
    sqlx::query("UPDATE operations SET lease_owner = 'replacement-claim', lease_until_ms = ?1 WHERE id = ?2")
        .bind(now_ms() + 60_000).bind(&op.id).execute(&fx.pool()).await.unwrap();
    barrier.release();
    // The runtime owns this same mutex during drive(), so acquiring it waits until the old
    // sampling/commit attempt has returned. The replacement claim prevents the wait loop reclaim.
    let guard = tokio::time::timeout(WAIT, fx.runtime.lock_for_track_delete_for_test())
        .await
        .unwrap();
    let after = gate_op(&fx, &task.id).await.unwrap();
    assert_eq!(after.phase.tag(), PhaseTag::Pending);
    assert_eq!(after.lease_owner.as_deref(), Some("replacement-claim"));
    assert!(after.tx_output.is_none());
    assert_eq!(current(&fx.boot, "claim-replaced").await.gate_attempt, 0);
    assert!(gate_result_events(&fx, &task.id).await.is_empty());
    // No sixty-second sleep: the same production wait() continues driving and reclaims the
    // expired Pending operation. It must finish rather than turn a lost claim into Stuck.
    sqlx::query("UPDATE operations SET lease_until_ms = ?1 WHERE id = ?2")
        .bind(now_ms() - 1)
        .bind(&op.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    drop(guard);
    tokio::time::timeout(WAIT, drive)
        .await
        .expect("scheduler wait reclaims the expired Pending operation")
        .unwrap();
    assert_eq!(
        gate_op(&fx, &task.id).await.unwrap().phase.tag(),
        PhaseTag::Succeeded
    );
    assert_eq!(current(&fx.boot, "claim-replaced").await.gate_attempt, 1);
}
