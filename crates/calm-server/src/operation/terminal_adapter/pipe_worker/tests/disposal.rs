use super::*;
mod batch_recovery;
use crate::db::RepoSyncDomainRaw;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

impl Harness {
    async fn state(&self) -> crate::state::AppState {
        use crate::{
            plugin_host::{PluginHost, PluginRegistry},
            state::{AppState, CodexClient},
        };
        let roles = CardRoleCache::new();
        let areas = TrackAreaCache::new();
        self.db.seed_card_role_cache(&roles).await.unwrap();
        self.db.seed_track_area_cache(&areas).await.unwrap();
        let events = EventBus::new();
        let plugin = Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            self.db.clone(),
            PathBuf::new(),
            self.root.path().join("plugins"),
            vec![],
            events.clone(),
            WriteContext::new(roles.clone(), areas.clone()),
        ));
        let runtime = Arc::new(
            OperationRuntime::new(
                self.ops.clone(),
                vec![Arc::new(self.adapter.clone())],
                events.clone(),
                self.ctx.completion.clone(),
                self.ctx.clone(),
            )
            .await
            .unwrap(),
        );
        AppState::from_parts(
            self.db.clone(),
            events,
            Arc::new(DaemonClient::new_stub()),
            plugin,
            Arc::new(CodexClient::new_stub()),
            Some(roles),
            Some(areas),
        )
        .with_operation_runtime(runtime)
        .with_workspace_root(self.root.path().join("workspaces"))
    }
    async fn delete(&self, scope: &str) -> StatusCode {
        let state = self.state().await;
        let track = self
            .db
            .track_get(&self.task.track_id)
            .await
            .unwrap()
            .unwrap();
        let id = match scope {
            "cards" => output_card_id(&self.op().await.tx_output.unwrap()).unwrap(),
            "tracks" => track.id.to_string(),
            "areas" => track.area_id.to_string(),
            _ => unreachable!(),
        };
        let app = crate::routes::router()
            .layer(axum::middleware::from_fn_with_state(
                crate::auth::AuthState::new(crate::auth::AuthConfig {
                    username: None,
                    password: None,
                    dev_autologin: true,
                    display_name: "Owner".into(),
                }),
                crate::auth::require_session,
            ))
            .layer(axum::middleware::from_fn(crate::actor::actor_middleware))
            .with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/{scope}/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(
            status == StatusCode::NO_CONTENT || status == StatusCode::CONFLICT,
            "{scope}: {status}: {}",
            String::from_utf8_lossy(&body)
        );
        status
    }
    async fn execution_row(&self) -> (String, Option<String>, Option<String>) {
        let record = pipe::record(CONFIG, &self.op().await).unwrap();
        sqlx::query_as(
            "SELECT desired,settlement_json,secrets_json FROM workspace_executions WHERE execution_id=?1",
        )
        .bind(record.execution_id.as_str())
        .fetch_one(self.db.pool())
        .await
        .unwrap()
    }
}

