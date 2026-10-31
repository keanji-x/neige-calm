//! The native CLI sees the existing creation Context, once, through retained activation.
use super::super::*;
use super::assistant::{configure, read, wait_frame};

#[tokio::test]
async fn canonical_claude_named_context_is_once_across_cold_restart() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture, "exit");
    let (state, app, cookie) = fixture.application().await;
    state.plugin.enable("gitforge").await.unwrap();
    let mut body = super::create_body(&app, &cookie).await;
    body["template_id"] = json!("dev");
    body["template_input"] = json!({"merge_policy":"ask"});
    body["first_message"] = json!(FIRST_BODY);
    let created = post(&app, &cookie, "/api/tracks", "first", body.clone()).await;
    let track = created["id"].as_str().unwrap().to_owned();
    let (card, worker, _) = fixture.carrier(&track).await;
    wait_frame(&fixture, &card, |frame| {
        frame.canonical_inputs.attempts.len() == 1
            && frame.canonical_inputs.active.is_none()
            && frame.canonical_inputs.pending.is_empty()
    })
    .await;
    let first = read(&fixture, "stdin");
    assert_eq!(first.lines().count(), 1);
    assert_eq!(first.matches(FIRST_BODY).count(), 1);
    assert_eq!(first.matches("## Selected Template").count(), 1);
    assert_eq!(first.matches("## Bound Template Input").count(), 1);
    assert!(first.contains("merge_policy"));
    assert!(!read(&fixture, "instructions").contains("## Selected Template"));
    assert!(!read(&fixture, "instructions").contains("## Bound Template Input"));
    let original = plan(&fixture, &card).await;
    let parsed: calm_types::conversation::SubmissionRenderPlan =
        serde_json::from_str(&original.0).unwrap();
    assert_eq!(parsed.input_refs().unwrap().len(), 1);
    assert_eq!(
        parsed
            .segments()
            .iter()
            .filter(|segment| matches!(segment,
                calm_types::conversation::SubmissionRenderSegment::Context {
                    payload: calm_types::conversation::PrivateHistoryPayload::Present { value }
                } if value.text().contains("## Bound Template Input")
            ))
            .count(),
        1
    );
    let receipt = fixture.receipt(body["area_id"].as_str().unwrap()).await;
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    fixture.shutdown_process(&state).await;
    drop((state, app));
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, cookie) = fixture.application().await;
    state.plugin.enable("gitforge").await.unwrap();
    assert_eq!(
        post(&app, &cookie, "/api/tracks", "first", body.clone()).await["id"],
        created["id"]
    );
    assert_eq!(
        fixture.receipt(body["area_id"].as_str().unwrap()).await,
        receipt
    );
    assert_eq!(plan(&fixture, &card).await, original);
    assert_eq!(read(&fixture, "stdin"), first, "replay cannot resend");
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "second",
        json!({"text":"Continue on the original Claude carrier"}),
    )
    .await;
    wait_frame(&fixture, &card, |frame| {
        frame.canonical_inputs.attempts.len() == 2
            && frame.canonical_inputs.active.is_none()
            && frame.canonical_inputs.pending.is_empty()
    })
    .await;
    let input = read(&fixture, "stdin");
    assert_eq!(input.lines().count(), 2);
    let second = input.lines().last().unwrap();
    assert!(second.contains("Continue on the original Claude carrier"));
    assert!(!second.contains("## Selected Template"));
    assert!(!second.contains("## Bound Template Input"));
    assert_eq!(fixture.carrier(&track).await.1, worker);
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn canonical_claude_named_unconfirmed_launch_preserves_pending_context_without_resend() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture, "exit");
    let (state, app, cookie) = fixture.application().await;
    let mut body = super::create_body(&app, &cookie).await;
    body["template_id"] = json!("small-change");
    body["first_message"] = json!(FIRST_BODY);
    u7_nondelivery::hold(&fixture).await;
    let created = post(&app, &cookie, "/api/tracks", "first", body.clone()).await;
    let track = created["id"].as_str().unwrap().to_owned();
    let (card, worker, _) = fixture.carrier(&track).await;
    u7_nondelivery::release_twice(&fixture, &app, &cookie, &track, &body, None).await;
    let before = fixture.carrier(&track).await.2.canonical_inputs;
    let receipt = fixture.receipt(body["area_id"].as_str().unwrap()).await;
    let spawns = read(&fixture, "spawns");
    assert_eq!(
        spawns.lines().count(),
        1,
        "the first session launch is unconfirmed"
    );
    assert!(read(&fixture, "stdin").is_empty());
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    fixture.shutdown_process(&state).await;
    drop((state, app));
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, cookie) = fixture.application().await;
    let wake = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("canonical-human-wake-finished", &card, wake.clone());
    assert_eq!(
        post(&app, &cookie, "/api/tracks", "first", body.clone()).await["id"],
        created["id"]
    );
    tokio::time::timeout(Duration::from_secs(10), wake.entered.notified())
        .await
        .unwrap();
    wake.release.notify_one();
    // BeforeSend proves the input was not sent. It cannot prove a launched
    // native session never existed; the existing identity owner must hold it.
    let handle = state.harness.get(&worker).unwrap();
    for _ in 0..3 {
        handle.refresh_canonical_input().await.unwrap();
    }
    assert_eq!(fixture.carrier(&track).await.2.canonical_inputs, before);
    assert_eq!(
        fixture.receipt(body["area_id"].as_str().unwrap()).await,
        receipt
    );
    assert_eq!(read(&fixture, "spawns"), spawns);
    assert!(read(&fixture, "stdin").is_empty());
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    fixture.shutdown_process(&state).await;
}

async fn plan(fixture: &Fixture, card: &str) -> (String, String) {
    sqlx::query_as(
        "SELECT render_plan_json,render_plan_sha256 FROM conversation_input_submissions
         WHERE card_id=?1 ORDER BY rowid DESC LIMIT 1",
    )
    .bind(card)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap()
}
