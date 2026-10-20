//! #2774 — wakes across a new Planner conversation, through the running dispatcher: a running
//! task's result wakes only the new session; an event committed while the new row is `starting`
//! arrives once; and a push racing the old owner's quiesce is neither lost nor doubled.

use std::time::Duration;

use axum::http::StatusCode;
use calm_server::codex_appserver::Notification;
use calm_server::db::prelude::*;
use calm_server::event::{Event, EventScope};
use calm_server::harness::HarnessPhaseTag;
use calm_server::ids::ActorId;
use calm_server::model::{new_id, now_ms};
use calm_server::test_seams::PLANNER_HIBERNATION_QUIESCE;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use serde_json::json;

use crate::planner_first_start::Boot;
use crate::planner_new_conversation::{START_COMMITTED, pause_at, person, send};
use crate::planner_repoint_restart_lock::{joined, wait_until};
use crate::planner_restart::{ordinary_track, wait_for};

/// Commit `event` on the Planner's Track as `actor` and broadcast it, as every writer does; the
/// boot's own dispatcher is the only thing that pushes it. Returns its event id.
pub(crate) async fn commit(boot: &Boot, actor: ActorId, event: Event) -> i64 {
    let card = boot
        .repo
        .card_get(&boot.planner_card_id)
        .await
        .unwrap()
        .unwrap();
    let track = boot
        .repo
        .track_get(card.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    boot.repo
        .log_pure_event(
            actor,
            EventScope::Track {
                track: track.id,
                area: track.area_id,
            },
            None,
            &boot.state.events,
            &boot.state.card_role_cache,
            &boot.state.track_area_cache,
            event,
        )
        .await
        .unwrap()
}

/// A wake whose only purpose is the Planner, carrying `text`.
pub(crate) async fn wake(boot: &Boot, text: &str) -> i64 {
    let track_id = boot
        .repo
        .card_get(&boot.planner_card_id)
        .await
        .unwrap()
        .unwrap()
        .track_id;
    commit(
        boot,
        ActorId::Kernel,
        Event::TrackWakeRequested {
            track_id,
            source: "test".into(),
            key: new_id(),
            text: text.into(),
        },
    )
    .await
}

/// How many times `text` went to the model on `thread`, as a turn or a steer.
pub(crate) fn handed(boot: &Boot, thread: &str, text: &str) -> usize {
    let daemon = &boot.state.shared_codex_appserver;
    let turns: usize = daemon
        .started_turns_for_test()
        .into_iter()
        .filter(|(on, _)| on == thread)
        .map(|(_, input)| format!("{input:?}").matches(text).count())
        .sum();
    let steers: usize = daemon
        .steered_turns_for_test()
        .into_iter()
        .filter(|steer| steer.0 == thread)
        .map(|steer| format!("{:?}", steer.2).matches(text).count())
        .sum();
    turns + steers
}

/// `text` reached the new session's model exactly once and the old thread never: it left the new
/// session's queue, and nothing delivers it a second time afterwards.
pub(crate) async fn arrives_once(boot: &Boot, new: &str, old_thread: &str, text: &str) {
    let new_thread = boot
        .repo
        .session_projection_by_id(new)
        .await
        .unwrap()
        .unwrap()
        .thread_id
        .unwrap();
    wait_for(
        "the observation reaches the new session's model",
        async || {
            handed(boot, &new_thread, text) > 0
                && !boot
                    .state
                    .harness
                    .get(&new.to_string())
                    .unwrap()
                    .snapshot()
                    .await
                    .unwrap()
                    .pending_entries()
                    .iter()
                    .any(|entry| format!("{:?}", entry.observation()).contains(text))
        },
    )
    .await;
    // A second copy would follow within the debounce window.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        handed(boot, &new_thread, text),
        1,
        "once, to the new thread"
    );
    assert_eq!(handed(boot, old_thread, text), 0, "never to the old thread");
}

