//! Boot replay folds contiguous `track.report_edited` events into one pending entry, like the live enqueue.
use super::completed_commit_tests::Fixture;
use super::*;
use crate::db::sqlite::append_decision_event_in_tx;
use crate::model::new_id;
use calm_types::event::EditAuthor;

async fn record_report_edit(fx: &Fixture, before: &str, after: &str) -> i64 {
    let inner = &fx.harness.inner;
    let event = Event::TrackReportEdited {
        track_id: inner.track_id.clone(),
        card_id: inner.card_id.clone(),
        author: EditAuthor::User,
        author_plugin_id: None,
        edit_id: new_id(),
        summary_before: String::new(),
        summary_after: String::new(),
        body_before: before.to_string(),
        body_after: after.to_string(),
        agent_message: None,
    };
    let scope = harness_event_scope(inner, event.kind_tag());
    let mut tx = fx.repo.pool().begin().await.unwrap();
    let id = append_decision_event_in_tx(&mut tx, &ActorId::KernelDispatcher, &scope, None, &event)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    id
}

fn report_edit_bodies(entry: &QueueEntry) -> (Option<String>, String) {
    match entry.observation() {
        Observation::ReportEdited {
            body_before, body, ..
        } => (body_before, body),
        other => panic!("expected a ReportEdited entry, got {other:?}"),
    }
}

#[tokio::test]
async fn replay_folds_contiguous_report_edits_into_one_entry() {
    let fx = Fixture::new().await;
    let inner = &fx.harness.inner;
    record_report_edit(&fx, "v0", "v1").await;
    record_report_edit(&fx, "v1", "v2").await;
    let last = record_report_edit(&fx, "v2", "v3").await;

    let mut replay = HarnessSnapshot::initial(0, vec![]);
    crate::harness::replay_harness_events_since(
        fx.repo.clone(),
        inner.card_id.as_str(),
        &inner.track_id,
        0,
        &mut replay,
    )
    .await
    .unwrap();

    let entries = replay.pending_entries();
    assert_eq!(
        entries.len(),
        1,
        "three contiguous saves replay as one entry"
    );
    assert_eq!(
        report_edit_bodies(&entries[0]),
        (Some("v0".to_string()), "v3".to_string()),
        "first before, newest after"
    );
    assert_eq!(
        entries[0].envelope_id(),
        Some(last),
        "the survivor acknowledges the newest push"
    );
    assert_eq!(replay.push_watermark, last);
}

/// Round-4 M2 applies on replay too: a save that does not continue the
/// held body keeps its own slot.
#[tokio::test]
async fn replay_keeps_a_non_contiguous_report_edit_separate() {
    let fx = Fixture::new().await;
    let inner = &fx.harness.inner;
    record_report_edit(&fx, "v0", "v1").await;
    record_report_edit(&fx, "v1-planner", "v2").await;

    let mut replay = HarnessSnapshot::initial(0, vec![]);
    crate::harness::replay_harness_events_since(
        fx.repo.clone(),
        inner.card_id.as_str(),
        &inner.track_id,
        0,
        &mut replay,
    )
    .await
    .unwrap();

    let entries = replay.pending_entries();
    assert_eq!(entries.len(), 2, "a broken chain is two entries");
    assert_eq!(
        report_edit_bodies(&entries[0]),
        (Some("v0".to_string()), "v1".to_string())
    );
    assert_eq!(
        report_edit_bodies(&entries[1]),
        (Some("v1-planner".to_string()), "v2".to_string())
    );
}
