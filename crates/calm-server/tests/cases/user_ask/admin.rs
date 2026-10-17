use super::*;

async fn admin_boot() -> Boot {
    let boot = boot().await;
    sqlx::query("UPDATE tracks SET plugin_scope = NULL WHERE id = ?1")
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
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
    for change in ["plugin_scope = 'new-owner'", "admin_granted_at = 1"] {
        let boot = admin_boot().await;
        let id = request_admin(&boot).await;
        sqlx::query(&format!("UPDATE tracks SET {change} WHERE id = ?1"))
            .bind(boot.track_id.as_str())
            .execute(&boot.repo.sqlite_pool().unwrap())
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

#[tokio::test]
async fn grant_admin_request_and_click_explain_live_ineligibility() {
    for (shape, reason, action) in [
        (
            "plugin_scope='unloaded-owner'",
            "bound to plugin `unloaded-owner`",
            "without a plugin-bound template",
        ),
        (
            "plugin_scope='other-owner',template_id='known-template'",
            "bound to plugin `other-owner` (template `known-template`)",
            "without a plugin-bound template",
        ),
        ("parent_track_id", "child Tracks", "directly in this Area"),
        (
            "creator_track_id='historical-creator',creator_key='key'",
            "created by another Track",
            "yourself in this Area",
        ),
        (
            "purpose='area-chat'",
            "Area chat Tracks",
            "separate top-level Track",
        ),
        ("system", "system Area", "non-system Area"),
        ("managed", "managed identity", "without a managed identity"),
    ] {
        let boot = admin_boot().await;
        let id = request_admin(&boot).await;
        let pool = boot.repo.sqlite_pool().unwrap();
        let track_id = boot.track_id.as_str();
        match shape {
            "parent_track_id" => {
                sqlx::query(
                    "INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) \
                     SELECT 'parent',area_id,'parent',0,1,1 FROM tracks WHERE id=?1",
                )
                .bind(track_id)
                .execute(&pool)
                .await
                .unwrap();
                sqlx::query("UPDATE tracks SET parent_track_id='parent' WHERE id=?1")
                    .bind(track_id)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            "system" => {
                sqlx::query("UPDATE areas SET kind='system' WHERE id=(SELECT area_id FROM tracks WHERE id=?1)")
                    .bind(track_id).execute(&pool).await.unwrap();
            }
            "managed" => {
                sqlx::query("INSERT INTO managed_track_identities VALUES('owner','identity',?1,'area','UTC','standard',0)")
                    .bind(track_id).execute(&pool).await.unwrap();
            }
            _ => {
                sqlx::query(&format!("UPDATE tracks SET {shape} WHERE id=?1"))
                    .bind(track_id)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        }
        let before = boot.repo.track_get(track_id).await.unwrap().unwrap();
        let error = call_tool(
            &boot,
            TOOL_USER_ASK,
            json!({"action":"grant_admin", "questions":[{"title":"Manage Tracks?"}]}),
        )
        .await
        .unwrap_err();
        assert!(error.message.contains(reason), "{shape}: {error:?}");
        assert!(error.message.contains(action), "{shape}: {error:?}");
        let (status, body) = answer(&boot, id, json!([{"option":0}])).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{shape}: {body}");
        let message = body["error"].as_str().unwrap();
        assert!(message.contains(reason), "{shape}: {body}");
        assert!(message.contains(action), "{shape}: {body}");
        assert_eq!(rows(&boot, "ask.requested").await.len(), 1);
        assert!(rows(&boot, "ask.answered").await.is_empty());
        assert!(rows(&boot, "track.updated").await.is_empty());
        assert_eq!(activity_items(&boot).await.len(), 1);
        let after = boot.repo.track_get(track_id).await.unwrap().unwrap();
        assert!(after.admin_granted_at.is_none());
        assert_eq!(after.updated_at, before.updated_at);
    }
}