/// End the running turn of `session` as Codex does, and wait until the owner is idle again.
pub(crate) async fn finish_turn(boot: &Boot, session: &str, reply: Option<&str>) -> String {
    let harness = boot.state.harness.get(&session.to_string()).unwrap();
    let thread = boot
        .repo
        .session_projection_by_id(session)
        .await
        .unwrap()
        .unwrap()
        .thread_id
        .unwrap();
    let mut turn = String::new();
    wait_for("the turn runs", async || {
        let snapshot = harness.snapshot().await.unwrap();
        turn = snapshot.last_turn_id.clone().unwrap_or_default();
        snapshot.phase == HarnessPhaseTag::TurnRunning
    })
    .await;
    let daemon = &boot.state.shared_codex_appserver;
    if let Some(text) = reply {
        daemon.emit_notification_for_test(Notification::Item {
            method: "item/completed".into(),
            params: json!({"threadId": thread, "turn": {"id": turn},
                "item": {"id": format!("reply-{turn}"), "type": "agentMessage", "text": text}}),
        });
    }
    daemon.clear_active_turn_for_test(&thread);
    daemon.emit_notification_for_test(Notification::TurnCompleted {
        thread_id: thread.clone(),
        turn: json!({"id": turn, "status": "completed"}),
    });
    wait_for("the owner is idle and persisted", async || {
        let persisted = boot
            .repo
            .session_projection_by_id(session)
            .await
            .unwrap()
            .unwrap()
            .handle_state_json
            .unwrap();
        harness.snapshot().await.unwrap().phase == HarnessPhaseTag::TurnCompleted
            && persisted["phase"] == "turn_completed"
            && persisted["last_turn_id"] == json!(turn)
    })
    .await;
    turn
}

fn cutover(boot: &Boot, key: &str) -> tokio::task::JoinHandle<(StatusCode, serde_json::Value)> {
    let app = person(boot, LEGACY_OWNER_USER_ID);
    let uri = boot.card_uri("planner/conversations");
    let key = key.to_string();
    tokio::spawn(async move { send(app, "POST", uri, Some(&key), None).await })
}

