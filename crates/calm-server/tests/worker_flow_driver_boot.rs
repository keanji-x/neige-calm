mod support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::sqlite::{SqlxRepo, runtime_bind_attribution_tx, runtime_set_status_tx};
use calm_server::event::{Event, EventBus};
use calm_server::ids::ActorId;
use calm_server::runtime_repo::{AgentProvider, RunStatus, ThreadAttribution};

use support::worker_flow as wf;

#[tokio::test]
async fn worker_flow_driver_boot_enumerates_active_codex_runtimes() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    wf::seed_card_and_runtime(&repo, "card-driver-live", Some("thread-driver-live")).await;
    wf::seed_card_and_runtime(&repo, "card-driver-no-thread", None).await;

    let state = wf::app_state(repo, EventBus::new());
    state.worker_flow.start_on_boot().await.unwrap();

    wf::wait_until(Duration::from_secs(1), || {
        let driver = state.worker_flow.clone();
        async move { driver.tasks_alive_for_test().await == 1 }
    })
    .await;
}

#[tokio::test]
async fn worker_flow_driver_attaches_when_thread_arrives_on_running_status() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let seed = wf::seed_card_and_runtime_with_status(
        &repo,
        "card-status-attach",
        None,
        RunStatus::Starting,
    )
    .await;

    let state = wf::app_state(repo.clone(), events.clone());
    state.worker_flow.start_on_boot().await.unwrap();
    events.emit(
        ActorId::Kernel,
        Event::RuntimeStarted {
            runtime_id: seed.runtime.id.clone(),
            card_id: seed.runtime.card_id.clone(),
            kind: seed.runtime.kind.clone(),
            agent_provider: seed.runtime.agent_provider.clone(),
            status: RunStatus::Starting,
        },
    );
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(state.worker_flow.tasks_alive_for_test().await, 0);

    let thread_id = "thread-status-attach";
    let path = wf::rollout_path(state.shared_codex_appserver.codex_home_path(), thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u-status", "attached after thread bind"),
        ],
    );
    let mut tx = repo.pool().begin().await.unwrap();
    runtime_bind_attribution_tx(
        &mut tx,
        &seed.runtime.id,
        ThreadAttribution {
            runtime_id: seed.runtime.id.clone(),
            provider: AgentProvider::Codex,
            thread_id: Some(thread_id.to_string()),
            session_id: Some(format!("sess-{thread_id}")),
            active_turn_id: None,
        },
    )
    .await
    .unwrap();
    runtime_set_status_tx(&mut tx, &seed.runtime.id, RunStatus::Running)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    events.emit(
        ActorId::Kernel,
        Event::RuntimeStatusChanged {
            runtime_id: seed.runtime.id.clone(),
            card_id: seed.runtime.card_id.clone(),
            old_status: RunStatus::Starting,
            new_status: RunStatus::Running,
        },
    );
    wf::wait_until(Duration::from_secs(1), || {
        let driver = state.worker_flow.clone();
        async move { driver.tasks_alive_for_test().await == 1 }
    })
    .await;

    events.emit(
        ActorId::Kernel,
        Event::RuntimeStatusChanged {
            runtime_id: seed.runtime.id.clone(),
            card_id: seed.runtime.card_id.clone(),
            old_status: RunStatus::Running,
            new_status: RunStatus::TurnPending,
        },
    );
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(state.worker_flow.tasks_alive_for_test().await, 1);
}
