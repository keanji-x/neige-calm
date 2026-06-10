#![cfg(unix)]

mod support;

use calm_server::model::CardRole;
use serde_json::json;
use support::mcp::{
    boot_shared_daemon_with_spec_thread, boot_with_role, connect, handshake, handshake_daemon,
    recv_frame, send_frame, tools_list_frame,
};

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
    let _ = &boot.server;
    names
}

#[tokio::test]
async fn tools_list_for_spec_role_returns_only_writes() {
    let names = tools_list_names_for_role(CardRole::Spec).await;
    assert_eq!(
        names,
        vec![
            "calm.report.edit",
            "calm.report.write",
            "calm.task.dispatch",
            "calm.task.verdict",
            "calm.update_wave_state",
        ]
    );
}

#[tokio::test]
async fn tools_list_for_spec_role_does_not_leak_aliases() {
    let names = tools_list_names_for_role(CardRole::Spec).await;
    for old_name in [
        "calm.dispatch_request",
        "calm.task_completed",
        "calm.task_failed",
        "calm.get_wave_state",
        "calm.update_task_meta",
    ] {
        assert!(
            !names.iter().any(|name| name == old_name),
            "deprecated alias leaked in tools/list: {old_name}; names={names:?}",
        );
    }
}

#[tokio::test]
async fn tools_list_for_worker_role_is_empty() {
    let names = tools_list_names_for_role(CardRole::Worker).await;
    assert!(names.is_empty(), "worker tools/list = {names:?}");
}

#[tokio::test]
async fn tools_list_for_report_card_role_is_empty() {
    let names = tools_list_names_for_role(CardRole::ReportCard).await;
    assert!(names.is_empty(), "report card tools/list = {names:?}");
}

#[tokio::test]
async fn tools_list_for_shared_daemon_resolves_thread_role() {
    let boot = boot_shared_daemon_with_spec_thread().await;
    let (mut rd, mut wr) = connect(&boot.socket_path).await;
    let daemon_token = boot.daemon_token.as_deref().expect("daemon token");
    handshake_daemon(&mut rd, &mut wr, daemon_token).await;

    send_frame(&mut wr, tools_list_frame(2, &boot.thread_id)).await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tools/list errored: {resp:#?}");
    let tools = resp
        .pointer("/result/tools")
        .and_then(|v| v.as_array())
        .expect("tools");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(|name| name.as_str()))
        .collect();
    assert!(
        names.contains(&"calm.task.dispatch"),
        "spec thread on shared daemon must see task.dispatch, got: {names:?}"
    );
    assert_eq!(
        names.len(),
        5,
        "spec thread sees the 5 write tools, got: {names:?}"
    );
    let _ = (&boot.server, &boot.repo);
}
