//! #1189 S2 — what an `CardRole::Assistant` token can and cannot do once
//! the block channel is open (§3.2b), driven through the real tool
//! handlers, the real decision sink, and the real recorder gate.
//!
//! | gate | assertion here |
//! |---|---|
//! | G-B2 | an assistant drives `blocks.upsert` / `.move` / `.delete` / `write_markdown` end to end; a Worker token is still refused at the entry |
//! | §3.4 | its edits persist as `EditAuthor::Assistant`, never as the spec |
//! | P1   | an assistant's block write leaves a Draft wave in Draft |
//! | P2   | its writes may not create, modify, or delete a task block — including the whole-document shapes — while a prose-only rewrite that carries the task fences through unchanged succeeds |
//!
//! Every negative here has a Spec-token control next to it. Without one,
//! "the assistant could not do X" would stay green if X had simply stopped
//! working for everybody.

#![cfg(unix)]

use crate::mcp_wave_report::{
    Boot, assistant_identity, boot, call_tool, spec_identity, worker_identity,
};
use calm_server::event::{EditAuthor, Event};
use calm_server::mcp_server::registry::ToolCallIdentity;
use calm_server::mcp_server::tools::wave_report_blocks::{
    TOOL_REPORT_BLOCKS_DELETE, TOOL_REPORT_BLOCKS_KINDS, TOOL_REPORT_BLOCKS_MOVE,
    TOOL_REPORT_BLOCKS_UPSERT, TOOL_REPORT_WRITE_MARKDOWN,
};
use calm_server::model::{WaveLifecycle, WavePatch};
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
    read(boot, spec_identity(boot), json!({})).await["docRev"]
        .as_u64()
        .expect("docRev is numeric")
}

async fn body_text(boot: &Boot) -> String {
    read(boot, spec_identity(boot), json!({}))
        .await
        .get("text")
        .and_then(Value::as_str)
        .expect("read returns text")
        .to_string()
}

async fn lifecycle(boot: &Boot) -> WaveLifecycle {
    boot.repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .expect("wave row")
        .lifecycle
}

async fn set_lifecycle(boot: &Boot, to: WaveLifecycle) {
    boot.repo
        .wave_update(
            boot.wave_id.as_str(),
            WavePatch {
                lifecycle: Some(to),
                ..Default::default()
            },
        )
        .await
        .expect("set fixture lifecycle");
}

/// The `author` of every `wave.report_edited` in the persisted log, oldest
/// first. Reading the stored event (not the tool's return value) is the
/// point: attribution is what lands in the log, goldens, and the
/// spec-wake decision.
async fn report_edit_authors(boot: &Boot) -> Vec<EditAuthor> {
    boot.repo
        .events_since(0, i64::MAX)
        .await
        .expect("read event log")
        .into_iter()
        .filter_map(|(_, _, _, event)| match event {
            Event::WaveReportEdited { author, .. } => Some(author),
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

/// Seed the report with prose plus **two** live task declarations, one
/// signed by the spec and one by the user.
///
/// Both authors are needed by §3.2a P2's positive case: `#1180` already
/// protects the user-signed one from every non-user writer, so a fixture
/// with only a user task would let the P2 test pass on the strength of a
/// guard S2 did not write. The spec-signed task is the one only P2 covers.
async fn seed_prose_and_two_tasks(boot: &Boot) -> (String, String) {
    let spec_fence = task_fence("spec", "build");
    let user_fence = task_fence("user", "review");

    let spec_body = format!("# Plan\n\nthe original prose\n\n{spec_fence}");
    call_tool(
        boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        spec_identity(boot),
        json!({ "body": spec_body, "summary": "seed", "if_doc_rev": doc_rev(boot).await }),
    )
    .await
    .expect("spec declares its task");

    // The user's own declaration goes through the persist boundary with
    // `EditAuthor::User` — the attribution rules pin `declared_by` to the
    // writer, so there is no way to mint a user task as anyone else.
    let with_user = format!("{}\n{user_fence}", body_text(boot).await.trim_end());
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .expect("wave row");
    let card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .expect("report card row");
    let current: calm_server::wave_report::WaveReportPayload =
        serde_json::from_value(card.payload.clone()).expect("report payload");
    let next = calm_server::wave_report::WaveReportPayload::new("seed", &with_user);
    let route_repo: std::sync::Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    calm_server::wave_report::persist_report(
        route_repo.as_ref(),
        &boot.ctx.events,
        &boot.ctx.write,
        calm_server::ids::ActorId::User,
        EditAuthor::User,
        wave,
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
        text.contains(&spec_fence) && text.contains(&user_fence),
        "the fixture must really hold both declarations, else P2's positive \
         case proves nothing: {text}"
    );
    (spec_fence, user_fence)
}

/// The report as `write_markdown` wants it: every block preceded by its
/// `<!-- neige:b_xxxx -->` marker, so identity survives the round trip.
async fn marked_text(boot: &Boot, identity: ToolCallIdentity) -> String {
    read(boot, identity, json!({ "with_markers": true })).await["text"]
        .as_str()
        .expect("marked read returns text")
        .to_string()
}

// ---------------------------------------------------------------------------
// G-B2 — the channel is open, and only to the right roles
// ---------------------------------------------------------------------------

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

    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_MOVE,
        assistant_identity(&boot),
        json!({ "id": id, "to_index": 0, "if_doc_rev": doc_rev(&boot).await }),
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

    let current_rev = read(&boot, spec_identity(&boot), json!({}))
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

/// §3.2b's negative half. The block channel opened by exactly one role,
/// not "for agents".
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

// ---------------------------------------------------------------------------
// §3.4 — attribution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_assistant_block_write_is_persisted_as_edit_author_assistant() {
    let boot = boot().await;

    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "kind": "prose", "markdown": "# Spec\n\nspec text\n", "if_doc_rev": doc_rev(&boot).await }),
    )
    .await
    .expect("spec writes first");
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
        vec![EditAuthor::Spec, EditAuthor::Assistant],
        "the sink must attribute by role: hard-coding `EditAuthor::Spec` \
         would make the assistant's edit indistinguishable from the spec's \
         in the log, the goldens, and the spec-wake decision"
    );
}

