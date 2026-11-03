//! Both sides of a real Dispatcher input keep their independent Human batches.
use super::*;

#[tokio::test]
async fn canonical_batch_system_input_separates_contiguous_human_prefixes() {
    let mut flow = Flow::running().await;
    for (key, text) in [
        ("before-a", "human before system A"),
        ("before-b", "human before system B"),
    ] {
        post(
            &flow.app,
            &flow.cookie,
            &format!("/api/cards/{}/planner/input", flow.card),
            key,
            json!({"text":text}),
        )
        .await;
    }
    flow.report(SYSTEM_BODY).await;
    for (key, text) in [
        ("after-a", "human after system A"),
        ("after-b", "human after system B"),
    ] {
        post(
            &flow.app,
            &flow.cookie,
            &format!("/api/cards/{}/planner/input", flow.card),
            key,
            json!({"text":text}),
        )
        .await;
    }
    let queued = flow
        .fixture
        .carrier(&flow.track)
        .await
        .2
        .canonical_inputs
        .pending;
    assert_eq!(queued.len(), 5);
    let PendingInput::System { observation, .. } = &queued[2] else {
        panic!("the actual Dispatcher input separates the Human prefixes")
    };
    let system_text = observation.observation().to_turn_text();
    flow.end(TURN).await;
    let before = flow.next().await.expect("first Human prefix");
    let text = before["params"]["input"][0]["text"].as_str().unwrap();
    assert!(
        text.find("human before system A").unwrap() < text.find("human before system B").unwrap()
    );
    assert!(!text.contains("human after system"));
    assert_eq!(
        flow.fixture
            .carrier(&flow.track)
            .await
            .2
            .canonical_inputs
            .pending,
        queued[2..]
    );
    flow.accept(&before, "human-batch-before-system").await;
    flow.end("human-batch-before-system").await;

    let system = flow
        .next()
        .await
        .expect("the real Dispatcher owns the boundary");
    let text = system["params"]["input"][0]["text"].as_str().unwrap();
    assert!(text.contains(&system_text));
    assert!(!text.contains("human before system"));
    assert!(!text.contains("human after system"));
    assert_eq!(
        flow.fixture
            .carrier(&flow.track)
            .await
            .2
            .canonical_inputs
            .pending,
        queued[3..]
    );
    flow.accept(&system, "system-between-batches").await;
    flow.end("system-between-batches").await;

    let after = flow.next().await.expect("later Human prefix");
    let text = after["params"]["input"][0]["text"].as_str().unwrap();
    assert!(
        text.find("human after system A").unwrap() < text.find("human after system B").unwrap()
    );
    assert!(!text.contains("human before system"));
    flow.accept(&after, "human-batch-after-system").await;
    flow.end("human-batch-after-system").await;
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    assert!(
        flow.fixture
            .carrier(&flow.track)
            .await
            .2
            .canonical_inputs
            .pending
            .is_empty()
    );
    let claims: Vec<(String, i64)> = sqlx::query_as(concat!(
        "SELECT i.message_id,i.revision FROM conversation_submission_inputs i ",
        "JOIN conversation_input_submissions s ON s.id=i.submission_id ",
        "WHERE s.card_id=?1 AND i.message_id IS NOT NULL ",
        "ORDER BY i.queue_position",
    ))
    .bind(&flow.card)
    .fetch_all(flow.fixture.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        claims.len(),
        5,
        "original Human plus four queued Humans, once each"
    );
    let expected: Vec<_> = queued
        .iter()
        .filter_map(|entry| match entry {
            PendingInput::Human { message, .. } => Some((
                message.message_id.to_string(),
                i64::from(message.revision.get()),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(claims[1..], expected);
    flow.close().await;
}
