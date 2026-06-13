mod support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::worker_flow::WorkerFlowDriver;
use calm_truth::worker_flow_sink::WorkerFlowSink;

use support::worker_flow as wf;

#[tokio::test]
async fn worker_flow_driver_drop_cancels_tail_tasks() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let thread_id = "thread-driver-drop";
    let seed = wf::seed_card_and_runtime(&repo, "card-driver-drop", Some(thread_id)).await;
    let shared = SharedCodexAppServer::new_stub(repo.clone());
    let path = wf::rollout_path(shared.codex_home_path(), thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u-drop", "running before drop"),
        ],
    );

    let driver = WorkerFlowDriver::new(
        repo.clone(),
        shared,
        Arc::new(WorkerFlowSink::new(repo)),
        EventBus::new(),
    );
    driver
        .attach_runtime_for_test(seed.runtime.clone())
        .await
        .unwrap();
    wf::wait_until(Duration::from_secs(1), || {
        let driver = driver.clone();
        async move { driver.tasks_alive_for_test().await == 1 }
    })
    .await;

    let stops = driver.task_stop_tokens_for_test().await;
    assert_eq!(stops.len(), 1);
    let stop = stops[0].clone();
    assert!(!stop.is_cancelled());
    drop(driver);

    wf::wait_until(Duration::from_millis(500), || {
        let stop = stop.clone();
        async move { stop.is_cancelled() }
    })
    .await;
}
