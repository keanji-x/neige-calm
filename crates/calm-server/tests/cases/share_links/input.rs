//! Scoped acceptance calls the ordinary production router and canonical message owner.
use super::fixtures::canonical;
use super::*;

async fn send(
    fx: &Fixture,
    link: &Value,
    path: &str,
    key: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let cookie = link["cookie"].as_str().unwrap();
    let mut req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .header(
            "X-Calm-Share-Link",
            link["link"]["delegation"]["id"].as_str().unwrap(),
        );
    if let Some(key) = key {
        req = req.header("Idempotency-Key", key);
    }
    let response = fx
        .app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (status, value)
}

#[tokio::test]
async fn guest_input_requires_text_only_key_and_active_source_bound_receipt() {
    let fx = Fixture::new().await;
    let (card, _) = canonical(&fx).await;
    let mut link = fx.create(true).await;
    link["cookie"] = super::tokens::unlock(&fx, &link).await.into();
    let token = link["token"].as_str().unwrap();
    let path = format!("/api/cards/{card}/planner/input");
    assert_eq!(
        send(&fx, &link, &path, None, json!({"text":"hello"}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    for (body, expected) in [
        (
            json!({"text":"hello","attachments":["550e8400-e29b-41d4-a716-446655440000.png"]}),
            StatusCode::FORBIDDEN,
        ),
        (
            json!({"text":"hello","replaces_turn":"turn"}),
            StatusCode::FORBIDDEN,
        ),
        (json!({"text":" "}), StatusCode::BAD_REQUEST),
    ] {
        assert_eq!(
            send(&fx, &link, &path, Some("guest-key"), body).await.0,
            expected
        );
    }
    let (status, ack) = send(
        &fx,
        &link,
        &path,
        Some("guest-key"),
        json!({"text":"guest speech"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ack}");
    assert_eq!(
        send(
            &fx,
            &link,
            &path,
            Some("guest-key"),
            json!({"text":"guest speech"})
        )
        .await,
        (status, ack.clone())
    );
    assert_eq!(
        send(
            &fx,
            &link,
            &path,
            Some("guest-key"),
            json!({"text":"changed"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut other = fx.create(true).await;
    other["cookie"] = super::tokens::unlock(&fx, &other).await.into();
    let other_path = path.clone();
    assert_eq!(
        send(
            &fx,
            &other,
            &other_path,
            Some("guest-key"),
            json!({"text":"guest speech"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let owner_replay = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&path)
                .header("content-type", "application/json")
                .header("cookie", &fx.cookie)
                .header("Idempotency-Key", "guest-key")
                .body(Body::from(json!({"text":"guest speech"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(owner_replay.status(), StatusCode::CONFLICT);
    let db = fx.state.raw_repo().sqlite_db().unwrap();
    let facts: (String,Option<String>,String) = sqlx::query_as("SELECT author_kind,author_user_id,author_share_link_id FROM conversation_messages WHERE id=?1")
        .bind(ack["entry_id"].as_str().unwrap()).fetch_one(db.read_pool()).await.unwrap();
    assert_eq!(
        facts,
        (
            "link_guest".into(),
            None,
            link["link"]["delegation"]["id"].as_str().unwrap().into()
        )
    );
    let (_, owner) = fx
        .request(
            "GET",
            &format!("/api/cards/{card}/harness/items?contract=conversation_v1"),
            Some(&fx.cookie),
            json!(null),
        )
        .await;
    assert!(owner.to_string().contains("guest speech"), "{owner}");
    assert!(owner.to_string().contains("Link guest"));
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    assert_eq!(
        fx.request(
            "DELETE",
            &format!("/api/share-links/{id}"),
            Some(&fx.cookie),
            json!(null)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        send(
            &fx,
            &link,
            &path,
            Some("guest-key"),
            json!({"text":"guest speech"})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let event_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE kind='harness.user_message.enqueued' AND scope_card=?1",
    )
    .bind(&card)
    .fetch_one(db.read_pool())
    .await
    .unwrap();
    assert_eq!(event_count, 1);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM conversation_messages WHERE author_share_link_id=?1",
    )
    .bind(id)
    .fetch_one(db.read_pool())
    .await
    .unwrap();
    assert_eq!(count, 1);
    let stored: Vec<String> =
        sqlx::query_scalar("SELECT payload_hash FROM planner_input_idempotency WHERE card_id=?1")
            .bind(&card)
            .fetch_all(db.read_pool())
            .await
            .unwrap();
    assert!(!format!("{stored:?}{ack}{owner}").contains(token));
}

#[tokio::test]
async fn guest_input_rejects_other_track_and_historical_card() {
    let fx = Fixture::new().await;
    let (card, _) = canonical(&fx).await;
    let mut link = fx.create(true).await;
    link["cookie"] = super::tokens::unlock(&fx, &link).await.into();
    let other = super::fixtures::track(&fx, &fx.area).await;
    let db = fx.state.raw_repo().sqlite_db().unwrap();
    db.writer()
        .execute(
            sqlx::query("UPDATE cards SET track_id=?2 WHERE id=?1")
                .bind(&card)
                .bind(&other),
        )
        .await
        .unwrap();
    let path = format!("/api/cards/{card}/planner/input");
    assert_eq!(
        send(&fx, &link, &path, Some("key"), json!({"text":"hello"}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let historical = fx
        .state
        .raw_repo()
        .card_create(calm_server::model::NewCard {
            track_id: fx.track.clone().into(),
            kind: "codex".into(),
            sort: None,
            title: None,
            payload: json!({"harness_profile":"assistant","assistant_provider":"codex"}),
        })
        .await
        .unwrap();
    let db = fx.state.raw_repo().sqlite_db().unwrap();
    db.writer()
        .execute(
            sqlx::query("UPDATE cards SET role='assistant' WHERE id=?1")
                .bind(historical.id.as_str()),
        )
        .await
        .unwrap();
    let path = format!("/api/cards/{}/planner/input", historical.id);
    assert_eq!(
        send(&fx, &link, &path, Some("key"), json!({"text":"hello"}))
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn guest_acceptance_rechecks_revocation_after_initial_resolution() {
    let fx = Fixture::new().await;
    let (card, _) = canonical(&fx).await;
    let mut link = fx.create(true).await;
    link["cookie"] = super::tokens::unlock(&fx, &link).await.into();
    let path = format!("/api/cards/{card}/planner/input");
    let pause = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "share-input-before-accept",
        &card,
        pause.clone(),
    );
    let cookie = link["cookie"].as_str().unwrap().to_owned();
    let link_id = link["link"]["delegation"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let app = fx.app.clone();
    let task = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .header("Idempotency-Key", "revocation-race")
                .header("cookie", cookie)
                .header("X-Calm-Share-Link", link_id)
                .body(Body::from(
                    json!({"text":"must stay unaccepted"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), pause.entered.notified())
        .await
        .unwrap();
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    assert_eq!(
        fx.request(
            "DELETE",
            &format!("/api/share-links/{id}"),
            Some(&fx.cookie),
            json!(null)
        )
        .await
        .0,
        StatusCode::OK
    );
    pause.release.notify_one();
    assert_eq!(task.await.unwrap().status(), StatusCode::UNAUTHORIZED);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM conversation_messages WHERE author_share_link_id=?1",
    )
    .bind(id)
    .fetch_one(fx.state.raw_repo().sqlite_db().unwrap().read_pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn guest_receipt_replays_after_current_session_pointer_is_cleared() {
    let fx = Fixture::new().await;
    let (card, _) = canonical(&fx).await;
    let mut link = fx.create(true).await;
    link["cookie"] = super::tokens::unlock(&fx, &link).await.into();
    let path = format!("/api/cards/{card}/planner/input");
    let body = json!({"text":"durable guest receipt"});
    let accepted = send(&fx, &link, &path, Some("cleared-session"), body.clone()).await;
    assert_eq!(accepted.0, StatusCode::OK, "{:?}", accepted.1);
    fx.state
        .raw_repo()
        .sqlite_db()
        .unwrap()
        .writer()
        .execute(sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1").bind(&card))
        .await
        .unwrap();
    assert_eq!(
        send(&fx, &link, &path, Some("cleared-session"), body).await,
        accepted
    );
    assert_eq!(
        send(
            &fx,
            &link,
            &path,
            Some("fresh-key"),
            json!({"text":"needs a carrier"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}
