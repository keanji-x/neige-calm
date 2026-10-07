//! The full role verdict for a `CardRole::Assistant` MCP token. Discovery is not inspected:
//! `tools/call` routes by name regardless, so each denied tool gets a raw call asserting `-32403`
//! AND the role-refusal message. The lists are the reviewed expectation: they must equal the
//! tools whose declared `roles` admit the Assistant, so a declaration change is a reviewed diff here.

#![cfg(unix)]

use crate::support;

use calm_server::model::CardRole;
use serde_json::json;
use support::mcp::{boot_with_role, connect, handshake, recv_frame, send_frame};

/// Tools an Assistant token may call. Its report writes are anchored by its own `neige_report_read`;
/// their `lifecycle` field alone is refused.
const ASSISTANT_ALLOWED_TOOLS: &[&str] = &[
    "plugin_calendar_ls",
    "plugin_calendar_add",
    "plugin_calendar_set",
    "plugin_calendar_rm",
    "neige_workspace_ls",
    "neige_workspace_cat",
    "neige_workspace_diff",
    "neige_workspace_log",
    "neige_report_read",
    "neige_report_describe",
    "neige_report_commit",
    "neige_report_write",
];

/// Denied tools whose handler a **Planner** token gets past; also the control list below.
const ASSISTANT_DENIED_TOOLS_PLANNER_REACHABLE: &[&str] = &[
    // Cross-track / cross-area report discovery reads.
    "neige_area_ls",
    "neige_link_ls",
    "neige_report_find",
    // Captured sources are the planner's evidence.
    "neige_source_capture",
    "neige_source_ls",
    // Track state + verdict.
    "neige_track_status",
    "neige_task_accept",
    "neige_task_reject",
    // Re-running a failed gate is a Planner decision (#2405).
    "neige_task_regate",
    // Naming the track is a planner judgement.
    "neige_track_rename",
    // Publishing the track's verified commit is a Planner action.
    "plugin_gitforge_publish",
    // Closing the track is a Planner action; only the user reopens.
    "neige_track_close",
    // Opening a top-level Track from a recipe is a Planner action.
    "neige_track_add",
    // Asking the user is a planner action.
    "neige_user_ask",
    // Mail between the Tracks of an Area wakes another Planner (#2130).
    "neige_mail_send",
    "neige_mail_ls",
    "neige_mail_cat",
    // Preview gateway registration is a Planner action.
    "neige_preview_add",
    "neige_preview_rm",
    "neige_terminal_open",
    "neige_terminal_show",
    "neige_terminal_read",
    "neige_terminal_control",
    "neige_terminal_input",
    // Track filesystem + history drill-ins (Planner|Worker, never Assistant).
    "neige_track_ls",
    "neige_track_cat",
    // `neige report tag report.md` (Planner lists and changes, Worker lists; never Assistant).
    "neige_report_tag",
    "neige_track_diff",
    "neige_track_show",
    "neige_track_log",
    // Planning, review, admin.
    "neige_task_cancel",
    "neige_task_ls",
    "neige_admin_gc",
    "neige_admin_vacuum",
];

/// Denied tools that only a **Worker** token gets past, so the Planner control does not assert
/// something false about them.
const ASSISTANT_DENIED_TOOLS_WORKER_REACHABLE: &[&str] = &["neige_task_done", "neige_task_fail"];

fn assistant_denied_tools() -> Vec<&'static str> {
    ASSISTANT_DENIED_TOOLS_PLANNER_REACHABLE
        .iter()
        .chain(ASSISTANT_DENIED_TOOLS_WORKER_REACHABLE)
        .copied()
        .collect()
}

/// The two hand-written lists must be a *partition* of the real registry, so
/// a new tool with no assistant verdict fails here.
#[test]
fn assistant_verdict_covers_every_registered_tool() {
    let registry = calm_server::mcp_server::build_default_registry();
    let mut registered = registry
        .descriptors()
        .into_iter()
        .map(|descriptor| descriptor.name)
        .collect::<Vec<_>>();
    registered.sort();

    let mut adjudicated = ASSISTANT_ALLOWED_TOOLS
        .iter()
        .chain(assistant_denied_tools().iter())
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    let adjudicated_len = adjudicated.len();
    adjudicated.sort();
    adjudicated.dedup();
    assert_eq!(
        adjudicated.len(),
        adjudicated_len,
        "a tool is listed twice across the allow/deny lists: {adjudicated:?}"
    );

    let missing = registered
        .iter()
        .filter(|name| !adjudicated.contains(name))
        .collect::<Vec<_>>();
    let unknown = adjudicated
        .iter()
        .filter(|name| !registered.contains(name))
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty() && unknown.is_empty(),
        "assistant role verdict must partition the tool registry.\n  \
         registered but unadjudicated (add to the allow or deny list): {missing:?}\n  \
         adjudicated but not registered (stale name): {unknown:?}"
    );
    assert_eq!(registered, adjudicated);
}

