use crate::support;

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
    let dir = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(dir.path(), thread_id);
    let shared = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    shared.answer_thread_path_for_test(thread_id, Some(&path));

    let (token, handle) =
        wf::spawn_source_with_reported_path(repo.clone(), seed.runtime.clone(), &seed, shared);
    tokio::time::sleep(Duration::from_millis(15)).await;
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u1", "created later"),
        ],
    );

    wf::wait_until(wf::LIVENESS_BUDGET, || {
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
    // Codex names the path at `thread/start`; the first turn never comes to create the file.
    let dir = tempfile::tempdir().unwrap();
    let shared = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    shared.answer_thread_path_for_test("ghost", Some(&wf::rollout_path(dir.path(), "ghost")));

    let driver = WorkerFlowDriver::new_with_flow_options_for_test(
        repo.clone(),
        shared,
        Arc::new(WorkerFlowSink::new(repo.clone())),
        EventBus::new(),
        CodexRolloutFlowSourceOptions {
            path_override: None,
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(50),
            lazy_retry_attempts: 3,
        },
    );
    driver.start_on_boot().await.unwrap();

    wf::wait_until(wf::LIVENESS_BUDGET, || {
        let driver = driver.clone();
        async move { driver.tasks_alive_for_test().await == 1 }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(driver.tasks_alive_for_test().await, 1);

    repo.session_projection_set_status_for_card(card_id, WorkerSessionState::Exited)
        .await
        .unwrap();
    wf::wait_until(wf::LIVENESS_BUDGET, || {
        let driver = driver.clone();
        async move { driver.tasks_alive_for_test().await == 0 }
    })
    .await;
}
