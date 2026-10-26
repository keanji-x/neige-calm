//! Scope HTTP deletion must finish card owners before cascade erases their history.
use super::*;

async fn request(
    h: &Harness,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> StatusCode {
    request_with_state(state(h).await, method, uri, body).await
}

async fn request_with_state(
    state: AppState,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> StatusCode {
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
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

async fn additional_owner(h: &Harness, track: &str) -> (String, ExecutionMetadata) {
    let card =
        h.db.card_create(NewCard {
            track_id: track.to_owned().into(),
            kind: "terminal".into(),
            title: None,
            sort: None,
            payload: json!({"terminal_presentation":{"source":"unrecorded","in_workbench":false}}),
        })
        .await
        .unwrap();
    let id = h
        .repo
        .insert_operation(
            "disposal-test",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash: "hash".into(),
            },
            json!({}),
        )
        .await
        .unwrap();
    let mut output = TxOutput::new("card", Some(card.id.to_string()), json!({}));
    output.data = json!({});
    sqlx::query("UPDATE operations SET phase='spawn_started',target_type='card',target_id=?2,tx_output_json=?3 WHERE id=?1")
        .bind(&id).bind(card.id.as_str()).bind(serde_json::to_string(&output).unwrap())
        .execute(h.db.pool()).await.unwrap();
    let op = h.repo.claim_drive_operation(&id).await.unwrap().unwrap();
    admit(
        CONFIG,
        &op,
        &h.ctx,
        h._executor._root.path().canonicalize().unwrap(),
        track,
        h.metadata.program.clone(),
    )
    .await
    .unwrap();
    let op = h.repo.get_operation(&id).await.unwrap().unwrap();
    let execution = record(CONFIG, &op).unwrap().execution_id;
    let mut tx = crate::db::sqlite::begin_immediate_tx(h.db.db())
        .await
        .unwrap();
    let metadata: ExecutionMetadata = serde_json::from_str(
        &calm_truth::db::sqlite::workspace_execution::intent_tx(&mut tx, execution.as_str())
            .await
            .unwrap(),
    )
    .unwrap();
    tx.commit().await.unwrap();
    let engine = h.ctx.execution.engine().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let generation = engine.output_generation().await.unwrap();
            if matches!(engine.output(&metadata.id, None).await,
                Ok(ExecutionReply::Observation {state: ExecutionState::Spawned, ref output, ..}) if output == b"ready") {
                break;
            }
            generation.changed().await.unwrap();
        }
    }).await.unwrap();
    (id, metadata)
}

async fn finish_owner(h: &Harness, id: &str, metadata: &ExecutionMetadata) {
    h.ctx
        .execution
        .engine()
        .unwrap()
        .wait_settled(&metadata.id)
        .await
        .unwrap();
    let op = h.repo.get_operation(id).await.unwrap().unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(h.db.db())
        .await
        .unwrap();
    crate::operation::repo_sqlite::require_live_claim_tx(&mut tx, &op)
        .await
        .unwrap();
    retire(CONFIG, &mut tx, &op).await.unwrap();
    sqlx::query("UPDATE operations SET phase='succeeded',completed_at_ms=1 WHERE id=?1")
        .bind(id)
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    h.ctx.execution.drive([metadata.id.clone()]);
}

