//! Real Assistant credentials use the shared Planner tool contract while
//! preserving actual card identity and Report attribution.

#![cfg(unix)]

use crate::support;

use calm_server::model::CardRole;
use serde_json::json;
use support::mcp::{boot_with_role, connect, handshake, recv_frame, send_frame};

/// Both conversation identities reach the same registered Planner capabilities.
#[tokio::test]
async fn assistant_token_reuses_planner_tools_without_rewriting_identity() {
    let registry = calm_server::mcp_server::build_default_registry();
    let tools: Vec<_> = registry
        .descriptors()
        .into_iter()
        .filter(|d| d.roles.contains(&CardRole::Planner))
        .map(|d| d.name)
        .collect();
    assert!(!tools.is_empty());
    for role in [CardRole::Assistant, CardRole::Planner] {
        let boot = boot_with_role(role).await;
        let (mut rd, mut wr) = connect(&boot.socket_path).await;
        handshake(&mut rd, &mut wr, &boot.raw_token).await;
        for (id, name) in tools.iter().enumerate() {
            send_frame(
                &mut wr,
                json!({"jsonrpc":"2.0", "id":id,
                "method":"tools/call", "params":{"name":name,"arguments":{}}}),
            )
            .await;
            let response = recv_frame(&mut rd).await;
            if let Some(error) = response.get("error") {
                assert!(
                    !error["message"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("tool requires role"),
                    "{role:?} {name}: {response}"
                );
            }
        }
        assert_eq!(
            boot.repo.card_role_get(&boot.card_id).await.unwrap(),
            Some(role)
        );
    }
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
    // Shared conversation capabilities include the task diagnostics used by Planner tools.
    assert!(
        payload.get("task_diagnostics").is_some(),
        "`task_diagnostics` must be shared with an assistant caller: {payload:#?}"
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

/// Control: the shared diagnostics remain populated for Planner as well.
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
         must be non-empty, matching the shared Assistant read: {payload:#?}"
    );

    let _ = (&boot.server, &boot.repo);
}

/// `boot_with_role` skips the track-report card production mints; add it back through the SAME
/// role cache the server gates on, then persist real content twice so `docRev` is CRDT-derived
/// ([`SEEDED_DOC_REV`], not the legacy `0`) and the body carries a live task fence.
async fn seed_track_report_card(boot: &support::mcp::CardBoot) -> String {
    let report_card_id = calm_server::model::new_id();
    let write_db = (boot.sqlx.db()).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session
                    .begin_immediate()
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
                        payload: serde_json::to_value(
                            calm_server::track_report::TrackReportPayload::initial(),
                        )
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
            })
        })
        .await
        .expect("independent write operation")
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
