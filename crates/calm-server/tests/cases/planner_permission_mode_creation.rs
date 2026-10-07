//! #2348: every production path that creates a Planner card mints `permission_mode: "never"`, and a
//! create cannot ask for another mode.

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::today_launchpad::{
    Boot, boot, count, create_area, create_child_track, create_track, ensure, post,
};

async fn stored_mode(b: &Boot, card_id: &str) -> Value {
    let payload: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id = ?1")
        .bind(card_id)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    serde_json::from_str::<Value>(&payload).unwrap()["permission_mode"].clone()
}

async fn planner_card_of(b: &Boot, track_id: &str) -> String {
    sqlx::query_scalar("SELECT id FROM cards WHERE track_id = ?1 AND role = 'planner'")
        .bind(track_id)
        .fetch_one(b.repo.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn every_planner_creation_path_mints_never() {
    let b = boot().await;

    let (status, launchpad) = ensure(b.app.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{launchpad}");
    let launchpad_planner = launchpad["planner_card_id"].as_str().unwrap();
    assert_eq!(stored_mode(&b, launchpad_planner).await, json!("never"));

    let area = create_area(&b, "Permissions").await;
    let track = create_track(
        &b,
        json!({
            "planner_provider": "codex",
            "area_id": area["id"],
            "title": "made by a person",
            "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
        }),
    )
    .await;
    let track_id = track["id"].as_str().unwrap();
    let planner = planner_card_of(&b, track_id).await;
    assert_eq!(stored_mode(&b, &planner).await, json!("never"));

    let child_track_id = create_child_track(&b, track_id).await;
    let child_planner = planner_card_of(&b, &child_track_id).await;
    assert_eq!(stored_mode(&b, &child_planner).await, json!("never"));
}

/// A create naming a mode is refused whole: nothing is minted, so it cannot yield an `ask` Planner.
#[tokio::test]
async fn a_create_cannot_ask_for_a_mode() {
    let b = boot().await;
    let area = create_area(&b, "Permissions").await;
    let tracks = "SELECT COUNT(*) FROM tracks";
    let cards = "SELECT COUNT(*) FROM cards";
    let before = (count(&b, tracks).await, count(&b, cards).await);
    for actor in [None, Some("ai:codex")] {
        let (status, body) = post(
            b.app.clone(),
            "/api/tracks",
            actor,
            Some(json!({
                "planner_provider": "codex",
                "area_id": area["id"],
                "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
                "permission_mode": "ask",
            })),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "actor={actor:?} body={body}"
        );
    }
    assert_eq!(
        (count(&b, tracks).await, count(&b, cards).await),
        before,
        "a refused create mints nothing"
    );
}
