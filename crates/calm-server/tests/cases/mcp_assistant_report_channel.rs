//! What a `CardRole::Assistant` token can and cannot do on the block channel, driven through
//! the real tool handlers, decision sink, and recorder gate. Every negative has a Planner-token
//! control next to it.

#![cfg(unix)]

use crate::mcp_track_report::{
    Boot, assistant_identity, boot, call_tool, planner_identity, worker_identity,
};
use calm_server::event::{EditAuthor, Event};
use calm_server::mcp_server::registry::ToolCallIdentity;
use calm_server::mcp_server::tools::track_report_blocks::{
    TOOL_REPORT_BLOCKS_DELETE, TOOL_REPORT_BLOCKS_KINDS, TOOL_REPORT_BLOCKS_MOVE,
    TOOL_REPORT_BLOCKS_UPSERT, TOOL_REPORT_WRITE_MARKDOWN,
};
use calm_server::model::{TrackLifecycle, TrackPatch};
use calm_server::plugin_host::mcp::RpcError;
use calm_types::report_blocks::{KIND_TASK, marker_line, render_fence};
use serde_json::{Value, json};

const TOOL_REPORT_READ: &str = "calm.report.read";

async fn read(boot: &Boot, identity: ToolCallIdentity, args: Value) -> Value {
    call_tool(boot, TOOL_REPORT_READ, identity, args)
        .await
        .expect("report read succeeds")
}

async fn doc_rev(boot: &Boot) -> u64 {
    read(boot, planner_identity(boot), json!({})).await["docRev"]
        .as_u64()
        .expect("docRev is numeric")
}

async fn body_text(boot: &Boot) -> String {
    read(boot, planner_identity(boot), json!({}))
        .await
        .get("text")
        .and_then(Value::as_str)
        .expect("read returns text")
        .to_string()
}

async fn lifecycle(boot: &Boot) -> TrackLifecycle {
    boot.repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .expect("track row")
        .lifecycle
}

async fn set_lifecycle(boot: &Boot, to: TrackLifecycle) {
    boot.repo
        .track_update(
            boot.track_id.as_str(),
            TrackPatch {
                lifecycle: Some(to),
                ..Default::default()
            },
        )
        .await
        .expect("set fixture lifecycle");
}

/// The `author` of every `track.report_edited` in the persisted log, oldest first — attribution
/// is what lands in the log, not what the tool returns.
async fn report_edit_authors(boot: &Boot) -> Vec<EditAuthor> {
    boot.repo
        .events_since(0, i64::MAX)
        .await
        .expect("read event log")
        .into_iter()
        .filter_map(|(_, _, _, event)| match event {
            Event::TrackReportEdited { author, .. } => Some(author),
            _ => None,
        })
        .collect()
}

fn task_fence(declared_by: &str, key: &str) -> String {
    render_fence(
        KIND_TASK,
        &json!({
            "key": key,
            "kind": "codex",
            "goal": format!("{declared_by} wants {key}"),
            "ready": true,
            "declared_by": declared_by,
        }),
    )
}

/// A gate-clean task declaration: it carries a `no_gate_reason`, so its only remaining barrier
/// to schedulability is the `ready` flag. `task_fence` has neither and is unschedulable regardless.
fn gated_task_fence(key: &str, ready: bool) -> String {
    render_fence(
        KIND_TASK,
        &json!({
            "key": key,
            "kind": "codex",
            "goal": "ship it",
            "ready": ready,
            "declared_by": "spec",
            "no_gate_reason": "fixture: this key needs no verification gate",
        }),
    )
}

/// `(key, status)` of every row in the track's task projection — what "a schedulable task" means.
async fn task_rows(boot: &Boot) -> Vec<(String, String)> {
    let pool = boot.repo.sqlite_pool().expect("sqlite-backed fixture repo");
    sqlx::query_as::<_, (String, String)>(
        "SELECT key, status FROM tasks WHERE track_id = ?1 ORDER BY key",
    )
    .bind(boot.track_id.as_str())
    .fetch_all(&pool)
    .await
    .expect("read the task projection")
}

