mod support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;

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
