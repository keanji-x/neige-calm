//! G-B (#1189 §7) — the full role verdict for a `CardRole::Assistant` MCP
//! token: which tools it may call, which it may not, and the meta-test that
//! keeps the two lists a *partition* of the tool registry.
//!
//! The assertion deliberately does **not** inspect `tools/list`:
//! `visible_to_roles` only governs discovery (see the note on
//! `ToolDescriptor` — "Wire-level `tools/call` still routes by name
//! regardless"), so pinning "the assistant's tool list is missing
//! `calm.plan.cancel`" would be a tautology about the wrong object. An
//! assistant's token can name any tool it likes on the wire.
//!
//! What actually carries the weight is the `require_role` line at the top
//! of each handler. So these tests skip discovery entirely and issue a raw
//! `tools/call` per denied tool, asserting the JSON-RPC error is
//! `-32602` **and** that its message is the role refusal (`"tool requires
//! role"`). The message check is load-bearing: a malformed-arguments
//! rejection is *also* `-32602`, so a code-only assertion would stay green
//! even if the role gate were removed and the call fell through to
//! argument parsing.

#![cfg(unix)]

use crate::support;

use calm_server::model::CardRole;
use serde_json::json;
use support::mcp::{boot_with_role, connect, handshake, recv_frame, send_frame};

/// Tools an Assistant token may call.
///
/// `calm.report.read` is the *only* source of `docRev` and the per-block
/// `rev`s (every block-channel write takes `if_doc_rev` / `if_rev`), so an
/// assistant locked out of it could never form a CAS write at all. The
/// report **write** channel (`calm.report.write` / `.edit`, which can carry
/// lifecycle) stays denied — that is the §3.2 dividing line, not "reports
/// are spec property".
///
/// #1189 S2 (§3.2b) opened the block channel too: `blocks.*` and
/// `write_markdown` are the only write surface an assistant has, and none
/// of them can carry a `lifecycle` field. What keeps that from being a
/// state-machine grant lives below the entry gate — no auto-promote, and
/// the task-block guard — not in this list.
const ASSISTANT_ALLOWED_TOOLS: &[&str] = &[
    "calm.report.read",
    "calm.report.blocks.kinds",
    "calm.report.blocks.upsert",
    "calm.report.blocks.move",
    "calm.report.blocks.delete",
    "calm.report.write_markdown",
];

/// Denied tools whose handler a **Spec** token gets past. Used both as the
/// negative list for the assistant and as the control list below.
const ASSISTANT_DENIED_TOOLS_SPEC_REACHABLE: &[&str] = &[
    // Report write channel — carries lifecycle, hence spec-only (§3.2).
    "calm.report.write",
    "calm.report.edit",
    // Cross-wave / cross-area report discovery reads.
    "calm.area.outline",
    "calm.report.links.backlinks",
    // Wave state + verdict.
    "calm.wave.state",
    "calm.task.verdict",
    // #1211 S3 — naming the wave is a spec judgement about what the track is;
    // an assistant has no plan of its own to name.
    "calm.wave.rename",
    // Wave filesystem + history drill-ins (Spec|Worker, never Assistant).
    "calm.wave.ls",
    "calm.wave.cat",
    "calm.wave.diff",
    "calm.wave.cat_at",
    "calm.wave.log",
    // Dispatch. Retired no-op shim today, still spec-only.
    "calm.task.dispatch",
    // Planning, review, admin.
    "calm.plan.upsert",
    "calm.plan.cancel",
    "calm.plan.list",
    "calm.review.round",
    "calm.ratify.request",
    "calm.admin.wave_gc",
    "calm.admin.vacuum",
    // Hidden deprecated aliases delegate to the handler above, so the role
    // refusal must survive the rename path too.
    "calm.get_wave_state",
    "calm.update_task_meta",
    "calm.dispatch_request",
];

/// Denied tools that only a **Worker** token gets past — the completion
/// pair and its aliases. Split out because the Spec control below would
/// otherwise be asserting something false about them.
const ASSISTANT_DENIED_TOOLS_WORKER_REACHABLE: &[&str] = &[
    "calm.task.complete",
    "calm.task.fail",
    "calm.task_completed",
    "calm.task_failed",
];

