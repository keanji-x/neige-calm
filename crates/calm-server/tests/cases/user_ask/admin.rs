use super::*;

async fn admin_boot() -> Boot {
    let boot = boot().await;
    sqlx::query("UPDATE tracks SET plugin_scope = NULL WHERE id = ?1")
        .bind(boot.track_id.as_str())
        .execute(boot.repo.sqlite_db().unwrap().writer_pool())
        .await
        .unwrap();
    boot
}

async fn request_admin(boot: &Boot) -> i64 {
    call_tool(
        boot,
        TOOL_USER_ASK,
        json!({"action":"grant_admin", "questions":[{"title":"Manage Tracks?"}]}),
    )
    .await
    .unwrap()["ask_id"]
        .as_i64()
        .unwrap()
}

#[tokio::test]
async fn grant_admin_click_grants_and_wakes() {
    let boot = admin_boot().await;
    let id = request_admin(&boot).await;
    let items = serde_json::to_value(activity_items(&boot).await).unwrap();
    assert_eq!(
        items[0]["questions"][0]["options"],
        json!(["Grant admin", "Not now"])
    );
    assert!(
        call_tool(
            &boot,
            TOOL_USER_ASK,
            json!({"action":"grant_admin", "questions":[{"title":"Again?"}]})
        )
        .await
        .is_err()
    );
    assert_eq!(
        answer(&boot, id, json!([{"option":0}])).await.0,
        StatusCode::NO_CONTENT
    );
    assert!(
        boot.repo
            .track_get(boot.track_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .admin_granted_at
            .is_some()
    );
    assert_eq!(rows(&boot, "track.updated").await.len(), 1);
    assert_eq!(rows(&boot, "track.updated").await[0].0, "User");
    assert_eq!(rows(&boot, "ask.answered").await.len(), 1);
    assert!(activity_items(&boot).await.is_empty());
    assert!(
        call_tool(
            &boot,
            TOOL_USER_ASK,
            json!({"action":"grant_admin", "questions":[{"title":"Again?"}]})
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn grant_admin_other_answers_grant_nothing() {
    for answers in [json!([{"option":1}]), json!([{"text":"Grant admin"}])] {
        let boot = admin_boot().await;
        let id = request_admin(&boot).await;
        assert_eq!(answer(&boot, id, answers).await.0, StatusCode::NO_CONTENT);
        assert!(
            boot.repo
                .track_get(boot.track_id.as_str())
                .await
                .unwrap()
                .unwrap()
                .admin_granted_at
                .is_none()
        );
        assert!(rows(&boot, "track.updated").await.is_empty());
        assert_eq!(rows(&boot, "ask.answered").await.len(), 1);
    }
}

#[tokio::test]
async fn grant_admin_answer_rechecks_eligibility() {
    for change in ["purpose = 'area-chat'", "admin_granted_at = 1"] {
        let boot = admin_boot().await;
        let id = request_admin(&boot).await;
        sqlx::query(&format!("UPDATE tracks SET {change} WHERE id = ?1"))
            .bind(boot.track_id.as_str())
            .execute(boot.repo.sqlite_db().unwrap().writer_pool())
            .await
            .unwrap();
        assert_eq!(
            answer(&boot, id, json!([{"option":0}])).await.0,
            StatusCode::BAD_REQUEST
        );
        assert!(rows(&boot, "ask.answered").await.is_empty());
        assert!(rows(&boot, "track.updated").await.is_empty());
        assert_eq!(activity_items(&boot).await.len(), 1);
    }
}

#[test]
fn admin_old_reopen_track_asks_still_deserialize() {
    assert_eq!(
        serde_json::from_value::<calm_server::event::AskAction>(
            json!({"kind":"reopen_track","closed_at":42})
        )
        .unwrap(),
        calm_server::event::AskAction::ReopenTrack { closed_at: 42 }
    );
}