/// The reviewed allow list is exactly the set of tools whose declared `roles` admit the Assistant;
/// the registry's gate enforces those declarations (`mcp_tool_role_matrix`).
#[test]
fn assistant_verdict_equals_the_declared_roles() {
    let mut declared = calm_server::mcp_server::build_default_registry()
        .descriptors()
        .into_iter()
        .filter(|descriptor| descriptor.roles.contains(&CardRole::Assistant))
        .map(|descriptor| descriptor.name)
        .collect::<Vec<_>>();
    declared.sort();
    let mut reviewed = ASSISTANT_ALLOWED_TOOLS
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    reviewed.sort();
    assert_eq!(
        declared, reviewed,
        "the tools whose declared roles admit the Assistant must equal ASSISTANT_ALLOWED_TOOLS; \
         review the declaration change and update the list"
    );
}

#[tokio::test]
async fn assistant_token_cannot_call_denied_tools_by_name() {
    let boot = boot_with_role(CardRole::Assistant).await;
    let (mut rd, mut wr) = connect(&boot.socket_path).await;
    handshake(&mut rd, &mut wr, &boot.raw_token).await;

    for (idx, tool) in assistant_denied_tools().iter().enumerate() {
        send_frame(
            &mut wr,
            json!({
                "jsonrpc": "2.0",
                "id": 100 + idx,
                "method": "tools/call",
                "params": { "name": tool, "arguments": {} }
            }),
        )
        .await;
        let resp = recv_frame(&mut rd).await;
        let error = resp
            .get("error")
            .unwrap_or_else(|| panic!("`{tool}` must refuse an assistant caller, got: {resp:#?}"));
        if *tool == "plugin_gitforge_publish" {
            assert_eq!(
                error["code"].as_i64(),
                Some(-32601),
                "development tools outside this Track are undiscoverable: {resp:#?}"
            );
            continue;
        }
        let message = error["message"].as_str().unwrap_or_default();
        // The role refusal (agent-commands.md §5): the registry's gate on the declared roles.
        let role_refusal = error["code"].as_i64() == Some(-32403)
            && message.contains("tool requires role in [")
            && message.contains("got=Assistant");
        assert!(
            role_refusal,
            "`{tool}` must refuse for the *role* reason (not argument parsing); got: {resp:#?}"
        );
    }

    let _ = (&boot.server, &boot.repo);
}

/// The field assertions are the point: "not an error" would stay green if the handler returned `{}`.
#[tokio::test]
async fn assistant_token_can_read_the_report_with_concurrency_tokens() {
    let boot = boot_with_role(CardRole::Assistant).await;
    seed_track_report_card(&boot).await;
    let (mut rd, mut wr) = connect(&boot.socket_path).await;
    handshake(&mut rd, &mut wr, &boot.raw_token).await;

    send_frame(
        &mut wr,
        json!({
            "jsonrpc": "2.0",
            "id": 300,
            "method": "tools/call",
            "params": { "name": "neige_report_read", "arguments": {} }
        }),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(
        resp.get("error").is_none(),
        "neige_report_read must serve an assistant caller: {resp:#?}"
    );

    // The structured payload is the JSON text of the single content item.
    let payload = tool_result_payload(&resp);

    // The exact value: the fixture wrote twice through the persist boundary, so a correct `docRev`
    // can only come from the CRDT root register (the NULL-CRDT legacy branch returns 0).
    assert_eq!(
        payload.get("doc_rev").and_then(serde_json::Value::as_u64),
        Some(SEEDED_DOC_REV),
        "`doc_rev` must be the CRDT-derived revision — it is what this read \
         anchors a later write to: {payload:#?}"
    );
    // `taskDiagnostics` is dispatched-task runtime state, the class `neige_task_ls` stays
    // Planner-only to withhold; it must not leak out the side.
    assert!(
        payload.get("task_diagnostics").is_none(),
        "`task_diagnostics` must be withheld from an assistant caller: {payload:#?}"
    );
    let blocks = payload
        .get("blocks")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("`blocks` must be an array: {payload:#?}"));
    assert!(
        !blocks.is_empty(),
        "the seeded report has at least one block; an empty index would make \
         the per-block `rev` assertion below vacuous: {payload:#?}"
    );
    for block in blocks {
        assert!(
            block
                .get("id")
                .and_then(serde_json::Value::as_str)
                .is_some(),
            "block index entry needs `id`: {block:#?}"
        );
        assert!(
            block
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .is_some(),
            "block index entry needs `kind`: {block:#?}"
        );
        assert!(
            block
                .get("rev")
                .and_then(serde_json::Value::as_u64)
                .is_some(),
            "block index entry needs a numeric `rev` — a write's per-block anchor: {block:#?}"
        );
    }

    let _ = (&boot.server, &boot.repo);
}