/// Seed prose plus two live task declarations, one planner-signed and one user-signed; the
/// user-signed one is already protected from every non-user writer, so only the planner-signed
/// one exercises the assistant rule.
async fn seed_prose_and_two_tasks(boot: &Boot) -> (String, String) {
    let planner_fence = task_fence("spec", "build");
    let user_fence = task_fence("user", "review");

    let planner_body = format!("# Plan\n\nthe original prose\n\n{planner_fence}");
    call_tool(
        boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        planner_identity(boot),
        json!({ "body": planner_body, "summary": "seed", "if_doc_rev": doc_rev(boot).await }),
    )
    .await
    .expect("planner declares its task");

    // The user's own declaration goes through the persist boundary with `EditAuthor::User`.
    let with_user = format!("{}\n{user_fence}", body_text(boot).await.trim_end());
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .expect("track row");
    let card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .expect("report card row");
    let current: calm_server::track_report::TrackReportPayload =
        serde_json::from_value(card.payload.clone()).expect("report payload");
    let next = calm_server::track_report::TrackReportPayload::new("seed", &with_user);
    let route_repo: std::sync::Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    calm_server::track_report::persist_report(
        route_repo.as_ref(),
        &boot.ctx.events,
        &boot.ctx.write,
        calm_server::ids::ActorId::User,
        EditAuthor::User,
        track,
        card,
        current,
        next,
        doc_rev(boot).await,
        None,
        None,
        false,
    )
    .await
    .expect("user declares its own task");

    let text = body_text(boot).await;
    assert!(
        text.contains(&planner_fence) && text.contains(&user_fence),
        "the fixture must really hold both declarations, else P2's positive \
         case proves nothing: {text}"
    );
    (planner_fence, user_fence)
}

/// The report as `write_markdown` wants it: every block preceded by its marker.
async fn marked_text(boot: &Boot, identity: ToolCallIdentity) -> String {
    read(boot, identity, json!({ "with_markers": true })).await["text"]
        .as_str()
        .expect("marked read returns text")
        .to_string()
}

#[tokio::test]
async fn assistant_drives_the_whole_block_channel() {
    let boot = boot().await;

    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_KINDS,
        assistant_identity(&boot),
        json!({}),
    )
    .await
    .expect("blocks.kinds serves an assistant");

    let created = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        assistant_identity(&boot),
        json!({
            "kind": "prose",
            "markdown": "# Assistant note\n\nfirst pass\n",
            "if_doc_rev": doc_rev(&boot).await
        }),
    )
    .await
    .expect("blocks.upsert create serves an assistant");
    let id = created["id"].as_str().expect("created id").to_string();
    let rev = created["rev"].as_u64().expect("created rev");

    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        assistant_identity(&boot),
        json!({
            "id": id,
            "kind": "prose",
            "markdown": "# Assistant note\n\nsecond pass\n",
            "if_rev": rev
        }),
    )
    .await
    .expect("blocks.upsert replace serves an assistant");

    // block 0 is the contract header block; the funnel rejects displacing it
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_MOVE,
        assistant_identity(&boot),
        json!({ "id": id, "to_index": 1, "if_doc_rev": doc_rev(&boot).await }),
    )
    .await
    .expect("blocks.move serves an assistant");

    let marked = marked_text(&boot, assistant_identity(&boot)).await;
    call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        assistant_identity(&boot),
        json!({ "body": marked, "if_doc_rev": doc_rev(&boot).await }),
    )
    .await
    .expect("write_markdown serves an assistant");

    let current_rev = read(&boot, planner_identity(&boot), json!({}))
        .await
        .get("blocks")
        .and_then(Value::as_array)
        .expect("blocks index")
        .iter()
        .find(|block| block["id"].as_str() == Some(id.as_str()))
        .map(|block| block["rev"].as_u64().expect("rev"))
        .expect("the assistant's block survived the round trip");
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_DELETE,
        assistant_identity(&boot),
        json!({ "id": id, "if_rev": current_rev }),
    )
    .await
    .expect("an assistant may delete a PROSE block it wrote");
    assert!(
        !body_text(&boot).await.contains("second pass"),
        "the prose block is really gone"
    );
}

