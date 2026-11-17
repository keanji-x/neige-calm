//! #2348: a Planner card's permission mode. A person sets it through its own route; no agent, and no
//! generic payload write, can change it; a payload replacement keeps whatever is stored, a corrupt
//! value included; `GET planner/run` reports it.

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::support::planner_queue_fixture::{Boot, boot_with, get, idle_snapshot, send_json};

fn mode_uri(card_id: &str) -> String {
    format!("/api/cards/{card_id}/planner/permission-mode")
}

async fn put_mode(boot: &Boot, actor: &str, body: Value) -> (StatusCode, Value) {
    send_json(
        boot.app.clone(),
        "PUT",
        mode_uri(boot.planner_card.id.as_str()),
        actor,
        body,
    )
    .await
}

async fn patch_payload(boot: &Boot, actor: &str, payload: Value) -> (StatusCode, Value) {
    send_json(
        boot.app.clone(),
        "PATCH",
        format!("/api/cards/{}", boot.planner_card.id),
        actor,
        json!({ "payload": payload }),
    )
    .await
}

/// The card's payload as it stands in the database.
async fn payload(boot: &Boot) -> Value {
    let (text,): (String,) = sqlx::query_as("SELECT payload FROM cards WHERE id = ?1")
        .bind(boot.planner_card.id.as_str())
        .fetch_one(boot.repo.pool())
        .await
        .expect("the planner card row");
    serde_json::from_str(&text).expect("a card payload is JSON")
}

async fn reported_mode(boot: &Boot) -> Value {
    let (status, body) = get(
        boot.app.clone(),
        format!("/api/cards/{}/planner/run", boot.planner_card.id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    body["permission_mode"].clone()
}

async fn store_raw(boot: &Boot, value: &str) {
    (boot.repo.db()).writer().execute(sqlx::query(
        "UPDATE cards SET payload = json_set(payload, '$.permission_mode', json(?1)) WHERE id = ?2",
    )
    .bind(value)
    .bind(boot.planner_card.id.as_str()))
    .await
    .expect("store a raw permission mode");
}

#[tokio::test]
async fn a_person_sets_the_mode_and_planner_run_reports_it() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    assert_eq!(reported_mode(&boot).await, json!("never"));

    let (status, body) = put_mode(&boot, "user", json!({"permission_mode": "ask"})).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        body,
        json!({"card_id": boot.planner_card.id.as_str(), "permission_mode": "ask"})
    );
    let stored = payload(&boot).await;
    assert_eq!(stored["permission_mode"], json!("ask"));
    assert_eq!(
        (&stored["planner_provider"], &stored["planner_harness"]),
        (&json!("codex"), &json!(true)),
        "the write replaces one key, not the payload: {stored}"
    );
    assert_eq!(reported_mode(&boot).await, json!("ask"));

    let events = boot.event_payloads("card.updated").await;
    let last = events.last().expect("a card.updated event was persisted");
    assert_eq!(last["payload"]["permission_mode"], json!("ask"), "{last}");
    let (actor,): (String,) = sqlx::query_as(
        "SELECT actor FROM events WHERE kind = 'card.updated' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(boot.repo.pool())
    .await
    .expect("the event row");
    let actor: Value = serde_json::from_str(&actor).expect("the actor column is JSON");
    assert_eq!(actor["kind"], json!("User"));

    let (status, body) = put_mode(&boot, "user", json!({"permission_mode": "full"})).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        body,
        json!({"card_id": boot.planner_card.id.as_str(), "permission_mode": "full"})
    );
    assert_eq!(payload(&boot).await["permission_mode"], json!("full"));
    assert_eq!(reported_mode(&boot).await, json!("full"));

    let (status, body) = put_mode(&boot, "user", json!({"permission_mode": "never"})).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(payload(&boot).await["permission_mode"], json!("never"));
}

/// The must-red fence: a Planner (or any agent) cannot raise its own permissions.
#[tokio::test]
async fn an_agent_actor_cannot_change_the_mode() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    for (actor, mode) in ["ai:codex", "ai:claude", "ai:planner"]
        .into_iter()
        .flat_map(|actor| ["ask", "full"].map(|mode| (actor, mode)))
    {
        let (status, body) = put_mode(&boot, actor, json!({"permission_mode": mode})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "actor={actor} body={body}");
        let message = body["error"].as_str().unwrap_or_default();
        assert!(
            message.contains("planner permission mode: only `X-Calm-Actor: user`"),
            "{message}"
        );
        assert!(message.ends_with("have no write path to it."), "{message}");
    }
    assert_eq!(payload(&boot).await["permission_mode"], json!("never"));
    assert!(
        boot.event_payloads("card.updated").await.is_empty(),
        "a refused write emits nothing"
    );
}

