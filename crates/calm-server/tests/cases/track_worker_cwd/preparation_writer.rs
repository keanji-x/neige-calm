//! Worker admission must not hold SQLite's writer while a real Git clean filter waits.
use super::*;
use crate::git_delivery::write_executable;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_admission_allows_unrelated_writer_before_clean_filter_release() {
    assert_writer_before_filter_release("codex", false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_worker_admission_releases_writer_during_clean_check() {
    assert_writer_before_filter_release("claude", false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ready_managed_worker_admission_releases_writer_during_clean_check() {
    for kind in ["codex", "claude"] {
        assert_writer_before_filter_release(kind, true).await;
    }
}

async fn assert_writer_before_filter_release(kind: &'static str, managed: bool) {
    use calm_server::db::sqlite::begin_immediate_tx;

    let world = Arc::new(if managed {
        managed_world().await
    } else {
        world().await
    });
    let fx = &world.fx;
    std::fs::write(fx.worktree.join("writer-probe.txt"), "tracked content\n").unwrap();
    std::fs::write(
        fx.worktree.join(".gitattributes"),
        "writer-probe.txt filter=workerbarrier\n",
    )
    .unwrap();
    git(&fx.worktree, &["add", "-A"]);
    git(
        &fx.worktree,
        &["commit", "-q", "-m", "prepare worker filter fixture"],
    );
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
        &fx.worktree,
        &[
            "config",
            "filter.workerbarrier.clean",
            hook.to_str().unwrap(),
        ],
    );
    let tracked = fx.worktree.join("writer-probe.txt");
    let bytes = std::fs::read(&tracked).unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    std::fs::write(tracked, bytes).unwrap();
    sqlx::query("CREATE TABLE worker_writer_probe (value INTEGER NOT NULL)")
        .execute(&fx.pool())
        .await
        .unwrap();

    let declaring = world.clone();
    let admission = tokio::spawn(async move {
        declare_task(&declaring.fx, "writer-barrier", json!({"kind":kind})).await;
        wait_running(&declaring.fx, "writer-barrier").await
    });
    tokio::time::timeout(WAIT, async {
        while !entered.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("real worker admission reached the clean filter");
    let pool = fx.pool();
    let mut writer = tokio::spawn(async move {
        let mut tx = begin_immediate_tx(&pool).await?;
        sqlx::query("INSERT INTO worker_writer_probe(value) VALUES (1)")
            .execute(&mut **tx)
            .await?;
        tx.commit().await?;
        Ok::<_, calm_server::error::CalmError>(())
    });
    // This bound is far shorter than the production clean-check budget. Timeout cancellation
    // of the Git command must not masquerade as releasing a writer before the filter release.
    let committed_before_release = tokio::time::timeout(Duration::from_secs(1), &mut writer).await;
    assert!(!release.exists());
    // Release and drain before asserting so the expected red run leaves no blocked subprocess.
    std::fs::write(&release, "release").unwrap();
    let started = tokio::time::timeout(WAIT, admission)
        .await
        .expect("worker admission completed after filter release")
        .unwrap();
    if committed_before_release.is_err() {
        tokio::time::timeout(Duration::from_secs(10), writer)
            .await
            .expect("writer finished after filter release")
            .unwrap()
            .unwrap();
    }
    git(
        &fx.worktree,
        &["config", "--unset", "filter.workerbarrier.clean"],
    );
    assert_eq!(started.cwd, fx.worktree);
    assert!(
        matches!(committed_before_release, Ok(Ok(Ok(())))),
        "worker admission held the DB writer while Git was blocked: {committed_before_release:?}"
    );
    let writes: i64 = sqlx::query_scalar("SELECT count(*) FROM worker_writer_probe")
        .fetch_one(&fx.pool())
        .await
        .unwrap();
    assert_eq!(writes, 1);
}

struct CleanBarrier {
    _dir: tempfile::TempDir,
    entered: PathBuf,
    release: PathBuf,
    worktree: PathBuf,
}
impl CleanBarrier {
    async fn install(fx: &Fx) -> Self {
        std::fs::write(fx.worktree.join("race-probe.txt"), "tracked\n").unwrap();
        std::fs::write(
            fx.worktree.join(".gitattributes"),
            "race-probe.txt filter=workerrace\n",
        )
        .unwrap();
        git(&fx.worktree, &["add", "-A"]);
        git(
            &fx.worktree,
            &["commit", "-q", "-m", "worker admission race fixture"],
        );
        let dir = tempfile::tempdir().unwrap();
        let hook = dir.path().join("clean-filter.sh");
        let entered = dir.path().join("entered");
        let release = dir.path().join("release");
        write_executable(
            &hook,
            &format!(
                "#!/bin/sh\ntouch '{}'\nwhile [ ! -f '{}' ]; do sleep 0.02; done\ncat\n",
                entered.display(),
                release.display(),
            ),
        );
        git(
            &fx.worktree,
            &["config", "filter.workerrace.clean", hook.to_str().unwrap()],
        );
        let probe = fx.worktree.join("race-probe.txt");
        let bytes = std::fs::read(&probe).unwrap();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        std::fs::write(probe, bytes).unwrap();
        Self {
            _dir: dir,
            entered,
            release,
            worktree: fx.worktree.clone(),
        }
    }
    async fn wait(&self) {
        tokio::time::timeout(WAIT, async {
            while !self.entered.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("worker entered clean-filter barrier");
    }
    fn release(&self) {
        std::fs::write(&self.release, "release").unwrap();
    }
}
impl Drop for CleanBarrier {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.release, "release");
        git(
            &self.worktree,
            &["config", "--unset", "filter.workerrace.clean"],
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn managed_missing_exclude_is_repaired_before_clean_admission() {
    for kind in ["codex", "claude"] {
        let w = managed_world().await;
        let fx = &w.fx;
        let reported = PathBuf::from(git(
            &fx.worktree,
            &["rev-parse", "--git-path", "info/exclude"],
        ));
        let exclude = if reported.is_absolute() {
            reported
        } else {
            fx.worktree.join(reported)
        };
        let original = std::fs::read_to_string(&exclude).unwrap();
        let without: Vec<_> = original
            .lines()
            .filter(|line| line.trim() != ".neige/")
            .collect();
        std::fs::write(&exclude, without.join("\n")).unwrap();
        std::fs::create_dir_all(fx.worktree.join(".neige")).unwrap();
        std::fs::write(fx.worktree.join(".neige/generated.txt"), "owned metadata\n").unwrap();
        declare_task(fx, "metadata-repair", json!({"kind":kind})).await;
        let started = wait_running(fx, "metadata-repair").await;
        assert_eq!(started.cwd, fx.worktree);
        assert!(
            std::fs::read_to_string(&exclude)
                .unwrap()
                .lines()
                .any(|line| line.trim() == ".neige/")
        );
        assert_eq!(git(&fx.worktree, &["status", "--porcelain"]), "");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_discards_obsolete_declared_head_error_and_reprepares() {
    for kind in ["codex", "claude"] {
        let w = Arc::new(world().await);
        let fx = &w.fx;
        let barrier = CleanBarrier::install(fx).await;
        let actual = git(&fx.worktree, &["rev-parse", "HEAD"]);
        let obsolete = git(&fx.worktree, &["rev-parse", "HEAD^"]);
        let declaring = w.clone();
        let admission = tokio::spawn(async move {
            declare_task(
                &declaring.fx,
                "head-race",
                json!({"kind":kind,"access":"read_only","head":obsolete}),
            )
            .await;
            wait_running(&declaring.fx, "head-race").await
        });
        barrier.wait().await;
        let task = current(&fx.boot, "head-race").await;
        let before = (
            fx.table_count("cards").await,
            fx.table_count("worker_sessions").await,
        );
        sqlx::query("UPDATE tasks SET head=?1 WHERE id=?2")
            .bind(&actual)
            .bind(&task.id)
            .execute(&fx.pool())
            .await
            .unwrap();
        assert_eq!(fx.table_count("workspace_leases").await, 0);
        assert!(task.worker_card_id.is_none());
        barrier.release();
        let started = tokio::time::timeout(WAIT, admission)
            .await
            .expect("fresh declared head permits admission")
            .unwrap();
        assert_eq!(started.cwd, fx.worktree);
        assert_eq!(fx.table_count("cards").await, before.0 + 1);
        assert_eq!(fx.table_count("worker_sessions").await, before.1 + 1);
        assert_eq!(fx.table_count("workspace_leases").await, 1);
        let op = fx
            .runtime
            .find_by_kind_and_idempotency(&format!("{kind}-worker"), &task.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(op.tx_output.unwrap().data["base_sha"], actual);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_claim_replacement_prevents_publication_and_wait_reclaims_after_expiry() {
    use calm_server::operation::PhaseTag;
    for kind in ["codex", "claude"] {
        let w = Arc::new(world().await);
        let fx = &w.fx;
        let barrier = CleanBarrier::install(fx).await;
        let before = (
            fx.table_count("cards").await,
            fx.table_count("worker_sessions").await,
        );
        let declaring = w.clone();
        let admission = tokio::spawn(async move {
            declare_task(&declaring.fx, "claim-race", json!({"kind":kind})).await;
            wait_running(&declaring.fx, "claim-race").await
        });
        barrier.wait().await;
        let task = current(&fx.boot, "claim-race").await;
        let operation_kind = format!("{kind}-worker");
        let op = fx
            .runtime
            .find_by_kind_and_idempotency(&operation_kind, &task.id)
            .await
            .unwrap()
            .unwrap();
        sqlx::query("UPDATE operations SET lease_owner='replacement-worker-claim', lease_until_ms=?1 WHERE id=?2")
            .bind(calm_server::model::now_ms() + 60_000).bind(&op.id).execute(&fx.pool()).await.unwrap();
        barrier.release();
        let guard = tokio::time::timeout(WAIT, fx.runtime.lock_for_track_delete_for_test())
            .await
            .unwrap();
        let pending = fx
            .runtime
            .find_by_kind_and_idempotency(&operation_kind, &task.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pending.phase.tag(), PhaseTag::Pending);
        assert_eq!(
            pending.lease_owner.as_deref(),
            Some("replacement-worker-claim")
        );
        assert!(pending.tx_output.is_none());
        assert_eq!(fx.table_count("cards").await, before.0);
        assert_eq!(fx.table_count("worker_sessions").await, before.1);
        assert_eq!(fx.table_count("workspace_leases").await, 0);
        sqlx::query("UPDATE operations SET lease_until_ms=?1 WHERE id=?2")
            .bind(calm_server::model::now_ms() - 1)
            .bind(&op.id)
            .execute(&fx.pool())
            .await
            .unwrap();
        drop(guard);
        let started = tokio::time::timeout(WAIT, admission)
            .await
            .expect("real wait reclaims expired worker preparation")
            .unwrap();
        assert_eq!(started.cwd, fx.worktree);
        assert_eq!(fx.table_count("workspace_leases").await, 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn obsolete_worker_task_or_allocation_publishes_no_prepared_rows() {
    use calm_types::task_recovery::{
        TASK_IN_TRACK_ROUTE, TaskAttemptOrigin, TaskRecoveryConstraint,
    };
    for kind in ["codex", "claude"] {
        for change in ["context", "allocation"] {
            let w = Arc::new(world().await);
            let fx = &w.fx;
            let barrier = CleanBarrier::install(fx).await;
            let before = (
                fx.table_count("cards").await,
                fx.table_count("worker_sessions").await,
            );
            let declaring = w.clone();
            let declaration = tokio::spawn(async move {
                declare_task(&declaring.fx, "obsolete-race", json!({"kind":kind})).await;
            });
            barrier.wait().await;
            let task = current(&fx.boot, "obsolete-race").await;
            let pool = fx.pool();
            let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
                .await
                .unwrap();
            if change == "context" {
                sqlx::query("UPDATE tasks SET context_stale_at_ms=?1 WHERE id=?2")
                    .bind(calm_server::model::now_ms())
                    .bind(&task.id)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
            } else {
                // A legitimate generation-2 allocation requires a failed predecessor. Preserve
                // all immutable allocation triggers; the new allocation need not have a task row.
                sqlx::query("UPDATE tasks SET status='failed', finished_at_ms=?1 WHERE id=?2")
                    .bind(calm_server::model::now_ms())
                    .bind(&task.id)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                let origin = TaskAttemptOrigin::Recovery {
                    previous_attempt_id: task.id.clone(),
                    idempotency_key: "new-generation".into(),
                    request_fingerprint: "worker-race-recovery".into(),
                    reason: "explicit recovery fixture".into(),
                    actor: calm_server::ids::ActorId::User,
                    constraint: TaskRecoveryConstraint::V1 {
                        refs: vec![calm_types::event::TaskContextRef {
                            track_id: fx.track().to_string().into(),
                            block_id: "worker-race-root".into(),
                            rev: 1,
                            hash: "0".repeat(64),
                            is_root: true,
                        }],
                        spawn: TASK_IN_TRACK_ROUTE.into(),
                        declared_by: "user".into(),
                    },
                };
                sqlx::query(include_str!(
                    "../../fixtures/worker_recovery_allocation.sql"
                ))
                .bind(format!("{}:generation-2", task.id))
                .bind(&task.track_id)
                .bind(&task.key)
                .bind(serde_json::to_string(&origin).unwrap())
                .bind(calm_server::model::now_ms())
                .execute(&mut **tx)
                .await
                .unwrap();
            }
            tx.commit().await.unwrap();
            assert_eq!(fx.table_count("workspace_leases").await, 0);
            barrier.release();
            declaration.await.unwrap();
            let guard = tokio::time::timeout(WAIT, fx.runtime.lock_for_track_delete_for_test())
                .await
                .unwrap();
            let op = fx
                .runtime
                .find_by_kind_and_idempotency(&format!("{kind}-worker"), &task.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                op.phase.tag(),
                calm_server::operation::PhaseTag::Failed,
                "obsolete worker must refuse: {op:?}"
            );
            assert!(
                op.tx_output.is_none(),
                "obsolete preparation cannot publish its checkpoint"
            );
            assert_eq!(fx.table_count("cards").await, before.0);
            assert_eq!(fx.table_count("worker_sessions").await, before.1);
            assert_eq!(fx.table_count("workspace_leases").await, 0);
            drop(guard);
        }
    }
}
