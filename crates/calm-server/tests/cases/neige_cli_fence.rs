//! #1801 old-client fence and forwarding-protocol identity at `initialize`, against a real kernel socket.

#![cfg(unix)]

use crate::support;

use calm_server::model::CardRole;
use serde_json::{Value, json};
use support::mcp::{
    boot_shared_daemon_with_planner_thread, boot_with_role, connect, forward_initialize_line,
    neige_cli_line, recv_frame, send_frame, send_line,
};

const OLD_NEIGE_CLIENT_CODE: i64 = -32426;

/// The first frame every on-disk fat `neige` sends (captured from `~/.local/share/neige-next/bin/neige state`
/// with a fake socket, docs/architecture/1801-kernel-served-cli.md §4.3); only the token differs.
fn captured_old_client_initialize(token: &str) -> String {
    format!(
        r#"{{"id":1,"jsonrpc":"2.0","method":"initialize","params":{{"_meta":{{"dev.neige/auth":{{"token":{}}}}},"capabilities":{{}},"clientInfo":{{"name":"neige","version":"0.1.0"}},"protocolVersion":"2024-11-05"}}}}"#,
        serde_json::to_string(token).unwrap()
    )
}

fn kernel_neige_path() -> String {
    std::env::current_exe()
        .expect("current_exe")
        .parent()
        .expect("test binary dir")
        .join("neige")
        .display()
        .to_string()
}

async fn initialize_line(socket: &std::path::Path, line: &str) -> Value {
    let (mut rd, mut wr) = connect(socket).await;
    send_line(&mut wr, line).await;
    recv_frame(&mut rd).await
}

fn initialize_with_client_info(token: &str, client_info: Option<Value>) -> Value {
    let mut params = json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "_meta": { "dev.neige/auth": { "token": token } }
    });
    if let Some(info) = client_info {
        params["clientInfo"] = info;
    }
    json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": params })
}

#[tokio::test]
async fn old_fat_neige_client_is_refused_with_kernel_bin_path() {
    let boot = boot_with_role(CardRole::Planner).await;
    let resp = initialize_line(
        &boot.socket_path,
        &captured_old_client_initialize(&boot.raw_token),
    )
    .await;
    let err = resp
        .get("error")
        .unwrap_or_else(|| panic!("old client accepted: {resp:#?}"));
    assert_eq!(err["code"], json!(OLD_NEIGE_CLIENT_CODE), "{resp:#?}");
    let message = err["message"].as_str().expect("message");
    let expected = format!("run {}.", kernel_neige_path());
    assert!(
        message.contains(&expected),
        "message = {message:?}, want {expected:?}"
    );
    assert!(
        message.contains("redeploy all binaries together"),
        "{message:?}"
    );
}

#[tokio::test]
async fn forward_protocol_version_other_than_1_is_refused() {
    let boot = boot_with_role(CardRole::Planner).await;
    let line =
        forward_initialize_line(&boot.raw_token).replace(r#""version":"1""#, r#""version":"2""#);
    let resp = initialize_line(&boot.socket_path, &line).await;
    let err = resp
        .get("error")
        .unwrap_or_else(|| panic!("v2 accepted: {resp:#?}"));
    assert_eq!(err["code"], json!(OLD_NEIGE_CLIENT_CODE), "{resp:#?}");
    let message = err["message"].as_str().expect("message");
    assert!(message.contains("version 2"), "{message:?}");
    assert!(message.contains(&kernel_neige_path()), "{message:?}");

    let ok = initialize_line(&boot.socket_path, &forward_initialize_line(&boot.raw_token)).await;
    assert!(ok.get("error").is_none(), "v1 refused: {ok:#?}");
}

/// Shim leg: `mcp_shim_round_trip::shim_round_trip_initialize_and_tools_call_completes` drives the same `handle_initialize`.
#[tokio::test]
async fn non_neige_client_infos_still_initialize() {
    let boot = boot_with_role(CardRole::Planner).await;
    for info in [
        Some(json!({ "name": "codex-mcp-client", "version": "0.153.4" })),
        Some(json!({ "name": "claude-code", "title": "Claude Code", "version": "2.1.280" })),
        None,
    ] {
        let (mut rd, mut wr) = connect(&boot.socket_path).await;
        send_frame(
            &mut wr,
            initialize_with_client_info(&boot.raw_token, info.clone()),
        )
        .await;
        let resp = recv_frame(&mut rd).await;
        assert!(resp.get("error").is_none(), "{info:?} refused: {resp:#?}");
        assert_eq!(
            resp["result"]["serverInfo"]["name"],
            json!("neige-calm-kernel")
        );
    }
}

#[tokio::test]
async fn old_client_fence_precedes_token_check() {
    let boot = boot_with_role(CardRole::Planner).await;
    let resp = initialize_line(
        &boot.socket_path,
        &captured_old_client_initialize("not-a-real-token"),
    )
    .await;
    assert_eq!(
        resp["error"]["code"],
        json!(OLD_NEIGE_CLIENT_CODE),
        "{resp:#?}"
    );
}

#[tokio::test]
async fn neige_cli_on_daemon_trust_connection_is_refused() {
    let boot = boot_shared_daemon_with_planner_thread().await;
    let daemon_token = boot.daemon_token.clone().expect("daemon token");
    for argv in [&["--help"][..], &["state"][..]] {
        let (mut rd, mut wr) = connect(&boot.socket_path).await;
        send_line(&mut wr, &forward_initialize_line(&daemon_token)).await;
        let init = recv_frame(&mut rd).await;
        assert!(init.get("error").is_none(), "daemon initialize: {init:#?}");
        send_line(&mut wr, &neige_cli_line(argv)).await;
        let resp = recv_frame(&mut rd).await;
        assert!(resp.get("result").is_none(), "{argv:?} served: {resp:#?}");
        assert_eq!(resp["error"]["code"], json!(-32600), "{argv:?}: {resp:#?}");
    }
}
