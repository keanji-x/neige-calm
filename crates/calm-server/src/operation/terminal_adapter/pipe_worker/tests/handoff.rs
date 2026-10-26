//! A terminal task whose preparation is deferred reaches Pipe Parked only after the scheduler
//! entered its wait; the scheduler must still hand it off as Running.
use super::*;
use crate::scheduler::{PostClaimDriveTestHook, Scheduler};

/// Waits, bounded, for a `go` file in its checkout so the test controls when it exits.
const WAIT_FOR_GO: &str =
    "i=0; until [ -e go ] || [ \"$i\" -ge 600 ]; do sleep 0.05; i=$((i+1)); done; test -e go";

#[tokio::test]
async fn w_terminal_worker_deferred_prepare_hands_off_running() {
    let first_root = tempfile::tempdir().unwrap();
    let base = super::super::super::tests::terminal_worker_harness_with_workspace(
        first_root.path().to_str().unwrap(),
    )
    .await;
    let db = base.repo;
    let second_root = tempfile::tempdir().unwrap();
    let second = super::super::super::tests::terminal_worker_harness_with_plan(
        db.clone(),
        second_root.path().to_str().unwrap(),
        &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
    )
    .await;
    let events = EventBus::new();
    let write = WriteContext::new(CardRoleCache::new(), TrackAreaCache::new());
    let mut tasks = Vec::new();
    for (track, key) in [(&base.track_id, "first"), (&second.track_id, "second")] {
        tasks.push(
            crate::operation::launch_cleanup_test_support::declared_task(
                db.clone(),
                events.clone(),
                write.clone(),
                track,
                json!({"key":key,"kind":"terminal","command":WAIT_FOR_GO,"ready":true,"declared_by":"user"}),
            )
            .await,
        );
    }
    let ops = Arc::new(SqlxOperationRepo::new(db.db().clone()));
    let executor = Executor::new(db.db().clone()).await;
    let ctx = SpawnCtx::new(
        db.clone(),
        ops.clone(),
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new_with_repo(db.clone()),
        events.clone(),
        OperationCompletionBus::new(),
    )
    .with_execution(executor.lifecycle.clone());
    let adapter =
        TerminalWorkerAdapter::new(db.clone(), CardRoleCache::new(), TrackAreaCache::new())
            .with_log_directory(first_root.path().join("logs"));
    let runtime = Arc::new(
        OperationRuntime::new(
            ops.clone(),
            vec![Arc::new(adapter)],
            events.clone(),
            ctx.completion.clone(),
            ctx.clone(),
        )
        .await
        .unwrap(),
    );
    // One permit: the second Track's task can start only after the first pass released it.
    let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
    let scheduler = Scheduler::new(
        db.clone(),
        events,
        write,
        Arc::downgrade(&runtime),
        crate::per_card_lock::new_per_card_locks(),
        semaphore.clone(),
        first_root.path().join("gate-logs"),
        crate::scheduler::WorkerLiveness::DEFAULT,
        crate::scheduler::WorkerIdleWake::new(
            crate::shared_codex_appserver::SharedCodexAppServer::new_stub(db.clone()),
            crate::scheduler::WORKER_IDLE_TURN_GRACE,
            crate::scheduler::WORKER_IDLE_PROBE_TIMEOUT,
        ),
    );
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();

    let (claimed, resume, wait_entered) = (
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(tokio::sync::Notify::new()),
    );
    scheduler.set_post_claim_drive_test_hook(PostClaimDriveTestHook {
        claimed: claimed.clone(),
        resume: resume.clone(),
    });
    runtime.install_wait_entered_hook_for_test(wait_entered.clone());
    let first_track = crate::ids::TrackId::from(base.track_id.clone());
    let mut scheduled = tokio::spawn({
        let (scheduler, track) = (scheduler.clone(), first_track.clone());
        async move { scheduler.schedule_track(track).await }
    });
    tokio::time::timeout(Duration::from_secs(5), claimed.notified())
        .await
        .expect("the claim reaches the post-claim hook");
    // Hold the Track fence so the submitted preparation is deferred past submit.
    let fence = crate::per_card_lock::lock_key(runtime.track_delete_locks(), &base.track_id).await;
    resume.notify_one();
    tokio::time::timeout(Duration::from_secs(5), wait_entered.notified())
        .await
        .expect("the scheduler waits on the deferred preparation");
    let op = |task: &crate::model::Task| {
        let (runtime, id) = (runtime.clone(), task.id.clone());
        async move {
            runtime
                .find_by_kind_and_idempotency("terminal-worker", &id)
                .await
                .unwrap()
        }
    };
    assert_eq!(op(&tasks[0]).await.unwrap().phase, Phase::Pending);
    drop(fence);

    let handed_off = tokio::time::timeout(Duration::from_secs(10), &mut scheduled)
        .await
        .is_ok();
    let first_phase = op(&tasks[0]).await.unwrap().phase.tag();
    let first_status = db.task_get(&tasks[0].id).await.unwrap().unwrap().status;
    let permits = semaphore.available_permits();
    let second_started = handed_off
        && tokio::time::timeout(
            Duration::from_secs(10),
            scheduler.schedule_track(crate::ids::TrackId::from(second.track_id.clone())),
        )
        .await
        .is_ok();
    let second_status = db.task_get(&tasks[1].id).await.unwrap().unwrap().status;
    let track_free = handed_off
        && tokio::time::timeout(
            Duration::from_secs(5),
            scheduler.schedule_track(first_track),
        )
        .await
        .is_ok();

    // Release both commands before asserting, so a red run leaves no waiting child.
    for root in [&first_root, &second_root] {
        std::fs::write(root.path().join("go"), b"").unwrap();
    }
    for task in &tasks {
        if let Some(op) = op(task).await {
            tokio::time::timeout(Duration::from_secs(15), runtime.wait(&op.id))
                .await
                .expect("the released command settles")
                .unwrap();
        }
    }
    assert!(
        handed_off,
        "the scheduling pass returns while the command runs (operation {first_phase:?})"
    );
    assert_eq!(first_status, crate::model::TaskStatus::Running);
    assert_eq!(permits, 1, "the dispatch permit is released");
    assert!(second_started, "another Track's ready task dispatches");
    assert_eq!(second_status, crate::model::TaskStatus::Running);
    assert!(track_free, "the Track scheduling lock is released");
    for task in &tasks {
        assert_eq!(
            db.task_get(&task.id).await.unwrap().unwrap().status,
            crate::model::TaskStatus::Done
        );
    }
}
