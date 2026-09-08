//! Lease liveness through real observation and recovery, with no timing race.
use super::*;
use std::sync::Arc;
use tokio::sync::{Notify, oneshot};

// Failed assertions also release both the observer and the read-hint barrier.
struct Resume(Option<oneshot::Sender<()>>);
impl Resume {
    fn release(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}
impl Drop for Resume {
    fn drop(&mut self) {
        self.release();
    }
}
struct HeldCandidate {
    fx: Fixture,
    publication: String,
    id: String,
    group: CandidateTestGroup,
    resume: Resume,
    entered: Arc<Notify>,
    _hook: calm_server::file_delivery::CandidateCompletionHook,
}
impl HeldCandidate {
    async fn new() -> Self {
        let (fx, _, _, publication) =
            source("while [ ! -e allow-finish ]; do sleep 0.02; done").await;
        let entered = Arc::new(Notify::new());
        let (tx, rx) = oneshot::channel();
        let rx = Arc::new(std::sync::Mutex::new(Some(rx)));
        let ready = entered.clone();
        let hook = calm_server::file_delivery::install_candidate_completion_hook(
            &publication,
            Arc::new(move |_| {
                let rx = rx.lock().unwrap().take().unwrap();
                let ready = ready.clone();
                Box::pin(async move {
                    ready.notify_one();
                    let _ = rx.await;
                })
            }),
        );
        let resume = Resume(Some(tx));
        schedule(&fx).await;
        let op = verification(&fx, &publication).await;
        let group = parked_test_group(&fx, &op.id).await;
        Self {
            fx,
            publication,
            id: op.id,
            group,
            resume,
            entered,
            _hook: hook,
        }
    }
    async fn finish(&self) {
        std::fs::write(
            self.group.workspace.join("input/source/allow-finish"),
            b"go",
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), self.entered.notified())
            .await
            .unwrap();
        // Barrier follows actual WNOWAIT observation and group cleanup.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.group.artifacts.pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            },
            0
        );
        assert_eq!(unsafe { info.si_pid() }, self.group.artifacts.pid);
        assert_eq!(info.si_code, libc::CLD_EXITED);
        assert_eq!(unsafe { info.si_status() }, 0);
        assert!(
            calm_server::proc_identity::scan_process_group_members(self.group.artifacts.pgid)
                .iter()
                .all(|m| m.is_zombie)
        );
        std::fs::write(self.group.workspace.join("gate.exit"), b"7\n").unwrap();
    }
    async fn settle(&mut self) {
        self.resume.release();
        let evidence = verified(&self.fx, &self.publication).await;
        assert_eq!(evidence["verdict"]["exit_code"], 0);
        assert_eq!(evidence["verdict"]["passed"], true);
        self.cleaned().await;
    }
    async fn cleaned(&self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while calm_server::proc_identity::verify_owned_pid(
                self.group.artifacts.pid,
                self.group.artifacts.start_time,
                &self.group.artifacts.boot_id,
            ) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            calm_server::proc_identity::scan_process_group_members(self.group.artifacts.pgid)
                .is_empty()
        );
        let active: i64 = sqlx::query_scalar("SELECT count(*) FROM task_candidate_verification_allocations a JOIN operations o ON o.operation_key=a.operation_key WHERE o.id=?1 AND o.phase NOT IN ('succeeded','failed')")
            .bind(&self.id).fetch_one(&self.fx.boot.repo.sqlite_pool().unwrap()).await.unwrap();
        assert_eq!(active, 0);
    }
}
async fn pause_hint(
    held: &HeldCandidate,
) -> (
    bool,
    Resume,
    calm_server::file_delivery::CandidateRecoveryHintHook,
) {
    let (observed_tx, observed_rx) = oneshot::channel();
    let (resume_tx, resume_rx) = oneshot::channel();
    let channels = std::sync::Mutex::new(Some((observed_tx, resume_rx)));
    let hook = calm_server::file_delivery::install_candidate_recovery_hint_hook(
        &held.id,
        Arc::new(move |eligible| {
            let (observed_tx, resume_rx) = channels.lock().unwrap().take().unwrap();
            Box::pin(async move {
                let _ = observed_tx.send(eligible);
                let _ = resume_rx.await;
            })
        }),
    );
    let resume = Resume(Some(resume_tx));
    // Production scheduler's waiter enters the hint. Its prior sweep has ended.
    let eligible = tokio::time::timeout(Duration::from_secs(10), observed_rx)
        .await
        .unwrap()
        .unwrap();
    (eligible, resume, hook)
}
async fn audit_claims(pool: &sqlx::SqlitePool, id: &str, deadline: i64) {
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(pool)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE lease_claim_audit (op TEXT, owner TEXT)")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER audit_parked_claim AFTER UPDATE OF lease_owner ON operations WHEN NEW.phase='parked' AND NEW.lease_owner IS NOT NULL AND NEW.lease_owner IS NOT OLD.lease_owner BEGIN INSERT INTO lease_claim_audit VALUES(NEW.id,NEW.lease_owner); END").execute(&mut *tx).await.unwrap();
    sqlx::query("UPDATE operations SET lease_owner=NULL,lease_until_ms=NULL,parked_deadline_ms=?2 WHERE id=?1").bind(id).bind(deadline).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}
