use crate::support;

use std::sync::Arc;
use std::time::Duration;

use calm_server::db::RepoRead;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::worker_flow::cursor::CODEX_ROLLOUT_SOURCE_KIND;
use calm_types::worker_flow::WorkerFlowItem;
use serde_json::{Value, json};

use support::worker_flow as wf;

#[tokio::test]
async fn codex_rollout_rewrite_with_changed_consumed_prefix_reingests_replacement_records() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-rewrite-prefix";
    let thread_id = "thread-rewrite-prefix";
    let seed = wf::seed_card_and_runtime(&repo, card_id, Some(thread_id)).await;
    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u-old-1", "old one"),
            wf::reasoning("r-old-2", "old two"),
            wf::assistant_message("a-old-3", "old three"),
        ],
    );

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wait_for_item_count(&repo, card_id, 3, Duration::from_secs(1)).await;
    wait_for_cursor(&repo, card_id, 4, Some("a-old-3")).await;
    token.cancel();
    handle.await.unwrap().unwrap();

    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::user_message("u-new-1", "new one"),
            wf::reasoning("r-new-2", "new two"),
            wf::assistant_message("a-new-3", "new three"),
            wf::function_call("call-new-4", "pwd"),
        ],
    );

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wait_for_item_count(&repo, card_id, 7, Duration::from_millis(120)).await;
    wait_for_cursor(&repo, card_id, 5, Some("call-new-4")).await;

    let items = flow_items(&repo, card_id).await;
    assert_eq!(
        source_uuids(&items[3..]),
        vec![
            Some("u-new-1"),
            Some("r-new-2"),
            Some("a-new-3"),
            Some("call-new-4")
        ]
    );
    assert_eq!(
        raw_ref_lines(&items[3..]),
        vec![Some(1), Some(2), Some(3), Some(4)]
    );

    token.cancel();
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn codex_rollout_rewrite_after_uuidless_prefix_uses_line_hash_and_reingests() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-rewrite-prefix-uuidless";
    let thread_id = "thread-rewrite-prefix-uuidless";
    let seed = wf::seed_card_and_runtime(&repo, card_id, Some(thread_id)).await;
    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::turn_context("turn-uuidless"),
            user_message_without_id("old one"),
            assistant_message_without_id("old two"),
        ],
    );

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wait_for_item_count(&repo, card_id, 2, Duration::from_secs(1)).await;
    let old_hash = wait_for_cursor_with_line_hash(&repo, card_id, 4, None, None).await;
    token.cancel();
    handle.await.unwrap().unwrap();

    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::turn_context("turn-uuidless"),
            user_message_without_id("new one"),
            assistant_message_without_id("new two"),
            assistant_message_without_id("new three"),
        ],
    );

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wait_for_item_count(&repo, card_id, 5, Duration::from_millis(120)).await;
    let new_hash = wait_for_cursor_with_line_hash(&repo, card_id, 5, None, Some(&old_hash)).await;
    assert_ne!(new_hash, old_hash);

    let items = flow_items(&repo, card_id).await;
    assert_eq!(
        message_texts(&items),
        vec!["old one", "old two", "new one", "new two", "new three"]
    );
    assert_eq!(source_uuids(&items), vec![None, None, None, None, None]);
    assert_eq!(raw_ref_lines(&items[2..]), vec![Some(2), Some(3), Some(4)]);

    token.cancel();
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn codex_rollout_same_uuidless_prefix_rewrite_keeps_cursor_without_reemit() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-rewrite-prefix-uuidless-same";
    let thread_id = "thread-rewrite-prefix-uuidless-same";
    let seed = wf::seed_card_and_runtime(&repo, card_id, Some(thread_id)).await;
    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);
    let consumed_prefix = [
        wf::session_meta(thread_id),
        wf::turn_context("turn-uuidless-same"),
        user_message_without_id("same one"),
        assistant_message_without_id("same two"),
    ];
    wf::write_rollout(&path, &consumed_prefix);

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wait_for_item_count(&repo, card_id, 2, Duration::from_secs(1)).await;
    let old_hash = wait_for_cursor_with_line_hash(&repo, card_id, 4, None, None).await;
    token.cancel();
    handle.await.unwrap().unwrap();

    wf::write_rollout(
        &path,
        &[
            consumed_prefix[0].clone(),
            consumed_prefix[1].clone(),
            consumed_prefix[2].clone(),
            consumed_prefix[3].clone(),
            assistant_message_without_id("new tail"),
        ],
    );

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wait_for_item_count(&repo, card_id, 3, Duration::from_millis(120)).await;
    let new_hash = wait_for_cursor_with_line_hash(&repo, card_id, 5, None, Some(&old_hash)).await;
    assert_ne!(new_hash, old_hash);

    let items = flow_items(&repo, card_id).await;
    assert_eq!(
        message_texts(&items),
        vec!["same one", "same two", "new tail"]
    );
    assert_eq!(source_uuids(&items), vec![None, None, None]);
    assert_eq!(raw_ref_lines(&items[2..]), vec![Some(4)]);

    token.cancel();
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn codex_rollout_rewrite_with_same_consumed_prefix_identity_does_not_reemit() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-rewrite-prefix-same";
    let thread_id = "thread-rewrite-prefix-same";
    let seed = wf::seed_card_and_runtime(&repo, card_id, Some(thread_id)).await;
    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);
    let consumed_prefix = [
        wf::session_meta(thread_id),
        wf::user_message("u-same-1", "same one"),
        wf::reasoning("r-same-2", "same two"),
        wf::assistant_message("a-same-3", "same three"),
    ];
    wf::write_rollout(&path, &consumed_prefix);

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wait_for_item_count(&repo, card_id, 3, Duration::from_secs(1)).await;
    wait_for_cursor(&repo, card_id, 4, Some("a-same-3")).await;
    token.cancel();
    handle.await.unwrap().unwrap();

    wf::write_rollout(
        &path,
        &[
            consumed_prefix[0].clone(),
            consumed_prefix[1].clone(),
            consumed_prefix[2].clone(),
            consumed_prefix[3].clone(),
            wf::assistant_message("a-same-4", "new tail"),
        ],
    );

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wait_for_item_count(&repo, card_id, 4, Duration::from_millis(120)).await;
    wait_for_cursor(&repo, card_id, 5, Some("a-same-4")).await;

    let items = flow_items(&repo, card_id).await;
    assert_eq!(
        source_uuids(&items),
        vec![
            Some("u-same-1"),
            Some("r-same-2"),
            Some("a-same-3"),
            Some("a-same-4")
        ]
    );
    assert_eq!(raw_ref_lines(&items[3..]), vec![Some(4)]);

    token.cancel();
    handle.await.unwrap().unwrap();
}

