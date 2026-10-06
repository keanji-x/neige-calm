//! #2206 T1: the projection row a turn writes says, per segment, which observation and which
//! event put it there. Driven through the real queue and `maybe_issue_turn`.
use super::completed_commit_tests::Fixture;
use super::*;
use crate::model::HarnessInputOrigin;
use calm_types::observation::MAIL_WAKE_SOURCE;

fn origin(observation: &str, event_id: Option<i64>) -> Option<HarnessInputOrigin> {
    Some(HarnessInputOrigin {
        observation: observation.into(),
        event_id,
    })
}

fn completed(key: &str) -> Event {
    Event::TaskCompleted {
        idempotency_key: key.into(),
        result: serde_json::json!({"summary": "done"}),
        artifacts: vec![],
        agent_message: None,
    }
}

fn mail_wake(fx: &Fixture) -> Event {
    Event::TrackWakeRequested {
        track_id: fx.harness.inner.track_id.clone(),
        source: MAIL_WAKE_SOURCE.into(),
        key: "mail-1".into(),
        text: "\"R\": question — neige mail cat mail-1".into(),
    }
}

#[tokio::test]
async fn a_task_completion_turn_records_the_completion_event() {
    let fx = Fixture::new().await;
    let (event_id, entry) = fx.event_entry(&completed("origin-task")).await;
    fx.enqueue(vec![entry]).await;
    fx.issue().await;
    assert_eq!(fx.daemon.turn_start_count_for_test(), 1);
    let segments = fx.projected_segments().await;
    assert_eq!(segments.len(), 1);
    assert_eq!(
        segments[0].origin,
        origin("task_completed", Some(event_id)),
        "the receipt enrichment keeps the origin"
    );
}

#[tokio::test]
async fn a_mail_wake_turn_records_the_wake_event() {
    let fx = Fixture::new().await;
    let (event_id, entry) = fx.event_entry(&mail_wake(&fx)).await;
    fx.enqueue(vec![entry]).await;
    fx.issue().await;
    assert_eq!(fx.daemon.turn_start_count_for_test(), 1);
    let segments = fx.projected_segments().await;
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].origin, origin("track_wake", Some(event_id)));
}

#[tokio::test]
async fn a_user_message_turn_records_a_user_message_with_no_event() {
    let fx = Fixture::new().await;
    fx.harness
        .observe_user_message_durable(
            "why did you stop?".into(),
            vec![],
            crate::harness::SendKey::unique_for_test(),
        )
        .await
        .unwrap();
    fx.issue().await;
    assert_eq!(fx.daemon.turn_start_count_for_test(), 1);
    let segments = fx.projected_segments().await;
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].origin, origin("user_message", None));
}

#[tokio::test]
async fn each_segment_of_a_batch_records_its_own_origin_in_order() {
    let fx = Fixture::new().await;
    let (completed_id, completion) = fx.event_entry(&completed("origin-batch")).await;
    let (wake_id, wake) = fx.event_entry(&mail_wake(&fx)).await;
    fx.enqueue(vec![completion, wake]).await;
    fx.issue().await;
    assert_eq!(fx.daemon.turn_start_count_for_test(), 1);
    let origins = fx
        .projected_segments()
        .await
        .into_iter()
        .map(|segment| segment.origin)
        .collect::<Vec<_>>();
    assert_eq!(
        origins,
        vec![
            origin("task_completed", Some(completed_id)),
            origin("track_wake", Some(wake_id)),
        ]
    );
}

/// Two contiguous report edits fold into one queue entry; its segment names the newer event.
#[tokio::test]
async fn a_folded_entry_records_the_newest_event() {
    let fx = Fixture::new().await;
    let edit = |before: &str, after: &str| Event::TrackReportEdited {
        track_id: fx.harness.inner.track_id.clone(),
        card_id: fx.harness.inner.card_id.clone(),
        author: calm_types::event::EditAuthor::User,
        author_plugin_id: None,
        edit_id: crate::model::new_id(),
        summary_before: String::new(),
        summary_after: String::new(),
        body_before: before.into(),
        body_after: after.into(),
        agent_message: None,
    };
    let (_, first) = fx.event_entry(&edit("# T\n\na\n", "# T\n\nb\n")).await;
    let (newest_id, second) = fx.event_entry(&edit("# T\n\nb\n", "# T\n\nc\n")).await;
    fx.enqueue(vec![first]).await;
    fx.enqueue(vec![second]).await;
    assert_eq!(
        fx.stored().await.pending_entries().len(),
        1,
        "premise: the queue folded the continuing edit"
    );
    // A report-edit-only queue waits for the editor to go quiet; age the window instead of sleeping.
    {
        let mut debounce = fx.harness.inner.debounce.lock().await;
        let quiet = Instant::now() - Duration::from_secs(121);
        debounce.first_pending_at = Some(quiet);
        debounce.last_pending_at = Some(quiet);
    }
    fx.issue().await;
    assert_eq!(fx.daemon.turn_start_count_for_test(), 1);
    let segments = fx.projected_segments().await;
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].origin, origin("report_edited", Some(newest_id)));
}
