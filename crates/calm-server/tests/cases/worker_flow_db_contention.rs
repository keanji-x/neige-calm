//! #1579: a worker-flow source outlives SQLite writer contention, and an idle source does not write.

use crate::support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::worker_flow::claude_transcript::CLAUDE_TRANSCRIPT_SOURCE_KIND;
use calm_server::worker_flow::cursor::CODEX_ROLLOUT_SOURCE_KIND;
use calm_truth::db::sqlite::SQLITE_BUSY_TIMEOUT_MS;
use calm_types::error::CoreError;
use serde_json::Value;
use sqlx::Connection;
use tokio::task::JoinHandle;

use support::worker_flow as wf;

/// A line no source can parse: it advances the cursor without recording an item, so the source's
/// next write is the cursor upsert itself.
const MALFORMED_LINE: &str = "{not json";

async fn file_repo(dir: &Path) -> (Arc<SqlxRepo>, String) {
    let db_url = format!("sqlite://{}?mode=rwc", dir.join("calm.db").display());
    let repo = Arc::new(SqlxRepo::open(&db_url).await.expect("open file db"));
    (repo, db_url)
}

fn append_raw(path: &Path, raw: &str) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{raw}").unwrap();
}

async fn cursor_record_index(repo: &SqlxRepo, card_id: &str, source_kind: &str) -> Option<i64> {
    repo.worker_flow_cursor_get(card_id, source_kind)
        .await
        .unwrap()
        .map(|cursor| cursor.record_index)
}

async fn items(repo: &SqlxRepo, card_id: &str) -> usize {
    repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
        .await
        .unwrap()
        .len()
}

/// Hold the database writer lock from another connection past the pool's busy timeout while the
/// source must advance its cursor, then release it and require the source to keep capturing.
async fn source_survives_writer_lock(
    repo: &Arc<SqlxRepo>,
    db_url: &str,
    card_id: &str,
    source_kind: &str,
    path: &Path,
    handle: &JoinHandle<Result<(), CoreError>>,
    later_line: Value,
) {
    let mut locker = sqlx::SqliteConnection::connect(db_url).await.unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut locker)
        .await
        .unwrap();
    append_raw(path, MALFORMED_LINE);
    // Past one full busy-timeout wait: the source's cursor upsert has met SQLITE_BUSY by now.
    tokio::time::sleep(Duration::from_millis(SQLITE_BUSY_TIMEOUT_MS + 1_500)).await;
    sqlx::query("ROLLBACK").execute(&mut locker).await.unwrap();
    drop(locker);

    let before = items(repo, card_id).await;
    wf::append_rollout(path, &[later_line]);
    wf::wait_until(wf::LIVENESS_BUDGET, || {
        let repo = repo.clone();
        async move { handle.is_finished() || items(&repo, card_id).await > before }
    })
    .await;
    assert!(
        !handle.is_finished(),
        "the {source_kind} source ended on a transient SQLITE_BUSY instead of waiting it out"
    );
    assert_eq!(items(repo, card_id).await, before + 1);
    wf::wait_until(wf::LIVENESS_BUDGET, || async {
        cursor_record_index(repo, card_id, source_kind).await == Some(4)
    })
    .await;
}

