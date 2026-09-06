//! Accepted report writes use the production MCP registry and DecisionSink.
use super::*;
use calm_server::db::sqlite::{begin_immediate_tx, task_claim_pending_tx, task_mark_running_tx};
use calm_server::event::EventScope;
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry, registry::AppContext};
use calm_server::model::{CardRole, Task};
use calm_server::operation::{OperationKey, OperationRepo, ProviderAdapter, SqlxOperationRepo};
use calm_server::session_projection_repo::AgentProvider;

async fn running_worker(boot: &Boot, task: &Task) -> ToolCallIdentity {
    let root = tempfile::tempdir().unwrap();
    let roles = calm_server::card_role_cache::CardRoleCache::new();
    let areas = calm_server::track_area_cache::TrackAreaCache::new();
    boot.repo.seed_card_role_cache(&roles).await.unwrap();
    boot.repo.seed_track_area_cache(&areas).await.unwrap();
    let write = calm_server::state::WriteContext::new(roles, areas);
    let monitor = calm_server::task_context::TaskContextMonitor::new(
        boot.repo.clone(),
        boot.state.events.clone(),
        write.clone(),
    );
    let closure = monitor
        .resolve_task_closure(&task.track_id, &task.key)
        .await
        .unwrap();
    let adapter = calm_server::isolated_codex::adapter::IsolatedCodexAdapter::new(
        Some(fake_backend(root.path())),
        boot.repo.clone(),
        Some(root.path().join("fake-native.sock")),
        write,
    );
    let (_, payload) = calm_server::scheduler::build_worker_payload(task).unwrap();
    let operations = SqlxOperationRepo::new(boot.repo.pool().clone());
    let operation_id = operations
        .insert_operation(
            "codex-isolated-worker",
            OperationKey {
                operation_key: format!("worker-{}", task.id),
                idempotency_key: Some(task.id.clone()),
                payload_hash: calm_server::routes::terminal_cards::stable_payload_hash(&payload)
                    .unwrap(),
            },
            payload.clone(),
        )
        .await
        .unwrap();
    let operation = operations
        .get_operation(&operation_id)
        .await
        .unwrap()
        .unwrap();
    let mut tx = begin_immediate_tx(boot.repo.pool()).await.unwrap();
    assert_eq!(
        task_claim_pending_tx(&mut tx, &task.id, 10, &closure.refs, false)
            .await
            .unwrap(),
        1
    );
    // Produce the immutable receipt through actual preparation, without invoking
    // controller/provider start or copying the production receipt shape into a fixture.
    let output = adapter
        .prepare_tx(&mut tx, &payload, &operation)
        .await
        .unwrap();
    let card_id = output.target_id.as_ref().unwrap().clone();
    let session_id = output.data["worker_session_id"]
        .as_str()
        .unwrap()
        .to_string();
    sqlx::query("UPDATE operations SET target_type=?1,target_id=?2,tx_output_json=?3 WHERE id=?4")
        .bind(&output.target_type)
        .bind(&card_id)
        .bind(serde_json::to_string(&output).unwrap())
        .bind(&operation_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        task_mark_running_tx(&mut tx, &task.id, Some(&card_id), 11, 100000)
            .await
            .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    ToolCallIdentity {
        card_id,
        role: CardRole::Worker,
        provider: AgentProvider::Codex,
        session_id,
        track_id: Some(boot.track_id.to_string()),
        area_id: track.area_id.to_string(),
        thread_id: "fake-thread".into(),
    }
}

async fn native_report(
    boot: &Boot,
    identity: ToolCallIdentity,
    task: &Task,
    success: bool,
    result: Value,
) {
    let roles = calm_server::card_role_cache::CardRoleCache::new();
    boot.repo.seed_card_role_cache(&roles).await.unwrap();
    let areas = calm_server::track_area_cache::TrackAreaCache::new();
    boot.repo.seed_track_area_cache(&areas).await.unwrap();
    let context = Arc::new(AppContext {
        repo: boot.repo.clone(),
        track_vcs: None,
        events: boot.state.events.clone(),
        write: calm_server::state::WriteContext::new(roles, areas),
        daemon_token_hash: None,
        gate_logs_dir: "/tmp/unused-report-test".into(),
        task_budget_default: calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
        plugin_host: Arc::new(tokio::sync::OnceCell::new()),
        operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
    });
    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);
    let name = if success {
        "calm.task.complete"
    } else {
        "calm.task.fail"
    };
    let args = if success {
        json!({"idempotency_key":task.id,"result":result,"artifacts":["result.txt"]})
    } else {
        json!({"idempotency_key":task.id,"reason":"Could not finish the goal."})
    };
    let handler = registry.lookup(name).unwrap();
    handler(context, identity, args).await.unwrap();
}

