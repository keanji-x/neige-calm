//! #2617: a report read leaves task blocks out unless it names them by id, and a whole write owns
//! only what a full read renders. Full and section reads render no task block and index none;
//! `blocks: [id]` returns one and anchors its `upsert`; `detail: "index"` lists every block. A
//! `neige_report_write` keeps every task block as it is and refuses a task fence.

#![cfg(unix)]

use crate::mcp_report_sections::{CONTRACT, INVALID_PARAMS, SECTIONS, payload};
use crate::mcp_track_report::{Boot, boot, call_tool, planner_identity};
use crate::mcp_track_report_blocks::{planner_task_payload, task_keys};
use calm_server::event::EditAuthor;
use calm_server::ids::ActorId;
use calm_server::mcp_server::ToolCallIdentity;
use calm_server::mcp_server::tools::track_report::TOOL_REPORT_READ;
use calm_server::mcp_server::tools::track_report_blocks::{TOOL_REPORT_COMMIT, TOOL_REPORT_WRITE};
use calm_server::track_report::{ReportBlock, TrackReportPayload, persist_report};
use calm_types::report_blocks::{KIND_TASK, marker_line, render_fence};
use serde_json::{Value, json};

async fn read(boot: &Boot, who: ToolCallIdentity, args: Value) -> Value {
    call_tool(boot, TOOL_REPORT_READ, who, args)
        .await
        .expect("neige_report_read")
}

fn index_kinds(read: &Value) -> Vec<String> {
    read["blocks"]
        .as_array()
        .expect("the blocks index")
        .iter()
        .map(|block| block["kind"].as_str().expect("kind").to_string())
        .collect()
}

fn task_blocks(blocks: &[ReportBlock]) -> Vec<ReportBlock> {
    blocks
        .iter()
        .filter(|block| block.kind == KIND_TASK)
        .cloned()
        .collect()
}

/// The contract sections with task `k1` inside `已完成` and task `k2` at the end, written as a
/// planner edit outside this session's read ledger (a previous session's, or before a restart), so
/// the session knows only what it reads. Returns the two task blocks as stored.
async fn seed(boot: &Boot) -> Vec<ReportBlock> {
    let fence = |key: &str| render_fence(KIND_TASK, &planner_task_payload(key, "build it"));
    let sections = SECTIONS.replace("beta\n", &format!("beta\n\n{}", fence("k1")));
    let body = format!("{CONTRACT}{sections}\n{}", fence("k2"));
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .unwrap();
    let current: TrackReportPayload = serde_json::from_value(card.payload.clone()).unwrap();
    let doc_rev = current.doc_rev;
    let route_repo: std::sync::Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    persist_report(
        route_repo.as_ref(),
        &boot.ctx.events,
        &boot.ctx.write,
        ActorId::Kernel,
        EditAuthor::Planner,
        track,
        card,
        current,
        TrackReportPayload::new("seed", &body),
        doc_rev,
        None,
    )
    .await
    .expect("seed the report");
    let blocks = payload(boot).await.blocks.expect("blocks");
    let tasks = task_blocks(&blocks);
    assert_eq!(tasks.len(), 2, "premise: two task blocks");
    let at = |id: &str| blocks.iter().position(|block| block.id == id).unwrap();
    assert!(
        calm_types::report_blocks::flat_text(&blocks[at(&tasks[0].id) - 1]).contains("beta")
            && at(&tasks[1].id) == blocks.len() - 1,
        "premise: k1 inside 已完成, k2 last"
    );
    tasks
}

