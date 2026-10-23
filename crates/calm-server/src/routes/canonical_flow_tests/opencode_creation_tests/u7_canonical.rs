//! U7 uses the existing ACP protocol peer and the actual retained activation path.
use super::*;

#[tokio::test]
async fn u7_canonical_opencode_template_input_is_context_once_and_retains_cold_plan() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    let (state, app, cookie) = fixture.application().await;
    state.plugin.enable("gitforge").await.unwrap();
    let mut create = body(&app, &cookie, &AgentProvider::OpenCode).await;
    create["template_id"] = json!("dev");
    create["template_input"] = json!({"merge_policy":"ask"});
    super::super::u7_nondelivery::hold(&fixture).await;
    let created = post(&app, &cookie, "/api/tracks", "first", create.clone()).await;
    let track = created["id"].as_str().unwrap();
    let (card, worker, _) = fixture.carrier(track).await;
    super::super::u7_nondelivery::release_twice(&fixture, &app, &cookie, track, &create, None)
        .await;
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    fixture.shutdown_process(&state).await;
    drop((state, app));
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, cookie) = fixture.application().await;
    state.plugin.enable("gitforge").await.unwrap();
    post(&app, &cookie, "/api/tracks", "first", create.clone()).await;
    wait_completed(&fixture, track, 1).await;
    let original: (String, String) = sqlx::query_as(
        "SELECT render_plan_json,render_plan_sha256 FROM conversation_input_submissions
         WHERE card_id=?1 ORDER BY rowid DESC LIMIT 1",
    )
    .bind(&card)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    let plan: calm_types::conversation::SubmissionRenderPlan =
        serde_json::from_str(&original.0).unwrap();
    let context_count = plan.segments().iter().filter(|segment| matches!(segment,
        calm_types::conversation::SubmissionRenderSegment::Context {
            payload: calm_types::conversation::PrivateHistoryPayload::Present { value }
        } if value.text().contains("## Bound Template Input") && value.text().contains("merge_policy")
    )).count();
    assert_eq!(
        context_count, 1,
        "input and working method are one frozen Context"
    );
    let first = prompt_records(&fixture);
    assert_eq!(first.len(), 1);
    let prompt = first[0]["params"]["prompt"].to_string();
    assert_eq!(prompt.matches("## Bound Template Input").count(), 1);
    assert_eq!(prompt.matches("## Selected Template").count(), 1);
    assert!(prompt.contains("merge_policy"));
    let receipt = fixture.receipt(create["area_id"].as_str().unwrap()).await;
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    fixture.shutdown_process(&state).await;
    drop((state, app));
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, cookie) = fixture.application().await;
    state.plugin.enable("gitforge").await.unwrap();
    assert_eq!(
        post(&app, &cookie, "/api/tracks", "first", create.clone()).await["id"],
        created["id"]
    );
    assert_eq!(
        fixture.receipt(create["area_id"].as_str().unwrap()).await,
        receipt
    );
    let retained: (String, String) = sqlx::query_as(
        "SELECT render_plan_json,render_plan_sha256 FROM conversation_input_submissions
         WHERE card_id=?1 ORDER BY rowid DESC LIMIT 1",
    )
    .bind(&card)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    assert_eq!(retained, original);
    assert_eq!(prompt_records(&fixture).len(), 1, "replay cannot resend");
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "second",
        json!({"text":"U7 followup on the original ACP carrier"}),
    )
    .await;
    wait_completed(&fixture, track, 2).await;
    assert_eq!(fixture.carrier(track).await.1, worker);
    let records = prompt_records(&fixture);
    assert_eq!(records.len(), 2);
    assert!(
        !records[1]["params"]["prompt"]
            .to_string()
            .contains("## Selected Template")
    );
    assert!(
        !records[1]["params"]["prompt"]
            .to_string()
            .contains("## Bound Template Input")
    );
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    fixture.shutdown_process(&state).await;
}

fn prompt_records(fixture: &Fixture) -> Vec<Value> {
    std::fs::read_to_string(fixture.root.path().join("requests.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["method"] == "session/prompt")
        .collect()
}

async fn wait_completed(fixture: &Fixture, track: &str, expected: usize) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (_, _, frame) = fixture.carrier(track).await;
            if frame.canonical_inputs.attempts.len() == expected + 2
                && frame.canonical_inputs.active.is_none()
                && frame.canonical_inputs.pending.is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the real ACP prompt must settle its original submission");
}
