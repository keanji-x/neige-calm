#![cfg(unix)]

mod support;

use calm_server::model::CardRole;
use serde_json::json;
use support::mcp::{boot_with_role, connect, handshake, recv_frame, send_frame};

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
            "calm.dispatch_request",
            "calm.report.edit",
            "calm.report.write",
            "calm.update_task_meta",
            "calm.update_wave_state",
        ]
    );
}

#[tokio::test]
async fn tools_list_for_worker_role_is_empty() {
    let names = tools_list_names_for_role(CardRole::Worker).await;
    assert!(names.is_empty(), "worker tools/list = {names:?}");
}

#[tokio::test]
async fn tools_list_for_plain_role_is_empty() {
    let names = tools_list_names_for_role(CardRole::Plain).await;
    assert!(names.is_empty(), "plain tools/list = {names:?}");
}