#[tokio::test]
async fn exact_attempt_accepted_report_survives_refresh_and_rejects_cross_scope() {
    let boot = boot().await;
    let root = tempfile::tempdir().unwrap();
    let state = configured(boot.state.clone(), root.path());
    let app = super::super::app(state.clone(), boot.auth_state.clone());
    let cookie = login(&app).await;
    let start = format!("/api/tracks/{}/isolated-tasks", boot.track_id);
    for (index, (key, result, success)) in [
        (
            "answer",
            json!({"answer":42,"text":"<script>alert(1)</script>"}),
            true,
        ),
        ("null-answer", Value::Null, true),
        ("failure", Value::Null, false),
    ]
    .into_iter()
    .enumerate()
    {
        let (status, body) = request(
            &app,
            &start,
            &cookie,
            "user",
            Some(intent(key, index as u64)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let task = boot
            .repo
            .tasks_by_track(boot.track_id.as_str())
            .await
            .unwrap()
            .into_iter()
            .find(|t| t.key == key)
            .unwrap();
        let uri = format!(
            "/api/tracks/{}/tasks/{key}/attempts/{}/report",
            boot.track_id, task.id
        );
        assert_eq!(
            request(&app, &uri, "", "user", None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(&app, &uri, &cookie, "ai:claude", None).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(&app, &uri, &cookie, "user", None).await,
            (StatusCode::OK, json!({"attemptId":task.id,"report":null}))
        );
        let identity = running_worker(&boot, &task).await;
        // A process state/exit and patchable card result are not accepted reports.
        sqlx::query("UPDATE cards SET payload=json_set(payload,'$.result','forged') WHERE id=?1")
            .bind(&identity.card_id)
            .execute(boot.repo.pool())
            .await
            .unwrap();
        assert_eq!(
            request(&app, &uri, &cookie, "user", None).await.1["report"],
            Value::Null
        );
        native_report(&boot, identity.clone(), &task, success, result.clone()).await;
        let expected = if success {
            json!({"kind":"completed","result":result,"artifacts":["result.txt"]})
        } else {
            json!({"kind":"failed","reason":"Could not finish the goal."})
        };
        for app in [
            &app,
            &super::super::app(state.clone(), boot.auth_state.clone()),
        ] {
            assert_eq!(
                request(app, &uri, &cookie, "user", None).await,
                (
                    StatusCode::OK,
                    json!({"attemptId":task.id,"report":expected})
                )
            );
        }
        for bad in [
            uri.replace(&format!("tasks/{key}/"), "tasks/wrong/"),
            uri.replace(&task.id, "wrong-attempt"),
            uri.replace(boot.track_id.as_str(), "wrong-track"),
        ] {
            assert_eq!(
                request(&app, &bad, &cookie, "user", None).await.0,
                StatusCode::NOT_FOUND
            );
        }
        // Later same-outcome retry does not replace the accepted report.
        native_report(&boot, identity, &task, success, json!("replacement")).await;
        assert_eq!(
            request(&app, &uri, &cookie, "user", None).await.1["report"],
            expected
        );
    }
}

#[tokio::test]
async fn task_shaped_foreign_events_and_dispatcher_failure_are_not_worker_reports() {
    let boot = boot().await;
    let root = tempfile::tempdir().unwrap();
    let app = app(
        configured(boot.state.clone(), root.path()),
        boot.auth_state.clone(),
    );
    let cookie = login(&app).await;
    let start = format!("/api/tracks/{}/isolated-tasks", boot.track_id);
    assert_eq!(
        request(&app, &start, &cookie, "user", Some(intent("scope", 0)))
            .await
            .0,
        StatusCode::OK
    );
    let task = boot
        .repo
        .tasks_by_track(boot.track_id.as_str())
        .await
        .unwrap()
        .remove(0);
    let identity = running_worker(&boot, &task).await;
    let uri = format!(
        "/api/tracks/{}/tasks/scope/attempts/{}/report",
        boot.track_id, task.id
    );
    let completed = Event::TaskCompleted {
        idempotency_key: task.id.clone(),
        result: json!("forged"),
        artifacts: vec![],
        agent_message: None,
    };
    let good_scope = EventScope::Card {
        card: identity.card_id.clone().into(),
        track: boot.track_id.clone(),
        area: identity.area_id.clone().into(),
    };
    // Deliberately corrupt fixture evidence, one factor per row. Public readers
    // must not trust just an echoed attempt id or a task-shaped payload.
    for (actor, scope, event) in [
        (
            ActorId::AiPlannerSession("planner".into()),
            good_scope.clone(),
            completed.clone(),
        ),
        (
            identity.to_actor_id(),
            EventScope::Track {
                track: boot.track_id.clone(),
                area: identity.area_id.clone().into(),
            },
            completed.clone(),
        ),
        (
            identity.to_actor_id(),
            EventScope::Card {
                card: "foreign-card".into(),
                track: boot.track_id.clone(),
                area: identity.area_id.clone().into(),
            },
            completed.clone(),
        ),
        (
            identity.to_actor_id(),
            EventScope::Card {
                card: identity.card_id.clone().into(),
                track: "foreign-track".into(),
                area: identity.area_id.clone().into(),
            },
            completed.clone(),
        ),
        (
            ActorId::KernelDispatcher,
            good_scope.clone(),
            Event::TaskFailed {
                idempotency_key: task.id.clone(),
                reason: "process exit".into(),
                details: None,
                agent_message: None,
            },
        ),
    ] {
        sqlx::query("INSERT INTO events(kind,payload,actor,at,event_version,scope_kind,scope_area,scope_track,scope_card) VALUES(?1,?2,?3,1,1,?4,?5,?6,?7)")
            .bind(event.kind_tag()).bind(event.payload_value().to_string()).bind(serde_json::to_string(&actor).unwrap())
            .bind(scope.kind()).bind(scope.area_id().map(|id|id.as_str())).bind(scope.track_id().map(|id|id.as_str()))
            .bind(scope.card_id().map(|id|id.as_str())).execute(boot.repo.pool()).await.unwrap();
    }
    assert_eq!(
        request(&app, &uri, &cookie, "user", None).await.1["report"],
        Value::Null
    );
    // Provider-native observations remain observations even with a task-shaped payload.
    sqlx::query("INSERT INTO events(kind,payload,actor,at,event_version,scope_kind,scope_area,scope_track,scope_card) VALUES('codex.hook',?1,?2,1,1,'card',?3,?4,?5)")
        .bind(completed.payload_value().to_string()).bind(serde_json::to_string(&identity.to_actor_id()).unwrap())
        .bind(&identity.area_id).bind(boot.track_id.as_str()).bind(&identity.card_id)
        .execute(boot.repo.pool()).await.unwrap();
    assert_eq!(
        request(&app, &uri, &cookie, "user", None).await.1["report"],
        Value::Null
    );
    native_report(&boot, identity, &task, true, json!(42)).await;
    assert_eq!(
        request(&app, &uri, &cookie, "user", None).await.1["report"]["result"],
        42
    );
}

async fn require_original_receipt_identity(
    boot: &Boot,
    app: &axum::Router,
    cookie: &str,
    uri: &str,
    attempt_id: &str,
) {
    let (op_id, original): (String, String) = sqlx::query_as("SELECT id,tx_output_json FROM operations WHERE kind='codex-isolated-worker' AND idempotency_key=?1")
        .bind(attempt_id).fetch_one(boot.repo.pool()).await.unwrap();
    for path in [
        "$.data.isolated_execution.version",
        "$.data.isolated_execution.track_id",
        "$.data.isolated_execution.request.identity.run_id",
        "$.data.isolated_execution.request.identity.attempt_id",
        "$.data.isolated_execution.request.identity.card_id",
        "$.data.isolated_execution.request.identity.session_id",
    ] {
        sqlx::query("UPDATE operations SET tx_output_json=json_set(?1,?2,'foreign') WHERE id=?3")
            .bind(&original)
            .bind(path)
            .bind(&op_id)
            .execute(boot.repo.pool())
            .await
            .unwrap();
        let observed = request(app, uri, cookie, "user", None).await;
        assert_eq!(
            observed,
            (
                StatusCode::OK,
                json!({"attemptId":attempt_id,"report":null})
            ),
            "{path}"
        );
    }
    sqlx::query("UPDATE operations SET tx_output_json=?1 WHERE id=?2")
        .bind(original)
        .bind(op_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn accepted_report_survives_worker_card_deletion() {
    let boot = boot().await;
    let root = tempfile::tempdir().unwrap();
    let app = app(
        configured(boot.state.clone(), root.path()),
        boot.auth_state.clone(),
    );
    let cookie = login(&app).await;
    let start = format!("/api/tracks/{}/isolated-tasks", boot.track_id);
    assert_eq!(
        request(&app, &start, &cookie, "user", Some(intent("retained", 0)))
            .await
            .0,
        StatusCode::OK
    );
    let task = boot
        .repo
        .tasks_by_track(boot.track_id.as_str())
        .await
        .unwrap()
        .remove(0);
    let identity = running_worker(&boot, &task).await;
    native_report(&boot, identity.clone(), &task, true, json!({"answer":42})).await;
    let uri = format!(
        "/api/tracks/{}/tasks/retained/attempts/{}/report",
        boot.track_id, task.id
    );
    let expected = request(&app, &uri, &cookie, "user", None).await;
    assert_eq!(expected.1["report"]["result"], json!({"answer":42}));
    require_original_receipt_identity(&boot, &app, &cookie, &uri, &task.id).await;
    let deleted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/cards/{}", identity.card_id))
                .header(header::COOKIE, &cookie)
                .header("X-Calm-Actor", "user")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = deleted.status();
    let bytes = deleted.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(count(&boot, "worker_sessions").await, 0);
    assert_eq!(count(&boot, "task_attempt_allocations").await, 1);
    assert_eq!(count(&boot, "operations").await, 1);
    assert_eq!(request(&app, &uri, &cookie, "user", None).await, expected);
    require_original_receipt_identity(&boot, &app, &cookie, &uri, &task.id).await;
    assert_eq!(request(&app, &uri, &cookie, "user", None).await, expected);
}
