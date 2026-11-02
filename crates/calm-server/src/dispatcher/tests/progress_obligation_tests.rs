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

async fn seed_failed_child(
    repo: &crate::db::sqlite::SqlxRepo,
    parent: &crate::model::Track,
    phase_changed: bool,
) -> (String, Event) {
    let child = repo
        .track_create(crate::model::NewTrack {
            area_id: parent.area_id.clone(),
            title: "failed child".into(),
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
    let card = repo
        .card_create(crate::model::NewCard {
            track_id: child.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::json!({"planner_provider":"codex"}),
        })
        .await
        .unwrap();
    let key = if phase_changed {
        "held-child-phase-failure"
    } else {
        "held-child-status-failure"
    };
    let task = seed(repo, &parent.id, key).await;
    let task_id = task.clone();
    let parent_id = parent.id.to_string();
    crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2")
                .bind(parent_id)
                .bind(child.id.as_str())
                .execute(&mut **tx)
                .await?;
            sqlx::query("UPDATE cards SET role='planner' WHERE id=?1")
                .bind(card.id.as_str())
                .execute(&mut **tx)
                .await?;
            sqlx::query("UPDATE tasks SET child_track_id=?1 WHERE id=?2")
                .bind(child.id.as_str())
                .bind(&task_id)
                .execute(&mut **tx)
                .await?;
            calm_truth::db::sqlite::task_mark_sub_track_running_tx(
                tx,
                &task_id,
                crate::model::now_ms(),
            )
            .await?;
            let session = crate::model::new_id();
            calm_truth::db::sqlite::session_start_runtime_tx(
                tx,
                crate::session_projection_repo::WorkerSessionInit {
                    id: session.clone(),
                    card_id: card.id.to_string(),
                    kind: crate::session_projection_repo::WorkerSessionKind::SharedPlanner,
                    agent_provider: Some(crate::session_projection_repo::AgentProvider::Codex),
                    status: calm_types::worker::WorkerSessionState::Idle,
                    terminal_run_id: None,
                    thread_id: None,
                    session_id: None,
                    active_turn_id: None,
                    handle_state_json: None,
                    spawn_op_id: None,
                    now_ms: crate::model::now_ms(),
                },
            )
            .await?;
            calm_truth::db::sqlite::session_fail_if_active_runtime_tx(tx, &session).await?;
            let event = if phase_changed {
                Event::HarnessPhaseChanged {
                    worker_session_id: session,
                    card_id: card.id,
                    track_id: child.id,
                    old_phase: calm_types::harness::HarnessPhaseTag::Idle,
                    new_phase: calm_types::harness::HarnessPhaseTag::Wedged,
                }
            } else {
                Event::WorkerSessionStatusChanged {
                    worker_session_id: session,
                    card_id: card.id.to_string(),
                    old_status: calm_types::worker::WorkerSessionState::Idle,
                    new_status: calm_types::worker::WorkerSessionState::Failed,
                }
            };
            Ok((task, event))
        })
    })
    .await
    .unwrap()
}

pub(super) async fn assert_progress_watchdog(
    inner: &Arc<Inner>,
    repo: &Arc<crate::db::sqlite::SqlxRepo>,
    handle: &crate::harness::PlannerHarness,
    track: &crate::model::Track,
) {
    let id = seed(repo, &track.id, "held-bootstrap").await;
    let held = inner.acquire_push_lock(&track.id).await;
    let permit = inner.semaphore.acquire().await.unwrap();
    let mut failure_keys = Vec::new();
    for phase_changed in [false, true] {
        let (failure_task, failure_event) = seed_failed_child(repo, track, phase_changed).await;
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            inner.clone().handle_envelope(BroadcastEnvelope {
                id: 0,
                event_version: 1,
                actor: ActorId::Kernel,
                scope: EventScope::Track {
                    track: track.id.clone(),
                    area: track.area_id.clone(),
                },
                event: failure_event,
            }),
        )
        .await
        .unwrap();
        let failure_notices = repo
            .events_for_track(track.id.as_str(), &["track.wake_requested"], None)
            .await
            .unwrap();
        let key = failure_notices.iter().find_map(|row| match &row.event {
            Event::TrackWakeRequested {key,text,..} if text.contains(&failure_task) => Some(key.clone()),
            _ => None,
        }).expect("each failure event family must commit while dispatch permit and parent ingress are held");
        failure_keys.push(key);
    }
    drop(permit);
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
    assert!(
        !handle
            .snapshot()
            .await
            .unwrap()
            .pending_observations()
            .iter()
            .any(|obs| matches!(obs,HarnessObservation::TrackWake {key,..} if failure_keys.contains(key)))
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
    for failure_key in failure_keys {
        assert_eq!(
            snapshot
                .pending_observations()
                .iter()
                .filter(
                    |obs| matches!(obs,HarnessObservation::TrackWake {key,..} if key==&failure_key)
                )
                .count(),
            1,
            "each durable failure observation reaches parent ingress once after its lock becomes available"
        );
    }
    assert_eq!(
        snapshot
            .pending_observations()
            .iter()
            .filter(|obs| matches!(obs,HarnessObservation::TrackWake{key:found,..} if found==&key))
            .count(),
        1
    );
}