async fn claims(pool: &sqlx::SqlitePool, id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM lease_claim_audit WHERE op=?1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn defers_quiescent(deadline: i64) {
    let mut held = HeldCandidate::new().await;
    held.finish().await;
    let (eligible, hint_resume, _hook) = pause_hint(&held).await;
    assert!(!eligible);
    let pool = held.fx.boot.repo.sqlite_pool().unwrap();
    audit_claims(&pool, &held.id, deadline).await;
    held.fx
        .state
        .operation_runtime
        .sweep_parked()
        .await
        .unwrap();
    let count = claims(&pool, &held.id).await;
    drop(hint_resume);
    // Verify and reap even on RED before reporting the audited claim count.
    held.settle().await;
    assert_eq!(
        count, 0,
        "quiescent retained wait status must not compete with recovery claims"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_lease_defers_pre_deadline() {
    defers_quiescent(i64::MAX).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_lease_defers_past_deadline() {
    defers_quiescent(0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_lease_read_hint_races_remain_advisory() {
    for initially_eligible in [true, false] {
        let mut held = HeldCandidate::new().await;
        if !initially_eligible {
            held.finish().await;
        }
        let (eligible, hint_resume, _hook) = pause_hint(&held).await;
        assert_eq!(eligible, initially_eligible);
        let pool = held.fx.boot.repo.sqlite_pool().unwrap();
        audit_claims(&pool, &held.id, 0).await;
        if initially_eligible {
            // Alive -> retained exit after allow. One stale claim is harmless;
            // post-claim recovery must LeaveParked, preserving the actual wait.
            held.finish().await;
            drop(hint_resume);
            tokio::time::timeout(Duration::from_secs(10), async {
                while claims(&pool, &held.id).await == 0 {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            held.settle().await;
        } else {
            // Retained exit -> committed/reaped while defer is paused. Resuming
            // this stale hint must neither reclaim nor rewrite the terminal row.
            held.settle().await;
            let before = claims(&pool, &held.id).await;
            drop(hint_resume);
            held.fx
                .state
                .operation_runtime
                .sweep_parked()
                .await
                .unwrap();
            assert_eq!(claims(&pool, &held.id).await, before);
            assert_eq!(before, 1, "only live completion acquired a lease");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_lease_boot_and_cancel_bypass_deferral() {
    for boot in [true, false] {
        let mut held = HeldCandidate::new().await;
        held.finish().await;
        let (eligible, hint_resume, _hook) = pause_hint(&held).await;
        assert!(!eligible);
        let pool = held.fx.boot.repo.sqlite_pool().unwrap();
        audit_claims(&pool, &held.id, 0).await;
        if boot {
            sqlx::query(
                "UPDATE operations SET lease_owner='stale-boot',lease_until_ms=?2 WHERE id=?1",
            )
            .bind(&held.id)
            .bind(calm_server::model::now_ms() + 60_000)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("DELETE FROM lease_claim_audit")
                .execute(&pool)
                .await
                .unwrap();
            let runtime = &held.fx.state.operation_runtime;
            runtime
                .apply_recovery(runtime.recover_on_boot().await.unwrap())
                .await
                .unwrap();
            let owner: Option<String> =
                sqlx::query_scalar("SELECT lease_owner FROM operations WHERE id=?1")
                    .bind(&held.id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert!(owner.is_none());
            assert!(claims(&pool, &held.id).await > 0, "boot still force-claims");
            drop(hint_resume);
            held.settle().await;
        } else {
            assert!(
                held.fx
                    .state
                    .operation_runtime
                    .cancel_parked(&held.id, "fixture cancellation")
                    .await
                    .unwrap()
            );
            assert!(claims(&pool, &held.id).await > 0, "cancel still claims");
            drop(hint_resume);
            held.resume.release();
            let result = held
                .fx
                .state
                .operation_runtime
                .wait(&held.id)
                .await
                .unwrap();
            assert!(matches!(result.outcome, OperationOutcome::Failed { .. }));
            held.cleaned().await;
        }
    }
}
