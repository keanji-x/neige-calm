mod support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::session_projection_repo::{WorkerSessionProjectionRepo, WorkerSessionState};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::worker_flow::WorkerFlowDriver;
use calm_server::worker_flow::codex_rollout::CodexRolloutFlowSourceOptions;
use calm_truth::worker_flow_sink::WorkerFlowSink;

use support::worker_flow as wf;

#[tokio::test]
async fn codex_rollout_source_waits_for_lazy_file_creation() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let thread_id = "thread-lazy";
    let seed = wf::seed_card_and_runtime(&repo, "card-lazy", Some(thread_id)).await;
    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);

    let (token, handle) = wf::spawn_source_with_discovery(
        repo.clone(),
        seed.runtime.clone(),
        &seed,
        codex_home.path(),
    );
    tokio::time::sleep(Duration::from_millis(15)).await;
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u1", "created later"),
        ],
    );

    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move {
            repo.worker_flow_item_list_by_card("card-lazy", 0, 100, false)
                .await
                .unwrap()
                .len()
                == 1
        }
    })
    .await;
    token.cancel();
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn codex_rollout_driver_waits_past_lazy_file_retry_budget_until_runtime_terminal() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-lazy-ghost";
    wf::seed_card_and_runtime(&repo, card_id, Some("ghost")).await;

    let driver = WorkerFlowDriver::new_with_flow_options_for_test(
        repo.clone(),
        SharedCodexAppServer::new_stub(repo.clone()),
        Arc::new(WorkerFlowSink::new(repo.clone())),
        EventBus::new(),
        CodexRolloutFlowSourceOptions {
            path_override: None,
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(50),
            lazy_retry_attempts: 3,
            cursor_persist_every: 1,
        },
    );
    driver.start_on_boot().await.unwrap();

    wf::wait_until(Duration::from_millis(500), || {
        let driver = driver.clone();
        async move { driver.tasks_alive_for_test().await == 1 }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(driver.tasks_alive_for_test().await, 1);

    repo.session_projection_set_status_for_card(card_id, WorkerSessionState::Exited)
        .await
        .unwrap();
    wf::wait_until(Duration::from_secs(2), || {
        let driver = driver.clone();
        async move { driver.tasks_alive_for_test().await == 0 }
    })
    .await;
}
