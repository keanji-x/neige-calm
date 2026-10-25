//! Card DELETE through the HTTP router with the real Engine and a registered Pipe owner.
use super::*;
use crate::{
    db::{RepoRead, RepoSyncDomainRaw},
    model::*,
    plugin_host::{PluginHost, PluginRegistry},
    state::{AppState, CodexClient, WriteContext},
};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

pub(super) struct WorkerProjection;
#[async_trait::async_trait]
impl ExecutionProjection for WorkerProjection {
    fn disposal(&self) -> Option<&dyn DisposalOwner> {
        static OWNER: crate::scheduler::card_disposal::CardDisposalOwner =
            crate::scheduler::card_disposal::CardDisposalOwner(CONFIG);
        Some(&OWNER)
    }
    async fn spawned(
        &self,
        _: &mut Tx<'_>,
        _: &str,
        _: i64,
    ) -> std::result::Result<(), sqlx::Error> {
        Ok(())
    }
    async fn settled(
        &self,
        _: &mut Tx<'_>,
        _: &str,
        _: &Settlement,
        _: i64,
    ) -> std::result::Result<(), sqlx::Error> {
        Ok(())
    }
}

pub(super) async fn seed_card(db: &SqlxRepo) -> (String, String) {
    let area = db
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            NewArea {
                name: "owned".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = db
        .track_create(NewTrack {
            area_id: area.id,
            title: "owned".into(),
            cwd: "/tmp".into(),
            sort: None,
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = db
        .card_create(NewCard {
            track_id: track.id.clone(),
            kind: "terminal".into(),
            title: None,
            sort: None,
            payload: json!({"terminal_presentation":{"source":"unrecorded","in_workbench":false}}),
        })
        .await
        .unwrap();
    (track.id.to_string(), card.id.to_string())
}

async fn state(h: &Harness) -> AppState {
    let roles = crate::card_role_cache::CardRoleCache::new();
    let areas = crate::track_area_cache::TrackAreaCache::new();
    h.db.seed_card_role_cache(&roles).await.unwrap();
    h.db.seed_track_area_cache(&areas).await.unwrap();
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        h.db.clone(),
        h._executor._root.path().join("plugins"),
        h._executor._root.path().join("data"),
        vec![],
        h.ctx.events.clone(),
        WriteContext::new(roles.clone(), areas.clone()),
    ));
    let state = AppState::from_parts(
        h.db.clone(),
        h.ctx.events.clone(),
        Arc::new(DaemonClient::new_stub()),
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(areas),
    );
    state.with_operation_runtime(h.runtime.clone())
}
async fn delete(state: AppState, card: &str) -> StatusCode {
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
    app.oneshot(
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/cards/{card}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}
fn card(h: &Harness) -> &str {
    h.op.tx_output
        .as_ref()
        .unwrap()
        .target_id
        .as_deref()
        .unwrap()
}
async fn wait_stopped(h: &Harness) {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        h.ctx
            .execution
            .engine()
            .unwrap()
            .wait_settled(&h.metadata.id),
    )
    .await
    .unwrap()
    .unwrap();
}
async fn complete(h: &Harness) {
    let op = h.repo.get_operation(&h.op.id).await.unwrap().unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(h.db.db())
        .await
        .unwrap();
    crate::operation::repo_sqlite::require_live_claim_tx(&mut tx, &op)
        .await
        .unwrap();
    retire(CONFIG, &mut tx, &op).await.unwrap();
    sqlx::query("UPDATE operations SET phase='succeeded',completed_at_ms=1 WHERE id=?1")
        .bind(&op.id)
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    h.ctx.execution.drive([h.metadata.id.clone()]);
}
async fn delete_and_finish(h: &Harness, state: AppState) -> StatusCode {
    let deletion = delete(state, card(h));
    let finish = async {
        wait_stopped(h).await;
        complete(h).await;
    };
    let (status, ()) = tokio::join!(deletion, finish);
    status
}

#[tokio::test]
async fn w_owned_delete_card_stops_and_settles_without_terminal_row() {
    let h = Harness::with_card(true).await;
    assert!(h.db.terminal_get_by_card(card(&h)).await.unwrap().is_none());
    let state = state(&h).await;
    assert_eq!(delete_and_finish(&h, state).await, StatusCode::NO_CONTENT);
    assert!(h.db.card_get(card(&h)).await.unwrap().is_none());
    let row =
        calm_truth::db::sqlite::workspace_execution::load(h.db.pool(), h.metadata.id.as_str())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.desired, "retire");
    assert!(row.secrets_json.is_none());
}

