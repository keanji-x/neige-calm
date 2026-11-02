//! The durable scan remains independent of a blocked real per-Track ingress.
use super::*;
use crate::db::prelude::*;
use calm_truth::db::RepoEventWrite as _;

async fn seed(repo: &crate::db::sqlite::SqlxRepo, track: &TrackId, key: &str) -> String {
    let id = format!("{track}:{key}");
    let task_id = id.clone();
    let track = track.to_string();
    let key = key.to_owned();
    crate::db::write_in_tx_typed(repo, move |tx| Box::pin(async move {
        sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,spawn,created_at_ms,updated_at_ms) \
                     VALUES(?1,?2,?3,'codex','handoff','{}','pending',?4,1,1)")
            .bind(&task_id).bind(track).bind(key)
            .bind(calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE).execute(&mut **tx).await?;
        calm_truth::db::sqlite::task_claim_pending_tx(tx,&task_id,1,&[],false).await?;
        Ok(())
    })).await.unwrap();
    id
}

pub(super) async fn assert_progress_watchdog(
    inner: &Arc<Inner>,
    repo: &Arc<crate::db::sqlite::SqlxRepo>,
    handle: &crate::harness::PlannerHarness,
    track: &crate::model::Track,
) {
    let id = seed(repo, &track.id, "held-bootstrap").await;
    let held = inner.acquire_push_lock(&track.id).await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        inner.progress_watchdog_once(),
    )
    .await
    .unwrap()
    .unwrap();
    let other = repo
        .track_create(crate::model::NewTrack {
            area_id: track.area_id.clone(),
            title: "other bootstrap owner".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: crate::model::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    inner
        .write
        .remember_track(other.id.clone(), other.area_id.clone());
    let other_id = seed(repo, &other.id, "other-bootstrap").await;
    for _ in 0..2 {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            inner.progress_watchdog_once(),
        )
        .await
        .unwrap()
        .unwrap();
    }
    let notices = repo
        .events_for_track(other.id.as_str(), &["track.wake_requested"], None)
        .await
        .unwrap();
    assert_eq!(
        notices
            .iter()
            .filter(|row| matches!(&row.event, Event::TrackWakeRequested{key,..}
        if key.as_str() == format!("{other_id}:child-track-bootstrap").as_str()))
            .count(),
        1
    );
    let key = format!("{id}:child-track-bootstrap");
    assert!(
        !handle
            .snapshot()
            .await
            .unwrap()
            .pending_observations()
            .iter()
            .any(|obs| matches!(obs,HarnessObservation::TrackWake{key:found,..} if found==&key))
    );
    drop(held);
    for _ in 0..3 {
        inner.progress_watchdog_once().await.unwrap();
        tokio::task::yield_now().await;
    }
    let _drained = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        inner.acquire_push_lock(&track.id),
    )
    .await
    .unwrap();
    use crate::harness::queue::{MutationRefused, QueueEntryId, QueueMutation};
    assert_eq!(
        handle
            .mutate_pending_entry(
                QueueMutation::Delete {
                    entry_id: QueueEntryId::from_wire("progress-barrier".into()),
                    if_entry_rev: 1,
                },
                ActorId::User
            )
            .await
            .unwrap(),
        Err(MutationRefused::NotFound)
    );
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(
        snapshot
            .pending_observations()
            .iter()
            .filter(|obs| matches!(obs,HarnessObservation::TrackWake{key:found,..} if found==&key))
            .count(),
        1
    );
}