/// A task the old conversation dispatched is still running across the cutover; its result wakes
/// the new session and only it.
#[tokio::test]
async fn planner_new_conversation_a_running_tasks_result_wakes_only_the_new_session() {
    const RESULT: &str = "the running task finished after the cutover";
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let old_thread = old.thread_id.clone().unwrap();
    let card = boot
        .repo
        .card_get(&boot.planner_card_id)
        .await
        .unwrap()
        .unwrap();
    let task = calm_server::model::Task {
        id: format!("{}:running", card.track_id.as_str()),
        track_id: card.track_id.to_string(),
        key: "running".into(),
        kind: calm_server::model::TaskKind::Codex,
        goal: "keep working across the cutover".into(),
        context_json: "null".into(),
        acceptance_criteria: None,
        cwd: None,
        depends_on_json: "[]".into(),
        priority: 0,
        gate_json: None,
        status: calm_server::model::TaskStatus::Running,
        status_detail: None,
        worker_card_id: None,
        gate_result_json: None,
        gate_attempt: 0,
        gate_pid: None,
        gate_pid_starttime: None,
        gate_pid_boot_id: None,
        running_deadline_ms: None,
        context_stale_at_ms: None,
        declared_by: "planner".into(),
        spawn: calm_types::task_recovery::TASK_IN_TRACK_ROUTE.into(),
        access: calm_server::model::TaskAccess::ReadWrite,
        start: calm_server::model::TaskStart::Checkout,
        result_contract: None,
        created_at_ms: now_ms(),
        updated_at_ms: now_ms(),
        finished_at_ms: None,
    };
    let task_id = task.id.clone();
    calm_server::db::write_in_tx_typed(boot.repo.as_ref() as &dyn Repo, move |tx| {
        Box::pin(async move { crate::support::task::insert_task_tx(tx, &task).await })
    })
    .await
    .unwrap();

    let (status, body) = boot.new_conversation(&new_id()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let new = boot.active().await;
    assert_ne!(new.id, old.id);
    let status: String = sqlx::query_scalar("SELECT status FROM tasks WHERE id = ?1")
        .bind(&task_id)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(status, "running", "the cutover leaves the task running");

    commit(
        &boot,
        ActorId::Kernel,
        Event::TaskCompleted {
            idempotency_key: task_id,
            result: json!({"summary": RESULT}),
            artifacts: vec![],
            agent_message: None,
        },
    )
    .await;
    arrives_once(&boot, &new.id, &old_thread, RESULT).await;
    assert!(
        boot.state.harness.get(&old.id).is_none(),
        "the superseded session is not woken"
    );
    assert!(
        !format!("{:?}", boot.row_snapshot(&old.id).await.pending_entries()).contains(RESULT),
        "nothing is queued on the superseded row"
    );
    assert!(
        boot.state
            .shared_codex_appserver
            .resumed_threads_for_test()
            .iter()
            .all(|(thread, _)| thread != &old_thread),
        "the old thread is never resumed"
    );
    boot.shutdown().await;
}

/// An event committed after the cutover's commit and before its new session runs, while the new
/// row is `starting`, reaches the new session exactly once.
#[tokio::test]
async fn planner_new_conversation_an_event_while_the_new_row_starts_arrives_once() {
    const WAKE: &str = "committed while the new conversation was starting";
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let old_thread = old.thread_id.clone().unwrap();
    let (entered, release) = pause_at(START_COMMITTED, &boot.planner_card_id);
    let request = cutover(&boot, "starting");
    entered.notified().await;
    let starting = boot.current_session().await.unwrap();
    assert_ne!(starting, old.id, "premise: the cutover committed");
    let state: String = sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id = ?1")
        .bind(&starting)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(state, "starting", "premise: the new row is starting");

    let held = boot
        .state
        .planner_recovery_lock_handles_for_test(&boot.planner_card_id);
    wake(&boot, WAKE).await;
    wait_until("the dispatcher's push waits on the card", || {
        boot.state
            .planner_recovery_lock_handles_for_test(&boot.planner_card_id)
            > held
    })
    .await;
    release.notify_one();
    let (status, body) = joined(request, "the cutover").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(boot.active().await.id, starting);
    arrives_once(&boot, &starting, &old_thread, WAKE).await;
    boot.shutdown().await;
}

/// A dispatcher push that loses the race to the quiesce waits on the card and lands on the new
/// session; one that wins it is delivered by the old owner before the quiesce, moves with its
/// watermark, and is not delivered again. Neither is lost or doubled.
#[tokio::test]
async fn planner_new_conversation_a_push_racing_the_quiesce_is_neither_lost_nor_doubled() {
    const BEFORE: &str = "pushed before the quiesce";
    const DURING: &str = "pushed while the quiesce held the owner";
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let old_thread = old.thread_id.clone().unwrap();

    // Won the race: the old owner takes it to its model and settles.
    let before = wake(&boot, BEFORE).await;
    wait_for("the old owner delivers it", async || {
        handed(&boot, &old_thread, BEFORE) == 1
    })
    .await;
    finish_turn(&boot, &old.id, None).await;

    // Lost the race: held behind the quiesce on the card's fence.
    let (entered, release) = pause_at(PLANNER_HIBERNATION_QUIESCE, &boot.planner_card_id);
    let request = cutover(&boot, "racing");
    entered.notified().await;
    let held = boot
        .state
        .planner_recovery_lock_handles_for_test(&boot.planner_card_id);
    wake(&boot, DURING).await;
    wait_until("the dispatcher's push waits on the card", || {
        boot.state
            .planner_recovery_lock_handles_for_test(&boot.planner_card_id)
            > held
    })
    .await;
    release.notify_one();
    let (status, body) = joined(request, "the cutover").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let new = boot.active().await;
    assert_ne!(new.id, old.id);
    let moved = boot.row_snapshot(&new.id).await.push_watermark;
    assert!(
        moved >= before,
        "the watermark moved with the delivered prefix: {moved} < {before}"
    );

    arrives_once(&boot, &new.id, &old_thread, DURING).await;
    let new_thread = new.thread_id.clone().unwrap();
    assert_eq!(handed(&boot, &old_thread, BEFORE), 1, "delivered once");
    assert_eq!(handed(&boot, &new_thread, BEFORE), 0, "and not again");
    boot.shutdown().await;
}
