//! #2598: a task block belongs to no H1 section, wherever it sits in block order. Section reads,
//! their write anchors and section ops cover a section's other blocks only; a task is read by id
//! and written by an `upsert` op.

#![cfg(unix)]

use crate::mcp_report_sections::{
    CONTRACT, INVALID_PARAMS, SECTIONS, commit, payload, read_full, read_sections, replace, seed,
};
use crate::mcp_track_report::{Boot, boot, planner_identity, read_then_write_markdown};
use crate::mcp_track_report_blocks::{planner_task_payload, task_keys};
use crate::task_projection_acceptance::user_upsert;
use calm_server::track_report::ReportBlock;
use calm_types::report_blocks::{KIND_TASK, render_fence};
use serde_json::json;

fn task_fence(key: &str) -> String {
    render_fence(KIND_TASK, &planner_task_payload(key, "build it"))
}

/// The planner seeds the contract sections with its live task `k1` inside `已完成`.
async fn seed_task_in_done(boot: &Boot) {
    let sections = SECTIONS.replace("beta\n", &format!("beta\n\n{}", task_fence("k1")));
    read_then_write_markdown(
        boot,
        planner_identity(boot),
        json!({ "body": format!("{CONTRACT}{sections}"), "message": "seed" }),
    )
    .await
    .expect("the planner seeds its task");
}

async fn task_block(boot: &Boot) -> ReportBlock {
    let blocks = payload(boot).await.blocks.expect("blocks");
    let mut tasks = blocks.into_iter().filter(|block| block.kind == KIND_TASK);
    let task = tasks.next().expect("a task block");
    assert!(tasks.next().is_none(), "one task block");
    task
}

#[tokio::test]
async fn section_ops_leave_a_live_task_in_their_span_untouched() {
    let boot = boot().await;
    seed_task_in_done(&boot).await;
    read_full(&boot).await;
    let task = task_block(&boot).await;

    commit(
        &boot,
        planner_identity(&boot),
        replace("已完成", "# 已完成\n\nbeta2\n"),
    )
    .await
    .expect("a replace realigns the section's own blocks only");
    assert_eq!(task_block(&boot).await, task, "same id, rev and payload");
    let body = payload(&boot).await.body;
    assert!(
        body.contains(&format!("# 已完成\n\nbeta2\n{}# 决策\n", task_fence("k1"))),
        "the task stays in the span, after the section's blocks: {body}"
    );
    let edited = planner_task_payload("k1", "build it better");
    let edited_fence = render_fence(KIND_TASK, &edited);
    commit(
        &boot,
        planner_identity(&boot),
        json!({ "ops": [{ "op": "upsert", "id": task.id, "kind": "task", "payload": edited }] }),
    )
    .await
    .expect("the replace keeps the session's read of the task: upsert it by id, no re-read");
    let task = task_block(&boot).await;
    assert_eq!(task.payload["goal"], json!("build it better"));

    commit(
        &boot,
        planner_identity(&boot),
        json!({ "ops": [{ "op": "delete", "section": "已完成" }] }),
    )
    .await
    .expect("a delete removes the section's own blocks only");
    assert_eq!(task_block(&boot).await, task);
    let body = payload(&boot).await.body;
    assert!(!body.contains("# 已完成"), "{body}");
    assert!(
        body.contains(&format!("a2\n\n{edited_fence}# 决策\n")),
        "{body}"
    );
    assert_eq!(task_keys(&boot).await, vec!["k1".to_string()]);
}

#[tokio::test]
async fn a_task_appended_without_position_joins_no_section() {
    let boot = boot().await;
    seed(&boot, SECTIONS).await;
    read_full(&boot).await;
    commit(
        &boot,
        planner_identity(&boot),
        json!({ "ops": [{ "op": "upsert", "kind": "task", "payload": planner_task_payload("k1", "build it") }] }),
    )
    .await
    .expect("create the task with no position");
    assert!(
        payload(&boot).await.body.ends_with(&task_fence("k1")),
        "premise: appended after the last section's blocks"
    );

    let text = read_sections(&boot, planner_identity(&boot), &["决策"]).await;
    assert!(!text.contains("neige-block task"), "{text}");
    commit(
        &boot,
        planner_identity(&boot),
        replace("决策", "# 决策\n\ngamma2\n"),
    )
    .await
    .expect("the last section is replaced without the task");
    let body = payload(&boot).await.body;
    assert!(
        body.ends_with(&format!("# 决策\n\ngamma2\n{}", task_fence("k1"))),
        "{body}"
    );
    assert_eq!(task_keys(&boot).await, vec!["k1".to_string()]);
}

#[tokio::test]
async fn another_writers_task_change_does_not_invalidate_a_section_read() {
    let boot = boot().await;
    seed_task_in_done(&boot).await;
    let text = read_sections(&boot, planner_identity(&boot), &["已完成"]).await;
    assert!(!text.contains("neige-block task"), "{text}");
    let task = task_block(&boot).await;

    let mut flipped = planner_task_payload("k1", "build it");
    flipped["ready"] = json!(false);
    user_upsert(&boot, &task.id, u64::from(task.rev), flipped).await;
    let changed = task_block(&boot).await;
    assert_eq!(
        (changed.id.as_str(), changed.rev),
        (task.id.as_str(), task.rev + 1),
        "premise: the user changed the task in the section's span"
    );

    commit(
        &boot,
        planner_identity(&boot),
        replace("已完成", &text.replace("beta", "beta2")),
    )
    .await
    .expect("the section read still anchors the section");
    assert_eq!(task_block(&boot).await, changed, "the user's task stays");
    assert!(payload(&boot).await.body.contains("beta2"));
}

#[tokio::test]
async fn a_section_replace_carrying_a_task_fence_is_refused() {
    let boot = boot().await;
    seed(&boot, SECTIONS).await;
    read_sections(&boot, planner_identity(&boot), &["决策"]).await;
    let before = payload(&boot).await;
    let err = commit(
        &boot,
        planner_identity(&boot),
        replace("决策", &format!("# 决策\n\ngamma\n\n{}", task_fence("k1"))),
    )
    .await
    .expect_err("a task belongs to no section");
    assert_eq!(err.code, INVALID_PARAMS, "{err:?}");
    assert!(
        err.message.contains("`task` block") && err.message.contains("`upsert` op"),
        "{err:?}"
    );
    assert_eq!(payload(&boot).await.body, before.body, "nothing written");
    assert!(task_keys(&boot).await.is_empty());
}

#[tokio::test]
async fn a_task_copied_with_its_marker_into_a_section_replace_gets_the_upsert_hint() {
    let boot = boot().await;
    seed_task_in_done(&boot).await;
    read_sections(&boot, planner_identity(&boot), &["已完成"]).await;
    let task = task_block(&boot).await;
    let before = payload(&boot).await;
    let err = commit(
        &boot,
        planner_identity(&boot),
        replace(
            "已完成",
            &format!(
                "# 已完成\n\nbeta\n\n<!-- neige:{} -->\n{}",
                task.id,
                task_fence("k1")
            ),
        ),
    )
    .await
    .expect_err("a task belongs to no section, even with its marker");
    assert_eq!(err.code, INVALID_PARAMS, "{err:?}");
    assert!(
        err.message.contains("`task` block") && err.message.contains("`upsert` op"),
        "the task hint, not the marker refusal: {err:?}"
    );
    assert_eq!(payload(&boot).await.body, before.body, "nothing written");
    assert_eq!(task_block(&boot).await, task);
}
