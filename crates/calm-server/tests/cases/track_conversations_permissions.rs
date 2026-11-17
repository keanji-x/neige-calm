//! Creation fingerprints and malformed choices at the authenticated HTTP boundary.
use super::*;

async fn saved(boot: &Boot, card: &str) -> String {
    sqlx::query_scalar("SELECT json_extract(payload,'$.permission_mode') FROM cards WHERE id=?1")
        .bind(card)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn permission_replay_never_overwrites_a_later_user_choice() {
    let boot = boot().await;
    let track = boot.create_track("Permission replay").await;
    let uri = format!("/api/tracks/{track}/conversations");
    let body = json!({"provider":"codex","text":"Permission replay","permission_mode":"full"});
    let (status, created) = boot
        .request("POST", &uri, Some("permission-replay"), Some(body.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(saved(&boot, &id).await, "full");
    let (status, reply) = boot
        .request(
            "PUT",
            &format!("/api/cards/{id}/planner/permission-mode"),
            None,
            Some(json!({"permission_mode":"ask"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(
        boot.request("POST", &uri, Some("permission-replay"), Some(body.clone()))
            .await,
        (StatusCode::CREATED, created)
    );
    assert_eq!(saved(&boot, &id).await, "ask");
    for mode in ["never", "ask"] {
        let mut changed = body.clone();
        changed["permission_mode"] = json!(mode);
        let (status, reply) = boot
            .request("POST", &uri, Some("permission-replay"), Some(changed))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{reply}");
        assert_eq!(reply["code"], "idempotency_key_reused");
        assert_eq!(saved(&boot, &id).await, "ask");
    }
}

#[tokio::test]
async fn malformed_creation_choices_are_refused_before_mint() {
    let boot = boot().await;
    let track = boot.create_track("Invalid permissions").await;
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM cards")
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    for mode in [
        Value::Null,
        json!("Full"),
        json!({"full":null}),
        json!("fullaccess"),
        json!(42),
    ] {
        let (status, body) = boot
            .request(
                "POST",
                &format!("/api/tracks/{track}/conversations"),
                Some("bad-mode"),
                Some(json!({"provider":"codex","text":"hello","permission_mode":mode})),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        let (status,body) = boot.request("POST","/api/tracks",Some("bad-track"),Some(json!({"area_id":boot.area_id,"planner_provider":"codex","theme":{"fg":[255,255,255],"bg":[0,0,0]},"permission_mode":mode}))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cards")
            .fetch_one(boot.repo.pool())
            .await
            .unwrap(),
        before
    );
}

#[tokio::test]
async fn an_agent_cannot_select_even_never_at_assistant_creation() {
    let boot = boot().await;
    let track = boot.create_track("Actor permissions").await;
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM cards")
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    for mode in ["never", "ask", "full"] {
        let response = boot
            .app
            .clone()
            .oneshot(
                Request::post(format!("/api/tracks/{track}/conversations"))
                    .header("content-type", "application/json")
                    .header("X-Calm-Actor", "ai:codex")
                    .header("Idempotency-Key", "agent-mode")
                    .body(Body::from(
                        json!({"provider":"codex","text":"hello","permission_mode":mode})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cards")
            .fetch_one(boot.repo.pool())
            .await
            .unwrap(),
        before
    );
}
