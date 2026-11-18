use super::*;
use crate::operation::{OperationKey, OperationRepo, ProviderAdapter, SqlxOperationRepo};
use crate::worker_model_defaults::tests::Warnings;

async fn worker(f: &Fixture, value: Option<Value>) -> String {
    let (card, _) = seed_cold_owner(f).await;
    if value.is_none() {
        // The legacy cold fixture has the same durable dispatcher ownership proof as a worker.
        let ops = SqlxOperationRepo::new(f.repo.db().clone());
        let op = ops
            .insert_operation(
                "codex-worker",
                OperationKey {
                    operation_key: crate::model::new_id(),
                    idempotency_key: None,
                    payload_hash: "legacy".into(),
                },
                json!({"actor":crate::ids::ActorId::KernelDispatcher}),
            )
            .await
            .unwrap();
        f.repo.db().writer().execute(sqlx::query(
            "UPDATE operations SET target_type='card',target_id=?,phase='succeeded' WHERE id=?"
        ).bind(&card).bind(op)).await.unwrap();
    }
    let id = card.clone();
    write_in_tx_typed(f.daemon.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let mut shape = crate::db::sqlite::card_execution_shape_tx(tx, &id).await?;
            if let Some(value) = value {
                shape.payload["worker_model_selection"] = value;
            }
            sqlx::query("UPDATE cards SET role='worker',payload=? WHERE id=?")
                .bind(shape.payload.to_string())
                .bind(id)
                .execute(&mut **tx)
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    card
}

#[tokio::test]
async fn worker_model_defaults_cold_resume_and_reconnect_replay_published_pair() {
    let f = fixture().await;
    worker(&f, Some(json!({"provider":"codex","selection":{"kind":"explicit","value":{"model":"private-a","effort":"high"}}}))).await;
    f.daemon
        .repo
        .settings_upsert(
            "worker_model_defaults_codex",
            r#"{"model":"private-b","effort":"low"}"#,
        )
        .await
        .unwrap();
    f.daemon.resume_cached_threads().await;
    let requests = f.requests.lock().unwrap().clone();
    let resume = requests
        .iter()
        .find(|r| r["method"] == "thread/resume")
        .unwrap();
    assert_eq!(resume["params"]["config"]["model"], "private-a");
    assert_eq!(resume["params"]["config"]["model_reasoning_effort"], "high");
    // Hot takeover only restores the subscription: it must not replace loaded choices.
    {
        let mut core = f.daemon.core.lock().await;
        let SupervisorState::Running { replay, .. } = &mut core.state else {
            panic!("running fixture")
        };
        *replay = ConnectionReplay::Pending(ReplayPlan::new(ResumeMode::HotTakeover));
    }
    f.daemon.resume_cached_threads().await;
    let requests = f.requests.lock().unwrap();
    let resume = requests
        .iter()
        .rfind(|r| r["method"] == "thread/resume")
        .unwrap();
    assert!(resume["params"].get("config").is_none());
}

#[tokio::test]
async fn worker_model_defaults_corrupt_snapshot_never_resumes_with_current_defaults() {
    let f = fixture().await;
    worker(&f, Some(Value::Null)).await;
    f.daemon
        .repo
        .settings_upsert(
            "worker_model_defaults_codex",
            r#"{"model":"private-b","effort":"low"}"#,
        )
        .await
        .unwrap();
    f.daemon.resume_cached_threads().await;
    assert!(
        !f.requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["method"] == "thread/resume")
    );
}

#[tokio::test]
async fn worker_model_defaults_legacy_resume_uses_provider_history_not_new_settings() {
    let f = fixture().await;
    let card = worker(&f, None).await;
    f.daemon
        .repo
        .settings_upsert(
            "worker_model_defaults_codex",
            r#"{"model":"private-b","effort":"low"}"#,
        )
        .await
        .unwrap();
    let warnings = Warnings::default();
    warnings.run(f.daemon.resume_cached_threads()).await;
    warnings.assert_legacy(&card, calm_types::worker_model::WorkerModelProvider::Codex);
    let requests = f.requests.lock().unwrap();
    let resume = requests
        .iter()
        .find(|r| r["method"] == "thread/resume")
        .unwrap();
    assert!(resume["params"]["config"].get("model").is_none());
    assert!(
        resume["params"]["config"]
            .get("model_reasoning_effort")
            .is_none()
    );
}

