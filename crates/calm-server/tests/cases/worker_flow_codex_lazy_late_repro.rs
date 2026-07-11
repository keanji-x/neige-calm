use crate::support;

use std::sync::Arc;
use std::time::Duration;

use calm_exec::flow::WorkerFlowSource;
use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::worker_flow::codex_rollout::{
    CodexRolloutFlowSource, CodexRolloutFlowSourceOptions,
};
use calm_truth::worker_flow_sink::WorkerFlowSink;
use tokio_util::sync::CancellationToken;

use support::worker_flow as wf;

/// Repro for #820: the codex rollout file appears AFTER the lazy-retry budget
/// elapses, while the runtime is still alive. A correct source should keep
/// waiting (like the Claude source) and ingest the conversation once the file
/// shows up. The hypothesis is that the codex source instead exits permanently.
#[tokio::test]
async fn codex_rollout_source_ingests_file_created_after_budget_while_alive() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let thread_id = "thread-late";
    let card_id = "card-late";
    // Runtime stays Running (alive) for the whole test — never goes terminal.
    let seed = wf::seed_card_and_runtime(&repo, card_id, Some(thread_id)).await;
    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);

    // Tiny budget: 3 attempts * 30ms ~= 90ms. The file will not exist during
    // that window; it is created well after the budget would have elapsed.
    let token = CancellationToken::new();
    let source = CodexRolloutFlowSource::new_with_options(
        repo.clone(),
        seed.runtime.clone(),
        codex_home.path().to_path_buf(),
        token.clone(),
        CodexRolloutFlowSourceOptions {
            path_override: None,
            poll_interval: Duration::from_millis(20),
            lazy_retry_delay: Duration::from_millis(30),
            lazy_retry_attempts: 3,
            cursor_persist_every: 1,
        },
    );
    let session = wf::worker_session(&seed);
    let sink = WorkerFlowSink::new(repo.clone());
    let handle = tokio::spawn(async move { source.capture(&session, &sink).await });

    // Sleep WELL past the lazy-retry budget (~90ms) before the file appears.
    tokio::time::sleep(Duration::from_millis(400)).await;
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u1", "created after budget"),
        ],
    );

    // A correct, liveness-gated source ingests the item once the file appears.
    wf::wait_until(Duration::from_secs(2), || {
        let repo = repo.clone();
        async move {
            repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
                .await
                .unwrap()
                .len()
                == 1
        }
    })
    .await;

    token.cancel();
    let _ = handle.await.unwrap();
}
