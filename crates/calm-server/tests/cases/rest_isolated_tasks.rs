//! Real authenticated REST boundaries; provider execution is never started here.
use super::*;
use calm_server::isolated_codex::config::{Backend, IsolatedCodexConfig};
use calm_server::model::{TaskStatus, TrackLifecycle};

fn fake_backend(root: &std::path::Path) -> Arc<Backend> {
    let config = root.join("provider.toml");
    let auth = root.join("auth.json");
    std::fs::write(&config, "model = \"fake\"\n").unwrap();
    std::fs::write(&auth, r#"{"tokens":{"access_token":"FAKE"}}"#).unwrap();
    let backend = Backend::new(IsolatedCodexConfig {
        workspace_root: root.join("workspaces"),
        private_root: root.join("private"),
        runtime_root: root.join("runtime"),
        runtime_helper: "/bin/true".into(),
        runtime_bwrap: "/bin/true".into(),
        sandbox_bwrap: "/bin/true".into(),
        codex_binary: "/bin/true".into(),
        code_mode_host_binary: "/bin/true".into(),
        mcp_shim: "/bin/true".into(),
        provider_config: config,
        provider_auth: auth,
        provider_environment: Default::default(),
        connect_timeout_ms: 1000,
        request_timeout_ms: 1000,
        task_timeout_ms: 1000,
    })
    .unwrap();
    Arc::new(backend)
}

fn configured(state: AppState, root: &std::path::Path) -> AppState {
    let state = state.with_isolated_codex_backend(fake_backend(root));
    // This suite owns authoring and report admission. Existing execution tests
    // own provider dispatch; keep that independent background actor out of assertions.
    state.dispatcher.abort_event_listener_for_test();
    state.dispatcher.semaphore().close();
    state
}

async fn request(
    app: &axum::Router,
    uri: &str,
    cookie: &str,
    actor: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(if body.is_some() { "POST" } else { "GET" })
                .uri(uri)
                .header("content-type", "application/json")
                .header(header::COOKIE, cookie)
                .header("X-Calm-Actor", actor)
                .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"raw":String::from_utf8_lossy(&bytes)})),
    )
}
fn intent(key: &str, revision: u64) -> Value {
    json!({"key":key,"goal":"Explain the answer.","ifDocRev":revision})
}
async fn count(boot: &Boot, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(boot.repo.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn start_is_user_only_and_unavailable_backend_authors_nothing() {
    let boot = boot().await;
    let app = app(boot.state.clone(), boot.auth_state.clone());
    let cookie = login(&app).await;
    let uri = format!("/api/tracks/{}/isolated-tasks", boot.track_id);
    assert_eq!(
        request(&app, &uri, "", "user", Some(intent("a", 0)))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    for actor in ["ai:codex", "ai:claude", "ai:planner", "ai:other"] {
        assert_eq!(
            request(&app, &uri, &cookie, actor, Some(intent("a", 0)))
                .await
                .0,
            StatusCode::FORBIDDEN,
            "{actor}"
        );
    }
    let (status, body) = request(&app, &uri, &cookie, "user", Some(intent("a", 0))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("No task was created")
    );
    assert!(!body.to_string().contains("--isolated-codex-config"));
    assert_eq!(count(&boot, "tasks").await, 0);
    assert_eq!(count(&boot, "events").await, 0);
    assert_eq!(
        boot.repo
            .track_get(boot.track_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        TrackLifecycle::Draft
    );
}

#[tokio::test]
async fn start_atomically_promotes_draft_and_duplicate_cas_cannot_add_execution() {
    let boot = boot().await;
    let root = tempfile::tempdir().unwrap();
    let app = app(
        configured(boot.state.clone(), root.path()),
        boot.auth_state.clone(),
    );
    let cookie = login(&app).await;
    let uri = format!("/api/tracks/{}/isolated-tasks", boot.track_id);
    let (a, b) = tokio::join!(
        request(
            &app,
            &uri,
            &cookie,
            "user",
            Some(intent("independent-nonce", 0))
        ),
        request(
            &app,
            &uri,
            &cookie,
            "user",
            Some(intent("independent-nonce", 0))
        )
    );
    let (receipt, conflict) = if a.0 == StatusCode::OK {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(receipt.0, StatusCode::OK, "{receipt:?}");
    assert_eq!(conflict.0, StatusCode::CONFLICT, "{conflict:?}");
    assert_eq!(receipt.1["taskKey"], "independent-nonce");
    assert_eq!(receipt.1["docRev"], 1);
    assert!(receipt.1["blockId"].as_str().unwrap().starts_with("b_"));
    assert_eq!(
        boot.repo
            .track_get(boot.track_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        TrackLifecycle::Planning
    );
    let tasks = boot
        .repo
        .tasks_by_track(boot.track_id.as_str())
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1);
    let task = &tasks[0];
    assert_eq!(task.declared_by, "user");
    assert_eq!(task.status, TaskStatus::Pending);
    assert!(calm_server::isolated_codex::selected(task).unwrap());
    assert!(task.gate_json.is_none());
    assert_eq!(task.depends_on_json, "[]");
    assert_eq!(
        calm_server::scheduler::build_worker_payload(task)
            .unwrap()
            .0,
        "codex-isolated-worker"
    );
    let events = boot.repo.events_since(0, 100).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|(_, _, _, e)| matches!(
                e,
                Event::TrackReportEdited {
                    author: EditAuthor::User,
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|(_, _, _, e)| matches!(
                e,
                Event::TrackLifecycleChanged {
                    from: TrackLifecycle::Draft,
                    to: TrackLifecycle::Planning,
                    ..
                }
            ))
            .count(),
        1
    );
    let before = count(&boot, "events").await;
    for body in [
        intent("independent-nonce", 0),
        intent("independent-nonce", 1),
        intent("another", 0),
    ] {
        assert_eq!(
            request(&app, &uri, &cookie, "user", Some(body)).await.0,
            StatusCode::CONFLICT
        );
    }
    assert_eq!(count(&boot, "events").await, before);
    assert_eq!(count(&boot, "task_attempt_allocations").await, 1);
    let history = format!(
        "/api/tracks/{}/tasks/independent-nonce/attempts",
        boot.track_id
    );
    let (status, view) = request(&app, &history, &cookie, "user", None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["current"]["attempt_id"], task.id);
}

#[tokio::test]
async fn invalid_start_rolls_back_draft_and_ordinary_edit_does_not_start_track() {
    let boot = boot().await;
    let root = tempfile::tempdir().unwrap();
    let app = app(
        configured(boot.state.clone(), root.path()),
        boot.auth_state.clone(),
    );
    let cookie = login(&app).await;
    let uri = format!("/api/tracks/{}/isolated-tasks", boot.track_id);
    for body in [
        intent(&"x".repeat(65), 0),
        intent("UPPER", 0),
        json!({"key":"a","goal":" \n ","ifDocRev":0}),
        json!({"key":"a","goal":"ok","ifDocRev":0,"context":{}}),
    ] {
        let (status, body) = request(&app, &uri, &cookie, "user", Some(body)).await;
        assert!(status.is_client_error(), "{status} {body}");
    }
    assert_eq!(
        request(&app, &uri, &cookie, "user", Some(intent("a", 1)))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(count(&boot, "events").await, 0);
    assert_eq!(
        boot.repo
            .track_get(boot.track_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        TrackLifecycle::Draft
    );
    let edit = format!("/api/tracks/{}/report/blocks", boot.track_id);
    let (status, body) = request(
        &app,
        &edit,
        &cookie,
        "user",
        Some(json!({"kind":"prose","markdown":"Draft notes","ifDocRev":0})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        boot.repo
            .track_get(boot.track_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        TrackLifecycle::Draft
    );
    let (status, body) = request(
        &app,
        &uri,
        &cookie,
        "user",
        Some(intent(&"a".repeat(64), 1)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn unsupported_lifecycle_cannot_author_an_inert_task() {
    let boot = boot().await;
    let root = tempfile::tempdir().unwrap();
    let app = app(
        configured(boot.state.clone(), root.path()),
        boot.auth_state.clone(),
    );
    let cookie = login(&app).await;
    let uri = format!("/api/tracks/{}/isolated-tasks", boot.track_id);
    for state in ["blocked", "done", "canceled", "failed"] {
        sqlx::query("UPDATE tracks SET lifecycle=?1 WHERE id=?2")
            .bind(state)
            .bind(boot.track_id.as_str())
            .execute(boot.repo.pool())
            .await
            .unwrap();
        let (status, body) = request(&app, &uri, &cookie, "user", Some(intent("a", 0))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{state} {body}");
        assert_eq!(count(&boot, "tasks").await, 0);
        assert_eq!(count(&boot, "events").await, 0);
    }
    sqlx::query("UPDATE tracks SET lifecycle='draft',archived_at=1 WHERE id=?1")
        .bind(boot.track_id.as_str())
        .execute(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        request(&app, &uri, &cookie, "user", Some(intent("a", 0)))
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn start_write_failure_rolls_back_promotion_task_projection_and_events() {
    let boot = boot().await;
    let root = tempfile::tempdir().unwrap();
    let app = app(
        configured(boot.state.clone(), root.path()),
        boot.auth_state.clone(),
    );
    let cookie = login(&app).await;
    // Fail after the report/Track/task effects but before the event batch commits.
    sqlx::query("CREATE TRIGGER reject_start_event BEFORE INSERT ON events WHEN NEW.kind='track.report_edited' BEGIN SELECT RAISE(ABORT,'forced start event failure'); END")
        .execute(boot.repo.pool()).await.unwrap();
    let uri = format!("/api/tracks/{}/isolated-tasks", boot.track_id);
    let (status, _) = request(&app, &uri, &cookie, "user", Some(intent("atomic", 0))).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        boot.repo
            .track_get(boot.track_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        TrackLifecycle::Draft
    );
    for table in ["tasks", "task_attempt_allocations", "events"] {
        assert_eq!(count(&boot, table).await, 0, "{table} must roll back");
    }
    let (_, _, payload) = calm_server::track_report::resolve_report_for_track(
        boot.repo.as_ref(),
        boot.track_id.as_str(),
    )
    .await
    .unwrap();
    assert_eq!(payload.doc_rev, 0);
    sqlx::query("DROP TRIGGER reject_start_event")
        .execute(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        request(&app, &uri, &cookie, "user", Some(intent("atomic", 0)))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn start_rejects_unreleased_declaration_and_retired_allocation_keys() {
    let boot = boot().await;
    let root = tempfile::tempdir().unwrap();
    let app = app(
        configured(boot.state.clone(), root.path()),
        boot.auth_state.clone(),
    );
    let cookie = login(&app).await;
    let edit = format!("/api/tracks/{}/report/blocks", boot.track_id);
    let (status, body) = request(&app, &edit, &cookie, "user", Some(json!({
        "kind":"task", "payload":{"key":"reserved","kind":"codex","goal":"Original goal", "ready":false,"declared_by":"user"}, "ifDocRev":0
    }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(count(&boot, "task_attempt_allocations").await, 0);
    // An allocation can outlive its pending projection and source declaration.
    sqlx::query("INSERT INTO task_attempt_allocations(attempt_id,track_id,key,generation,origin_json,created_at_ms) VALUES('old-attempt',?1,'retired',1,'{\"kind\":\"initial\"}',1)")
        .bind(boot.track_id.as_str()).execute(boot.repo.pool()).await.unwrap();
    let before = count(&boot, "events").await;
    let start = format!("/api/tracks/{}/isolated-tasks", boot.track_id);
    for key in ["reserved", "retired"] {
        assert_eq!(
            request(&app, &start, &cookie, "user", Some(intent(key, 1)))
                .await
                .0,
            StatusCode::CONFLICT,
            "{key}"
        );
    }
    assert_eq!(count(&boot, "events").await, before);
    assert_eq!(
        boot.repo
            .track_get(boot.track_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        TrackLifecycle::Draft
    );
}

#[path = "rest_isolated_task_report.rs"]
mod reports;
