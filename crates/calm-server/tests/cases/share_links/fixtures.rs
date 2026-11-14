use super::*;
pub(super) async fn track(fx: &Fixture, area: &str) -> String {
    fx.state
        .raw_repo()
        .track_create(NewTrack {
            area_id: area.into(),
            title: "future Track".into(),
            sort: None,
            cwd: "/private/workspace".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap()
        .id
        .to_string()
}

pub(super) async fn canonical(fx: &Fixture) -> (String, String) {
    let response = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tracks/{}/conversations", fx.track))
                .header("cookie", &fx.cookie)
                .header("content-type", "application/json")
                .header("Idempotency-Key", "projection")
                .body(Body::from(
                    json!({"provider":"codex","text":"Pending human"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let card = created["id"].as_str().unwrap().to_string();
    let worker: String = sqlx::query_scalar("SELECT id FROM worker_sessions WHERE card_id=?1")
        .bind(&card)
        .fetch_one(fx.state.raw_repo().sqlite_db().unwrap().read_pool())
        .await
        .unwrap();
    (card, worker)
}
