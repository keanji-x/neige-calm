//! Anonymous acceptance calls the real public router and canonical message owner.
use super::conversations::canonical;
use super::*;

async fn send(fx: &Fixture, path: &str, key: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
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
    let link = fx.create(true).await;
    let token = link["token"].as_str().unwrap();
    let path = format!(
        "/api/share/{token}/tracks/{}/conversations/{card}/input",
        fx.track
    );
    assert_eq!(
        send(&fx, &path, None, json!({"text":"hello"})).await.0,
        StatusCode::BAD_REQUEST
    );
    for body in [
        json!({"text":"hello","attachments":[]}),
        json!({"text":"hello","actor":"user"}),
        json!({"text":" "}),
    ] {
        assert_eq!(
            send(&fx, &path, Some("guest-key"), body).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let (status, ack) = send(
        &fx,
        &path,
        Some("guest-key"),
        json!({"text":"guest speech"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ack}");
    assert_eq!(
        send(
            &fx,
            &path,
            Some("guest-key"),
            json!({"text":"guest speech"})
        )
        .await,
        (status, ack.clone())
    );
    assert_eq!(
        send(&fx, &path, Some("guest-key"), json!({"text":"changed"}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let other = fx.create(true).await;
    let other_path = path.replace(token, other["token"].as_str().unwrap());
    assert_eq!(
        send(
            &fx,
            &other_path,
            Some("guest-key"),
            json!({"text":"guest speech"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
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
            &path,
            Some("guest-key"),
            json!({"text":"guest speech"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
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
    let link = fx.create(true).await;
    let token = link["token"].as_str().unwrap();
    let other = super::reads::track(&fx, &fx.area).await;
    let path = format!("/api/share/{token}/tracks/{other}/conversations/{card}/input");
    assert_eq!(
        send(&fx, &path, Some("key"), json!({"text":"hello"}))
            .await
            .0,
        StatusCode::NOT_FOUND
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
    let path = format!(
        "/api/share/{token}/tracks/{}/conversations/{}/input",
        fx.track, historical.id
    );
    assert_eq!(
        send(&fx, &path, Some("key"), json!({"text":"hello"}))
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn guest_acceptance_rechecks_revocation_after_initial_resolution() {
    let fx = Fixture::new().await;
    let (card, _) = canonical(&fx).await;
    let link = fx.create(true).await;
    let path = format!(
        "/api/share/{}/tracks/{}/conversations/{card}/input",
        link["token"].as_str().unwrap(),
        fx.track
    );
    let pause = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "share-input-before-accept",
        &card,
        pause.clone(),
    );
    let app = fx.app.clone();
    let task = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .header("Idempotency-Key", "revocation-race")
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
    assert_eq!(task.await.unwrap().status(), StatusCode::NOT_FOUND);
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
async fn shared_list_prepends_canonical_planner_without_owner_title_fallback() {
    let fx = Fixture::new().await;
    let (card, _) = canonical(&fx).await;
    let db = fx.state.raw_repo().sqlite_db().unwrap();
    db.writer().execute(sqlx::query("UPDATE cards SET role='planner',payload=json_set(json_remove(payload,'$.assistant_provider','$.harness_profile'),'$.planner_harness',json('true'),'$.planner_provider','codex') WHERE id=?1").bind(&card)).await.unwrap();
    let link = fx.create(true).await;
    let (status, rows) = fx
        .request(
            "GET",
            &format!(
                "/api/share/{}/tracks/{}/conversations",
                link["token"].as_str().unwrap(),
                fx.track
            ),
            None,
            json!(null),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    assert_eq!(rows[0]["id"], card);
    assert_eq!(rows[0]["kind"], "shared-spec");
    assert!(rows[0]["title"].is_null());
    let (status, owner) = fx
        .request(
            "GET",
            &format!("/api/tracks/{}/conversations", fx.track),
            Some(&fx.cookie),
            json!(null),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{owner}");
    assert_eq!(owner, json!([]));
}
