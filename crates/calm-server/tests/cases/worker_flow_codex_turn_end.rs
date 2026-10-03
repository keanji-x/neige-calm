//! #1968: a codex worker turn's end, and the upstream error that ended it, reach the Planner-readable
//! `cards/<id>/conversation.md` from the worker's own rollout record.

use crate::support;

use std::sync::Arc;

use calm_server::db::sqlite::SqlxRepo;

use support::worker_flow as wf;

const FORBIDDEN_403: &str = "unexpected status 403 Forbidden: <!doctype html><meta charset=\"utf-8\"><meta name=viewport content=\"width=device-width, initial-scale=1\"><title>403</title>403 Forbidden, url: https://upstream.example/v1/responses";

#[tokio::test]
async fn codex_failed_turn_end_and_its_error_show_in_conversation_md() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-turn-end-failed";
    let thread_id = "thread-turn-end-failed";
    let seed = wf::seed_card_and_runtime(&repo, card_id, Some(thread_id)).await;
    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::turn_context("turn-1"),
            wf::user_message("msg-user", "do the task"),
            wf::task_complete("turn-1", Some(FORBIDDEN_403)),
        ],
    );

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_for_codex_cursor(&repo, card_id, 4).await;
    token.cancel();
    handle.await.unwrap().unwrap();

    let conversation = wf::cat_conversation(&repo, &seed.card).await;
    assert!(
        conversation.contains(
            "- Turn ended: failed — unexpected status 403 Forbidden: 403 Forbidden, url: https://upstream.example/v1/responses\n"
        ),
        "conversation.md = {conversation}"
    );
}

#[tokio::test]
async fn codex_long_turn_error_is_not_cut_in_conversation_md() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-turn-end-long";
    let thread_id = "thread-turn-end-long";
    let seed = wf::seed_card_and_runtime(&repo, card_id, Some(thread_id)).await;
    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);
    // Longer than the 120-character cut other conversation lines get; its tail says when to retry.
    let usage_limit = concat!(
        "You've hit your usage limit. Visit https://chatgpt.example/codex/settings/usage ",
        "to purchase more credits or try again at Sep 19th, 2026 4:22 PM."
    );
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::turn_context("turn-1"),
            wf::user_message("msg-user", "do the task"),
            wf::task_complete("turn-1", Some(usage_limit)),
        ],
    );

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_for_codex_cursor(&repo, card_id, 4).await;
    token.cancel();
    handle.await.unwrap().unwrap();

    let conversation = wf::cat_conversation(&repo, &seed.card).await;
    assert!(
        conversation.contains(&format!("- Turn ended: failed — {usage_limit}\n")),
        "conversation.md = {conversation}"
    );
}

#[tokio::test]
async fn codex_completed_and_aborted_turn_ends_show_in_conversation_md() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let card_id = "card-turn-end-ok";
    let thread_id = "thread-turn-end-ok";
    let seed = wf::seed_card_and_runtime(&repo, card_id, Some(thread_id)).await;
    let codex_home = tempfile::tempdir().unwrap();
    let path = wf::rollout_path(codex_home.path(), thread_id);
    wf::write_rollout(
        &path,
        &[
            wf::session_meta(thread_id),
            wf::turn_context("turn-1"),
            wf::user_message("msg-user-1", "first"),
            wf::assistant_message("msg-assistant-1", "done"),
            wf::task_complete("turn-1", None),
            wf::turn_context("turn-2"),
            wf::user_message("msg-user-2", "second"),
            wf::turn_aborted("turn-2", "interrupted"),
        ],
    );

    let (token, handle) =
        wf::spawn_source_with_path(repo.clone(), seed.runtime.clone(), &seed, &path);
    wf::wait_for_codex_cursor(&repo, card_id, 8).await;
    token.cancel();
    handle.await.unwrap().unwrap();

    let conversation = wf::cat_conversation(&repo, &seed.card).await;
    let turn_1 = conversation.find("### Turn 1").expect("turn 1 heading");
    let completed = conversation
        .find("- Turn ended: completed\n")
        .unwrap_or_else(|| panic!("completed turn end; conversation.md = {conversation}"));
    let turn_2 = conversation.find("### Turn 2").expect("turn 2 heading");
    let aborted = conversation
        .find("- Turn ended: aborted (interrupted)\n")
        .unwrap_or_else(|| panic!("aborted turn end; conversation.md = {conversation}"));
    assert!(
        turn_1 < completed && completed < turn_2 && turn_2 < aborted,
        "conversation.md = {conversation}"
    );
}