#[tokio::test]
async fn worker_model_defaults_interactive_card_cold_replay_is_not_legacy_task_worker() {
    use crate::operation::codex_adapter::CodexAdapter;
    let f = fixture().await;
    let (seed, _) = seed_cold_owner(&f).await;
    let track = f
        .daemon
        .repo
        .card_get(&seed)
        .await
        .unwrap()
        .unwrap()
        .track_id;
    f.daemon.thread_cache.remove("cold-thread");
    let events = crate::event::EventBus::new();
    let adapter = CodexAdapter::new(
        f.daemon.repo.clone(),
        Arc::new(crate::state::CodexClient::new_stub()),
        f.daemon.clone(),
        Arc::new(
            crate::pending_codex_threads::PendingThreadStartRegistry::new(
                f.daemon.repo.clone(),
                events,
            ),
        ),
        Arc::new(tokio::sync::Mutex::new(())),
        crate::card_role_cache::CardRoleCache::new(),
        crate::track_area_cache::TrackAreaCache::new(),
    );
    let payload = json!({"actor":crate::ids::ActorId::User,"request":{
        "track_id":track,"cwd":f._root.path(),"theme":crate::routes::theme::RequestTheme::default_dark()
    }});
    adapter.validate(&payload).await.unwrap();
    let ops = SqlxOperationRepo::new(f.repo.db().clone());
    ops.insert_operation(
        "codex-create",
        OperationKey {
            operation_key: crate::model::new_id(),
            idempotency_key: None,
            payload_hash: "interactive".into(),
        },
        payload,
    )
    .await
    .unwrap();
    let pending = ops.claim_drive_batch(1).await.unwrap().remove(0);
    let (op, _) = ops
        .prepare_tx_and_advance(&pending, &adapter, None)
        .await
        .unwrap()
        .committed()
        .unwrap();
    let output = op.tx_output.unwrap();
    let card = output.data["card_id"].as_str().unwrap().to_owned();
    assert_eq!(
        f.daemon.repo.card_role_get(&card).await.unwrap(),
        Some(CardRole::Worker)
    );
    assert!(
        output.result["payload"]
            .get("worker_model_selection")
            .is_none()
    );
    // A provider-reported thread is now available for this normal interactive card.
    let session = f
        .daemon
        .repo
        .session_projection_active_for_card(&card)
        .await
        .unwrap()
        .unwrap();
    f.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE worker_sessions SET thread_id='interactive-thread' WHERE id=?")
                .bind(&session.id),
        )
        .await
        .unwrap();
    f.daemon
        .thread_cache
        .insert("interactive-thread".into(), card.clone());
    let warnings = Warnings::default();
    warnings.run(f.daemon.resume_cached_threads()).await;
    warnings.assert_no_legacy(&card);
    let requests = f.requests.lock().unwrap();
    let resume = requests
        .iter()
        .find(|r| r["method"] == "thread/resume" && r["params"]["threadId"] == "interactive-thread")
        .expect("interactive cold replay still resumes");
    assert!(resume["params"]["config"].get("model").is_none());
    assert!(
        resume["params"]["config"]
            .get("model_reasoning_effort")
            .is_none()
    );
}

#[tokio::test]
async fn worker_model_defaults_provider_refusal_has_no_fallback_actual_model_is_reported() {
    let f = fixture().await;
    let card = worker(&f, Some(json!({"provider":"codex","selection":{"kind":"explicit","value":{"model":"private-a","effort":"high"}}}))).await;
    // No automatic cold pass is needed for this new-thread boundary exercise.
    {
        let mut core = f.daemon.core.lock().await;
        let SupervisorState::Running { replay, .. } = &mut core.state else {
            panic!("running fixture")
        };
        *replay = ConnectionReplay::Complete;
    }
    f.answers.lock().unwrap().insert(
        "thread/start".into(),
        json!({"error":{"code":-32602,"message":"unknown model"}}),
    );
    let refused = f
        .daemon
        .thread_start_mint_mcp_shell(
            &card,
            "/tmp".into(),
            None,
            f._root.path().join("mcp.sock"),
            "token".into(),
        )
        .await;
    assert!(
        matches!(refused, Err(CalmError::CodexRefused(_))),
        "{refused:?}"
    );
    {
        let requests = f.requests.lock().unwrap();
        let starts: Vec<_> = requests
            .iter()
            .filter(|r| r["method"] == "thread/start")
            .collect();
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0]["params"]["config"]["model"], "private-a");
    }
    f.daemon
        .repo
        .settings_upsert(
            "worker_model_defaults_codex",
            r#"{"model":"new-default","effort":"low"}"#,
        )
        .await
        .unwrap();
    f.answers.lock().unwrap().insert(
        "thread/start".into(),
        json!({"result":{"thread":{"id":"new-thread"},"model":"provider-reported"}}),
    );
    let minted = f
        .daemon
        .thread_start_mint_mcp_shell(
            &card,
            "/tmp".into(),
            None,
            f._root.path().join("mcp.sock"),
            "token".into(),
        )
        .await
        .unwrap();
    assert_eq!(minted.resolved_model.as_deref(), Some("provider-reported"));
    let requests = f.requests.lock().unwrap();
    let last = requests
        .iter()
        .rfind(|r| r["method"] == "thread/start")
        .unwrap();
    assert_eq!(last["params"]["config"]["model"], "private-a");
    assert_eq!(last["params"]["config"]["model_reasoning_effort"], "high");
}