/// The block channel is opened by exactly one role, not "for agents".
#[tokio::test]
async fn worker_is_still_refused_at_the_block_channel_entry() {
    let boot = boot().await;
    for tool in [
        TOOL_REPORT_BLOCKS_KINDS,
        TOOL_REPORT_BLOCKS_UPSERT,
        TOOL_REPORT_BLOCKS_MOVE,
        TOOL_REPORT_BLOCKS_DELETE,
        TOOL_REPORT_WRITE_MARKDOWN,
    ] {
        let err = call_tool(&boot, tool, worker_identity(&boot), json!({}))
            .await
            .err()
            .unwrap_or_else(|| panic!("{tool}: a worker token must be refused"));
        assert_eq!(err.code, RpcError::INVALID_PARAMS, "{tool}: {err:?}");
        assert!(
            err.message.contains("tool requires role"),
            "{tool} must refuse for the ROLE reason, not on argument parsing \
             (both are -32602): {}",
            err.message
        );
    }
}

#[tokio::test]
async fn an_assistant_block_write_is_persisted_as_edit_author_assistant() {
    let boot = boot().await;

    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        json!({ "kind": "prose", "markdown": "# Planner\n\nspec text\n", "if_doc_rev": doc_rev(&boot).await }),
    )
    .await
    .expect("planner writes first");
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        assistant_identity(&boot),
        json!({ "kind": "prose", "markdown": "# Assistant\n\nassistant text\n", "if_doc_rev": doc_rev(&boot).await }),
    )
    .await
    .expect("assistant writes second");

    assert_eq!(
        report_edit_authors(&boot).await,
        vec![EditAuthor::Planner, EditAuthor::Assistant],
        "the sink must attribute by role: hard-coding `EditAuthor::Planner` \
         would make the assistant's edit indistinguishable from the planner's \
         in the log, the goldens, and the planner-wake decision"
    );
}

#[tokio::test]
async fn an_assistant_block_write_does_not_promote_a_draft_track() {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Draft).await;

    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        assistant_identity(&boot),
        json!({ "kind": "prose", "markdown": "# Assistant\n\nnotes\n", "if_doc_rev": doc_rev(&boot).await }),
    )
    .await
    .expect("the write itself must succeed — P1 suppresses the promotion, not the write");

    assert_eq!(
        lifecycle(&boot).await,
        TrackLifecycle::Draft,
        "an assistant must not walk the track out of Draft; auto-promote is \
         one of the two implicit routes from the block channel into the \
         state machine (§3.2a)"
    );
}

/// Control: auto-promote is suppressed *for the assistant*, not removed.
#[tokio::test]
async fn a_planner_block_write_still_promotes_a_draft_track() {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Draft).await;

    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        json!({ "kind": "prose", "markdown": "# Planner\n\nnotes\n", "if_doc_rev": doc_rev(&boot).await }),
    )
    .await
    .expect("planner block write succeeds");

    assert_eq!(lifecycle(&boot).await, TrackLifecycle::Planning);
}