/// Control: the trim is a *role* decision, not a field that quietly stopped being produced.
#[tokio::test]
async fn planner_token_still_gets_task_diagnostics_from_the_report_read() {
    let boot = boot_with_role(CardRole::Planner).await;
    seed_track_report_card(&boot).await;
    let (mut rd, mut wr) = connect(&boot.socket_path).await;
    handshake(&mut rd, &mut wr, &boot.raw_token).await;

    send_frame(
        &mut wr,
        json!({
            "jsonrpc": "2.0",
            "id": 500,
            "method": "tools/call",
            "params": { "name": "neige_report_read", "arguments": {} }
        }),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(
        resp.get("error").is_none(),
        "neige_report_read must serve a planner caller: {resp:#?}"
    );
    let payload = tool_result_payload(&resp);
    let diagnostics = payload
        .get("task_diagnostics")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| {
            panic!("planner keeps the full `task_diagnostics` payload: {payload:#?}")
        });
    assert!(
        !diagnostics.is_empty(),
        "the seeded report declares a live task, so the planner-side diagnostics \
         must be non-empty — otherwise the assistant-side absence assertion \
         is withholding nothing: {payload:#?}"
    );

    let _ = (&boot.server, &boot.repo);
}

/// `boot_with_role` skips the track-report card production mints; add it back through the SAME
/// role cache the server gates on, then persist real content twice so `docRev` is CRDT-derived
/// ([`SEEDED_DOC_REV`], not the legacy `0`) and the body carries a live task fence.
async fn seed_track_report_card(boot: &support::mcp::CardBoot) -> String {
    let report_card_id = calm_server::model::new_id();
    let mut tx = boot
        .sqlx
        .pool()
        .begin()
        .await
        .expect("begin report card tx");
    let report_card = calm_server::db::sqlite::card_create_with_id_tx(
        &mut tx,
        report_card_id.clone(),
        calm_server::model::NewCard {
            track_id: boot.track_id.clone(),
            title: None,
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(calm_server::track_report::TrackReportPayload::initial())
                .unwrap(),
        },
        calm_server::model::CardRole::ReportCard,
        false,
        &boot.card_role_cache,
    )
    .await
    .expect("mint track-report card");
    tx.commit().await.expect("commit report card tx");

    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .expect("home track exists");
    let write = calm_server::state::WriteContext::new(
        boot.card_role_cache.clone(),
        boot.track_area_cache.clone(),
    );
    let body = format!(
        "# Goal\n\nship it\n\n{}",
        calm_types::report_blocks::render_fence(
            calm_types::report_blocks::KIND_TASK,
            &json!({
                "key": "build",
                "kind": "codex",
                "goal": "build it",
                "ready": true,
                "declared_by": "spec"
            }),
        )
    );
    let mut card = report_card;
    for (doc_rev, summary) in [(0u64, "seed"), (1u64, "seeded")] {
        let current: calm_server::track_report::TrackReportPayload =
            serde_json::from_value(card.payload.clone()).expect("report payload");
        card = calm_server::track_report::persist_report(
            boot.sqlx.as_ref(),
            &boot.events,
            &write,
            calm_server::ids::ActorId::Kernel,
            calm_server::event::EditAuthor::Planner,
            track.clone(),
            card,
            current,
            calm_server::track_report::TrackReportPayload::new(summary, &body),
            doc_rev,
            None,
        )
        .await
        .expect("persist seeded report body");
    }
    report_card_id
}

/// Two persists, each incrementing the CRDT root's `doc_rev`: the legacy branch returns `0` and
/// a single write `1`, which is too easy to hit by accident.
const SEEDED_DOC_REV: u64 = 2;

/// Pull the structured tool payload out of an MCP `tools/call` result.
fn tool_result_payload(resp: &serde_json::Value) -> serde_json::Value {
    let result = &resp["result"];
    if let Some(structured) = result.get("structuredContent") {
        return structured.clone();
    }
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool result has no text content: {resp:#?}"));
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("tool result text is not JSON ({e}): {text}"))
}

/// Control: the refusals must be a *role* decision, not "this wire path refuses everything".
#[tokio::test]
async fn planner_token_is_never_refused_for_the_role_reason() {
    assert_role_reason_absent(
        CardRole::Planner,
        ASSISTANT_DENIED_TOOLS_PLANNER_REACHABLE,
        200,
    )
    .await;
}

/// Same control for the worker-only completion pair.
#[tokio::test]
async fn worker_token_is_never_refused_for_the_role_reason() {
    assert_role_reason_absent(
        CardRole::Worker,
        ASSISTANT_DENIED_TOOLS_WORKER_REACHABLE,
        400,
    )
    .await;
}

async fn assert_role_reason_absent(role: CardRole, tools: &[&str], id_base: usize) {
    let boot = boot_with_role(role).await;
    let (mut rd, mut wr) = connect(&boot.socket_path).await;
    handshake(&mut rd, &mut wr, &boot.raw_token).await;

    for (idx, tool) in tools.iter().enumerate() {
        send_frame(
            &mut wr,
            json!({
                "jsonrpc": "2.0",
                "id": id_base + idx,
                "method": "tools/call",
                "params": { "name": tool, "arguments": {} }
            }),
        )
        .await;
        let resp = recv_frame(&mut rd).await;
        let message = resp
            .get("error")
            .and_then(|error| error["message"].as_str())
            .unwrap_or_default();
        assert!(
            !message.contains("tool requires role"),
            "`{tool}` must not refuse a {role:?} caller on role grounds; got: {message}"
        );
    }

    let _ = (&boot.server, &boot.repo);
}