/// The generic card write refuses the key from any actor, so the dedicated route is the only way in.
#[tokio::test]
async fn a_generic_payload_write_cannot_change_the_mode() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let before = payload(&boot).await;
    // Everything else server-owned is left out, so the refusal is about this key alone.
    let mut echo = before.clone();
    let map = echo.as_object_mut().unwrap();
    for key in calm_server::validation::SERVER_OWNED_CARD_PAYLOAD_KEYS {
        map.remove(key);
    }
    for actor in ["ai:codex", "user"] {
        for value in [json!("ask"), json!("never"), Value::Null] {
            let mut forged = echo.clone();
            forged["permission_mode"] = value;
            let (status, body) = patch_payload(&boot, actor, forged).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "actor={actor} body={body}");
            let message = body["error"].as_str().unwrap_or_default();
            assert!(
                message.contains("`permission_mode` is server-owned"),
                "{message}"
            );
        }
    }
    assert_eq!(payload(&boot).await, before);
}

/// Replacing the payload without the key keeps the stored mode, and a corrupt stored value too:
/// only the dedicated writer changes it. A corrupt value is shown as `never`.
#[tokio::test]
async fn a_payload_replacement_keeps_the_stored_mode_even_a_corrupt_one() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let (status, body) = put_mode(&boot, "user", json!({"permission_mode": "ask"})).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    // A client echo of the payload: every server-owned key is refused on its own, so it omits them all.
    let mut replacement = payload(&boot).await;
    let map = replacement.as_object_mut().unwrap();
    for key in calm_server::validation::SERVER_OWNED_CARD_PAYLOAD_KEYS {
        map.remove(key);
    }
    replacement["note"] = json!("replaced");
    let (status, body) = patch_payload(&boot, "user", replacement.clone()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["payload"]["permission_mode"], json!("ask"), "{body}");
    let stored = payload(&boot).await;
    assert_eq!(stored["permission_mode"], json!("ask"));
    assert_eq!(stored["note"], json!("replaced"));

    for corrupt in [r#""yolo""#, "42", "null", r#"{"mode":"ask"}"#] {
        store_raw(&boot, corrupt).await;
        let expected: Value = serde_json::from_str(corrupt).unwrap();
        let (status, body) = patch_payload(&boot, "user", replacement.clone()).await;
        assert_eq!(status, StatusCode::OK, "corrupt={corrupt} body={body}");
        assert_eq!(
            payload(&boot).await["permission_mode"],
            expected,
            "the corruption survives a replacement"
        );
        assert_eq!(
            reported_mode(&boot).await,
            json!("never"),
            "an unreadable mode never reads as asking"
        );
    }

    let (status, body) = put_mode(&boot, "user", json!({"permission_mode": "ask"})).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        payload(&boot).await["permission_mode"],
        json!("ask"),
        "choosing a mode overwrites a corrupt one"
    );
}

#[tokio::test]
async fn the_body_is_one_mode_and_nothing_else() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let before = payload(&boot).await;
    for (body, expected) in [
        (json!({}), StatusCode::UNPROCESSABLE_ENTITY),
        (
            json!({"permission_mode": null}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"permission_mode": "yolo"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"permission_mode": "Full"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"permission_mode": {"full": null}}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"permission_mode": "Ask"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"permission_mode": {"ask": null}}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"permission_mode": "ask", "surprise": 1}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let (status, response) = put_mode(&boot, "user", body.clone()).await;
        assert_eq!(status, expected, "body={body} response={response}");
    }
    assert_eq!(payload(&boot).await, before);
}

