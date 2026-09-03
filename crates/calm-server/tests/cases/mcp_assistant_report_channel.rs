//! #1189 S2 — what an `CardRole::Assistant` token can and cannot do once
//! the block channel is open (§3.2b), driven through the real tool
//! handlers, the real decision sink, and the real recorder gate.
//!
//! | gate | assertion here |
//! |---|---|
//! | G-B2 | an assistant drives `blocks.upsert` / `.move` / `.delete` / `write_markdown` end to end; a Worker token is still refused at the entry |
//! | §3.4 | its edits persist as `EditAuthor::Assistant`, never as the spec |
//! | P1   | an assistant's block write leaves a Draft track in Draft |
//! | P2   | its writes may not create, modify, or delete a task block — including the whole-document shapes — while a prose-only rewrite that carries the task fences through unchanged succeeds |
//!
//! Every negative here has a Spec-token control next to it. Without one,
//! "the assistant could not do X" would stay green if X had simply stopped
//! working for everybody.

#![cfg(unix)]

use crate::mcp_track_report::{
    Boot, assistant_identity, boot, call_tool, spec_identity, worker_identity,
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

/// The `author` of every `track.report_edited` in the persisted log, oldest
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

/// A task declaration that is **gate-clean**: it carries a `no_gate_reason`,
/// so it satisfies the track's default `require_task_gates` policy and its
/// only remaining barrier to schedulability is the `ready` flag.
///
/// `task_fence` above is deliberately *not* this: it has no gate and no
/// `no_gate_reason`, so it is unschedulable no matter who writes it. That is
/// fine for the equivalence assertions, but it cannot carry §7 P2's
/// counterexample, which is about a write that would have produced a
/// dispatchable task.
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

/// `(key, status)` of every row in the track's task projection, which is what
/// "a schedulable task" means concretely: `tasks_rebuild_tx` runs inside the
/// same write transaction as the report edit, and the scheduler reads these
/// rows and nothing else.
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

/// P1's control. Auto-promote is suppressed *for the assistant*, not
/// removed — a Draft track still leaves Draft on the spec's first block
/// write, so the assertion above is about the role.
#[tokio::test]
async fn a_spec_block_write_still_promotes_a_draft_track() {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Draft).await;

    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "kind": "prose", "markdown": "# Spec\n\nnotes\n", "if_doc_rev": doc_rev(&boot).await }),
    )
    .await
    .expect("spec block write succeeds");

    assert_eq!(lifecycle(&boot).await, TrackLifecycle::Planning);
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

/// §7 P2's counterexample in its load-bearing form: the refused assistant
/// write is one that **would have produced a dispatchable task**, and the
/// control group proves this fixture can produce one.
///
/// Three steps, because each of the first two alone is satisfiable by an
/// accident:
///
/// 1. the **spec** declares a gate-clean task with `ready: false` — no task
///    row, because the declaration is withdrawn, not because the environment
///    forbids tasks;
/// 2. the **assistant** flips exactly that block to `ready: true` — refused,
///    and still no task row;
/// 3. the **spec** makes the identical edit — a `pending` row appears.
///
/// Step 3 is what makes step 2 mean something. Without it "no task row after
/// the assistant's write" would stay green if the track simply could not carry
/// a schedulable task at all (which is exactly the state the earlier P2
/// fixtures are in: their fences carry neither a gate nor a `no_gate_reason`,
/// so under the track's default `require_task_gates` they project no
/// schedulable row regardless of the writer).
///
/// What removing the P2 guard actually does, verified by mutation: step 2's
/// write is no longer stopped at the guard and runs into the task projection.
/// Because this particular edit *changes the projected key set*, the write
/// emits `Event::PlanUpdated` (`track_report.rs`, guarded by
/// `!task_projection.changed_keys.is_empty()`), and the in-tx *role gate*
/// refuses that event with "only spec cards (or User/Kernel) may emit
/// dispatch-request events (actor=AiCodex(<assistant card>))". So the row does
/// not appear, because the whole write rolls back at that second, independent
/// layer.
///
/// Read that message precisely, and do not over-read it: `role_gate.rs`
/// handles `PlanUpdated` in the *same match arm* as `CodexWorkerRequested` /
/// `TerminalWorkerRequested` and reuses one `NotSpecForDispatch` string for
/// all three. The mutation therefore does **not** exercise the real
/// worker-request emission path; what it shows is that the released task
/// reached track-level plan authority, no more than that.
///
/// **And that second layer is not a reason to drop P2 — it is strictly
/// narrower.** It exists only when the edit changes the projected key set. A
/// tamper that leaves the key set alone — an assistant rewriting an existing
/// task's `goal` text, say — projects no `changed_keys`, emits no
/// `PlanUpdated`, never reaches the role gate at all. For that whole class of
/// edit P2 is the only defence there is: delete P2 and such a write lands.
/// Anyone reading this fixture later (S3 included) must not conclude
/// "the mutation was still refused, so P2 is redundant".
///
/// This is also why step 2 asserts P2's own message and not just an error
/// code: with only the code asserted, the mutation would still be red, but for
/// the wrong reason, and the fixture would silently stop pinning P2.
#[tokio::test]
async fn an_assistant_may_not_flip_a_spec_task_to_ready() {
    let boot = boot().await;

    // 1. Spec seeds a gate-clean but withdrawn declaration.
    let withheld = gated_task_fence("dispatchable", false);
    let released = gated_task_fence("dispatchable", true);
    call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        spec_identity(&boot),
        json!({
            "body": format!("# Plan\n\nthe original prose\n\n{withheld}"),
            "summary": "seed",
            "if_doc_rev": doc_rev(&boot).await,
        }),
    )
    .await
    .expect("the spec may declare a not-yet-ready task");
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
    .expect_err("an assistant releasing a spec-declared task must be refused");
    // The refusal must be P2's own, by message and not just by code. Delete
    // the P2 guard and this write does not merely change error code: it runs
    // all the way into the task projection, emits `PlanUpdated`, and is
    // stopped only by the role gate's shared dispatch-request arm ("only spec
    // cards (or User/Kernel) may emit dispatch-request events"). That backstop
    // fires only because *this* edit changes the projected key set, so
    // asserting P2's own message is what keeps the fixture pinned on P2 rather
    // than on the narrower layer behind it.
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

    // 3. Control: the spec makes the identical edit and a task appears.
    call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        spec_identity(&boot),
        json!({ "body": flipped, "if_doc_rev": doc_rev(&boot).await }),
    )
    .await
    .expect("the spec releases its own declaration");
    assert_eq!(
        task_rows(&boot).await,
        vec![("dispatchable".to_string(), "pending".to_string())],
        "the control group proves this exact edit does produce a schedulable \
         task, so step 2's empty projection is the guard's doing"
    );
}

/// The three shapes that reach a task block someone else declared. All of
/// them funnel through the same before/after diff, which is why the guard
/// lives there and not in a handler.
///
/// These three assert `before == after` on the report body rather than
/// counting task rows, and that is sufficient — but only because of a fact
/// worth writing down, since the assertion is otherwise strictly weaker than
/// the one in `an_assistant_may_not_flip_a_spec_task_to_ready` above:
/// `tasks_rebuild_with_tree_term_tx` (`track_report.rs:128-151`) projects the
/// task table from the track-report card's `payload` + `body_crdt` and nothing
/// else. It is a pure function of the report document. All three attempts
/// here are refused as whole writes, so the document is bit-identical before
/// and after; an unchanged input to a pure function cannot yield a changed
/// projection. "No task was created" therefore follows from `before == after`
/// and does not need its own assertion here.
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
