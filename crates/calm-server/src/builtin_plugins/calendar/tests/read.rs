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

#[tokio::test]
async fn calendar_list_hides_closed_and_missing_tracks_and_restores_reopened_entries() {
    let fx = Fixture::new().await;
    let app = fx.http_app();
    let owner = fx.identity(CardRole::Planner).await;
    let other = fx.identity(CardRole::Planner).await;
    let task = json!({"title":"Weekly review","description":"","schedule":{
        "kind":"weekly","weekdays":["fri"],"start":"09:00","end":"10:00",
        "timezone":"Asia/Shanghai","from":"2026-10-02"
    }});
    let owned = super::wake::create(&fx, &owner, "owned", task.clone()).await;
    let orphan = super::wake::create(&fx, &other, "orphan", task.clone()).await;
    let cancelled = super::wake::create(&fx, &owner, "cancelled", task).await;
    super::wake::rm(&fx, &owner, &cancelled).await;
    let human_entry = store::create(&fx.ctx, human(), request()).await.unwrap();
    fx.repo
        .track_delete(other.track_id.as_deref().unwrap())
        .await
        .unwrap();

    for closed in [false, true, false] {
        sqlx::query("UPDATE tracks SET closed_at = ? WHERE id = ?")
            .bind(closed.then_some(1_i64))
            .bind(owner.track_id.as_deref().unwrap())
            .execute(fx.repo.pool())
            .await
            .unwrap();
        let response = app.clone().oneshot(
            Request::builder()
                .uri("/api/calendar/tasks?from=2026-10-02&until=2026-10-03&timezone=Asia%2FShanghai")
                .header("x-calm-actor", "user")
                .body(Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(response.status(), 200);
        let listed: Vec<serde_json::Value> =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let mut ids: Vec<_> = listed
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_owned())
            .collect();
        ids.sort();
        let mut expected = vec![human_entry.id.clone()];
        if !closed {
            expected.push(owned.id.clone());
        }
        expected.sort();
        assert_eq!(ids, expected, "REST list with closed={closed}");

        let registry = crate::mcp_server::build_default_registry();
        let list = registry.lookup("plugin_calendar_ls").unwrap();
        let result = list(
            fx.ctx.clone(),
            owner.clone(),
            json!({
                "from":"2026-10-02","to":"2026-10-03","timezone":"Asia/Shanghai"
            }),
        )
        .await
        .unwrap();
        let result = serde_json::to_value(result).unwrap();
        let entries = result["structuredContent"]["entries"].as_array().unwrap();
        assert_eq!(
            entries.len(),
            usize::from(!closed),
            "tool list with closed={closed}"
        );
        if !closed {
            assert_eq!(entries[0]["entry_id"], owned.id);
        }

        for entry in [&owned, &orphan, &human_entry] {
            let (status, read) = get(&app, &entry.id, "user").await;
            assert_eq!(status, 200);
            assert_eq!(
                read,
                serde_json::to_value(entry).unwrap(),
                "listing must not mutate entries"
            );
        }
    }
}
