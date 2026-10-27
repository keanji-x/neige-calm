//! Session-owned routing through production pool construction, without selection or writes.
use super::agent_backend_pool::{bind_fixture, pool};
use super::agent_backend_upgrade::multi_backend_cfg;
use super::*;

#[tokio::test]
async fn agent_backend_session_and_thread_route_to_fixture_binding() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let backends = pool(&multi_backend_cfg(&root), repo.clone()).await;
    let card = seed_card(&repo, 1).await;
    let session = seed_runtime_thread(&repo, &card, "bound-thread").await;
    assert!(Arc::ptr_eq(
        backends.for_session(&session).await.unwrap(),
        backends.default_only()
    ));
    bind_fixture(&repo, &session, "bound-thread", "other").await;
    assert!(Arc::ptr_eq(
        backends.for_session(&session).await.unwrap(),
        &backends.instances()[1]
    ));
    assert!(Arc::ptr_eq(
        backends.for_thread("bound-thread").await.unwrap(),
        &backends.instances()[1]
    ));
    // Routing only reads the existing binding. No new publication or selection occurs.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worker_session_backends")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}

use super::agent_backend_auth_router as authenticated_router;

#[tokio::test]
async fn agent_backend_planner_assistant_interactive_and_models_stay_default_under_pool() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use calm_server::{
        event::EventBus,
        plugin_host::{PluginHost, PluginRegistry},
        state::{AppState, CodexClient, DaemonClient, WriteContext},
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let backends = pool(&multi_backend_cfg(&root), repo.clone()).await;
    let mut cleanup = Vec::new();
    for instance in backends.instances() {
        instance.boot().await.unwrap();
        cleanup.push(super::agent_backend_upgrade::DaemonGroup(
            instance.status_snapshot().runtime.unwrap().pid,
        ));
    }
    let events = EventBus::new();
    let mcp = calm_server::mcp_server::McpServer::spawn(
        repo.clone(),
        events.clone(),
        WriteContext::new(
            repo.card_role_cache().clone(),
            repo.track_area_cache().clone(),
        ),
        root.path().join("mcp.sock"),
        root.path().join("shim"),
        calm_server::mcp_server::build_default_registry(),
        None,
        Arc::default(),
        Arc::default(),
        root.path().join("gates"),
    )
    .await
    .unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            root.path().join("plugins"),
            root.path().join("plugin-data"),
            vec![],
            events,
            WriteContext::new(
                repo.card_role_cache().clone(),
                repo.track_area_cache().clone(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(repo.card_role_cache().clone()),
        Some(repo.track_area_cache().clone()),
    )
    .with_codex_backends(backends.clone())
    .with_mcp_server(mcp);
    let mut app = authenticated_router::application(state.clone()).await;
    async fn call(
        app: &axum::Router,
        method: &str,
        uri: &str,
        body: Value,
        key: Option<&str>,
    ) -> (StatusCode, Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .header(
                        "idempotency-key",
                        key.map(str::to_owned).unwrap_or_else(new_id),
                    )
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    let area = repo
        .area_create(
            LEGACY_OWNER_USER_ID,
            NewArea {
                name: "product defaults".into(),
                color: "#123456".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let (status, track) = call(&app, "POST", "/api/tracks", json!({"area_id":area.id,"title":"pool defaults","planner_provider":"codex","theme":{"fg":[255,255,255],"bg":[0,0,0]}}), None).await;
    assert_eq!(status, StatusCode::CREATED, "{track}");
    let track_id = track["id"].as_str().unwrap();
    let assistant_key = new_id();
    let assistant_card = calm_server::conversation_keys::derive_track_conversation_card_id_for_test(
        track_id,
        &assistant_key,
    );
    let wake = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "canonical-human-wake-finished",
        &assistant_card,
        wake.clone(),
    );
    let (status, assistant) = call(
        &app,
        "POST",
        &format!("/api/tracks/{track_id}/conversations"),
        json!({"text":"hello"}),
        Some(&assistant_key),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{assistant}");
    // The 201 acknowledges durable acceptance; its planner start runs asynchronously.
    // Wait for that wake to finish before installing a runtime that cannot drive
    // planner-harness-start rows from the same operation journal.
    let settled = tokio::time::timeout(Duration::from_secs(15), wake.entered.notified()).await;
    wake.release.notify_one();
    settled.expect("assistant activation must settle before replacing the runtime");
    let phase: String = sqlx::query_scalar(
        "SELECT phase FROM operations WHERE kind = 'planner-harness-start' \
         AND json_extract(payload_json, '$.card_id') = ?1",
    )
    .bind(&assistant_card)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(phase, "succeeded");
    // Keep the real interactive native start/turn; replace only the PTY viewer.
    let adapter = Arc::new(
        calm_server::operation::codex_adapter::CodexAdapter::new_with_spawn_hook(
            repo.clone(),
            state.codex.clone(),
            state.shared_codex_appserver.clone(),
            state.pending_codex_threads.clone(),
            state.pending_codex_threads_spawn_serial.clone(),
            state.card_role_cache.clone(),
            state.track_area_cache.clone(),
            Arc::new(|_, _, _, _| {
                Box::pin(async { Ok(calm_server::operation::SpawnHandle::NoOp) })
            }),
        ),
    );
    let operations = Arc::new(calm_server::operation::SqlxOperationRepo::new(
        repo.db().clone(),
    ));
    let completion = calm_server::operation::OperationCompletionBus::new();
    let context = calm_server::operation::SpawnCtx::new_with_codex_backends(
        repo.clone(),
        operations.clone(),
        state.daemon.clone(),
        state.terminal_renderer.clone(),
        state.events.clone(),
        completion.clone(),
        backends.clone(),
    )
    .with_location_admission(state.location_admission_for_test());
    let runtime = Arc::new(calm_server::operation::OperationRuntime::new_unchecked(
        operations,
        vec![adapter],
        state.events.clone(),
        completion,
        context,
    ));
    app = authenticated_router::application(state.with_operation_runtime(runtime)).await;
    let (status, interactive) = call(
        &app,
        "POST",
        &format!("/api/tracks/{track_id}/codex-cards"),
        json!({"cwd":root.path(),"prompt":"hello","theme":{"fg":[255,255,255],"bg":[0,0,0]}}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{interactive}");
    let (status, models) = call(&app, "GET", "/api/models", Value::Null, None).await;
    assert_eq!(status, StatusCode::OK, "{models}");
    assert_eq!(models["source"], "live");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worker_session_backends")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let other_methods = std::fs::read_to_string(
        backends.instances()[1]
            .sock_path()
            .with_extension("methods"),
    )
    .unwrap();
    assert!(
        !other_methods.lines().any(|m| matches!(
            m,
            "thread/start" | "thread/resume" | "turn/start" | "model/list" | "config/read"
        )),
        "{other_methods}"
    );
    let default_methods = std::fs::read_to_string(
        backends
            .default_only()
            .sock_path()
            .with_extension("methods"),
    )
    .unwrap();
    assert!(default_methods.lines().any(|m| m == "thread/start"));
    assert!(default_methods.lines().any(|m| m == "model/list"));
}