#[tokio::test]
async fn codex_source_keeps_capturing_after_sqlite_busy_on_cursor_write() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, db_url) = file_repo(dir.path()).await;
    let thread_id = "thread-busy";
    let card_id = "card-codex-busy";
    let seed = wf::seed_card_and_runtime(&repo, card_id, Some(thread_id)).await;
    let path = wf::rollout_path(dir.path(), thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u-busy", "before the lock"),
        ],
    );
    let (stop, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_for_codex_cursor(&repo, card_id, 2).await;

    source_survives_writer_lock(
        &repo,
        &db_url,
        card_id,
        CODEX_ROLLOUT_SOURCE_KIND,
        &path,
        &handle,
        wf::assistant_message("a-busy", "after the lock"),
    )
    .await;

    stop.cancel();
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn claude_source_keeps_capturing_after_sqlite_busy_on_cursor_write() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, db_url) = file_repo(dir.path()).await;
    let card_id = "card-claude-busy";
    let cwd = "/tmp/claude-busy";
    let seed = wf::seed_claude_card_and_runtime(&repo, card_id, "session-claude-busy", cwd).await;
    let path = dir.path().join("session-claude-busy.jsonl");
    wf::write_transcript(
        &path,
        &[
            wf::claude_system("sys-busy", cwd),
            wf::claude_user_string("user-busy", "before the lock"),
        ],
    );
    let (stop, handle) =
        wf::spawn_claude_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_until(wf::LIVENESS_BUDGET, || async {
        cursor_record_index(&repo, card_id, CLAUDE_TRANSCRIPT_SOURCE_KIND).await == Some(2)
    })
    .await;

    source_survives_writer_lock(
        &repo,
        &db_url,
        card_id,
        CLAUDE_TRANSCRIPT_SOURCE_KIND,
        &path,
        &handle,
        wf::claude_user_string("user-after", "after the lock"),
    )
    .await;

    stop.cancel();
    handle.await.unwrap().unwrap();
}

/// An idle source re-reads its file every poll; only a cursor that moved is written back.
#[tokio::test]
async fn idle_codex_source_does_not_rewrite_an_unchanged_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, _db_url) = file_repo(dir.path()).await;
    let thread_id = "thread-idle";
    let card_id = "card-codex-idle";
    let seed = wf::seed_card_and_runtime(&repo, card_id, Some(thread_id)).await;
    let path = wf::rollout_path(dir.path(), thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u-idle", "only line"),
        ],
    );
    let (stop, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_for_codex_cursor(&repo, card_id, 2).await;
    let written = repo
        .worker_flow_cursor_get(card_id, CODEX_ROLLOUT_SOURCE_KIND)
        .await
        .unwrap()
        .unwrap()
        .updated_at_ms;

    // The helper polls every 20 ms: this spans many idle polls.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let after = repo
        .worker_flow_cursor_get(card_id, CODEX_ROLLOUT_SOURCE_KIND)
        .await
        .unwrap()
        .unwrap()
        .updated_at_ms;
    assert_eq!(after, written, "an idle poll rewrote an unchanged cursor");
    assert!(!handle.is_finished());

    stop.cancel();
    handle.await.unwrap().unwrap();
}

/// A cancelled source (the driver cancels it before attaching a replacement for the card) must not
/// keep retrying a contended cursor write: a late write would overwrite the replacement's row.
#[tokio::test]
async fn cancelled_codex_source_abandons_a_contended_cursor_write() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, db_url) = file_repo(dir.path()).await;
    let thread_id = "thread-cancel";
    let card_id = "card-codex-cancel";
    let seed = wf::seed_card_and_runtime(&repo, card_id, Some(thread_id)).await;
    let path = wf::rollout_path(dir.path(), thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u-cancel", "before the lock"),
        ],
    );
    let (stop, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_for_codex_cursor(&repo, card_id, 2).await;

    let mut locker = sqlx::SqliteConnection::connect(&db_url).await.unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut locker)
        .await
        .unwrap();
    append_raw(&path, MALFORMED_LINE);
    // The source polls every 20 ms; by now it is blocked in the cursor write.
    tokio::time::sleep(Duration::from_millis(300)).await;
    stop.cancel();
    // Past the blocked attempt's busy timeout, with the lock still held.
    tokio::time::sleep(Duration::from_millis(SQLITE_BUSY_TIMEOUT_MS + 1_500)).await;
    let ended_under_lock = handle.is_finished();
    sqlx::query("ROLLBACK").execute(&mut locker).await.unwrap();
    drop(locker);

    assert!(
        ended_under_lock,
        "a cancelled source kept retrying its cursor write under contention"
    );
    handle.await.unwrap().unwrap();
    assert_eq!(
        cursor_record_index(&repo, card_id, CODEX_ROLLOUT_SOURCE_KIND).await,
        Some(2)
    );
}
