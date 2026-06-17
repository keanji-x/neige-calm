mod support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus};
use calm_server::ids::ActorId;
use calm_server::runtime_repo::WorkerSessionState;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::worker_flow::WorkerFlowDriver;
use calm_server::worker_flow::codex_rollout::CodexRolloutFlowSourceOptions;
use calm_truth::worker_flow_sink::WorkerFlowSink;

use support::worker_flow as wf;

#[tokio::test]
async fn codex_rollout_session_meta_mismatch_exits_without_ingesting_wrong_file() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let wanted_thread_id = "wanted";
    let seed = wf::seed_card_and_runtime(&repo, "card-wrong-file", Some(wanted_thread_id)).await;
    let shared_codex = SharedCodexAppServer::new_stub(repo.clone());
    let path = wf::rollout_path(shared_codex.codex_home_path(), wanted_thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta("different"),
            wf::user_message("wrong-u1", "wrong file"),
        ],
    );

    let driver = WorkerFlowDriver::new_with_flow_options_for_test(
        repo.clone(),
        shared_codex,
        Arc::new(WorkerFlowSink::new(repo.clone())),
        events.clone(),
        CodexRolloutFlowSourceOptions {
            path_override: None,
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(10),
            lazy_retry_attempts: 3,
            cursor_persist_every: 1,
        },
    );
    driver.start_on_boot().await.unwrap();

    wf::wait_until(Duration::from_millis(500), || {
        let driver = driver.clone();
        async move { driver.tasks_alive_for_test().await == 0 }
    })
    .await;
    assert_eq!(item_count(&repo, "card-wrong-file").await, 0);

    events.emit(
        ActorId::Kernel,
        Event::RuntimeStatusChanged {
            runtime_id: seed.runtime.id.clone(),
            card_id: seed.runtime.card_id.clone(),
            old_status: WorkerSessionState::Idle,
            new_status: WorkerSessionState::Running,
        },
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    wf::wait_until(Duration::from_millis(500), || {
        let driver = driver.clone();
        async move { driver.tasks_alive_for_test().await == 0 }
    })
    .await;
    assert_eq!(item_count(&repo, "card-wrong-file").await, 0);
}

async fn item_count(repo: &SqlxRepo, card_id: &str) -> usize {
    repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
        .await
        .unwrap()
        .len()
}