/// Keeps the rule from being written as "the write must not contain task blocks".
#[tokio::test]
async fn an_assistant_may_rewrite_prose_around_user_and_planner_task_blocks() {
    let boot = boot().await;
    let (planner_fence, user_fence) = seed_prose_and_two_tasks(&boot).await;

    let marked = marked_text(&boot, assistant_identity(&boot)).await;
    let rewritten = marked.replace("the original prose", "the assistant's rewrite");
    assert_ne!(rewritten, marked, "the rewrite must actually change prose");

    call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        assistant_identity(&boot),
        json!({ "body": rewritten, "if_doc_rev": doc_rev(&boot).await }),
    )
    .await
    .expect(
        "a prose-only rewrite that carries both task declarations through \
         unchanged must go through — P2 is per-block equivalence, not \
         'no task blocks in the result'",
    );

    let text = body_text(&boot).await;
    assert!(text.contains("the assistant's rewrite"), "prose changed");
    assert!(
        text.contains(&planner_fence) && text.contains(&user_fence),
        "both declarations survived byte-for-byte: {text}"
    );
    assert_eq!(
        report_edit_authors(&boot).await.last(),
        Some(&EditAuthor::Assistant)
    );
}

#[tokio::test]
async fn an_assistant_may_not_declare_a_task_block() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        assistant_identity(&boot),
        json!({
            "kind": KIND_TASK,
            "payload": {
                "key": "sneaky",
                "kind": "codex",
                "goal": "dispatch a worker",
                "ready": true,
                "declared_by": "assistant"
            },
            "if_doc_rev": doc_rev(&boot).await
        }),
    )
    .await
    .expect_err("an assistant declaring a task must be refused");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(
        !body_text(&boot).await.contains("sneaky"),
        "and nothing was written"
    );
}

/// The refused assistant write is one that would have produced a dispatchable task, and the
/// planner control proves this fixture can produce one. The guard is not redundant with the
/// role gate behind it: that gate fires only when the edit changes the projected key set, so an
/// in-place rewrite of a task's `goal` never reaches it.
#[tokio::test]
async fn an_assistant_may_not_flip_a_planner_task_to_ready() {
    let boot = boot().await;

    // 1. Planner seeds a gate-clean but withdrawn declaration.
    let withheld = gated_task_fence("dispatchable", false);
    let released = gated_task_fence("dispatchable", true);
    call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        planner_identity(&boot),
        json!({
            "body": format!("# Plan\n\nthe original prose\n\n{withheld}"),
            "summary": "seed",
            "if_doc_rev": doc_rev(&boot).await,
        }),
    )
    .await
    .expect("the planner may declare a not-yet-ready task");
    assert_eq!(
        task_rows(&boot).await,
        Vec::<(String, String)>::new(),
        "a `ready: false` declaration projects no task row — this is the \
         baseline the next two steps are measured against"
    );

    // 2. The assistant flips exactly that block to ready.
    let marked = marked_text(&boot, assistant_identity(&boot)).await;
    let flipped = marked.replace(&withheld, &released);
    assert_ne!(
        flipped, marked,
        "the fixture's fence must round-trip byte-for-byte, or step 2 is not \
         actually flipping `ready`"
    );
    let before = body_text(&boot).await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        assistant_identity(&boot),
        json!({ "body": flipped.clone(), "if_doc_rev": doc_rev(&boot).await }),
    )
    .await
    .expect_err("an assistant releasing a planner-declared task must be refused");
    // Assert the guard's own message, not just the code: without the guard this write is stopped
    // later by the role gate's dispatch-request arm, and the fixture would silently stop pinning it.
    assert!(
        err.message
            .contains("an assistant may not modify task block"),
        "the refusal must be P2's, not an incidental one from a layer \
         further in: {err:?}"
    );
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert_eq!(
        before,
        body_text(&boot).await,
        "the refused write left the report untouched"
    );
    assert_eq!(
        task_rows(&boot).await,
        Vec::<(String, String)>::new(),
        "and produced no dispatchable task"
    );

    // 3. Control: the planner makes the identical edit and a task appears.
    call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        planner_identity(&boot),
        json!({ "body": flipped, "if_doc_rev": doc_rev(&boot).await }),
    )
    .await
    .expect("the planner releases its own declaration");
    assert_eq!(
        task_rows(&boot).await,
        vec![("dispatchable".to_string(), "pending".to_string())],
        "the control group proves this exact edit does produce a schedulable \
         task, so step 2's empty projection is the guard's doing"
    );
}