async fn scope_delete(area: bool) {
    let h = Harness::with_card(true).await;
    let track = h.db.track_get(&h.metadata.track.0).await.unwrap().unwrap();
    let other_track = if area {
        h.db.track_create(NewTrack {
            area_id: track.area_id.clone(),
            title: "second".into(),
            cwd: "/tmp".into(),
            sort: None,
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap()
        .id
        .to_string()
    } else {
        track.id.to_string()
    };
    let (other_op, other) = additional_owner(&h, &other_track).await;
    task(&h, "worker", true, true).await;
    let uri = if area {
        format!("/api/areas/{}", track.area_id)
    } else {
        format!("/api/tracks/{}", track.id)
    };
    let deletion = request(&h, "DELETE", &uri, None);
    let finish = async {
        wait_stopped(&h).await;
        // The task row is observable until every owner finishes, before cascade.
        let (status, detail) = task_status(&h, "worker").await;
        assert_eq!(status, "failed");
        assert_eq!(
            crate::db::sqlite::status_detail_class(detail.as_deref().unwrap()),
            "worker-card-deleted"
        );
        assert_eq!(failed_events(&h).await, 1);
        complete(&h).await;
        finish_owner(&h, &other_op, &other).await;
    };
    let (status, ()) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        tokio::join!(deletion, finish)
    })
    .await
    .unwrap();
    assert_eq!(status, StatusCode::NO_CONTENT);
    for id in [track.id.as_str(), &other_track] {
        assert!(h.db.track_get(id).await.unwrap().is_none());
    }
    for id in [&h.metadata.id, &other.id] {
        let row = calm_truth::db::sqlite::workspace_execution::load(h.db.pool(), id.as_str())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.desired, "retire");
        assert!(row.settlement_json.is_some());
        assert!(row.secrets_json.is_none());
    }
    if area {
        assert!(
            h.db.area_get(track.area_id.as_str())
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn w_owned_delete_track_stops_all_card_owners() {
    scope_delete(false).await;
}

#[tokio::test]
async fn w_owned_delete_area_stops_all_card_owners() {
    scope_delete(true).await;
}

#[tokio::test]
async fn w_owned_track_delete_unknown_retains_scope() {
    let h = Harness::with_card(true).await;
    task(&h, "worker", true, false).await;
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
    assert!(engine.recover().await.error.is_none());
    let lifecycle = crate::execution_quiesce::ExecutionLifecycle::default();
    lifecycle.bind(engine);
    let mut ctx = h.ctx.clone();
    ctx.execution = lifecycle;
    let state = state(&h)
        .await
        .with_operation_runtime(Arc::new(OperationRuntime::new_unchecked(
            h.repo.clone(),
            vec![],
            h.ctx.events.clone(),
            h.ctx.completion.clone(),
            ctx,
        )));
    assert_eq!(
        request_with_state(
            state,
            "DELETE",
            &format!("/api/tracks/{}", h.metadata.track.0),
            None
        )
        .await,
        StatusCode::CONFLICT
    );
    assert!(h.db.track_get(&h.metadata.track.0).await.unwrap().is_some());
    assert!(h.db.card_get(card(&h)).await.unwrap().is_some());
    assert_eq!(task_status(&h, "worker").await.0, "running");
    assert_eq!(failed_events(&h).await, 0);
    let row =
        calm_truth::db::sqlite::workspace_execution::load(h.db.pool(), h.metadata.id.as_str())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.desired, "run");
    assert!(row.settlement_json.is_none());
    h.stop().await;
    h.settle().await;
}

#[tokio::test]
async fn w_owned_repoint_does_not_stop() {
    let h = Harness::with_card(true).await;
    let target = h._executor._root.path().join("attached");
    std::fs::create_dir(&target).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&target)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        request(
            &h,
            "PATCH",
            &format!("/api/tracks/{}", h.metadata.track.0),
            Some(json!({"workspace":{"kind":"attached","path":target,"attach_folder":true}}))
        )
        .await,
        StatusCode::CONFLICT
    );
    let row =
        calm_truth::db::sqlite::workspace_execution::load(h.db.pool(), h.metadata.id.as_str())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.desired, "run");
    assert!(row.settlement_json.is_none());
    assert!(matches!(
        h.ctx
            .execution
            .engine()
            .unwrap()
            .output(&h.metadata.id, None)
            .await
            .unwrap(),
        ExecutionReply::Observation {
            state: ExecutionState::Spawned,
            ..
        }
    ));
    h.stop().await;
    h.settle().await;
}