/// Completed and its retirement finished: DELETE finds no owned execution to dispose.
async fn completed_delete(scope: &str) {
    let h = Harness::new("exit 0").await;
    h.park().await;
    h.release().await;
    h.settled().await;
    h.complete().await;
    assert_eq!(h.status().await, crate::model::TaskStatus::Done);
    // Completion drives retirement asynchronously; delete only after it finished.
    tokio::time::timeout(Duration::from_secs(8), async {
        while h.execution_row().await.2.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let card = output_card_id(&h.op().await.tx_output.unwrap()).unwrap();
    assert_eq!(h.delete(scope).await, StatusCode::NO_CONTENT);
    assert!(h.db.card_get(&card).await.unwrap().is_none());
}

#[tokio::test]
async fn w_terminal_worker_completed_delete_card() {
    completed_delete("cards").await;
}
#[tokio::test]
async fn w_terminal_worker_completed_delete_track() {
    completed_delete("tracks").await;
}
#[tokio::test]
async fn w_terminal_worker_completed_delete_area() {
    completed_delete("areas").await;
}

/// Completed with its retirement not yet finished: DELETE disposes the owned execution.
async fn retiring_delete(scope: &str) {
    let h = Harness::new("exit 0").await;
    h.park().await;
    h.release().await;
    h.settled().await;
    // Fault fixture: refuse the Engine's finish so the retirement window stays open.
    (h.db.db())
        .writer()
        .execute(sqlx::query(
            "CREATE TRIGGER hold_retirement BEFORE UPDATE OF secrets_json ON workspace_executions \
         WHEN NEW.secrets_json IS NULL BEGIN SELECT RAISE(ABORT,'retirement held'); END",
        ))
        .await
        .unwrap();
    h.complete().await;
    assert_eq!(h.status().await, crate::model::TaskStatus::Done);
    let record = pipe::record(CONFIG, &h.op().await).unwrap();
    let engine = h.ctx.execution.engine().unwrap();
    // The completion's retirement drive has run and stopped at the refused finish.
    tokio::time::timeout(Duration::from_secs(8), async {
        while !engine
            .error(&record.execution_id)
            .is_some_and(|error| format!("{error:?}").contains("retirement held"))
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    (h.db.db())
        .writer()
        .execute(sqlx::query("DROP TRIGGER hold_retirement"))
        .await
        .unwrap();
    let row = h.execution_row().await;
    assert_eq!(row.0, "retire");
    assert!(row.2.is_some(), "the retirement window is still open");
    let card = output_card_id(&h.op().await.tx_output.unwrap()).unwrap();
    assert_eq!(h.delete(scope).await, StatusCode::NO_CONTENT);
    assert!(h.db.card_get(&card).await.unwrap().is_none());
    let row = h.execution_row().await;
    assert_eq!(row.0, "retire");
    assert!(row.1.is_some());
    assert!(row.2.is_none());
}

#[tokio::test]
async fn w_terminal_worker_retiring_delete_card() {
    retiring_delete("cards").await;
}
#[tokio::test]
async fn w_terminal_worker_retiring_delete_track() {
    retiring_delete("tracks").await;
}
#[tokio::test]
async fn w_terminal_worker_retiring_delete_area() {
    retiring_delete("areas").await;
}

async fn running_delete(scope: &str) {
    let h = Harness::new("sleep 120").await;
    h.park().await;
    h.release().await;
    let card = output_card_id(&h.op().await.tx_output.unwrap()).unwrap();
    // Retain the original observer lease until the task failure is observed, before
    // Track/Area cascade can remove the task row. Disposal must not steal this lease.
    let claimed = h.ops.claim_parked(&h.id).await.unwrap().unwrap();
    let observe = async {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let task = h.db.task_get(&h.task.id).await.unwrap().unwrap();
                if task.status == crate::model::TaskStatus::Failed {
                    assert_eq!(
                        crate::db::sqlite::status_detail_class(
                            task.status_detail.as_deref().unwrap()
                        ),
                        "worker-card-deleted"
                    );
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        h.clear_claim().await;
    };
    let (status, ()) = tokio::join!(h.delete(scope), observe);
    assert_eq!(status, StatusCode::NO_CONTENT);
    drop(claimed);
    assert!(h.db.card_get(&card).await.unwrap().is_none());
    let record = pipe::record(CONFIG, &h.op().await).unwrap();
    let row: (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT desired,settlement_json,secrets_json FROM workspace_executions WHERE execution_id=?1")
        .bind(record.execution_id.as_str()).fetch_one(h.db.pool()).await.unwrap();
    assert_eq!(row.0, "retire");
    assert!(row.1.is_some());
    assert!(row.2.is_none());
}
#[tokio::test]
async fn w_terminal_worker_running_delete_card() {
    running_delete("cards").await;
}
#[tokio::test]
async fn w_terminal_worker_running_delete_track() {
    running_delete("tracks").await;
}
#[tokio::test]
async fn w_terminal_worker_running_delete_area() {
    running_delete("areas").await;
}

#[tokio::test]
async fn w_terminal_worker_child_path_leads_with_kernel_bin() {
    let h = Harness::new("printf '%s' \"$PATH\" > child-path").await;
    h.park().await;
    h.release().await;
    h.settled().await;
    h.complete().await;
    assert_eq!(h.status().await, crate::model::TaskStatus::Done);
    let path = std::fs::read_to_string(h.root.path().join("child-path")).unwrap();
    assert_eq!(
        std::env::split_paths(&path).next().unwrap(),
        crate::kernel_bin_path::kernel_bin_dir().unwrap()
    );
}

#[tokio::test]
async fn reclaim_recovery_global_drive_busy_releases_scheduler_capacity() {
    let h = Harness::new("printf started > ran").await;
    let state = h.state().await;
    let scheduler = state.scheduler_for_test();
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();
    let drive = state
        .operation_runtime
        .lock_for_track_delete_for_test()
        .await;

    tokio::time::timeout(Duration::from_secs(2), scheduler.sweep_all())
        .await
        .expect("a busy global driver must not retain the scheduler permit");
    assert_eq!(h.status().await, crate::model::TaskStatus::Dispatched);
    let blocked = h.op().await;
    let blocker = crate::operation::pending_blocker(&blocked)
        .unwrap()
        .expect("the exact pending operation records the blocker");
    assert!(matches!(
        blocker,
        PendingBlocker::V1 {
            class: PendingBlockerClass::DriverBusy,
            ..
        }
    ));
    assert!(!h.root.path().join("ran").exists());

    drop(drive);
    let resumed = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            scheduler.sweep_all().await;
            if matches!(
                h.status().await,
                crate::model::TaskStatus::Running | crate::model::TaskStatus::Done
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    if resumed.is_err() {
        let task = h.db.task_get(&h.task.id).await.unwrap().unwrap();
        let op = h.op().await;
        panic!(
            "the original task and operation did not resume: status={:?} detail={:?} \
             op_phase={:?} op_detail={:?} lease={:?}/{:?} last_error={:?}",
            task.status,
            task.status_detail,
            op.phase,
            op.phase_detail,
            op.lease_owner,
            op.lease_until_ms,
            op.last_error
        );
    }
    assert_eq!(
        state
            .operation_runtime
            .find_by_kind_and_idempotency("terminal-worker", &h.task.id)
            .await
            .unwrap()
            .unwrap()
            .id,
        h.id
    );
}

#[tokio::test]
async fn reclaim_recovery_pipe_rechecks_checkout_before_preparing() {
    let h = Harness::new("printf started >> ran").await;
    let sibling = format!("{}:follower", h.task.track_id);
    // A real current task row models a follower already admitted while A yielded.
    h.db.db().writer().execute(sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,access,created_at_ms,updated_at_ms) \
         VALUES (?1,?2,'follower','terminal','true','{}','running','read_write',1,1)",
    )
    .bind(&sibling).bind(&h.task.track_id)).await.unwrap();
    let (_, payload) = crate::scheduler::build_worker_payload(&h.task).unwrap();
    let key = || OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(h.task.id.clone()),
        payload_hash: crate::routes::idempotency_key::stable_payload_hash(&payload).unwrap(),
    };
    let (_, blocked) = h
        .runtime
        .submit_task_bounded("terminal-worker", key(), payload.clone())
        .await
        .unwrap();
    assert!(matches!(
        blocked,
        crate::operation::OperationWait::Deferred { .. }
    ));
    assert_eq!(h.op().await.phase, Phase::Pending);
    assert!(h.op().await.tx_output.is_none());
    assert!(!h.root.path().join("ran").exists());
    h.db.db()
        .writer()
        .execute(sqlx::query("UPDATE tasks SET status='done' WHERE id=?1").bind(sibling))
        .await
        .unwrap();
    let (id, resumed) = h
        .runtime
        .submit_task_bounded("terminal-worker", key(), payload)
        .await
        .unwrap();
    assert_eq!(id, h.id);
    assert!(matches!(
        resumed,
        crate::operation::OperationWait::OwnedParked(_)
    ));
    h.wait_done().await;
    assert_eq!(
        std::fs::read_to_string(h.root.path().join("ran")).unwrap(),
        "started"
    );
}

#[tokio::test]
async fn reclaim_recovery_ordinary_drive_returns_while_launch_fence_is_held() {
    let h = Harness::new("printf once >> ran").await;
    let fence =
        crate::per_card_lock::lock_key(h.runtime.track_locks_for_test(), &h.task.track_id).await;
    tokio::time::timeout(Duration::from_secs(2), h.runtime.drive())
        .await
        .expect("ordinary drive must not repeatedly reclaim a deferred row")
        .unwrap();
    let deferred = h.op().await;
    assert_eq!(deferred.phase, Phase::Pending);
    assert!(deferred.tx_output.is_none());
    assert!(deferred.lease_owner.is_none());
    assert!(deferred.lease_until_ms.is_some());
    assert!(!h.root.path().join("ran").exists());
    drop(fence);
    let (_, payload) = crate::scheduler::build_worker_payload(&h.task).unwrap();
    let key = OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(h.task.id.clone()),
        payload_hash: crate::routes::idempotency_key::stable_payload_hash(&payload).unwrap(),
    };
    let (id, result) = h
        .runtime
        .submit_task_bounded("terminal-worker", key, payload)
        .await
        .unwrap();
    assert_eq!(id, h.id);
    assert!(matches!(
        result,
        crate::operation::OperationWait::OwnedParked(_)
    ));
    h.wait_done().await;
    assert_eq!(
        std::fs::read_to_string(h.root.path().join("ran")).unwrap(),
        "once"
    );
}

#[tokio::test]
async fn reclaim_recovery_pipe_held_checkout_conflict_preserves_execution() {
    let h = Harness::new("printf once >> ran").await;
    h.park().await;
    let id = pipe::record(CONFIG, &h.op().await).unwrap().execution_id;
    let sibling = format!("{}:follower", h.task.track_id);
    h.db.db().writer().execute(sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,access,created_at_ms,updated_at_ms) \
         VALUES (?1,?2,'follower','terminal','true','{}','running','read_write',1,1)",
    )
    .bind(&sibling).bind(&h.task.track_id)).await.unwrap();
    let op = h.ops.claim_parked(&h.id).await.unwrap().unwrap();
    h.adapter.act_pipe(&op, &h.ctx).await.unwrap();
    let held = pipe::record(CONFIG, &h.op().await).unwrap();
    assert_eq!(held.execution_id, id);
    assert_eq!(held.release, Release::Held);
    assert_eq!(held.decision, Decision::Observing);
    assert!(!h.root.path().join("ran").exists());
    h.clear_claim().await;
    h.db.db()
        .writer()
        .execute(sqlx::query("UPDATE tasks SET status='done' WHERE id=?1").bind(sibling))
        .await
        .unwrap();
    h.release().await;
    h.settled().await;
    h.complete().await;
    assert_eq!(
        pipe::record(CONFIG, &h.op().await).unwrap().execution_id,
        id
    );
    assert_eq!(
        std::fs::read_to_string(h.root.path().join("ran")).unwrap(),
        "once"
    );
}

#[tokio::test]
async fn reclaim_recovery_other_track_runs_while_first_track_is_blocked() {
    let h = Harness::new("printf original >> ran").await;
    let track = h.db.track_get(&h.task.track_id).await.unwrap().unwrap();
    let other_root = h.root.path().join("other");
    std::fs::create_dir(&other_root).unwrap();
    let other =
        h.db.track_create(crate::model::NewTrack {
            template_input: None,
            area_id: track.area_id,
            title: "other".into(),
            sort: None,
            cwd: other_root.to_str().unwrap().into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: crate::model::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let state = h.state().await;
    let follower = crate::operation::launch_cleanup_test_support::claimed_task(
        h.db.clone(), state.events.clone(), state.write().clone(),
        other.id.as_str(),
        json!({"key":"other","kind":"terminal","command":"printf other >> ran","ready":true,"declared_by":"user"}),
    ).await;
    let scheduler = state.scheduler_for_test();
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();
    let fence = crate::per_card_lock::lock_key(
        state.operation_runtime.track_locks_for_test(),
        &h.task.track_id,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            scheduler.sweep_all().await;
            if h.db.task_get(&follower.id).await.unwrap().unwrap().status
                == crate::model::TaskStatus::Done
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the second track completes while the first fence stays held");
    assert_eq!(h.status().await, crate::model::TaskStatus::Dispatched);
    assert!(!h.root.path().join("ran").exists());
    assert_eq!(
        std::fs::read_to_string(other_root.join("ran")).unwrap(),
        "other"
    );
    drop(fence);
}

#[tokio::test]
async fn reclaim_recovery_pipe_running_ack_does_not_authorize_after_close() {
    let h = Harness::new("printf forbidden > ran").await;
    h.park().await;
    let held = pipe::record(CONFIG, &h.op().await).unwrap();
    assert_eq!(held.release, Release::Held);
    // Hold the observer claim while the real scheduler acknowledges the durable
    // park. Running acknowledges ownership, not delivery of the go token.
    let _claim = h.ops.claim_parked(&h.id).await.unwrap().unwrap();
    let state = h.state().await;
    let scheduler = state.scheduler_for_test();
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();
    scheduler.sweep_all().await;
    assert_eq!(h.status().await, crate::model::TaskStatus::Running);
    assert_eq!(pipe::record(CONFIG, &h.op().await).unwrap(), held);
    h.db.db()
        .writer()
        .execute(sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1").bind(&h.task.track_id))
        .await
        .unwrap();
    h.clear_claim().await;
    h.release().await;
    let refused = pipe::record(CONFIG, &h.op().await).unwrap();
    assert_eq!(refused.execution_id, held.execution_id);
    assert_eq!(refused.release, Release::Held);
    assert!(matches!(refused.decision, Decision::Stopping { .. }));
    h.settled().await;
    h.boot().await;
    h.wait_done().await;
    assert!(!h.root.path().join("ran").exists());
    assert_eq!(
        pipe::record(CONFIG, &h.op().await).unwrap().release,
        Release::Held
    );
}

#[tokio::test]
async fn reclaim_recovery_closed_track_refuses_late_blocked_launch() {
    let h = Harness::new("printf forbidden > ran").await;
    let fence =
        crate::per_card_lock::lock_key(h.runtime.track_locks_for_test(), &h.task.track_id).await;
    h.runtime.drive().await.unwrap();
    h.db.db()
        .writer()
        .execute(sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1").bind(&h.task.track_id))
        .await
        .unwrap();
    drop(fence);
    let (_, payload) = crate::scheduler::build_worker_payload(&h.task).unwrap();
    let key = OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(h.task.id.clone()),
        payload_hash: crate::routes::idempotency_key::stable_payload_hash(&payload).unwrap(),
    };
    let (id, result) = h
        .runtime
        .submit_task_bounded("terminal-worker", key, payload)
        .await
        .unwrap();
    assert_eq!(id, h.id);
    assert!(matches!(
        result,
        crate::operation::OperationWait::Settled(crate::operation::OperationResult {
            outcome: crate::operation::OperationOutcome::Failed { .. },
            ..
        })
    ));
    assert!(!h.root.path().join("ran").exists());
}

#[tokio::test]
async fn w_terminal_worker_never_admitted_stuck_delete() {
    for scope in ["cards", "tracks", "areas"] {
        let h = Harness::new("printf forbidden > ran").await;
        h.prepare().await;
        (h.db.db())
            .writer()
            .execute(sqlx::query(
                "CREATE TRIGGER refuse_spawn_started BEFORE UPDATE OF phase ON operations \
            WHEN OLD.phase='tx_committed' AND NEW.phase='spawn_started' \
            BEGIN SELECT RAISE(ABORT,'pre-admission transition interrupted'); END",
            ))
            .await
            .unwrap();
        h.runtime.drive().await.unwrap();
        assert!(matches!(h.op().await.phase, Phase::Stuck { .. }));
        let state = h.state().await;
        let scheduler = state.scheduler_for_test();
        scheduler.mark_context_sweep_boot_complete();
        scheduler.sweep_all().await;
        (h.db.db())
            .writer()
            .execute(sqlx::query("DROP TRIGGER refuse_spawn_started"))
            .await
            .unwrap();
        assert_eq!(h.status().await, crate::model::TaskStatus::Failed);
        assert!(h.op().await.tx_output.unwrap().data.get(KEY).is_none());
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_executions")
            .fetch_one(h.db.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
        assert!(!h.root.path().join("ran").exists());
        assert_eq!(h.delete(scope).await, StatusCode::NO_CONTENT);
    }
}

#[tokio::test]
async fn w_terminal_worker_admitted_stuck_delete_refused() {
    let h = Harness::new("exit 0").await;
    h.park().await;
    // Fault fixture: a terminal op still owns an admitted, unsettled execution.
    (h.db.db()).writer().execute(sqlx::query("UPDATE operations SET phase='stuck', phase_detail_json=json_object('reason','interrupted','since',1) WHERE id=?1")
        .bind(&h.id)).await.unwrap();
    (h.db.db())
        .writer()
        .execute(sqlx::query("UPDATE tasks SET status='failed' WHERE id=?1").bind(&h.task.id))
        .await
        .unwrap();
    assert!(matches!(h.op().await.phase, Phase::Stuck { .. }));
    for scope in ["cards", "tracks", "areas"] {
        assert_eq!(h.delete(scope).await, StatusCode::CONFLICT);
    }
    // Restore the fixture so the real owner can release and settle the held execution.
    (h.db.db())
        .writer()
        .execute(sqlx::query("UPDATE tasks SET status='dispatched' WHERE id=?1").bind(&h.task.id))
        .await
        .unwrap();
    (h.db.db())
        .writer()
        .execute(
            sqlx::query("UPDATE operations SET phase='parked', phase_detail_json=NULL WHERE id=?1")
                .bind(&h.id),
        )
        .await
        .unwrap();
    h.release().await;
    h.settled().await;
    h.complete().await;
}
