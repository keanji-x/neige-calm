use crate::support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus};
use calm_server::ids::ActorId;
use calm_server::session_projection_repo::WorkerSessionState;

use support::worker_flow as wf;

#[tokio::test]
async fn worker_flow_driver_attaches_codex_runtime_on_card_added() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let state = wf::app_state(repo.clone(), events.clone());
    state.worker_flow.start_on_boot().await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;

    let thread_id = "thread-card-added";
    let path = wf::rollout_path(state.shared_codex_appserver.codex_home_path(), thread_id);
    wf::write_rollout(&path, &[wf::session_meta(thread_id)]);
    let seed = wf::seed_card_and_runtime(&repo, "card-added-attach", Some(thread_id)).await;

    events.emit(ActorId::Kernel, Event::CardAdded(seed.card.clone()));

    wf::wait_until(Duration::from_millis(500), || {
        let driver = state.worker_flow.clone();
        async move { driver.tasks_alive_for_test().await == 1 }
    })
    .await;
}

#[tokio::test]
async fn worker_flow_driver_card_added_race_attaches_on_later_status() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let state = wf::app_state(repo.clone(), events.clone());
    state.worker_flow.start_on_boot().await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;

    let thread_id = "thread-card-added-race";
    let card = wf::seed_codex_card(&repo, "card-added-race").await;

    events.emit(ActorId::Kernel, Event::CardAdded(card.clone()));
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(state.worker_flow.tasks_alive_for_test().await, 0);

    let path = wf::rollout_path(state.shared_codex_appserver.codex_home_path(), thread_id);
    wf::write_rollout(&path, &[wf::session_meta(thread_id)]);
    let runtime = wf::seed_runtime_for_card_with_status(
        &repo,
        &card,
        Some(thread_id),
        WorkerSessionState::Running,
    )
    .await;

    events.emit(
        ActorId::Kernel,
        Event::RuntimeStatusChanged {
            runtime_id: runtime.id.clone(),
            card_id: runtime.card_id.clone(),
            old_status: WorkerSessionState::Starting,
            new_status: WorkerSessionState::Running,
        },
    );

    wf::wait_until(Duration::from_millis(500), || {
        let driver = state.worker_flow.clone();
        async move { driver.tasks_alive_for_test().await == 1 }
    })
    .await;
}
