//! Public read affordances consume the same authenticated historical effect owner.
use super::*;

async fn run(app: &axum::Router, cookie: &str, card: &str) -> Value {
    let response = app.clone().oneshot(Request::get(format!(
        "/api/cards/{card}/planner/run?contract=conversation_v1"
    )).header(header::COOKIE, cookie).body(Body::empty()).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn historical_edit_capability_uses_current_private_source_and_actual_backend() {
    let fixture = Fixture::new().await;
    let (state, app, cookie, auth) = fixture.application_with_auth(true).await;
    let area = post(&app, &cookie, "/api/areas", "private-area",
        json!({"name":"Private historical editing","color":"#123456"})).await;
    let track = post(&app, &cookie, "/api/tracks", "historical-edit",
        json!({"area_id":area["id"],"planner_provider":"codex","title":"",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]}})).await;
    let (card, worker): (String, String) = sqlx::query_as(
        "SELECT id,session_id FROM cards WHERE track_id=?1 AND role='planner'"
    ).bind(track["id"].as_str().unwrap()).fetch_one(fixture.repo.pool()).await.unwrap();
    let owner = run(&app, &cookie, &card).await;
    assert_eq!(owner["history_replacement_supported"], true, "{owner}");
    let paired = format!("calm-session={}", auth.sessions.create(
        crate::auth::SessionIdentity::owner(&auth.config), crate::auth::SessionAuthority::PairedDevice));
    assert_eq!(run(&app, &paired, &card).await["history_replacement_supported"], true);
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.pool()).await.unwrap();
    crate::db::sqlite::area_membership::application_user_register(&mut tx, "alice", "Alice")
        .await.unwrap();
    sqlx::query("INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES(?1,'alice','member',0)")
        .bind(area["id"].as_str().unwrap()).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let alice = format!("calm-session={}", auth.sessions.create(
        crate::auth::SessionIdentity { user_id: "alice".into(), display_name: "Alice".into(), role: "member".into() },
        crate::auth::SessionAuthority::PairedDevice));
    assert_eq!(run(&app, &alice, &card).await["history_replacement_supported"], false,
        "read access and an actual capable backend do not grant historical private effects");
    let handle = state.harness.remove(&worker).unwrap();
    handle.shutdown().await.unwrap();
    assert_eq!(run(&app, &cookie, &card).await["history_replacement_supported"], false,
        "without the actual execution backend the reader cannot offer an edit");
}

#[tokio::test]
async fn historical_zero_limit_returns_an_empty_page_without_a_cursor() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application_with_historical_native(true).await;
    let area = post(&app, &cookie, "/api/areas", "empty-page-area",
        json!({"name":"Historical empty page","color":"#123456"})).await;
    let track = post(&app, &cookie, "/api/tracks", "empty-page-track",
        json!({"area_id":area["id"],"planner_provider":"codex","title":"",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]}})).await;
    let (card, worker): (String, String) = sqlx::query_as(
        "SELECT id,session_id FROM cards WHERE track_id=?1 AND role='planner'"
    ).bind(track["id"].as_str().unwrap()).fetch_one(fixture.repo.pool()).await.unwrap();
    for limit in [0, -1] {
        for negotiated in [true, false] {
            let uri = format!("/api/cards/{card}/harness/items?limit={limit}{}",
                if negotiated { "&contract=conversation_v1" } else { "" });
            let response = app.clone().oneshot(Request::get(uri)
                .header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let page: Value = serde_json::from_slice(&bytes).unwrap();
            if negotiated {
                assert_eq!(page["items"], json!([]));
                assert_eq!(page["next_cursor"], Value::Null);
            } else { assert_eq!(page, json!([])); }
        }
    }
    state.harness.remove(&worker).unwrap().shutdown().await.unwrap();
}