#[tokio::test]
async fn a_card_nobody_has_is_a_404() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let (status, _) = send_json(
        boot.app.clone(),
        "PUT",
        mode_uri("card-that-does-not-exist"),
        "user",
        json!({"permission_mode": "ask"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A card is a Planner exactly when `PlannerBinding` binds it so: a non-codex kind or a
/// `planner_provider` that names no provider is not one.
#[tokio::test]
async fn a_card_that_is_not_a_planner_is_refused() {
    for retarget in [
        "UPDATE cards SET kind = 'terminal' WHERE id = ?1",
        "UPDATE cards SET payload = json_set(payload, '$.planner_provider', 'gpt') WHERE id = ?1",
    ] {
        let boot = boot_with(idle_snapshot(vec![])).await;
        (boot.repo.db())
            .writer()
            .execute(sqlx::query(retarget).bind(boot.planner_card.id.as_str()))
            .await
            .expect("retarget the card");
        let (status, body) = put_mode(&boot, "user", json!({"permission_mode": "ask"})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{retarget}: {body}");
        assert_eq!(payload(&boot).await["permission_mode"], json!("never"));
    }
}

async fn assistant(boot: &Boot, key: &str) -> String {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let response = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/tracks/{}/conversations",
                    boot.planner_card.track_id
                ))
                .header("content-type", "application/json")
                .header("x-calm-actor", "user")
                .header("idempotency-key", key)
                .body(Body::from(
                    json!({"text":"hello", "provider":"codex"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn assistant_agent_actor_cannot_change_the_mode() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let id = assistant(&boot, "assistant-actor").await;
    for actor in ["ai:codex", "ai:claude", "ai:planner"] {
        let (status, body) = send_json(
            boot.app.clone(),
            "PUT",
            mode_uri(&id),
            actor,
            json!({"permission_mode":"full"}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
    let (status, body) = get(
        boot.app.clone(),
        format!("/api/cards/{id}/planner/run?contract=conversation_v1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["permission_mode"], "never");
}

#[tokio::test]
async fn assistant_mode_is_independent_of_the_planner() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    assert_eq!(
        put_mode(&boot, "user", json!({"permission_mode":"full"}))
            .await
            .0,
        StatusCode::OK
    );
    let id = assistant(&boot, "assistant-independent").await;
    let (status, body) = get(
        boot.app.clone(),
        format!("/api/cards/{id}/planner/run?contract=conversation_v1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["permission_mode"], "never");
    for mode in ["ask", "full", "never"] {
        let (status, body) = send_json(
            boot.app.clone(),
            "PUT",
            mode_uri(&id),
            "user",
            json!({"permission_mode":mode}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, read) = get(
            boot.app.clone(),
            format!("/api/cards/{id}/planner/run?contract=conversation_v1"),
        )
        .await;
        assert_eq!(read["permission_mode"], mode);
        assert_eq!(reported_mode(&boot).await, "full");
    }
}

#[tokio::test]
async fn the_writer_checks_the_persisted_role_even_when_the_cache_is_stale() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    boot.repo.db().writer().execute(sqlx::query(
        "UPDATE cards SET role='worker',payload=json_set(payload,'$.harness_profile','plain_chat') WHERE id=?1",
    ).bind(boot.planner_card.id.as_str())).await.unwrap();
    // The cached Planner precheck still passes; the actual row must refuse in the transaction.
    let (status, body) = put_mode(&boot, "user", json!({"permission_mode":"full"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(payload(&boot).await["permission_mode"], "never");
    assert!(boot.event_payloads("card.updated").await.is_empty());
}

/// Real delegated cookie plus user attribution must never grant execution authority.
#[tokio::test]
async fn share_user_cannot_put_conversation_permission_modes() {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let boot = boot_with(idle_snapshot(vec![])).await;
    let assistant = assistant(&boot, "shared-permissions").await;
    let (status, link) = send_json(
        boot.app.clone(),
        "POST",
        format!("/api/tracks/{}/share-links", boot.planner_card.track_id),
        "user",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{link}");
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    let unlocked = boot
        .app
        .clone()
        .oneshot(
            Request::post(format!("/api/share-links/{id}/unlock"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"token":link["token"]}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unlocked.status(), StatusCode::OK);
    let cookie = unlocked.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    for card in [assistant.as_str(), boot.planner_card.id.as_str()] {
        let before: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id=?1")
            .bind(card)
            .fetch_one(boot.repo.pool())
            .await
            .unwrap();
        let events = boot.event_payloads("card.updated").await;
        let turns = boot.daemon.started_turns_for_test().len();
        let mut observations = Vec::new();
        for mode in ["never", "ask", "full"] {
            let response = boot
                .app
                .clone()
                .oneshot(
                    Request::put(format!("{}?share_link={id}", mode_uri(card)))
                        .header("cookie", &cookie)
                        .header("X-Calm-Actor", "user")
                        .header("content-type", "application/json")
                        .body(Body::from(json!({"permission_mode":mode}).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let body = response.into_body().collect().await.unwrap().to_bytes();
            observations.push((mode, status, body));
        }
        assert!(
            observations
                .iter()
                .all(|(_, status, _)| *status == StatusCode::FORBIDDEN),
            "card={card} {observations:?}"
        );
        let after: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id=?1")
            .bind(card)
            .fetch_one(boot.repo.pool())
            .await
            .unwrap();
        assert_eq!(after, before);
        assert_eq!(boot.event_payloads("card.updated").await, events);
        assert_eq!(boot.daemon.started_turns_for_test().len(), turns);
        let (status, body) = send_json(
            boot.app.clone(),
            "PUT",
            mode_uri(card),
            "user",
            json!({"permission_mode":"full"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
}
