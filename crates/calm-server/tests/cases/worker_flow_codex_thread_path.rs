//! #2494: Codex capture reads the rollout file the app-server names in `Thread.path`; it never
//! derives the file from Codex's on-disk layout.
use crate::support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::worker_flow::WorkerFlowDriver;
use calm_server::worker_flow::codex_rollout::CodexRolloutFlowSourceOptions;
use calm_server::worker_flow::cursor::CODEX_ROLLOUT_SOURCE_KIND;
use calm_truth::worker_flow_sink::WorkerFlowSink;

use support::worker_flow as wf;

fn driver(repo: &Arc<SqlxRepo>, shared: Arc<SharedCodexAppServer>) -> Arc<WorkerFlowDriver> {
    WorkerFlowDriver::new_with_flow_options_for_test(
        repo.clone(),
        shared,
        Arc::new(WorkerFlowSink::new(repo.clone())),
        EventBus::new(),
        CodexRolloutFlowSourceOptions {
            path_override: None,
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(10),
            lazy_retry_attempts: 3,
        },
    )
}

/// The reported file has no Codex-style name or directory, and it appears only after the source
/// has attached: Codex names the path at `thread/start`, before the first turn writes it.
#[tokio::test]
async fn codex_capture_tails_the_rollout_the_app_server_reports() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-thread-path";
    let thread_id = "thread-path-reported";
    wf::seed_card_and_runtime(&repo, card_id, Some(thread_id)).await;
    let dir = tempfile::tempdir().unwrap();
    let reported = dir.path().join("reported/any-name.jsonl");
    let shared = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    shared.answer_thread_path_for_test(thread_id, Some(&reported));

    let driver = driver(&repo, shared);
    driver.start_on_boot().await.unwrap();
    wf::wait_until(wf::LIVENESS_BUDGET, || {
        let driver = driver.clone();
        async move { driver.tasks_alive_for_test().await == 1 }
    })
    .await;
    wf::write_rollout(
        &reported,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u1", "one"),
            wf::assistant_message("a1", "two"),
        ],
    );

    wf::wait_for_codex_cursor(&repo, card_id, 3).await;
    let cursor = repo
        .worker_flow_cursor_get(card_id, CODEX_ROLLOUT_SOURCE_KIND)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cursor.source_path, reported.to_string_lossy());
    assert_eq!(item_count(&repo, card_id).await, 2);
    for stop in driver.task_stop_tokens_for_test().await {
        stop.cancel();
    }
}

/// `Thread.path: null` is a loud, specific warning and an exit, not a silent wait for a file.
#[tokio::test]
async fn codex_capture_warns_and_exits_when_the_thread_path_is_null() {
    let (logs, _guard) = Logs::capture();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-thread-path-null";
    let thread_id = "thread-path-null";
    wf::seed_card_and_runtime(&repo, card_id, Some(thread_id)).await;
    let shared = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    shared.answer_thread_path_for_test(thread_id, None);

    let driver = driver(&repo, shared);
    driver.start_on_boot().await.unwrap();
    // The runtime stays Running: only the null path can end the source.
    wf::wait_until(wf::LIVENESS_BUDGET, || {
        let logs = logs.clone();
        async move {
            logs.text()
                .contains("codex app-server reported no rollout path")
        }
    })
    .await;
    wf::wait_until(wf::LIVENESS_BUDGET, || {
        let driver = driver.clone();
        async move { driver.tasks_alive_for_test().await == 0 }
    })
    .await;
    assert!(logs.text().contains(thread_id), "{}", logs.text());
    assert_eq!(item_count(&repo, card_id).await, 0);
}

async fn item_count(repo: &SqlxRepo, card_id: &str) -> usize {
    repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
        .await
        .unwrap()
        .len()
}

/// WARN-and-above lines from this test's thread; a current-thread runtime polls the sources here.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
    type Writer = Logs;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Logs {
    fn capture() -> (Self, tracing::subscriber::DefaultGuard) {
        let logs = Logs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        (logs, tracing::subscriber::set_default(subscriber))
    }

    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}
