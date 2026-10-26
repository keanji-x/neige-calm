use super::*;
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
}

async fn completed_delete(scope: &str) {
    let h = Harness::new("exit 0").await;
    h.park().await;
    h.release().await;
    h.settled().await;
    h.complete().await;
    assert_eq!(h.status().await, crate::model::TaskStatus::Done);
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
async fn w_terminal_worker_never_admitted_stuck_delete() {
    for scope in ["cards", "tracks", "areas"] {
        let h = Harness::new("printf forbidden > ran").await;
        h.prepare().await;
        sqlx::query(
            "CREATE TRIGGER refuse_spawn_started BEFORE UPDATE OF phase ON operations \
            WHEN OLD.phase='tx_committed' AND NEW.phase='spawn_started' \
            BEGIN SELECT RAISE(ABORT,'pre-admission transition interrupted'); END",
        )
        .execute(h.db.pool())
        .await
        .unwrap();
        h.runtime.drive().await.unwrap();
        assert!(matches!(h.op().await.phase, Phase::Stuck { .. }));
        let state = h.state().await;
        let scheduler = state.scheduler_for_test();
        scheduler.mark_context_sweep_boot_complete();
        scheduler.sweep_all().await;
        sqlx::query("DROP TRIGGER refuse_spawn_started")
            .execute(h.db.pool())
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
    sqlx::query("UPDATE operations SET phase='stuck', phase_detail_json=json_object('reason','interrupted','since',1) WHERE id=?1")
        .bind(&h.id).execute(h.db.pool()).await.unwrap();
    sqlx::query("UPDATE tasks SET status='failed' WHERE id=?1")
        .bind(&h.task.id)
        .execute(h.db.pool())
        .await
        .unwrap();
    assert!(matches!(h.op().await.phase, Phase::Stuck { .. }));
    for scope in ["cards", "tracks", "areas"] {
        assert_eq!(h.delete(scope).await, StatusCode::CONFLICT);
    }
    // Restore the fixture so the real owner can release and settle the held execution.
    sqlx::query("UPDATE tasks SET status='dispatched' WHERE id=?1")
        .bind(&h.task.id)
        .execute(h.db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE operations SET phase='parked', phase_detail_json=NULL WHERE id=?1")
        .bind(&h.id)
        .execute(h.db.pool())
        .await
        .unwrap();
    h.release().await;
    h.settled().await;
    h.complete().await;
}
