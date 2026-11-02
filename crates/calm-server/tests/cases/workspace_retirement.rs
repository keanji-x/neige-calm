//! Logical retirement uses real close/delete routes; cleanup must not run online.
use super::*;

#[tokio::test]
async fn retirement_close_defers_unconfined_cleanup_and_keeps_primary_reopenable() {
    let f = Fixture::new().await;
    f.close().await;
    assert!(
        f.clean.exists(),
        "unconfined cleanup must defer before deleting"
    );
    assert_eq!(f.clean_attempts.load(Ordering::SeqCst), 0);
    assert_eq!(f.dirty_attempts.load(Ordering::SeqCst), 0);
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT root_path,cleanup_reason FROM workspace_resources WHERE retired_at_ms IS NOT NULL",
    )
    .fetch_all(f.b.repo.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|(_, reason)| reason == "unknown_location"));
    let (status, body) =
        f.b.as_user(
            "PATCH",
            &format!("/api/tracks/{}", f.track),
            json!({"closed":false}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(f.kernel.exists());
    f.b.shutdown_harnesses().await;
}

#[tokio::test]
async fn retirement_delete_retains_primary_and_extra_without_successful_removal_receipt() {
    let f = Fixture::new().await;
    let (status, body) = f.b.delete_track(&f.track).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(
        f.kernel.exists(),
        "primary physical cleanup is deferred too"
    );
    assert!(f.clean.exists());
    assert!(f.dirty.exists());
    assert!(git_ref_exists(
        &f.up.clone,
        &format!("refs/heads/neige/track-{}", f.track)
    ));
    assert_eq!(f.clean_attempts.load(Ordering::SeqCst), 0);
    let retired: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_resources WHERE retired_at_ms IS NOT NULL AND source_track_id=?")
        .bind(&f.track).fetch_one(f.b.repo.pool()).await.unwrap();
    assert_eq!(retired, 3);
    let removed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE kind='worktree.removed' AND scope_track=?",
    )
    .bind(&f.track)
    .fetch_one(f.b.repo.pool())
    .await
    .unwrap();
    assert_eq!(removed, 0, "retirement is not a physical removal receipt");
}

#[tokio::test]
async fn retirement_restart_reserves_paths_without_blocking_unrelated_terminals() {
    let mut f = Fixture::new().await;
    let other = crate::support::planner::historical_track(
        &f.b.state,
        json!({"area_id":f.b.area_id,"title":"unrelated","planner_provider":"codex",
            "theme":calm_server::routes::theme::RequestTheme::default_dark()}),
        None,
    )
    .await
    .unwrap();
    let other = other["id"].as_str().unwrap().to_owned();
    f.b.shutdown_harnesses().await;
    f.close().await;
    assert!(f.clean.exists());
    // A fresh runtime has no in-memory teardown registration.
    f.b.state = state_for_reclaim(&f.b, &f._execution.socket);
    f.b.state
        .operation_runtime
        .execution()
        .bind(Arc::new(f._execution.lifecycle.engine().unwrap().clone()));
    for typed in [false, true] {
        f.b.state = f.b.state.clone().with_terminal_execution(typed);
        f.b.app = crate::support::authenticated_router::application(f.b.state.clone()).await;
        let uri = format!("/api/tracks/{other}/terminal-cards");
        let (status, body) =
            f.b.as_user(
                "POST",
                &uri,
                json!({"cwd":f.unrecorded,"program":"true",
            "theme":calm_server::routes::theme::RequestTheme::default_dark()}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "unrelated startup: {body}");
        let (status, body) =
            f.b.as_user(
                "POST",
                &uri,
                json!({"cwd":f.clean,"program":"true",
            "theme":calm_server::routes::theme::RequestTheme::default_dark()}),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "retired startup must not wait: {body}"
        );
    }
    let (status, body) =
        f.b.as_user(
            "POST",
            "/api/tracks",
            json!({"area_id":f.b.area_id,
        "cwd":f.clean,"isolation":"none","attach_folder":true,"planner_provider":"codex",
        "theme":calm_server::routes::theme::RequestTheme::default_dark()}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "retired attach: {body}");
    f.b.shutdown_harnesses().await;
}

#[tokio::test]
async fn retirement_nested_extra_does_not_prevent_track_delete() {
    nested_retirement(false).await;
}

#[tokio::test]
async fn retirement_nested_extra_does_not_prevent_area_delete() {
    nested_retirement(true).await;
}

async fn nested_retirement(area: bool) {
    let f = Fixture::new().await;
    let nested = f.kernel.join("nested-extra");
    let card: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=? AND role='planner'")
            .bind(&f.track)
            .fetch_one(f.b.repo.pool())
            .await
            .unwrap();
    gitforge_worktree_add_for_test(
        &f.b.state.operation_runtime,
        &f.b.tmp.path().join("nested-logs"),
        &f.track,
        &card,
        &f.kernel,
        &nested,
        "nested-retired",
    )
    .await
    .unwrap();
    let (status, body) = if area {
        f.b.as_user(
            "DELETE",
            &format!("/api/areas/{}", f.b.area_id),
            Value::Null,
        )
        .await
    } else {
        f.b.delete_track(&f.track).await
    };
    assert_eq!(status, StatusCode::NO_CONTENT, "nested retirement: {body}");
    assert!(f.kernel.exists());
    assert!(nested.exists());
    assert!(f.clean.exists());
    let (state,): (String,) = sqlx::query_as("SELECT cleanup_reason FROM workspace_resources WHERE root_path=? AND retired_at_ms IS NOT NULL")
        .bind(nested.to_str().unwrap()).fetch_one(f.b.repo.pool()).await.unwrap();
    assert_eq!(state, "unknown_location");
}

#[tokio::test]
async fn retirement_default_terminal_cwd_cannot_reuse_another_owners_retired_root() {
    let mut f = Fixture::new().await;
    let (status,body)=f.b.as_user("POST","/api/tracks",json!({"area_id":f.b.area_id,"cwd":f.clean,"isolation":"none",
        "attach_folder":true,"planner_provider":"codex","theme":calm_server::routes::theme::RequestTheme::default_dark()})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let borrower = body["id"].as_str().unwrap().to_owned();
    f.b.shutdown_harnesses().await;
    f.close().await;
    for typed in [false, true] {
        f.b.state = f.b.state.clone().with_terminal_execution(typed);
        f.b.app = crate::support::authenticated_router::application(f.b.state.clone()).await;
        let (status, body) =
            f.b.as_user(
                "POST",
                &format!("/api/tracks/{borrower}/terminal-cards"),
                json!({"program":"true",
            "theme":calm_server::routes::theme::RequestTheme::default_dark()}),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "default cwd must be rejected without waiting: {body}"
        );
    }
    assert!(f.clean.exists());
}

#[tokio::test]
async fn retirement_closed_primary_preserves_ignored_output() {
    let f = Fixture::new().await;
    let exclude = f.up.clone.join(".git/info/exclude");
    let mut content = std::fs::read_to_string(&exclude).unwrap();
    content.push_str("\nretirement-build/\n");
    std::fs::write(exclude, content).unwrap();
    let output = f.kernel.join("retirement-build/output");
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    std::fs::write(&output, b"keep until cleanup is isolated").unwrap();
    f.close().await;
    f.tick(&mut HashSet::new()).await;
    assert_eq!(
        std::fs::read(output).unwrap(),
        b"keep until cleanup is isolated"
    );
}