fn assistant_denied_tools() -> Vec<&'static str> {
    ASSISTANT_DENIED_TOOLS_SPEC_REACHABLE
        .iter()
        .chain(ASSISTANT_DENIED_TOOLS_WORKER_REACHABLE)
        .copied()
        .collect()
}

/// #1189 F2 — the allowed/denied lists above are hand-written, and a
/// hand-written list of tools is exactly the thing that silently rots when
/// someone registers tool #33. This asserts the two lists are a *partition*
/// of the real registry: disjoint, and their union is set-equal to every
/// registered name (aliases included — they route to a handler and so carry
/// a role verdict of their own).
///
/// A new tool with no assistant verdict therefore fails here rather than
/// landing in the gap between two tests that each only check what they list.
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
        assert_eq!(
            error["code"].as_i64(),
            Some(-32602),
            "`{tool}` must refuse with INVALID_PARAMS, got: {resp:#?}"
        );
        let message = error["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("tool requires role"),
            "`{tool}` must refuse for the *role* reason (not argument parsing); got: {message}"
        );
    }

    let _ = (&boot.server, &boot.repo);
}

/// #1189 F1 — the positive half, and the precondition for S2's CAS block
/// channel: an assistant token calling `calm.report.read` gets a real
/// answer, with the concurrency tokens in it.
///
/// The field assertions are the point. "not an error" would stay green if
/// the handler started returning `{}`, which is exactly the failure mode
/// that would strand S2 (a write needs `if_doc_rev` from `docRev` and
/// `if_rev` from `blocks[].rev`; neither exists anywhere else).
#[tokio::test]
async fn assistant_token_can_read_the_report_with_concurrency_tokens() {
    let boot = boot_with_role(CardRole::Assistant).await;
    seed_wave_report_card(&boot).await;
    let (mut rd, mut wr) = connect(&boot.socket_path).await;
    handshake(&mut rd, &mut wr, &boot.raw_token).await;

    send_frame(
        &mut wr,
        json!({
            "jsonrpc": "2.0",
            "id": 300,
            "method": "tools/call",
            "params": { "name": "calm.report.read", "arguments": {} }
        }),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(
        resp.get("error").is_none(),
        "calm.report.read must serve an assistant caller: {resp:#?}"
    );

    // Tool results come back as MCP content frames; the structured payload
    // is the JSON text of the single content item.
    let payload = tool_result_payload(&resp);

    // G5 — the exact value, not "present and numeric". The fixture wrote
    // the report twice through the persist boundary, so a correct `docRev`
    // can only come from the CRDT root register; a handler that hardcoded
    // it (or a fixture that fell back onto the NULL-CRDT legacy branch,
    // where `doc_rev` is the literal 0) fails here.
    assert_eq!(
        payload.get("docRev").and_then(serde_json::Value::as_u64),
        Some(SEEDED_DOC_REV),
        "`docRev` must be the CRDT-derived revision — it is the only \
         `if_doc_rev` source a block-channel write has: {payload:#?}"
    );
    // G1 — `taskDiagnostics` is dispatched-task runtime state (status,
    // gate result, worker card id, child wave id): the very class
    // `calm.plan.list` stays Spec-only to withhold. Opening `report.read`
    // to the assistant must not smuggle it out the side.
    assert!(
        payload.get("taskDiagnostics").is_none(),
        "`taskDiagnostics` must be withheld from an assistant caller: {payload:#?}"
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
            "block index entry needs a numeric `rev` — the `if_rev` source: {block:#?}"
        );
    }

    let _ = (&boot.server, &boot.repo);
}

