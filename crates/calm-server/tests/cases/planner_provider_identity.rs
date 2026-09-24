//! #1791: `POST /api/tracks` names the Planner's backend; the Planner card carries it as the
//! server-owned, sticky `planner_provider`, and the session row persists it.

use super::*;

fn planner_card(detail: &Value) -> Value {
    detail["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["payload"]["planner_harness"] == true)
        .unwrap()
        .clone()
}

async fn patch_card(b: &Boot, card_id: &str, payload: Value) -> (StatusCode, Value) {
    let response = b
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/cards/{card_id}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "payload": payload }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn stored_provider(b: &Boot, card_id: &str) -> Value {
    let payload: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id = ?1")
        .bind(card_id)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    serde_json::from_str::<Value>(&payload).unwrap()["planner_provider"].clone()
}

async fn created_planner(b: &Boot, key: &str) -> (String, Value) {
    let (status, body) = b.create_track(Some(key), Some("hello planner")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let track_id = body["id"].as_str().unwrap().to_string();
    let planner = planner_card(&b.track_detail(&track_id).await);
    (track_id, planner)
}

#[tokio::test]
async fn a_codex_create_stamps_the_planner_card_and_its_session_row() {
    let b = boot().await;
    let (_, planner) = created_planner(&b, "provider-codex").await;
    let card_id = planner["id"].as_str().unwrap();
    assert_eq!(planner["payload"]["planner_provider"], "codex", "{planner}");
    let runtime = b.active_runtime_of_card(card_id).await;
    let identity: (String, String, String) =
        sqlx::query_as("SELECT provider, mode, contract FROM worker_sessions WHERE id = ?1")
            .bind(&runtime)
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        identity,
        ("codex".into(), "resumable".into(), "planner".into())
    );
    b.shutdown_harnesses().await;
}

#[tokio::test]
async fn a_claude_create_is_refused_before_anything_is_minted() {
    let b = boot().await;
    let before = (
        b.track_count().await,
        b.card_count().await,
        b.binding_count().await,
    );
    for (key, first_message) in [(None, None), (Some("claude-keyed"), Some("hello"))] {
        let mut body = json!({
            "planner_provider": "claude",
            "area_id": b.area_id,
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        });
        if let Some(text) = first_message {
            body["first_message"] = json!(text);
        }
        let (status, response) = b.post_create(key, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
        assert!(
            response["error"]
                .as_str()
                .is_some_and(|error| error.contains("planner_provider")),
            "{response}"
        );
    }
    assert_eq!(
        (
            b.track_count().await,
            b.card_count().await,
            b.binding_count().await
        ),
        before,
        "a refused create mints nothing"
    );
}

#[tokio::test]
async fn a_create_without_planner_provider_is_refused() {
    let b = boot().await;
    let (status, response) = b
        .post_create(
            None,
            json!({
                "area_id": b.area_id,
                "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
            }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    assert_eq!(b.track_count().await, 0);
}

#[tokio::test]
async fn the_planner_provider_is_server_owned_and_survives_omission() {
    let b = boot().await;
    let (_, planner) = created_planner(&b, "provider-sticky").await;
    let card_id = planner["id"].as_str().unwrap();
    for value in [json!("claude"), json!("codex"), Value::Null] {
        let mut forged = planner["payload"].clone();
        forged["planner_provider"] = value;
        let (status, response) = patch_card(&b, card_id, forged).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    }
    let mut replacement = planner["payload"].clone();
    replacement
        .as_object_mut()
        .unwrap()
        .remove("planner_provider");
    let (status, patched) = patch_card(&b, card_id, replacement).await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["payload"]["planner_provider"], "codex", "{patched}");
    assert_eq!(stored_provider(&b, card_id).await, "codex");
    b.shutdown_harnesses().await;
}

#[tokio::test]
async fn a_corrupt_planner_provider_survives_and_the_card_is_not_a_harness() {
    let b = boot().await;
    let (_, planner) = created_planner(&b, "provider-corrupt").await;
    let card_id = planner["id"].as_str().unwrap();
    b.shutdown_harnesses().await;
    sqlx::query(
        "UPDATE cards SET payload = json_set(payload, '$.planner_provider', 'gpt') WHERE id = ?1",
    )
    .bind(card_id)
    .execute(b.repo.pool())
    .await
    .unwrap();
    let mut replacement = planner["payload"].clone();
    replacement
        .as_object_mut()
        .unwrap()
        .remove("planner_provider");
    let (status, patched) = patch_card(&b, card_id, replacement).await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(
        stored_provider(&b, card_id).await,
        "gpt",
        "the corruption survives"
    );
    let (status, response) = b.send_planner_input(card_id, "are you there").await;
    assert!(status.is_client_error(), "{status} {response}");
}