#[tokio::test]
async fn a_full_read_renders_and_indexes_no_task_block() {
    let boot = boot().await;
    let tasks = seed(&boot).await;
    for args in [json!({}), json!({ "with_markers": true })] {
        let out = read(&boot, planner_identity(&boot), args.clone()).await;
        let text = out["text"].as_str().expect("a full read returns text");
        assert!(!text.contains("neige-block task"), "{args}: {text}");
        assert!(
            text.contains("# 已完成") && text.contains("gamma"),
            "{text}"
        );
        assert!(
            !index_kinds(&out).iter().any(|kind| kind == KIND_TASK),
            "{args}: {out}"
        );
    }
    let index = read(&boot, planner_identity(&boot), json!({ "detail": "index" })).await;
    let ids: Vec<&str> = index["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| block["kind"] == KIND_TASK)
        .map(|block| block["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        "the index read still lists every task block"
    );
}

#[tokio::test]
async fn a_section_read_indexes_no_task_block() {
    let boot = boot().await;
    seed(&boot).await;
    let out = read(
        &boot,
        planner_identity(&boot),
        json!({ "sections": ["已完成"] }),
    )
    .await;
    assert!(!out["text"].as_str().unwrap().contains("neige-block task"));
    assert!(
        !index_kinds(&out).iter().any(|kind| kind == KIND_TASK),
        "{out}"
    );
}

#[tokio::test]
async fn a_task_read_by_id_returns_it_and_anchors_its_upsert() {
    let boot = boot().await;
    let task = seed(&boot).await.remove(0);
    let edited = planner_task_payload("k1", "build it better");
    let upsert = json!({ "message": "revise k1",
        "ops": [{ "op": "upsert", "id": task.id, "kind": KIND_TASK, "payload": edited }] });

    read(&boot, planner_identity(&boot), json!({})).await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        upsert.clone(),
    )
    .await
    .expect_err("a full read is no read of a task block");
    assert_eq!(err.code, INVALID_PARAMS, "{err:?}");
    assert!(err.message.contains("has not been read"), "{err:?}");

    let out = read(
        &boot,
        planner_identity(&boot),
        json!({ "blocks": [task.id] }),
    )
    .await;
    assert_eq!(
        out["text"],
        json!(format!(
            "{}{}",
            marker_line(&task.id),
            render_fence(KIND_TASK, &planner_task_payload("k1", "build it"))
        ))
    );
    assert!(
        index_kinds(&out).iter().any(|kind| kind == KIND_TASK),
        "a read naming a task indexes it: {out}"
    );
    // A later full read, which renders no task, does not forget the unchanged task read by id.
    read(&boot, planner_identity(&boot), json!({})).await;
    call_tool(&boot, TOOL_REPORT_COMMIT, planner_identity(&boot), upsert)
        .await
        .expect("the read by id anchors the upsert");
    let stored = task_blocks(&payload(&boot).await.blocks.unwrap()).remove(0);
    assert_eq!(
        (stored.id.as_str(), stored.rev),
        (task.id.as_str(), task.rev + 1)
    );
    assert_eq!(stored.payload["goal"], "build it better");
}

#[tokio::test]
async fn a_whole_write_after_a_task_free_full_read_keeps_every_task_block() {
    let boot = boot().await;
    let tasks = seed(&boot).await;
    let marked = read(
        &boot,
        planner_identity(&boot),
        json!({ "with_markers": true }),
    )
    .await["text"]
        .as_str()
        .unwrap()
        .replace("gamma", "gamma2")
        .replace("beta", "beta2");
    assert!(
        !marked.contains("neige-block task"),
        "premise: the write carries no task: {marked}"
    );
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        planner_identity(&boot),
        json!({ "body": marked, "message": "rewrite" }),
    )
    .await
    .expect("a task-free full read anchors the whole write");

    let after = payload(&boot).await;
    assert!(after.body.contains("beta2") && after.body.contains("gamma2"));
    let blocks = after.blocks.expect("blocks");
    assert_eq!(
        task_blocks(&blocks),
        tasks,
        "every task block is kept: id, rev and payload"
    );
    let at = |id: &str| blocks.iter().position(|block| block.id == id).unwrap();
    let text = |index: usize| calm_types::report_blocks::flat_text(&blocks[index]);
    let k1 = at(&tasks[0].id);
    assert!(
        text(k1 - 1).contains("beta2") && text(k1 + 1).starts_with("# 决策"),
        "k1 stays after the block it followed: {}",
        after.body
    );
    assert_eq!(at(&tasks[1].id), blocks.len() - 1, "k2 stays last");
    assert_eq!(task_keys(&boot).await, ["k1", "k2"]);
}

#[tokio::test]
async fn a_whole_write_carrying_a_task_fence_is_refused() {
    let boot = boot().await;
    let tasks = seed(&boot).await;
    let marked = read(
        &boot,
        planner_identity(&boot),
        json!({ "with_markers": true }),
    )
    .await["text"]
        .as_str()
        .unwrap()
        .to_string();
    let before = payload(&boot).await;
    let new_task = render_fence(KIND_TASK, &planner_task_payload("k3", "new"));
    let copied = format!(
        "{}{}",
        marker_line(&tasks[0].id),
        render_fence(KIND_TASK, &tasks[0].payload)
    );
    for body in [format!("{marked}{new_task}"), format!("{marked}{copied}")] {
        let err = call_tool(
            &boot,
            TOOL_REPORT_WRITE,
            planner_identity(&boot),
            json!({ "body": body, "message": "smuggle a task" }),
        )
        .await
        .expect_err("a whole write owns no task block");
        assert_eq!(err.code, INVALID_PARAMS, "{err:?}");
        assert!(
            err.message.contains("`task` block") && err.message.contains("`upsert` op"),
            "{err:?}"
        );
        assert_eq!(payload(&boot).await, before, "nothing written");
    }
    assert_eq!(task_keys(&boot).await, ["k1", "k2"]);
}