async fn task(h: &Harness, id: &str, stamped: bool, gated: bool) {
    let card_row = h.db.card_get(card(h)).await.unwrap().unwrap();
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id,gate_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?1,'terminal','owned','{}','running',?3,?4,1,1)")
        .bind(id).bind(card_row.track_id.as_str()).bind(stamped.then(|| card(h)))
        .bind(gated.then_some(r#"{"steps":[{"name":"check","command":"true"}]}"#))
        .execute(h.db.pool()).await.unwrap();
    if !stamped {
        let proof = h
            .repo
            .insert_operation(
                "terminal-worker",
                OperationKey {
                    operation_key: crate::model::new_id(),
                    idempotency_key: Some(id.into()),
                    payload_hash: "proof".into(),
                },
                json!({"actor":{"kind":"KernelDispatcher"}}),
            )
            .await
            .unwrap();
        sqlx::query("UPDATE operations SET target_type='card',target_id=?2 WHERE id=?1")
            .bind(&proof)
            .bind(card(h))
            .execute(h.db.pool())
            .await
            .unwrap();
    }
}
async fn task_status(h: &Harness, id: &str) -> (String, Option<String>) {
    sqlx::query_as("SELECT status,status_detail FROM tasks WHERE id=?1")
        .bind(id)
        .fetch_one(h.db.pool())
        .await
        .unwrap()
}
async fn failed_events(h: &Harness) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind='task.failed'")
        .fetch_one(h.db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn w_owned_disposal_records_failed_task_once() {
    for stamped in [false, true] {
        let h = Harness::with_card(true).await;
        task(&h, "worker", stamped, true).await;
        // Rolling back the owner's stop also rolls back its task outcome and event.
        let mut tx = crate::db::sqlite::begin_immediate_tx(h.db.db())
            .await
            .unwrap();
        WorkerProjection
            .disposal()
            .unwrap()
            .stop_tx(&mut tx, &h.metadata)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(task_status(&h, "worker").await.0, "running");
        assert_eq!(failed_events(&h).await, 0);
        assert_eq!(
            record(
                CONFIG,
                &h.repo.get_operation(&h.op.id).await.unwrap().unwrap()
            )
            .unwrap()
            .decision,
            Decision::Observing
        );

        let state = state(&h).await;
        let deletion = delete(state.clone(), card(&h));
        let finish = async {
            wait_stopped(&h).await;
            let row = calm_truth::db::sqlite::workspace_execution::load(
                h.db.pool(),
                h.metadata.id.as_str(),
            )
            .await
            .unwrap()
            .unwrap();
            let settlement: Settlement =
                serde_json::from_str(row.settlement_json.as_deref().unwrap()).unwrap();
            assert!(matches!(
                settlement,
                Settlement::Journal(
                    ExecutionState::Drained { status: Some(0) }
                        | ExecutionState::OriginalGroupEmpty { status: Some(0) }
                )
            ));
            let (status, detail) = task_status(&h, "worker").await;
            assert_eq!(status, "failed");
            assert_eq!(
                crate::db::sqlite::status_detail_class(detail.as_deref().unwrap()),
                "worker-card-deleted"
            );
            // A late successful worker report cannot mark Done or start a gate.
            let mut tx = crate::db::sqlite::begin_immediate_tx(h.db.db())
                .await
                .unwrap();
            assert_eq!(
                crate::db::sqlite::task_report_success_from_worker_tx(
                    &mut tx,
                    "worker",
                    &h.metadata.track.0,
                    crate::db::sqlite::TaskReporter::Kernel,
                    crate::model::now_ms()
                )
                .await
                .unwrap(),
                crate::db::sqlite::SuccessReportFlip::None
            );
            tx.commit().await.unwrap();
            assert_eq!(failed_events(&h).await, 1);
            complete(&h).await;
        };
        let (status, ()) = tokio::join!(deletion, finish);
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(delete(state, card(&h)).await, StatusCode::NOT_FOUND);
        assert_eq!(task_status(&h, "worker").await.0, "failed");
        assert_eq!(failed_events(&h).await, 1);
    }
    // Natural completion and its task CAS win before deletion.
    let h = Harness::with_card(true).await;
    task(&h, "natural", true, false).await;
    let mut tx = crate::db::sqlite::begin_immediate_tx(h.db.db())
        .await
        .unwrap();
    assert_eq!(
        crate::db::sqlite::task_report_success_from_worker_tx(
            &mut tx,
            "natural",
            &h.metadata.track.0,
            crate::db::sqlite::TaskReporter::Kernel,
            crate::model::now_ms()
        )
        .await
        .unwrap(),
        crate::db::sqlite::SuccessReportFlip::Done
    );
    tx.commit().await.unwrap();
    super::super::super::send_release(&h.ctx, &h.metadata.id, b"go\n")
        .await
        .unwrap();
    wait_stopped(&h).await;
    complete(&h).await;
    assert_eq!(
        delete(state(&h).await, card(&h)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(task_status(&h, "natural").await.0, "done");
    assert_eq!(failed_events(&h).await, 0);
}

#[tokio::test]
async fn w_owned_disposal_unknown_retains_scope() {
    let h = Harness::with_card(true).await;
    let mut state = state(&h).await;
    let no_ports = SqliteExecutionStore::new(h.db.db().clone()).with_projection(
        CONFIG.consumer.into(),
        Arc::new(crate::execution_store::tests::NoopProjection),
    );
    let secret = serde_json::from_reader(
        std::fs::File::open(h._executor._root.path().join("key.json")).unwrap(),
    )
    .unwrap();
    let engine = Arc::new(Engine::new(
        no_ports,
        session_executor::SessionExecutor::new(
            h._executor._socket.path().join("control.sock"),
            secret,
        ),
    ));
    engine.recover().await;
    let lifecycle = crate::execution_quiesce::ExecutionLifecycle::default();
    lifecycle.bind(engine);
    let mut ctx = h.ctx.clone();
    ctx.execution = lifecycle;
    state = state.with_operation_runtime(Arc::new(OperationRuntime::new_unchecked(
        h.repo.clone(),
        vec![],
        h.ctx.events.clone(),
        h.ctx.completion.clone(),
        ctx,
    )));
    assert_eq!(delete(state, card(&h)).await, StatusCode::CONFLICT);
    assert!(h.db.card_get(card(&h)).await.unwrap().is_some());
    let row =
        calm_truth::db::sqlite::workspace_execution::load(h.db.pool(), h.metadata.id.as_str())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.desired, "run");
    assert!(row.secrets_json.is_some());
    h.stop().await;
    h.settle().await;
}

#[tokio::test]
async fn w_owned_disposal_blocks_release_and_readmission() {
    let h = Harness::with_card(true).await;
    let state = state(&h).await;
    let mut deletion = Box::pin(delete(state, card(&h)));
    tokio::select! {
        result = &mut deletion => panic!("delete completed before owner: {result:?}"),
        () = wait_stopped(&h) => {}
    }
    assert!(matches!(
        h.runtime.try_launch_fence(&h.metadata.track.0),
        Err(crate::error::CalmError::WorkspaceBusy(_))
    ));
    let current = h.repo.get_operation(&h.op.id).await.unwrap().unwrap();
    for op in [&h.op, &current] {
        assert!(
            release(
                CONFIG,
                &h.ctx,
                op,
                &Authority,
                ReleaseRequest {
                    token: b"go\n",
                    timeout: std::time::Duration::from_secs(1)
                },
                None
            )
            .await
            .is_err()
        );
        assert!(
            admit(
                CONFIG,
                op,
                &h.ctx,
                h._executor._root.path().canonicalize().unwrap(),
                &h.metadata.track.0,
                h.metadata.program.clone()
            )
            .await
            .is_err()
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace_executions")
        .fetch_one(h.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    complete(&h).await;
    assert_eq!(deletion.await, StatusCode::NO_CONTENT);
}
