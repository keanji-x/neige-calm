use super::*;

async fn request(b: &Boot, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = b
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .header("Idempotency-Key", path)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

fn planner_card(detail: &Value) -> &Value {
    detail["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["payload"]["planner_harness"] == true)
        .unwrap()
}

async fn create(b: &Boot, template: &str) -> String {
    let (status, body) = b.post_create(Some("template-context"), json!({
        "area_id": b.area_id,
        "template_id": template,
        "first_message": "Investigate the latency regression without changing the repository.",
        "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
    })).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn template_startup_creates_no_placeholder_tasks() {
    let b = boot().await;
    let track = create(&b, "investigation").await;
    let (status, detail) = b.get_json(&format!("/api/tracks/{track}")).await;
    assert_eq!(status, StatusCode::OK);
    let blocks = report_payload(&detail)["blocks"].as_array().unwrap();
    assert!(
        blocks.iter().all(|block| block["kind"] != "task"),
        "a template supplies working instructions, not dormant task declarations: {blocks:?}"
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE track_id = ?1")
        .bind(&track)
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    b.shutdown_harnesses().await;
}

#[tokio::test]
async fn template_startup_injects_the_creation_snapshot_before_any_report_read() {
    let b = boot().await;
    let track = create(&b, "investigation").await;
    let (status, detail) = b.get_json(&format!("/api/tracks/{track}")).await;
    assert_eq!(status, StatusCode::OK);
    let planner = detail["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["payload"]["planner_harness"] == true)
        .unwrap();
    let snapshot = &planner["payload"]["template_context"];
    assert_eq!(
        snapshot["version"], 1,
        "creation persists its template context"
    );
    assert_eq!(snapshot["title"], "Investigation");
    let original = calm_server::templates::TemplateRoster::builtin()
        .get("investigation")
        .unwrap()
        .recipe();
    assert_eq!(snapshot["body"], original.body);
    let starts = b
        .state
        .shared_codex_appserver
        .started_thread_params_for_test();
    assert_eq!(starts.len(), 1);
    let instructions = starts[0].0.as_ref().unwrap();
    let injected: Value =
        serde_json::from_str(instructions.split_once("## Selected Template\n").unwrap().1).unwrap();
    assert_eq!(
        &injected, snapshot,
        "the actual thread/start carries the exact immutable snapshot"
    );
    assert!(
        b.started_turn_text("Investigate the latency regression")
            .await
            .contains("Investigate the latency regression")
    );
    b.shutdown_harnesses().await;
}

#[tokio::test]
async fn template_startup_recipe_snapshot_survives_source_edits_and_reset() {
    let b = boot().await;
    let table = json!({
        "columns": [{"key": "name", "label": "Name", "align": "left"}],
        "rows": [{"name": "preserved evidence"}], "caption": "Evidence",
    });
    let source = format!(
        "{}\n# Evidence\n\nKeep literal {{track_id}} and {{planner_wake_authors}}.\n\n{}",
        recipe_body(),
        calm_types::report_blocks::render_fence("table", &table)
    );
    let recipe = b.create_recipe("Original method", &source).await;
    let (_, original) = b.get_json(&format!("/api/track-recipes/{recipe}")).await;
    let (status, created) = b.post_create(Some("recipe-context"), json!({
        "area_id": b.area_id, "recipe_id": recipe, "first_message": "Carry out this investigation",
        "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
    })).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let track = created["id"].as_str().unwrap();
    let (_, detail) = b.get_json(&format!("/api/tracks/{track}")).await;
    let planner = planner_card(&detail);
    let card = planner["id"].as_str().unwrap();
    let snapshot = planner["payload"]["template_context"].clone();
    assert_eq!(snapshot["body"], original["body"]);
    let table_block = report_payload(&detail)["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["kind"] == "table")
        .expect("non-task data survives instantiation");
    assert_eq!(table_block["payload"], table);
    assert!(
        report_payload(&detail)["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|block| block["kind"] != "task")
    );
    assert!(
        report_payload(&detail)["body"]
            .as_str()
            .unwrap()
            .contains("Keep literal {track_id}")
    );
    let first = b
        .state
        .shared_codex_appserver
        .started_thread_params_for_test()[0]
        .0
        .clone()
        .unwrap();
    let injected: Value =
        serde_json::from_str(first.split_once("## Selected Template\n").unwrap().1).unwrap();
    assert_eq!(injected, snapshot);

    let (status, updated) = request(&b, "PUT", &format!("/api/track-recipes/{recipe}"), json!({
        "title": "Changed method", "body": "# Different\n\nThis must not change existing tracks.", "if_revision": 1,
    })).await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let (status, removed) = request(
        &b,
        "DELETE",
        &format!("/api/track-recipes/{recipe}"),
        Value::Null,
    )
    .await;
    assert!(status.is_success(), "{removed}");
    let report = report_payload(&detail);
    let block = report["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["kind"] == "prose")
        .unwrap();
    let (status, edited) = request(&b, "PATCH", &format!("/api/tracks/{track}/report/blocks/{}", block["id"].as_str().unwrap()), json!({
        "kind": "prose", "markdown": "# Rollout\n\nActual findings, not template instructions.\n", "ifBlockRev": block["rev"],
    })).await;
    assert_eq!(status, StatusCode::OK, "{edited}");

    let (status, reset) = b
        .post_json(&format!("/api/cards/{card}/planner/reset"), "{}")
        .await;
    assert_eq!(status, StatusCode::OK, "{reset}");
    let (status, sent) = b.send_planner_input(card, "Continue after reset").await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    assert!(
        b.started_turn_text("Continue after reset")
            .await
            .contains("Continue after reset")
    );
    let starts = b
        .state
        .shared_codex_appserver
        .started_thread_params_for_test();
    assert_eq!(starts.len(), 2, "reset creates a fresh provider thread");
    assert_eq!(
        starts[1].0.as_ref().unwrap(),
        &first,
        "reset uses the original context, not an edited report or deleted recipe"
    );
    b.shutdown_harnesses().await;
}

#[tokio::test]
async fn template_startup_snapshot_is_server_owned_and_sticky() {
    let b = boot().await;
    let track = create(&b, "investigation").await;
    let (_, detail) = b.get_json(&format!("/api/tracks/{track}")).await;
    let planner = planner_card(&detail);
    let card = planner["id"].as_str().unwrap();
    let snapshot = planner["payload"]["template_context"].clone();
    for value in [
        Value::Null,
        json!({"version": 1, "title": "forged", "body": "forged"}),
    ] {
        let (status, response) = request(
            &b,
            "PATCH",
            &format!("/api/cards/{card}"),
            json!({
                "payload": {"schemaVersion": 1, "template_context": value},
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    }
    let mut replacement = planner["payload"].clone();
    replacement
        .as_object_mut()
        .unwrap()
        .remove("template_context");
    let (status, patched) = request(
        &b,
        "PATCH",
        &format!("/api/cards/{card}"),
        json!({"payload": replacement}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(
        patched["payload"]["template_context"], snapshot,
        "omission cannot erase context"
    );
    let (_, detail) = b.get_json(&format!("/api/tracks/{track}")).await;
    assert_eq!(
        planner_card(&detail)["payload"]["template_context"],
        snapshot
    );
    b.shutdown_harnesses().await;
}

#[tokio::test]
async fn template_startup_does_not_inject_into_blank_tracks_or_assistants() {
    let b = boot().await;
    let (status, _) = b
        .create_track(Some("blank-context"), Some("Discuss options only"))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let starts = b
        .state
        .shared_codex_appserver
        .started_thread_params_for_test();
    assert_eq!(starts.len(), 1);
    assert!(
        !starts[0]
            .0
            .as_ref()
            .unwrap()
            .contains("## Selected Template\n")
    );
    let track = create(&b, "investigation").await;
    let (status, response) = request(
        &b,
        "POST",
        &format!("/api/tracks/{track}/conversations"),
        json!({"text": "Explain the findings"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{response}");
    let starts = b
        .state
        .shared_codex_appserver
        .started_thread_params_for_test();
    assert_eq!(starts.len(), 3);
    assert!(
        starts[1]
            .0
            .as_ref()
            .unwrap()
            .contains("## Selected Template\n")
    );
    assert!(
        !starts[2]
            .0
            .as_ref()
            .unwrap()
            .contains("## Selected Template\n"),
        "a conversation is not the Planner"
    );
    b.shutdown_harnesses().await;
}
