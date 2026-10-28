//! Summary requests use production boot, HTTP and the native protocol boundary.
use super::*;

async fn activity(app: &axum::Router, cookie: &str) -> String {
    let area = post(app, cookie, "/api/areas", "area", json!({"name":"Summary activity","color":"#123456"})).await;
    let track = post(app, cookie, "/api/tracks", "activity", json!({"area_id":area["id"],"title":"Activity","planner_provider":"codex","theme":{"fg":[255,255,255],"bg":[0,0,0]}})).await;
    let track = track["id"].as_str().unwrap();
    post(app, cookie, &format!("/api/tracks/{track}/report"), "report", json!({"ifDocRev":0,"summary":"One real report edit","body":"# Activity\n"})).await;
    track.into()
}

#[tokio::test]
async fn summary_post_accepts_original_system_bootstrap_and_frozen_body() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    activity(&app, &cookie).await;
    let summary = tokio::time::timeout(Duration::from_secs(5), post(&app, &cookie, "/api/today/summary", "same-http-key", json!(null))).await.expect("acceptance does not wait on a provider");
    let card = summary["card_id"].as_str().unwrap();
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_system_assistant_receipts WHERE card_id=?1").bind(card).fetch_one(fixture.repo.pool()).await.unwrap();
    assert_eq!(receipts, 2, "bootstrap and frozen summary use canonical System acceptance");
    shutdown(&fixture, &state).await;
}