// ---------------------------------------------------------------------------
// P1 — no auto-promote
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_assistant_block_write_does_not_promote_a_draft_wave() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Draft).await;

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
        WaveLifecycle::Draft,
        "an assistant must not walk the wave out of Draft; auto-promote is \
         one of the two implicit routes from the block channel into the \
         state machine (§3.2a)"
    );
}

/// P1's control. Auto-promote is suppressed *for the assistant*, not
/// removed — a Draft wave still leaves Draft on the spec's first block
/// write, so the assertion above is about the role.
#[tokio::test]
async fn a_spec_block_write_still_promotes_a_draft_wave() {
    let boot = boot().await;
    set_lifecycle(&boot, WaveLifecycle::Draft).await;

    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "kind": "prose", "markdown": "# Spec\n\nnotes\n", "if_doc_rev": doc_rev(&boot).await }),
    )
    .await
    .expect("spec block write succeeds");

    assert_eq!(lifecycle(&boot).await, WaveLifecycle::Planning);
}

// ---------------------------------------------------------------------------
// P2 — task blocks are untouchable, prose around them is not
// ---------------------------------------------------------------------------

/// The positive case §3.2a calls for by name: the report holds **both** a
/// user-declared and a spec-declared task, the assistant rewrites only the
/// prose, and the write **succeeds**.
///
/// This is the assertion that keeps P2 from being written as "the write
/// must not contain task blocks" — a criterion that would reject the
/// ordinary case of an assistant editing the text around a task list.
#[tokio::test]
async fn an_assistant_may_rewrite_prose_around_user_and_spec_task_blocks() {
    let boot = boot().await;
    let (spec_fence, user_fence) = seed_prose_and_two_tasks(&boot).await;

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
        text.contains(&spec_fence) && text.contains(&user_fence),
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

/// The three shapes that reach a task block someone else declared. All of
/// them funnel through the same before/after diff, which is why the guard
/// lives there and not in a handler.
#[tokio::test]
async fn an_assistant_may_not_modify_or_delete_an_existing_task_block() {
    let boot = boot().await;
    let (spec_fence, user_fence) = seed_prose_and_two_tasks(&boot).await;
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
    assert_eq!(tasks.len(), 2, "fixture holds the spec and user tasks");

    // 1. In-place rewrite of a declaration (here: the SPEC-signed one,
    //    which #1180's user-only rule does not cover at all).
    let (spec_task_id, spec_task_rev) = {
        let marked = marked_text(&boot, assistant_identity(&boot)).await;
        let spec_marker_owner = tasks
            .iter()
            .find(|(id, _)| {
                let marker = marker_line(id);
                marked
                    .split_once(&marker)
                    .is_some_and(|(_, rest)| rest.starts_with(&spec_fence))
            })
            .expect("locate the spec-declared task block");
        spec_marker_owner.clone()
    };
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        assistant_identity(&boot),
        json!({
            "id": spec_task_id,
            "kind": KIND_TASK,
            "payload": {
                "key": "build",
                "kind": "codex",
                "goal": "rewritten by the assistant",
                "ready": true,
                "declared_by": "spec"
            },
            "if_rev": spec_task_rev
        }),
    )
    .await
    .expect_err("an assistant rewriting a spec-declared task must be refused");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);

    // 2. Block-level delete — the one exemption the #1179 rule grants, and
    //    it is not the assistant's to use.
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_DELETE,
        assistant_identity(&boot),
        json!({ "id": spec_task_id, "if_rev": spec_task_rev }),
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
    assert!(after.contains(&spec_fence) && after.contains(&user_fence));
}

/// P2's control: the spec-declared task the assistant could not touch is
/// still the spec's to rewrite. Without this, every assertion above would
/// hold equally well if task blocks had simply become immutable.
#[tokio::test]
async fn the_spec_may_still_rewrite_its_own_task_block() {
    let boot = boot().await;
    seed_prose_and_two_tasks(&boot).await;
    let blocks = read(&boot, spec_identity(&boot), json!({})).await["blocks"]
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
            spec_identity(&boot),
            json!({
                "id": block["id"],
                "kind": KIND_TASK,
                "payload": {
                    "key": "build",
                    "kind": "codex",
                    "goal": "spec revises its own goal",
                    "ready": true,
                    "declared_by": "spec"
                },
                "if_rev": block["rev"]
            }),
        )
        .await;
        // The user-declared block legitimately refuses (its `key` and
        // `declared_by` are immutable, and #1180 protects it from every
        // non-user writer); the spec-declared one must go through.
        if out.is_ok() {
            rewritten = true;
        }
    }
    assert!(
        rewritten,
        "the spec must still be able to rewrite its own declaration — \
         otherwise the assistant refusals above are not about the role"
    );
    assert!(body_text(&boot).await.contains("spec revises its own goal"));
}
