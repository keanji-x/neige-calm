//! Boot replay folds contiguous `track.report_edited` events into one pending entry, like the live enqueue.
use super::completed_commit_tests::Fixture;
use super::*;
use crate::model::{NewCard, new_id};
use calm_truth::db::{RepoEventWrite, RepoRead};
use calm_types::event::EditAuthor;

async fn record_report_edit(fx: &Fixture, before: &str, after: &str) -> i64 {
    let inner = &fx.harness.inner;
    let report: Option<String> =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=? AND kind='track-report'")
            .bind(inner.track_id.as_str())
            .fetch_optional(fx.repo.pool())
            .await
            .unwrap();
    let write = crate::state::WriteContext::new(
        inner.card_role_cache.clone(),
        inner.track_area_cache.clone(),
    );
    if report.is_none() {
        let track = fx
            .repo
            .track_get(inner.track_id.as_str())
            .await
            .unwrap()
            .unwrap();
        let id = new_id();
        let scope = EventScope::Card {
            card: id.clone().into(),
            track: track.id.clone(),
            area: track.area_id,
        };
        let new_card = NewCard {
            track_id: track.id,
            title: None,
            kind: "track-report".into(),
            sort: None,
            payload: serde_json::to_value(crate::track_report::TrackReportPayload::initial())
                .unwrap(),
        };
        let roles = inner.card_role_cache.clone();
        fx.repo
            .write_with_event(
                ActorId::Kernel,
                scope,
                None,
                &inner.events,
                &write,
                Box::new(move |tx| {
                    Box::pin(async move {
                        let card = crate::db::sqlite::card_create_with_id_tx(
                            tx,
                            id,
                            new_card,
                            crate::model::CardRole::ReportCard,
                            false,
                            &roles,
                        )
                        .await?;
                        Ok(Event::CardAdded(card))
                    })
                }),
            )
            .await
            .unwrap();
        let (track, card, current) = crate::track_report::resolve_report_for_track(
            fx.repo.as_ref(),
            inner.track_id.as_str(),
        )
        .await
        .unwrap();
        let rev = current.doc_rev;
        crate::track_report::persist_report(
            fx.repo.as_ref(),
            &inner.events,
            &write,
            ActorId::Kernel,
            EditAuthor::Kernel,
            track,
            card,
            current,
            crate::track_report::TrackReportPayload::new("", before),
            rev,
            None,
        )
        .await
        .unwrap();
    }
    let (track, card, current) =
        crate::track_report::resolve_report_for_track(fx.repo.as_ref(), inner.track_id.as_str())
            .await
            .unwrap();
    let rev = current.doc_rev;
    crate::track_report::persist_report(
        fx.repo.as_ref(),
        &inner.events,
        &write,
        ActorId::User,
        EditAuthor::User,
        track,
        card,
        current,
        crate::track_report::TrackReportPayload::new("", after),
        rev,
        None,
    )
    .await
    .unwrap();
    fx.repo
        .events_for_track(inner.track_id.as_str(), &["track.report_edited"], None)
        .await
        .unwrap()
        .last()
        .unwrap()
        .id
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
async fn report_knowledge_delta_legacy_replay_folds_latest_complete_suggestion() {
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
        &inner.worker_session_id,
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
        (Some("v2".to_string()), "v3".to_string()),
        "latest complete observation, without splicing historical bodies"
    );
    assert_eq!(
        entries[0].envelope_id(),
        Some(last),
        "the survivor acknowledges the newest push"
    );
    assert_eq!(replay.push_watermark, last);
}

/// Knowledge suggestions fold independently of the historical before/after chain.
#[tokio::test]
async fn report_knowledge_delta_legacy_replay_uses_current_owner_suggestion() {
    let fx = Fixture::new().await;
    let inner = &fx.harness.inner;
    record_report_edit(&fx, "v0", "v1").await;
    record_report_edit(&fx, "v1", "v2").await;
    let mut replay = HarnessSnapshot::initial(0, vec![]);
    crate::harness::replay_harness_events_since(
        fx.repo.clone(),
        inner.card_id.as_str(),
        &inner.track_id,
        &inner.worker_session_id,
        0,
        &mut replay,
    )
    .await
    .unwrap();
    let entries = replay.pending_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        report_edit_bodies(&entries[0]),
        (Some("v1".into()), "v2".into())
    );
    assert!(
        entries[0]
            .observation()
            .to_turn_text()
            .contains("Report knowledge delta")
    );
}

#[tokio::test]
async fn report_knowledge_delta_header_reads_report_without_or_with_same_head() {
    let fx = Fixture::new().await;
    record_report_edit(&fx, "before", "unseen").await;
    let inner = &fx.harness.inner;
    let head = track_vcs::head(fx.repo.pool(), &inner.track_id)
        .await
        .unwrap()
        .unwrap();
    *inner.last_seen_head.lock().await = Some(head.clone());
    let same =
        since_last_turn_diff_block(inner, Some(&head), track_vcs::ReportPatch::Include).await;
    assert!(
        same.block
            .as_deref()
            .is_some_and(|text| text.contains("known_rev") && text.contains("current_rev"))
    );
    sqlx::query("DELETE FROM track_vcs_refs WHERE track_id=?")
        .bind(inner.track_id.as_str())
        .execute(fx.repo.pool())
        .await
        .unwrap();
    let absent = since_last_turn_diff_block(inner, None, track_vcs::ReportPatch::Include).await;
    assert!(absent.current_head.is_none());
    assert_eq!(
        same.block, absent.block,
        "HEAD cannot short-circuit Report knowledge"
    );
}
