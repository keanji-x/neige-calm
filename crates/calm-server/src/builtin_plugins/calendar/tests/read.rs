//! #2175-15: `GET /api/calendar/tasks/{id}` reads one task by id, a cancelled one included, so a client can read back
//! an update or cancel whose answer was lost without guessing a list window around it.
use super::*;
use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn get(app: &axum::Router, id: &str, actor: &str) -> (u16, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/calendar/tasks/{id}"))
                .header("x-calm-actor", actor)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn reads_one_task_by_id_including_a_cancelled_one() {
    let fx = Fixture::new().await;
    let app = fx.http_app();
    let created = store::create(&fx.ctx, human(), request()).await.unwrap();

    let (status, read) = get(&app, &created.id, "user").await;
    assert_eq!(status, 200, "{read}");
    assert_eq!(read, serde_json::to_value(&created).unwrap());

    let cancelled = store::update(
        &fx.ctx,
        human(),
        created.id.clone(),
        created.version,
        store::Change::Replace(Update {
            expected_version: created.version,
            task: draft(),
            cancelled: true,
        }),
    )
    .await
    .unwrap();
    assert!(
        store::list(&fx.ctx, &human(), window())
            .await
            .unwrap()
            .is_empty(),
        "the list leaves a cancelled task out"
    );
    let (status, read) = get(&app, &created.id, "user").await;
    assert_eq!(status, 200, "{read}");
    assert_eq!(read, serde_json::to_value(&cancelled).unwrap());
    assert_eq!(read["cancelled"], true);
}

#[tokio::test]
async fn reading_an_unknown_task_answers_404_and_an_agent_is_refused() {
    let fx = Fixture::new().await;
    let app = fx.http_app();
    let created = store::create(&fx.ctx, human(), request()).await.unwrap();

    let (status, body) = get(&app, "no-such-task", "user").await;
    assert_eq!((status, body["code"].as_str()), (404, Some("not_found")));
    let (status, _) = get(&app, &created.id, "ai:codex").await;
    assert_eq!(status, 403);

    fx.host.stop(PLUGIN_ID).await.unwrap();
    let (status, _) = get(&app, &created.id, "user").await;
    assert_eq!(status, 503);
}
