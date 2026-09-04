#![cfg(unix)]

use crate::support;

use calm_server::model::CardRole;
use serde_json::json;
use support::mcp::{
    boot_shared_daemon_with_planner_thread, boot_with_role, connect, handshake, handshake_daemon,
    recv_frame, send_frame, tools_list_frame,
};

fn expected_planner_toolset() -> Vec<&'static str> {
    vec![
        "calm.area.outline",
        "calm.plan.cancel",
        "calm.plan.list",
        "calm.ratify.request",
        "calm.report.blocks.delete",
        "calm.report.blocks.kinds",
        "calm.report.blocks.move",
        "calm.report.blocks.upsert",
        "calm.report.edit",
        "calm.report.links.backlinks",
        "calm.report.read",
        "calm.report.write",
        "calm.report.write_markdown",
        "calm.review.round",
        "calm.task.verdict",
        // #1211 S3 — the planner agent's naming write. Added as an entry, not by
        // loosening the assertion: the exact set is the contract.
        "calm.track.rename",
    ]
}

fn tool_names_from_response(resp: &serde_json::Value) -> Vec<String> {
    let mut names = resp["result"]["tools"]
        .as_array()
        .expect("tools is an array")
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("tool name is a string")
                .to_string()
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

async fn tools_list_names_for_role(role: CardRole) -> Vec<String> {
    let boot = boot_with_role(role).await;
    let (mut rd, mut wr) = connect(&boot.socket_path).await;
    handshake(&mut rd, &mut wr, &boot.raw_token).await;

    send_frame(
        &mut wr,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tools/list errored: {resp:#?}");

    let names = tool_names_from_response(&resp);
    let _ = &boot.server;
    names
}

#[tokio::test]
async fn tools_list_for_planner_role_returns_planner_toolset() {
    let names = tools_list_names_for_role(CardRole::Planner).await;
    assert_eq!(names, expected_planner_toolset());
}

#[tokio::test]
async fn tools_list_for_planner_role_does_not_leak_aliases() {
    let names = tools_list_names_for_role(CardRole::Planner).await;
    for hidden_name in [
        "calm.dispatch_request",
        "calm.task.dispatch",
        "calm.task_completed",
        "calm.task_failed",
        "calm.get_track_state",
        "calm.plan.upsert",
        "calm.update_task_meta",
    ] {
        assert!(
            !names.iter().any(|name| name == hidden_name),
            "hidden tool leaked in tools/list: {hidden_name}; names={names:?}",
        );
    }
}

#[tokio::test]
async fn retired_update_track_state_shadow_is_not_registered() {
    let registry = calm_server::mcp_server::build_default_registry();
    assert!(
        registry.lookup("calm.update_track_state").is_none(),
        "retired update_track_state name must not remain as a hidden tool or alias",
    );
}

/// #838 Move 2 — a worker's `tools/list` now advertises exactly the two
/// native completion tools (`calm.task.complete` / `calm.task.fail`), which
/// were flipped from `visible_to_roles: &[]` to `&[CardRole::Worker]` so a
/// codex worker can report completion via MCP instead of the `neige` CLI.
#[tokio::test]
async fn tools_list_for_worker_role_returns_completion_tools() {
    let names = tools_list_names_for_role(CardRole::Worker).await;
    assert_eq!(
        names,
        vec!["calm.task.complete", "calm.task.fail"],
        "worker tools/list must contain exactly the two completion tools",
    );
}

/// #1189 F6 — the `visible_to_roles` widening on the block channel had no
/// test of its own (Planner / Worker / ReportCard each have an exact-set
/// assertion; Assistant did not). Exact set, not `contains`: a future
/// descriptor that quietly adds `CardRole::Assistant` — say
/// `calm.report.write`, whose whole point is that it can carry lifecycle —
/// must turn this red rather than slip in under a subset check.
///
/// `calm.report.read` is deliberately NOT here: it is callable by an
/// assistant (see `mcp_assistant_tool_gate`) but its descriptor is visible
/// only to Planner, so an assistant still receives the report read contract
/// through its agent brief rather than `tools/list`.
#[tokio::test]
async fn tools_list_for_assistant_role_returns_block_channel_only() {
    let names = tools_list_names_for_role(CardRole::Assistant).await;
    assert_eq!(
        names,
        vec![
            "calm.report.blocks.delete",
            "calm.report.blocks.kinds",
            "calm.report.blocks.move",
            "calm.report.blocks.upsert",
            "calm.report.write_markdown",
        ],
        "assistant tools/list must be exactly the report block channel",
    );
}

#[tokio::test]
async fn tools_list_for_report_card_role_is_empty() {
    let names = tools_list_names_for_role(CardRole::ReportCard).await;
    assert!(names.is_empty(), "report card tools/list = {names:?}");
}

#[tokio::test]
async fn tools_list_for_shared_daemon_resolves_thread_role() {
    let boot = boot_shared_daemon_with_planner_thread().await;
    let (mut rd, mut wr) = connect(&boot.socket_path).await;
    let daemon_token = boot.daemon_token.as_deref().expect("daemon token");
    handshake_daemon(&mut rd, &mut wr, daemon_token).await;

    send_frame(&mut wr, tools_list_frame(2, &boot.thread_id)).await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tools/list errored: {resp:#?}");
    let names = tool_names_from_response(&resp);
    assert_eq!(names, expected_planner_toolset());
    let _ = (&boot.server, &boot.repo);
}

#[tokio::test]
async fn tools_list_for_shared_daemon_without_thread_returns_role_union() {
    let boot = boot_shared_daemon_with_planner_thread().await;
    let (mut rd, mut wr) = connect(&boot.socket_path).await;
    let daemon_token = boot.daemon_token.as_deref().expect("daemon token");
    handshake_daemon(&mut rd, &mut wr, daemon_token).await;

    send_frame(
        &mut wr,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tools/list errored: {resp:#?}");

    let names = tool_names_from_response(&resp);
    assert!(
        !names.contains(&"calm.task.dispatch".to_string()),
        "daemon-trust tools/list without threadId must hide retired task.dispatch, got: {names:?}"
    );
    assert!(
        !names.contains(&"calm.plan.upsert".to_string()),
        "daemon-trust role union must hide retired plan.upsert, got: {names:?}"
    );
    assert!(
        names.contains(&"calm.report.write".to_string()),
        "daemon-trust tools/list without threadId must include report.write, got: {names:?}"
    );
    let _ = (&boot.server, &boot.repo);
}

#[tokio::test]
async fn plan_upsert_hidden_shim_retains_original_input_schema() {
    let registry = calm_server::mcp_server::build_default_registry();
    let descriptors = registry.descriptors();
    let upsert = descriptors
        .iter()
        .find(|d| d.name == "calm.plan.upsert")
        .expect("calm.plan.upsert descriptor");

    assert!(
        upsert.description.contains("Deprecated compatibility shim"),
        "shim migration description missing: {}",
        upsert.description
    );
    assert!(upsert.visible_to_roles.is_empty());

    let golden: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/plan_upsert_input_schema.json"))
            .expect("plan.upsert schema golden JSON");
    assert_eq!(
        upsert.input_schema, golden,
        "hidden shim must retain the complete legacy input schema"
    );
}