/// G1 control — the trim is a *role* decision, not a field that quietly
/// stopped being produced. The identical call from a Spec token still
/// carries `taskDiagnostics`, and the seeded live task block makes it a
/// non-empty array, so the assistant assertion above is withholding
/// something real rather than an always-empty key.
#[tokio::test]
async fn spec_token_still_gets_task_diagnostics_from_the_report_read() {
    let boot = boot_with_role(CardRole::Spec).await;
    seed_wave_report_card(&boot).await;
    let (mut rd, mut wr) = connect(&boot.socket_path).await;
    handshake(&mut rd, &mut wr, &boot.raw_token).await;

    send_frame(
        &mut wr,
        json!({
            "jsonrpc": "2.0",
            "id": 500,
            "method": "tools/call",
            "params": { "name": "calm.report.read", "arguments": {} }
        }),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(
        resp.get("error").is_none(),
        "calm.report.read must serve a spec caller: {resp:#?}"
    );
    let payload = tool_result_payload(&resp);
    let diagnostics = payload
        .get("taskDiagnostics")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("spec keeps the full `taskDiagnostics` payload: {payload:#?}"));
    assert!(
        !diagnostics.is_empty(),
        "the seeded report declares a live task, so the spec-side diagnostics \
         must be non-empty — otherwise the assistant-side absence assertion \
         is withholding nothing: {payload:#?}"
    );

    let _ = (&boot.server, &boot.repo);
}

/// `boot_with_role` mints cards straight through `card_with_codex_create_tx`
/// and so skips the wave-report card `routes::waves::create_wave` mints. Add
/// it back the way production does — `card_create_with_id_tx` with
/// `CardRole::ReportCard`, writing through the SAME role cache the booted
/// server gates on (#1189 review round 2, G6: a report card with no cached
/// role is not the shape `enforce_assistant_scope` will meet in S2, whose
/// "may I write here" test is precisely `cache.get(target) == ReportCard`).
///
/// Then push real content through the report persist boundary — twice — so
/// the row carries a genuine `body_crdt` and the read path returns a
/// CRDT-derived `docRev` of [`SEEDED_DOC_REV`] instead of the `doc_rev: 0`
/// literal the NULL-CRDT legacy branch hands back (G5: on the legacy branch
/// every `docRev` assertion is vacuous). The body carries a live task fence
/// so `taskDiagnostics` is non-empty and the G1 trim below has something
/// real to withhold.
async fn seed_wave_report_card(boot: &support::mcp::CardBoot) -> String {
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
            wave_id: boot.wave_id.clone(),
            title: None,
            kind: "wave-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(calm_server::wave_report::WaveReportPayload::initial())
                .unwrap(),
        },
        calm_server::model::CardRole::ReportCard,
        false,
        &boot.card_role_cache,
    )
    .await
    .expect("mint wave-report card");
    tx.commit().await.expect("commit report card tx");

    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .expect("home wave exists");
    let write = calm_server::state::WriteContext::new(
        boot.card_role_cache.clone(),
        boot.wave_area_cache.clone(),
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
        let current: calm_server::wave_report::WaveReportPayload =
            serde_json::from_value(card.payload.clone()).expect("report payload");
        card = calm_server::wave_report::persist_report(
            boot.sqlx.as_ref(),
            &boot.events,
            &write,
            calm_server::ids::ActorId::Kernel,
            calm_server::event::EditAuthor::Spec,
            wave.clone(),
            card,
            current,
            calm_server::wave_report::WaveReportPayload::new(summary, &body),
            doc_rev,
            None,
            None,
            false,
        )
        .await
        .expect("persist seeded report body");
    }
    report_card_id
}

/// Two persists through the report boundary, each incrementing the CRDT
/// root's `doc_rev` register — so the read path's `docRev` is a value only
/// the CRDT can produce (the legacy NULL-CRDT branch returns `0`, and a
/// single write would return `1`, which is too easy to hit by accident).
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

/// Control for the negative test: the refusals must be a *role* decision,
/// not "this wire path refuses everything". The identical call sequence
/// from a Spec token gets past every `require_role` — the calls may still
/// fail on arguments, but never with the role message. Without this
/// control, a broken handshake or a blanket deny would make the negative
/// test pass vacuously.
#[tokio::test]
async fn spec_token_is_never_refused_for_the_role_reason() {
    assert_role_reason_absent(CardRole::Spec, ASSISTANT_DENIED_TOOLS_SPEC_REACHABLE, 200).await;
}

/// Same control for the worker-only completion pair, which a Spec token
/// legitimately *is* refused for the role reason.
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