async fn wait_for_item_count(repo: &SqlxRepo, card_id: &str, expected: usize, timeout: Duration) {
    wf::wait_until(timeout, || async {
        repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
            .await
            .unwrap()
            .len()
            == expected
    })
    .await;
}

async fn wait_for_cursor(
    repo: &SqlxRepo,
    card_id: &str,
    record_index: i64,
    last_source_uuid: Option<&str>,
) {
    wf::wait_until(Duration::from_secs(1), || async {
        repo.worker_flow_cursor_get(card_id, CODEX_ROLLOUT_SOURCE_KIND)
            .await
            .unwrap()
            .is_some_and(|cursor| {
                cursor.record_index == record_index
                    && cursor.last_source_uuid.as_deref() == last_source_uuid
            })
    })
    .await;
}

async fn wait_for_cursor_with_line_hash(
    repo: &SqlxRepo,
    card_id: &str,
    record_index: i64,
    last_source_uuid: Option<&str>,
    previous_hash: Option<&str>,
) -> String {
    wf::wait_until(Duration::from_secs(1), || async {
        repo.worker_flow_cursor_get(card_id, CODEX_ROLLOUT_SOURCE_KIND)
            .await
            .unwrap()
            .is_some_and(|cursor| {
                cursor.record_index == record_index
                    && cursor.last_source_uuid.as_deref() == last_source_uuid
                    && cursor
                        .last_line_hash
                        .as_deref()
                        .is_some_and(|hash| Some(hash) != previous_hash)
            })
    })
    .await;
    repo.worker_flow_cursor_get(card_id, CODEX_ROLLOUT_SOURCE_KIND)
        .await
        .unwrap()
        .and_then(|cursor| cursor.last_line_hash)
        .unwrap()
}

async fn flow_items(repo: &SqlxRepo, card_id: &str) -> Vec<WorkerFlowItem> {
    repo.worker_flow_item_list_by_card(card_id, 0, 100, false)
        .await
        .unwrap()
        .into_iter()
        .map(|row| serde_json::from_str::<WorkerFlowItem>(&row.payload).unwrap())
        .collect()
}

fn source_uuids(items: &[WorkerFlowItem]) -> Vec<Option<&str>> {
    items
        .iter()
        .map(|item| item.env().source_uuid.as_deref())
        .collect()
}

fn raw_ref_lines(items: &[WorkerFlowItem]) -> Vec<Option<u64>> {
    items
        .iter()
        .map(|item| item.env().raw_ref.as_ref().and_then(|raw_ref| raw_ref.line))
        .collect()
}

fn user_message_without_id(text: &str) -> Value {
    json!({
        "timestamp": "2026-06-13T00:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": text }]
        }
    })
}

fn assistant_message_without_id(text: &str) -> Value {
    json!({
        "timestamp": "2026-06-13T00:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "phase": "final_answer",
            "content": [{ "type": "output_text", "text": text }]
        }
    })
}

fn message_texts(items: &[WorkerFlowItem]) -> Vec<&str> {
    items
        .iter()
        .map(|item| match item {
            WorkerFlowItem::UserMessage { content, .. } => content
                .iter()
                .find_map(|block| match block {
                    calm_types::worker_flow::MessageBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .unwrap(),
            WorkerFlowItem::AgentMessage { text, .. } => text.as_str(),
            _ => panic!("expected message item"),
        })
        .collect()
}
