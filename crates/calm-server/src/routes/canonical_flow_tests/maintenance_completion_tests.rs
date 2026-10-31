//! Late maintenance facts must converge through the admitted operation's owner.
use super::*;
use crate::harness::backend::{MaintenanceObservation, MaintenanceTarget};
use crate::harness::compact_outcome::CompactStatus;

const QUEUED: &str = "Continue after the exact original compaction actually completes";

#[tokio::test]
async fn canonical_http_late_compaction_end_releases_pending_without_resubmission() {
    Box::pin(late_compaction(LateEnd::Completed)).await;
}

#[tokio::test]
async fn canonical_http_late_failed_compaction_end_keeps_pending_held() {
    Box::pin(late_compaction(LateEnd::Failed)).await;
}

#[tokio::test]
async fn canonical_http_late_compaction_end_does_not_clear_another_hold() {
    Box::pin(late_compaction(LateEnd::SystemError)).await;
}

#[derive(Clone, Copy, PartialEq)]
enum LateEnd {
    Completed,
    Failed,
    SystemError,
}

async fn late_compaction(case: LateEnd) {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Late compaction","color":"#123456"}),
    )
    .await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created = post(&app, &cookie, "/api/tracks", "first", json!({"area_id":area["id"],"title":"","planner_provider":"codex","theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY})).await;
    let track = created["id"].as_str().unwrap().to_owned();
    let first = native_attempt_recovery_tests::initial_request(&mut peer).await;
    send(
        &mut peer,
        json!({"id":first["id"],"result":{"turn":{"id":TURN}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":TURN,"status":"inProgress"}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":TURN,"status":"completed","items":[]}),
    )
    .await;
    let before = fixture
        .wait_phase(&track, HarnessPhaseTag::TurnCompleted)
        .await;
    let (card, worker, _) = fixture.carrier(&track).await;
    let original_ack = fixture.receipt(area["id"].as_str().unwrap()).await;
    let compacting = tokio::spawn({
        let (app, cookie, card) = (app.clone(), cookie.clone(), card.clone());
        async move {
            status(
                &app,
                &cookie,
                &format!("/api/cards/{card}/planner/compact"),
                "compact",
                json!({}),
            )
            .await
        }
    });
    let compact = request(&mut peer).await;
    assert_eq!(compact["method"], "thread/compact/start");
    assert_eq!(compact["params"], json!({"threadId":THREAD}));
    send(&mut peer, json!({"id":compact["id"],"result":null})).await;
    assert!(!compacting.await.unwrap().is_success());
    let held = fixture.wait_phase(&track, HarnessPhaseTag::Wedged).await;
    let outcome = held.header.compact_outcome.as_ref().unwrap();
    assert_eq!(outcome.status, CompactStatus::Unconfirmed);
    assert!(!held.header.context_usage_invalidated);
    assert_eq!(held.canonical_inputs, before.canonical_inputs);
    let target = MaintenanceTarget {
        card_id: card.clone(),
        track_id: track.clone(),
        worker_session_id: worker.clone(),
        thread_id: outcome.thread_id.clone(),
        operation_id: outcome.operation_id.clone(),
    };
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "queued",
        json!({"text":QUEUED}),
    )
    .await;
    let pending = fixture.carrier(&track).await.2.canonical_inputs.pending;
    assert_eq!(pending.len(), 1);
    if case == LateEnd::SystemError {
        send(&mut peer, json!({"method":"thread/status/changed","params":{"threadId":THREAD,"status":{"type":"systemError"}}})).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while fixture
                .carrier(&track)
                .await
                .2
                .header
                .wedged_reason
                .as_deref()
                != Some(calm_types::harness::HARNESS_SYSTEM_ERROR_REASON)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the actual system-error hold must be persisted first");
    }
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":"late-maintenance","status":"inProgress"}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":"late-maintenance","status":if case == LateEnd::Failed { "failed" } else { "completed" }}),
    )
    .await;
    // Check the authoritative provider observation, not just the raw wakeup.
    assert!(
        matches!(state.shared_codex_appserver.observe_maintenance(&target),
        Some(MaintenanceObservation::Turn { turn_id, settled: Some(success) }) if turn_id == "late-maintenance" && success == (case != LateEnd::Failed))
    );
    if case != LateEnd::Completed {
        // This real boundary follows END on the same native stream. Its durable
        // invalidation is a processing barrier, not proof the context is usable.
        send(&mut peer, json!({"method":"item/completed","params":{"threadId":THREAD,"turnId":"late-maintenance","item":{"id":"late-boundary","type":"contextCompaction"}}})).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !fixture
                .carrier(&track)
                .await
                .2
                .header
                .context_usage_invalidated
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the maintenance boundary must be processed after END");
        let after = fixture.carrier(&track).await.2;
        assert_eq!(after.header.phase, HarnessPhaseTag::Wedged);
        assert_eq!(
            after.header.compact_outcome.as_ref().unwrap().status,
            CompactStatus::Unconfirmed
        );
        assert_eq!(after.canonical_inputs.pending, pending);
        assert_eq!(
            after.canonical_inputs.attempts,
            before.canonical_inputs.attempts
        );
        if case == LateEnd::SystemError {
            assert_eq!(
                after.header.wedged_reason.as_deref(),
                Some(calm_types::harness::HARNESS_SYSTEM_ERROR_REASON)
            );
        }
        fixture.shutdown_process(&state).await;
        return;
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = fixture.carrier(&track).await.2;
            if current
                .header
                .compact_outcome
                .as_ref()
                .is_some_and(|o| o.status == CompactStatus::Completed)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("qualified late maintenance END must settle the original Unconfirmed operation");
    let next = request(&mut peer).await;
    assert_eq!(
        next["method"], "turn/start",
        "maintenance must never be submitted again"
    );
    assert_eq!(next["params"]["threadId"], THREAD);
    let body = next["params"]["input"].to_string();
    assert!(body.contains(QUEUED));
    assert!(
        !body.contains(FIRST_BODY),
        "original accepted input must not be resent"
    );
    send(
        &mut peer,
        json!({"id":next["id"],"result":{"turn":{"id":"after-maintenance"}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":"after-maintenance","status":"inProgress"}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":"after-maintenance","status":"completed","items":[]}),
    )
    .await;
    let done = fixture
        .wait_phase(&track, HarnessPhaseTag::TurnCompleted)
        .await;
    assert!(done.header.wedged_reason.is_none());
    assert_eq!(
        done.header.compact_outcome.as_ref().unwrap().operation_id,
        target.operation_id
    );
    assert_eq!(
        done.header.compact_outcome.as_ref().unwrap().status,
        CompactStatus::Completed
    );
    assert_eq!(
        done.header
            .compact_outcome
            .as_ref()
            .unwrap()
            .turn_id
            .as_deref(),
        Some("late-maintenance")
    );
    assert_eq!(
        &done.canonical_inputs.attempts[..before.canonical_inputs.attempts.len()],
        before.canonical_inputs.attempts.as_slice()
    );
    assert_eq!(
        done.canonical_inputs.attempts.len(),
        before.canonical_inputs.attempts.len() + 1
    );
    assert!(done.canonical_inputs.active.is_none());
    assert!(done.canonical_inputs.pending.is_empty());
    assert_eq!(
        fixture.receipt(area["id"].as_str().unwrap()).await,
        original_ack
    );
    fixture.shutdown_process(&state).await;
}