/// All three shapes funnel through the same before/after diff. `before == after` on the body
/// suffices: the task projection is a pure function of the report document.
#[tokio::test]
async fn an_assistant_may_not_modify_or_delete_an_existing_task_block() {
    let boot = boot().await;
    let (planner_fence, user_fence) = seed_prose_and_two_tasks(&boot).await;
    let before = body_text(&boot).await;

    let index = read(&boot, assistant_identity(&boot), json!({})).await;
    let blocks = index["blocks"].as_array().expect("blocks index").clone();
    let tasks: Vec<(String, u64)> = blocks
        .iter()
        .filter(|block| block["kind"].as_str() == Some(KIND_TASK))
        .map(|block| {
            (
                block["id"].as_str().unwrap().to_string(),
                block["rev"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(tasks.len(), 2, "fixture holds the planner and user tasks");

    // 1. In-place rewrite of the PLANNER-signed declaration (the user-only rule does not cover it).
    let (planner_task_id, planner_task_rev) = {
        let marked = marked_text(&boot, assistant_identity(&boot)).await;
        let planner_marker_owner = tasks
            .iter()
            .find(|(id, _)| {
                let marker = marker_line(id);
                marked
                    .split_once(&marker)
                    .is_some_and(|(_, rest)| rest.starts_with(&planner_fence))
            })
            .expect("locate the planner-declared task block");
        planner_marker_owner.clone()
    };
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        assistant_identity(&boot),
        json!({
            "id": planner_task_id,
            "kind": KIND_TASK,
            "payload": {
                "key": "build",
                "kind": "codex",
                "goal": "rewritten by the assistant",
                "ready": true,
                "declared_by": "spec"
            },
            "if_rev": planner_task_rev
        }),
    )
    .await
    .expect_err("an assistant rewriting a planner-declared task must be refused");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);

    // 2. Block-level delete.
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_DELETE,
        assistant_identity(&boot),
        json!({ "id": planner_task_id, "if_rev": planner_task_rev }),
    )
    .await
    .expect_err("an assistant deleting a task block must be refused");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);

    // 3. Whole-document rewrite that simply drops both fences.
    let err = call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        assistant_identity(&boot),
        json!({ "body": "# Plan\n\nno more tasks\n", "if_doc_rev": doc_rev(&boot).await }),
    )
    .await
    .expect_err("a whole-document write may not launder a task deletion");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);

    let after = body_text(&boot).await;
    assert_eq!(before, after, "none of the three attempts wrote anything");
    assert!(after.contains(&planner_fence) && after.contains(&user_fence));
}

/// Control: without this, every assertion above would hold if task blocks had simply become immutable.
#[tokio::test]
async fn the_planner_may_still_rewrite_its_own_task_block() {
    let boot = boot().await;
    seed_prose_and_two_tasks(&boot).await;
    let blocks = read(&boot, planner_identity(&boot), json!({})).await["blocks"]
        .as_array()
        .expect("blocks index")
        .clone();
    let mut rewritten = false;
    for block in blocks
        .iter()
        .filter(|block| block["kind"].as_str() == Some(KIND_TASK))
    {
        let out = call_tool(
            &boot,
            TOOL_REPORT_BLOCKS_UPSERT,
            planner_identity(&boot),
            json!({
                "id": block["id"],
                "kind": KIND_TASK,
                "payload": {
                    "key": "build",
                    "kind": "codex",
                    "goal": "planner revises its own goal",
                    "ready": true,
                    "declared_by": "spec"
                },
                "if_rev": block["rev"]
            }),
        )
        .await;
        // The user-declared block legitimately refuses (`key` and `declared_by` are immutable); the
        // planner-declared one must go through.
        if out.is_ok() {
            rewritten = true;
        }
    }
    assert!(
        rewritten,
        "the planner must still be able to rewrite its own declaration — \
         otherwise the assistant refusals above are not about the role"
    );
    assert!(
        body_text(&boot)
            .await
            .contains("planner revises its own goal")
    );
}
