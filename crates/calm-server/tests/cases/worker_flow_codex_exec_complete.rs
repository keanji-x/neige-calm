use crate::support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::worker_flow::cursor::CODEX_ROLLOUT_SOURCE_KIND;
use calm_types::worker_flow::{ExecStatus, WorkerFlowItem};

use support::worker_flow as wf;

#[tokio::test]
async fn codex_rollout_records_exec_command_end_and_suppresses_begin() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let thread_id = "thread-exec-complete";
    let seed = wf::seed_card_and_runtime(&repo, "card-exec-complete", Some(thread_id)).await;
    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::function_call("call-complete", "echo hi"),
            wf::exec_command_begin("call-complete"),
            wf::exec_command_end(
                "call-complete",
                "completed",
                0,
                "hi",
                42,
                &["bash", "-lc", "echo hi"],
            ),
        ],
    );

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wait_for_cursor(&repo, "card-exec-complete", 4).await;
    token.cancel();
    handle.await.unwrap().unwrap();

    let rows = command_items(&repo, "card-exec-complete").await;
    assert_eq!(rows.len(), 2);
    assert_command(&rows[0], ExecStatus::InProgress, None, None, None);
    assert_command(
        &rows[1],
        ExecStatus::Completed,
        Some(0),
        Some("hi"),
        Some(42),
    );
}

#[tokio::test]
async fn codex_rollout_records_failed_exec_command_end() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let thread_id = "thread-exec-failed";
    let seed = wf::seed_card_and_runtime(&repo, "card-exec-failed", Some(thread_id)).await;
    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::function_call("call-failed", "false"),
            wf::exec_command_end(
                "call-failed",
                "failed",
                1,
                "boom",
                7,
                &["bash", "-lc", "false"],
            ),
        ],
    );

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wait_for_cursor(&repo, "card-exec-failed", 3).await;
    token.cancel();
    handle.await.unwrap().unwrap();

    let rows = command_items(&repo, "card-exec-failed").await;
    assert_eq!(rows.len(), 2);
    assert_command(&rows[1], ExecStatus::Failed, Some(1), Some("boom"), Some(7));
}

async fn wait_for_cursor(repo: &SqlxRepo, card_id: &str, record_index: i64) {
    wf::wait_until(Duration::from_secs(1), || async {
        repo.worker_flow_cursor_get(card_id, CODEX_ROLLOUT_SOURCE_KIND)
            .await
            .unwrap()
            .is_some_and(|cursor| cursor.record_index == record_index)
    })
    .await;
}

async fn command_items(repo: &SqlxRepo, card_id: &str) -> Vec<WorkerFlowItem> {
    repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
        .await
        .unwrap()
        .into_iter()
        .map(|row| serde_json::from_str::<WorkerFlowItem>(&row.payload).unwrap())
        .filter(|item| matches!(item, WorkerFlowItem::CommandExecution { .. }))
        .collect()
}

fn assert_command(
    item: &WorkerFlowItem,
    status: ExecStatus,
    exit_code: Option<i32>,
    aggregated_output: Option<&str>,
    duration_ms: Option<i64>,
) {
    let WorkerFlowItem::CommandExecution {
        status: actual_status,
        exit_code: actual_exit_code,
        aggregated_output: actual_aggregated_output,
        duration_ms: actual_duration_ms,
        ..
    } = item
    else {
        panic!("expected command execution item");
    };
    assert_eq!(*actual_status, status);
    assert_eq!(*actual_exit_code, exit_code);
    assert_eq!(actual_aggregated_output.as_deref(), aggregated_output);
    assert_eq!(*actual_duration_ms, duration_ms);
}
