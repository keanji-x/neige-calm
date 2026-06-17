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
async fn codex_tail_task_exits_after_terminal_completion_without_event() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let thread_id = "thread-terminal-complete";
    let card_id = "card-terminal-complete";
    let seed = wf::seed_card_and_runtime(&repo, card_id, Some(thread_id)).await;
    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u-terminal", "one"),
            wf::assistant_message("a-terminal", "two"),
        ],
    );
    let poll_interval = Duration::from_millis(50);

    let driver = WorkerFlowDriver::new_with_flow_options_for_test(
        repo.clone(),
        SharedCodexAppServer::new_stub(repo.clone()),
        Arc::new(WorkerFlowSink::new(repo.clone())),
        EventBus::new(),
        CodexRolloutFlowSourceOptions {
            path_override: Some(path.clone()),
            poll_interval,
            lazy_retry_delay: Duration::from_millis(10),
            lazy_retry_attempts: 3,
            cursor_persist_every: 1,
        },
    );
    driver
        .attach_runtime_for_test(seed.runtime.clone())
        .await
        .unwrap();

    wf::wait_until(Duration::from_secs(1), || {
        let repo = repo.clone();
        async move { item_count(&repo, card_id).await == 2 }
    })
    .await;
    wait_for_cursor(&repo, card_id, 3).await;

    wf::append_rollout(
        &path,
        &[
            wf::reasoning("r-terminal-final", "three"),
            wf::assistant_message("a-terminal-final", "four"),
        ],
    );
    repo.session_projection_set_status_for_card(card_id, WorkerSessionState::Exited)
        .await
        .unwrap();

    wf::wait_until(poll_interval * 2, || {
        let driver = driver.clone();
        let repo = repo.clone();
        async move { item_count(&repo, card_id).await == 4 && driver.tasks_alive_for_test().await == 0 }
    })
    .await;
}

async fn item_count(repo: &SqlxRepo, card_id: &str) -> usize {
    repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
        .await
        .unwrap()
        .len()
}

async fn wait_for_cursor(repo: &SqlxRepo, card_id: &str, record_index: i64) {
    wf::wait_until(Duration::from_secs(1), || async {
        repo.worker_flow_cursor_get(
            card_id,
            calm_server::worker_flow::cursor::CODEX_ROLLOUT_SOURCE_KIND,
        )
        .await
        .unwrap()
        .is_some_and(|cursor| cursor.record_index == record_index)
    })
    .await;
}
