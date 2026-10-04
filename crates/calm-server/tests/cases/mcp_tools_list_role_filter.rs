#![cfg(unix)]

use crate::support;

use calm_server::model::CardRole;
use serde_json::json;
use support::mcp::{
    boot_shared_daemon_with_planner_thread, boot_with_role, connect, handshake, handshake_daemon,
    recv_frame, send_frame, tools_call_frame, tools_list_frame,
};

fn expected_planner_toolset() -> Vec<&'static str> {
    vec![
        "neige.area.outline",
        "neige.plan.cancel",
        "neige.plan.list",
        "neige.preview.register",
        "neige.preview.unregister",
        "neige.ratify.request",
        "neige.report.backlinks",
        "neige.report.commit",
        "neige.report.kinds",
        "neige.report.read",
        "neige.report.write",
        "neige.source.capture",
        "neige.source.list",
        "neige.task.verdict",
        "neige.terminal.control",
        "neige.terminal.input",
        "neige.terminal.observe",
        "neige.terminal.open",
        "neige.terminal.resolve",
        "neige.track.close",
        "neige.track.rename",
        "neige.user.notify",
        "neige.workspace.changes",
        "neige.workspace.edits",
        "neige.workspace.report",
        "neige.workspace.reports",
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

/// #2003: the deprecated aliases and the retired shims are gone, not hidden: no handler is left
/// under any of their names.
#[test]
fn removed_aliases_and_retired_shims_are_not_registered() {
    let registry = calm_server::mcp_server::build_default_registry();
    for removed in [
        "calm.get_track_state",  // retired-name: rejection input
        "calm.update_task_meta", // retired-name: rejection input
        "calm.task_completed",   // retired-name: rejection input
        "calm.task_failed",      // retired-name: rejection input
        "calm.dispatch_request", // retired-name: rejection input
        "calm.plan.upsert",      // retired-name: rejection input
        "neige.task.complete",
        "neige.task.fail",
    ] {
        assert!(
            registry.lookup(removed).is_none(),
            "{removed} must not remain as a hidden tool or alias",
        );
    }
}

/// #2003: `tools/call` with a name the session cannot reach is `-32601`, and the message lists
/// exactly the names that session's `tools/list` shows, so a stale name (here a removed alias)
/// points at the valid choices. Two roles prove the list is the session's, not a fixed one.
#[tokio::test]
async fn unknown_tool_error_lists_the_sessions_tools() {
    for (role, stale) in [
        (CardRole::Planner, "calm.update_task_meta"), // retired-name: rejection input
        (CardRole::Worker, "calm.task_completed"),    // retired-name: rejection input
        (CardRole::Worker, "neige.task.complete"),
        (CardRole::Worker, "neige.task.fail"),
    ] {
        let boot = boot_with_role(role).await;
        let (mut rd, mut wr) = connect(&boot.socket_path).await;
        handshake(&mut rd, &mut wr, &boot.raw_token).await;

        send_frame(&mut wr, tools_list_frame(2, &boot.thread_id)).await;
        let listed = recv_frame(&mut rd).await;
        assert!(
            listed.get("error").is_none(),
            "tools/list errored: {listed:#?}"
        );
        let visible = tool_names_from_response(&listed);
        assert!(!visible.is_empty(), "{role:?} sees no tools");

        send_frame(
            &mut wr,
            tools_call_frame(3, stale, &boot.thread_id, json!({})),
        )
        .await;
        let resp = recv_frame(&mut rd).await;
        assert_eq!(resp["error"]["code"], -32601, "{role:?}: {resp:#?}");
        let message = resp["error"]["message"].as_str().expect("message");
        let (head, list) = message
            .split_once("; tools visible to this session: ")
            .unwrap_or_else(|| {
                panic!("{role:?}: the error must list the session's tools: {message}")
            });
        assert_eq!(head, format!("method not found: tools/call: {stale}"));
        let mut listed_in_error: Vec<String> = list.split(", ").map(str::to_string).collect();
        listed_in_error.sort();
        assert_eq!(
            listed_in_error, visible,
            "{role:?}: the error lists exactly the session's tools/list names"
        );
        let _ = (&boot.server, &boot.repo);
    }
}

/// #1874 / #1883: the Planner writes the report through `commit` and `write` only; the
/// retired writers are neither listed nor callable under their old names.
#[tokio::test]
async fn retired_report_write_and_edit_are_neither_listed_nor_registered() {
    let names = tools_list_names_for_role(CardRole::Planner).await;
    let registry = calm_server::mcp_server::build_default_registry();
    for retired in [
        "calm.report.write",         // retired-name: rejection input
        "calm.report.edit",          // retired-name: rejection input
        "calm.report.blocks.upsert", // retired-name: rejection input
        "calm.report.blocks.move",   // retired-name: rejection input
        "calm.report.blocks.delete", // retired-name: rejection input
    ] {
        assert!(
            !names.iter().any(|name| name == retired),
            "retired report writer in the Planner's tools/list: {retired}; names={names:?}",
        );
        assert!(
            registry.lookup(retired).is_none(),
            "retired report writer must not remain as a hidden tool or alias: {retired}",
        );
    }
    for kept in ["neige.report.commit", "neige.report.write"] {
        assert!(
            names.iter().any(|name| name == kept),
            "the Planner keeps {kept}; names={names:?}",
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

#[tokio::test]
async fn tools_list_for_worker_role_returns_completion_tools() {
    let names = tools_list_names_for_role(CardRole::Worker).await;
    assert_eq!(
        names,
        vec!["neige.task.report_failure", "neige.task.report_success"],
        "worker tools/list must contain exactly the two completion tools",
    );
}

/// `neige.report.read` is deliberately absent: an assistant can call it, but its descriptor is visible only to Planner.
/// #1883: the single-op block writers are gone; the assistant writes through `commit`.
#[tokio::test]
async fn tools_list_for_assistant_role_returns_the_report_write_surface_only() {
    let names = tools_list_names_for_role(CardRole::Assistant).await;
    assert_eq!(
        names,
        vec![
            "neige.report.commit",
            "neige.report.kinds",
            "neige.report.write",
        ],
        "assistant tools/list must be exactly the report write surface",
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
        names.contains(&"neige.task.verdict".to_string()),
        "daemon-trust tools/list without threadId must advertise Planner task.verdict, got: {names:?}"
    );
    assert!(
        names.contains(&"neige.report.commit".to_string()),
        "daemon-trust tools/list without threadId must include report.commit, got: {names:?}"
    );
    let _ = (&boot.server, &boot.repo);
}
