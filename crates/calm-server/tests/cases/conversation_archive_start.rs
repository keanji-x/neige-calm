use super::*;
use calm_truth::db::RepoRead;

async fn cancelled_start(action: &str, prepared: bool) {
    use axum::extract::FromRef;
    use std::time::Duration;
    let b = boot().await;
    let track = b.create_track("Pending start archive").await;
    let (status, row) = b
        .create_internal_conversation(&track, "initial", "original prompt")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{row}");
    let card = row["id"].as_str().unwrap().to_string();
    finish_turn(&b, &card).await;
    let original = b
        .repo
        .session_projection_active_for_card(&card)
        .await
        .unwrap()
        .unwrap();
    let history_path = format!("/api/cards/{card}/harness/items");
    let history = b.request("GET", &history_path, None, None).await.1;
    assert!(
        !history.as_array().unwrap().is_empty(),
        "existing transcript"
    );
    let route = calm_server::state::RouteState::from_ref(&b.state);
    let drive = route
        .operation_runtime
        .lock_for_track_delete_for_test()
        .await;
    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/cards/{card}/planner/{action}"))
        .body(Body::empty())
        .unwrap();
    let app = b.app.clone();
    let http = tokio::spawn(async move { app.oneshot(request).await.unwrap() });
    let op: String = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let op: Option<String> = sqlx::query_scalar(concat!(
                "SELECT id FROM operations WHERE kind='planner-harness-start' ",
                "AND phase='pending' AND json_extract(payload_json, ?) = ?"
            ))
            .bind(format!(
                "$.{}",
                calm_server::db::sqlite::PLANNER_START_CARD_KEY
            ))
            .bind(&card)
            .fetch_optional(b.repo.pool())
            .await
            .unwrap();
            if let Some(op) = op {
                break op;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("real HTTP start persisted Pending before drive");
    http.abort();
    assert!(http.await.unwrap_err().is_cancelled());
    drop(drive);
    if prepared {
        use calm_server::test_seams::{PausePoint, install_pause_for_test};
        let hook = PausePoint::default();
        install_pause_for_test("conversation-start-prepared", &card, hook.clone());
        let runtime = route.operation_runtime.clone();
        let driving = tokio::spawn(async move { runtime.drive().await.unwrap() });
        tokio::time::timeout(Duration::from_secs(5), hook.entered.notified())
            .await
            .unwrap();
        let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id=?")
            .bind(&op)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
        assert_eq!(
            phase, "app_server_interact",
            "real preparation committed before cancellation"
        );
        driving.abort();
        assert!(driving.await.unwrap_err().is_cancelled());
    }
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    let at_archive = b
        .repo
        .session_projection_active_for_card(&card)
        .await
        .unwrap()
        .unwrap();
    if !prepared {
        assert_eq!(at_archive.id, original.id);
    }
    let card_before_archive = b.repo.card_get(&card).await.unwrap().unwrap();
    let archived = archive(&b, &track, &card, true).await;
    let before = b
        .repo
        .session_projection_active_for_card(&card)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before.id, at_archive.id, "archive itself preserves runtime");
    assert_eq!(b.request("GET", &history_path, None, None).await.1, history);
    if archived.0 == StatusCode::NO_CONTENT {
        if prepared {
            let plan = route.operation_runtime.recover_on_boot().await.unwrap();
            route.operation_runtime.apply_recovery(plan).await.unwrap();
        } else {
            route.operation_runtime.drive().await.unwrap();
        }
        let after = b
            .repo
            .session_projection_active_for_card(&card)
            .await
            .unwrap()
            .unwrap();
        let after_history = b.request("GET", &history_path, None, None).await.1;
        if action == "reset" {
            assert_eq!(
                after_history, history,
                "successful archive must preserve reset transcript"
            );
        }
        assert_eq!(
            after.id, at_archive.id,
            "successful archive must fence persisted {action}"
        );
        assert_eq!(
            after.thread_id, at_archive.thread_id,
            "successful archive must fence provider start"
        );
        assert_eq!(after_history, history, "archived history retained");
    } else {
        assert_eq!(archived.0, StatusCode::CONFLICT, "{archived:?}");
        assert!(
            b.state
                .repo
                .conversation_archived_at(&card)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            serde_json::to_value(b.repo.card_get(&card).await.unwrap().unwrap()).unwrap(),
            serde_json::to_value(card_before_archive).unwrap(),
            "refused archive does not change metadata, timestamp or binding"
        );
        let after_events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
        assert_eq!(events, after_events, "refusal writes no event");
        // Boot recovery claims the real Pending row, then drives all remaining phases.
        let plan = route.operation_runtime.recover_on_boot().await.unwrap();
        route.operation_runtime.apply_recovery(plan).await.unwrap();
        let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id=?")
            .bind(&op)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
        assert_eq!(
            phase, "succeeded",
            "cancelled HTTP does not strand the operation"
        );
        assert_eq!(
            archive(&b, &track, &card, true).await.0,
            StatusCode::NO_CONTENT
        );
        let saved = b
            .repo
            .session_projection_active_for_card(&card)
            .await
            .unwrap()
            .unwrap();
        let provider_starts = b
            .state
            .shared_codex_appserver
            .started_thread_params_for_test()
            .len();
        let saved_history = b.request("GET", &history_path, None, None).await.1;
        route.operation_runtime.drive().await.unwrap();
        assert_eq!(
            b.repo
                .session_projection_active_for_card(&card)
                .await
                .unwrap()
                .unwrap()
                .id,
            saved.id
        );
        assert_eq!(
            b.request("GET", &history_path, None, None).await.1,
            saved_history
        );
        assert_eq!(
            b.state
                .shared_codex_appserver
                .started_thread_params_for_test()
                .len(),
            provider_starts,
            "no provider start after successful archive"
        );
    }
}

#[tokio::test]
async fn cancelled_pending_restart_cannot_cross_archive() {
    cancelled_start("restart", false).await;
}

#[tokio::test]
async fn cancelled_pending_reset_cannot_cross_archive() {
    cancelled_start("reset", false).await;
}

#[tokio::test]
async fn cancelled_prepared_restart_cannot_cross_archive() {
    cancelled_start("restart", true).await;
}

#[tokio::test]
async fn cancelled_prepared_reset_cannot_cross_archive() {
    cancelled_start("reset", true).await;
}
