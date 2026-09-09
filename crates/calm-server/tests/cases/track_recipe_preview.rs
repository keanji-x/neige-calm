//! Saved preview and real instantiation share the production compiler.
use super::*;

async fn activity_counts(boot: &Boot) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM tracks), (SELECT count(*) FROM cards), (SELECT count(*) FROM tasks), (SELECT count(*) FROM events)")
        .fetch_one(boot.sqlx_repo.pool()).await.unwrap()
}

#[tokio::test]
async fn saved_recipe_preview_is_native_read_only_and_matches_instantiation() {
    let boot = boot().await;
    let body = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fe/web/src/features/report/recipe/examples/portfolio.md"),
    )
    .unwrap();
    let recipe = create_recipe(boot.app.clone(), "Portfolio preview", &body).await;
    let counts = activity_counts(&boot).await;
    let url = format!(
        "/api/track-recipes/{}/preview?if_revision={}",
        recipe["id"].as_str().unwrap(),
        recipe["revision"]
    );
    let (status, response) = send(boot.app.clone(), "GET", &url, None).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["id"], recipe["id"]);
    assert_eq!(response["revision"], recipe["revision"]);
    let preview: TrackReportPayload = serde_json::from_value(response["payload"].clone()).unwrap();
    assert_eq!(preview.summary, recipe["title"]);
    assert_eq!(preview.body, recipe["body"]);
    assert_eq!(
        preview
            .blocks
            .as_ref()
            .unwrap()
            .iter()
            .filter(|block| block.kind == "layout")
            .count(),
        3
    );
    assert_eq!(
        activity_counts(&boot).await,
        counts,
        "preview must not create or write runtime state"
    );
    let (status, created) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(create_track_body(
            &boot.area_id,
            "preview-equality",
            json!({"recipe_id":recipe["id"]}),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let detail = track_detail(boot.app.clone(), created["id"].as_str().unwrap()).await;
    assert_eq!(detail["track"]["recipe_revision"], recipe["revision"]);
    let actual = report_payload(&detail);
    assert_eq!(actual.summary, preview.summary);
    assert_eq!(actual.body, preview.body);
    let contents = |payload: &TrackReportPayload| {
        payload
            .blocks
            .as_ref()
            .unwrap()
            .iter()
            .map(|block| (block.kind.clone(), block.payload.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(contents(&actual), contents(&preview));
}

#[tokio::test]
async fn saved_recipe_preview_refuses_missing_stale_and_invalid_rows_without_writes() {
    let boot = boot().await;
    let recipe = create_recipe(boot.app.clone(), "First title", "# First section\n").await;
    let id = recipe["id"].as_str().unwrap();
    let revision = recipe["revision"].as_i64().unwrap();
    let (status, updated) = send(
        boot.app.clone(),
        "PUT",
        &format!("/api/track-recipes/{id}"),
        Some(
            json!({"title": "Second title", "body": "# Second section\n", "if_revision": revision}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let counts = activity_counts(&boot).await;
    let (status, _) = send(
        boot.app.clone(),
        "GET",
        &format!("/api/track-recipes/{id}/preview?if_revision={revision}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, current) = send(
        boot.app.clone(),
        "GET",
        &format!("/api/track-recipes/{id}/preview"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{current}");
    assert_eq!(current["revision"], updated["revision"]);
    assert_eq!(current["payload"]["summary"], "Second title");
    assert_eq!(current["payload"]["body"], "# Second section\n");
    for body in [
        "```neige-block layout\n{}\n```",
        "```neige-block future-kind\n{}\n```",
        "```neige-block table\nnot-json\n```",
    ] {
        // Simulate an invalid persisted row, not an invalid create request.
        sqlx::query("UPDATE track_recipes SET body = ? WHERE id = ?")
            .bind(body)
            .bind(id)
            .execute(boot.sqlx_repo.pool())
            .await
            .unwrap();
        let (status, _) = send(
            boot.app.clone(),
            "GET",
            &format!("/api/track-recipes/{id}/preview"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    let (status, _) = send(
        boot.app.clone(),
        "GET",
        "/api/track-recipes/missing/preview",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(activity_counts(&boot).await, counts);
}

#[tokio::test]
async fn saved_recipe_preview_keeps_task_and_app_declarations_without_runtime_writes() {
    let boot = boot().await;
    let body = format!(
        "{}\n```neige-block app\n{}\n```\n",
        two_task_body(),
        json!({
            "src": "/preview-app-must-not-load", "title": "Saved app", "height": 240
        })
    );
    let recipe = create_recipe(boot.app.clone(), "Definitions", &body).await;
    let counts = activity_counts(&boot).await;
    let (status, response) = send(
        boot.app.clone(),
        "GET",
        &format!(
            "/api/track-recipes/{}/preview",
            recipe["id"].as_str().unwrap()
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let preview: TrackReportPayload = serde_json::from_value(response["payload"].clone()).unwrap();
    assert_eq!(task_blocks(&preview).len(), 2);
    assert!(
        preview
            .blocks
            .as_ref()
            .unwrap()
            .iter()
            .any(|block| block.kind == "app")
    );
    assert_eq!(
        activity_counts(&boot).await,
        counts,
        "saved task declarations cannot start runtime work"
    );
}
