//! Codex's application-owned config starts the production shim; no Codex model is invoked.
use super::*;
use calm_server::mcp_server::McpShimConfig;
use calm_server::shared_codex_home::SharedCodexHome;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::{Duration, timeout};

struct ConfiguredShim {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    _home: tempfile::TempDir,
}
impl ConfiguredShim {
    fn start(socket: &std::path::Path, token: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = SharedCodexHome::new(root.path().join("home"));
        home.ensure_daemon_mcp_config(
            &McpShimConfig {
                shim_bin: support::mcp::locate_shim_bin(),
                socket_path: socket.to_owned(),
            },
            token,
        )
        .unwrap();
        let config: toml::Value =
            toml::from_str(&std::fs::read_to_string(home.path().join("config.toml")).unwrap())
                .unwrap();
        let entry = &config["mcp_servers"]["neige"];
        let mut command = Command::new(entry["command"].as_str().unwrap());
        command.args(
            entry["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap()),
        );
        command.env_clear();
        for (key, value) in entry["env"].as_table().unwrap() {
            command.env(key, value.as_str().unwrap());
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Self {
            input: child.stdin.take().unwrap(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
            _home: root,
        }
    }
    async fn rpc(&mut self, frame: Value) -> Value {
        self.input
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        timeout(Duration::from_secs(5), self.output.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(!line.is_empty(), "shim exited before response");
        serde_json::from_str(&line).unwrap()
    }
    async fn close(self) {
        let Self {
            mut child, input, ..
        } = self;
        drop(input);
        assert!(
            timeout(Duration::from_secs(5), child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
}

#[tokio::test]
async fn shared_codex_home_projects_real_report_commit_and_task_ls() {
    let boot = support::mcp::boot_shared_daemon_with_planner_thread().await;
    add_report(&boot).await;
    let mut shim = ConfiguredShim::start(&boot.socket_path, boot.daemon_token.as_deref().unwrap());
    let init = shim.rpc(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"projection-test","version":"0"}}})).await;
    assert!(init.get("error").is_none(), "{init}");
    let catalog = shim
        .rpc(support::mcp::tools_list_frame(2, &boot.thread_id))
        .await;
    let tools = catalog["result"]["tools"].as_array().unwrap();
    assert!(!tools.is_empty());
    assert!(tools.iter().all(|tool| tool.get("outputSchema").is_none()));
    let read = shim
        .rpc(support::mcp::tools_call_frame(
            3,
            "neige_report_read",
            &boot.thread_id,
            json!({}),
        ))
        .await;
    assert!(read.get("error").is_none(), "{read}");
    for (name, args) in [
        (
            "neige_report_commit",
            json!({"message":"declare task","ops":[{"op":"upsert","kind":"task","payload":task_payload("build")}]}),
        ),
        ("neige_task_ls", json!({})),
    ] {
        let actual = shim
            .rpc(support::mcp::tools_call_frame(
                3,
                name,
                &boot.thread_id,
                args,
            ))
            .await;
        assert!(actual.get("error").is_none(), "{actual}");
        let result = &actual["result"];
        assert_eq!(result["isError"], false, "{actual}");
        assert!(result.get("structuredContent").is_none(), "{actual}");
        let state: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert!(state.is_object());
        if name == "neige_task_ls" {
            // The server default still returns both complete representations.
            let direct = call_tool_via_socket(
                &boot.socket_path,
                &boot.raw_token,
                &boot.thread_id,
                4,
                name,
                json!({}),
            )
            .await;
            assert_eq!(direct["result"]["content"], result["content"]);
            assert_eq!(direct["result"]["structuredContent"], state);
        } else {
            assert!(state["doc_rev"].is_u64(), "{state}");
        }
    }
    shim.close().await;
}
