//! Durable removal reservations through production reclaim and HTTP admission.
use super::*;

#[tokio::test]
async fn c5_gitforge_reclaim_failed_wait_keeps_target_excluded_until_settlement() {
    durable_target_admission(false).await;
}

#[tokio::test]
async fn c5_gitforge_reclaim_restart_keeps_target_excluded_until_settlement() {
    durable_target_admission(true).await;
}

async fn durable_target_admission(restart: bool) {
    let mut f = Fixture::new().await;
    // Keep the real execution's settlement unavailable, forcing reclaim's wait to fail.
    // The executor may have finished deleting; that alone cannot release admission.
    sqlx::query("CREATE TRIGGER hold_reclaim_settlement BEFORE UPDATE OF settled_at_ms \
        ON workspace_executions WHEN json_extract(NEW.intent_json,'$.consumer')='workspace-reclaim' \
        BEGIN SELECT RAISE(FAIL,'settlement unavailable'); END")
        .execute(f.b.repo.pool()).await.unwrap();
    f.close().await;
    f.tick(&mut HashSet::new()).await;
    let id: String = sqlx::query_scalar(
        "SELECT execution_id FROM workspace_executions WHERE \
        json_extract(intent_json,'$.consumer')='workspace-reclaim' AND settled_at_ms IS NULL",
    )
    .fetch_one(f.b.repo.pool())
    .await
    .unwrap();
    let desired: String =
        sqlx::query_scalar("SELECT desired FROM workspace_executions WHERE execution_id=?")
            .bind(&id)
            .fetch_one(f.b.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        desired, "stop",
        "failed wait requested stop without settlement"
    );
    assert!(!f.clean.exists());
    // A fresh directory at the same location is still at risk until durable settlement.
    std::fs::create_dir(&f.clean).unwrap();
    run_git(&f.clean, ["init", "-q"]);
    if restart {
        f.b.state = state_for_reclaim(&f.b, &f._execution.socket);
        f.b.state
            .operation_runtime
            .execution()
            .bind(Arc::new(f._execution.lifecycle.engine().unwrap().clone()));
        f.b.app = crate::support::authenticated_router::application(f.b.state.clone()).await;
    }
    let (status, other) =
        f.b.as_user(
            "POST",
            "/api/tracks",
            json!({
                "area_id":f.b.area_id, "title":"destination owner", "planner_provider":"codex",
                "theme":calm_server::routes::theme::RequestTheme::default_dark()
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{other}");
    let other = other["id"].as_str().unwrap().to_owned();
    f.b.shutdown_harnesses().await;
    let before = f.b.repo.track_get(&other).await.unwrap().unwrap().workspace;
    // Collect every boundary result before asserting, so the red run diagnoses each consumer.
    let attach_result = attach(&f).await;
    let repoint_result = repoint(&f, &other).await;
    let mut terminals = Vec::new();
    for typed in [false, true] {
        f.b.state = f.b.state.clone().with_terminal_execution(typed);
        f.b.app = crate::support::authenticated_router::application(f.b.state.clone()).await;
        terminals.push(terminal(&f, &other).await);
    }
    assert_eq!(
        attach_result.0,
        StatusCode::BAD_REQUEST,
        "attach: {attach_result:?}; repoint: {repoint_result:?}; terminals: {terminals:?}"
    );
    assert_eq!(
        repoint_result.0,
        StatusCode::BAD_REQUEST,
        "{repoint_result:?}"
    );
    assert_eq!(
        f.b.repo.track_get(&other).await.unwrap().unwrap().workspace,
        before
    );
    for result in terminals {
        assert_eq!(result.0, StatusCode::CONFLICT, "{result:?}");
        assert_eq!(result.1["code"], "workspace_busy");
    }
    sqlx::query("DROP TRIGGER hold_reclaim_settlement")
        .execute(f.b.repo.pool())
        .await
        .unwrap();
    f.b.state
        .operation_runtime
        .execution()
        .drive([
            serde_json::from_value::<workspace_runtime::execution::ExecutionId>(json!(id)).unwrap(),
        ]);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let settled: bool = sqlx::query_scalar(
                "SELECT settled_at_ms IS NOT NULL FROM workspace_executions WHERE execution_id=?",
            )
            .bind(&id)
            .fetch_one(f.b.repo.pool())
            .await
            .unwrap();
            if settled {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let result = repoint(&f, &other).await;
    assert_eq!(result.0, StatusCode::OK, "{result:?}");
    let result = attach(&f).await;
    assert_eq!(result.0, StatusCode::CREATED, "{result:?}");
    for typed in [false, true] {
        f.b.state = f.b.state.clone().with_terminal_execution(typed);
        f.b.app = crate::support::authenticated_router::application(f.b.state.clone()).await;
        let result = terminal(&f, &other).await;
        assert_eq!(result.0, StatusCode::CREATED, "{result:?}");
    }
    f.b.shutdown_harnesses().await;
}

async fn attach(f: &Fixture) -> (StatusCode, Value) {
    f.b.as_user("POST", "/api/tracks", json!({
        "area_id":f.b.area_id, "cwd":f.clean, "isolation":"none", "attach_folder":true,
        "planner_provider":"codex", "theme":calm_server::routes::theme::RequestTheme::default_dark()
    })).await
}
async fn repoint(f: &Fixture, other: &str) -> (StatusCode, Value) {
    f.b.as_user(
        "PATCH",
        &format!("/api/tracks/{other}"),
        json!({
            "workspace":{"kind":"attached", "path":f.clean, "attach_folder":true}
        }),
    )
    .await
}
async fn terminal(f: &Fixture, other: &str) -> (StatusCode, Value) {
    f.b.as_user("POST", &format!("/api/tracks/{other}/terminal-cards"), json!({
        "cwd":f.clean, "program":"true", "theme":calm_server::routes::theme::RequestTheme::default_dark()
    })).await
}
