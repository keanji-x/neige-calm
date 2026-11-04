#![cfg(test)]
//! Outage acceptance, one canonical batch and a later original-session turn.
use super::*;

pub(super) async fn outage_and_continuation() {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let uri = format!("/api/tracks/{track}/conversations");
    let body = json!({"provider":"claude","text":FIRST_BODY});
    let expected_card =
        crate::conversation_keys::derive_track_conversation_keys(&track, "assistant").card_id;
    let wake = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "canonical-human-wake-finished",
        &expected_card,
        wake.clone(),
    );
    let created = post(&app, &cookie, &uri, "assistant", body.clone()).await;
    let card = created["id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(5), wake.entered.notified())
        .await
        .expect("fresh acceptance wakes even during provider outage");
    wake.release.notify_one();
    let (worker, frame) = carrier(&fixture, card).await;
    assert_eq!(frame.header.phase, HarnessPhaseTag::Idle);
    assert!(frame.header.last_thread_id.is_none());
    assert_eq!(frame.canonical_inputs.pending.len(), 1);
    assert!(frame.canonical_inputs.attempts.is_empty());
    assert_eq!(fixture.counts(card).await, (1, 1, 1, 0, 0));
    assert_eq!(
        created["initialInput"]["receipt"],
        initial(&app, &cookie, &track, card, "assistant").await["receipt"]
    );
    let receipts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM conversation_assistant_creations WHERE card_id=?1",
    )
    .bind(card)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    assert_eq!(receipts, 1);
    for (field, value) in [("text", json!("changed")), ("provider", json!("codex"))] {
        let mut changed = body.clone();
        changed[field] = value;
        assert_eq!(
            status(&app, &cookie, &uri, "assistant", changed).await,
            StatusCode::CONFLICT
        );
    }
    fixture.shutdown_process(&state).await;
    drop(app);
    drop(state);
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    configure(&mut fixture, "exit");
    let (state, app, cookie) = fixture.application().await;
    let wake = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("canonical-human-wake-finished", card, wake.clone());
    let replay = post(&app, &cookie, &uri, "assistant", body.clone()).await;
    assert_eq!(replay["initialInput"], created["initialInput"]);
    let woke = tokio::time::timeout(Duration::from_secs(1), wake.entered.notified())
        .await
        .is_ok();
    wake.release.notify_one();
    assert!(
        !woke,
        "creation replay must not wake or resend accepted work"
    );
    assert!(read(&fixture, "spawns").is_empty());
    let later = post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "later",
        json!({"text":"activate retained Claude"}),
    )
    .await;
    let frame = wait_frame(&fixture, card, |f| {
        f.idle_for_lifecycle() && f.canonical_inputs.attempts.len() == 1
    })
    .await;
    use crate::test_support::canonical_batch::{
        assert_native_text, assert_submission, human_ref, native_text,
    };
    assert_eq!(frame.header.phase, HarnessPhaseTag::TurnCompleted);
    assert!(frame.canonical_inputs.pending.is_empty());
    assert!(frame.canonical_inputs.active.is_none());
    assert!(matches!(
        frame.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    assert_eq!(fixture.counts(card).await, (2, 2, 1, 1, 0));
    assert_submission(
        &fixture.repo,
        card,
        &frame.canonical_inputs.attempts[0].attempt.submission_id,
        &[
            (
                human_ref(
                    created["initialInput"]["receipt"]["entryId"]
                        .as_str()
                        .unwrap(),
                ),
                FIRST_BODY,
            ),
            (
                human_ref(later["entry_id"].as_str().unwrap()),
                "activate retained Claude",
            ),
        ],
        (calm_types::organization::LEGACY_OWNER_USER_ID, "jian"),
    )
    .await;
    let lines: Vec<Value> = read(&fixture, "stdin")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "one native submission for the two accepted Humans"
    );
    assert_native_text(
        &native_text(&lines[0]["message"]["content"]),
        &[FIRST_BODY, "activate retained Claude"],
        "jian",
    );
    assert_eq!(
        fixture
            .transcript_count(card, Some(&worker), Some("turn/completed"), None)
            .await,
        1
    );
    assert_eq!(carrier(&fixture, card).await.0, worker);
    let thread = frame.header.last_thread_id.as_deref().unwrap();
    assert!(uuid::Uuid::parse_str(thread).is_ok());
    let row: (String, String) =
        sqlx::query_as("SELECT thread_id,agent_session_id FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(row, (thread.to_owned(), thread.to_owned()));
    assert!(
        frame
            .canonical_inputs
            .attempts
            .iter()
            .all(|a| a.correlation.thread_id == thread)
    );
    assert_eq!(read(&fixture, "spawns").lines().count(), 1);
    // This input is accepted only after the complete batch ended. It must be
    // a separate turn on the original native session after handle removal.
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    let continuation = post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "load",
        json!({"text":"continue original Claude session"}),
    )
    .await;
    let continued = wait_frame(&fixture, card, |f| {
        f.idle_for_lifecycle() && f.canonical_inputs.attempts.len() == 2
    })
    .await;
    assert_eq!(continued.header.phase, HarnessPhaseTag::TurnCompleted);
    assert!(continued.canonical_inputs.pending.is_empty());
    assert!(continued.canonical_inputs.active.is_none());
    assert_eq!(
        continued.canonical_inputs.attempts[0],
        frame.canonical_inputs.attempts[0]
    );
    assert!(matches!(
        continued.canonical_inputs.attempts[1].delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    assert_eq!(continued.header.last_thread_id.as_deref(), Some(thread));
    assert_eq!(carrier(&fixture, card).await.0, worker);
    assert_eq!(fixture.counts(card).await, (3, 3, 1, 2, 0));
    assert_submission(
        &fixture.repo,
        card,
        &continued.canonical_inputs.attempts[1].attempt.submission_id,
        &[(
            human_ref(continuation["entry_id"].as_str().unwrap()),
            "continue original Claude session",
        )],
        (calm_types::organization::LEGACY_OWNER_USER_ID, "jian"),
    )
    .await;
    let lines: Vec<Value> = read(&fixture, "stdin")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    let text = native_text(&lines[1]["message"]["content"]);
    assert_native_text(&text, &["continue original Claude session"], "jian");
    assert!(!text.contains(FIRST_BODY));
    assert!(!text.contains("activate retained Claude"));
    assert!(read(&fixture, "argv").contains(&format!("--resume\n{thread}\n")));
    assert_eq!(
        fixture
            .transcript_count(card, Some(&worker), Some("turn/completed"), None)
            .await,
        2
    );
    let wake = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("canonical-human-wake-finished", card, wake.clone());
    let replay = post(&app, &cookie, &uri, "assistant", body).await;
    assert_eq!(replay["initialInput"], created["initialInput"]);
    let woke = tokio::time::timeout(Duration::from_secs(1), wake.entered.notified())
        .await
        .is_ok();
    wake.release.notify_one();
    assert!(
        !woke,
        "completed receipt replay must not schedule another wake"
    );
    assert_eq!(
        carrier(&fixture, card).await.1.canonical_inputs,
        continued.canonical_inputs
    );
    let current_identity: (String, String) =
        sqlx::query_as("SELECT thread_id,agent_session_id FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(current_identity, row);
    assert_eq!(read(&fixture, "spawns").lines().count(), 2);
    assert_eq!(
        read(&fixture, "stdin")
            .lines()
            .filter(|l| l.contains(FIRST_BODY))
            .count(),
        1
    );
    fixture.shutdown_process(&state).await;
}
