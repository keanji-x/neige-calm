//! #2227: through the real MCP socket and the running git-forge plugin, a Worker of a git-forge
//! Track sees every git-forge row of `neige tool ls` name its plugin and declared kind, and a
//! kernel tool name none.

use super::*;

async fn cli(fx: &Fixture, argv: &[&str]) -> String {
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    send_frame(
        &mut wr,
        json!({"jsonrpc":"2.0","id":1,"method":"neige/cli","params":{"argv":argv}}),
    )
    .await;
    let frame = recv_frame(&mut rd).await;
    assert_eq!(frame["result"]["exit"], 0, "{frame:#}");
    frame["result"]["stdout"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn tool_rows_name_the_git_forge_plugin_and_its_forge_action_kind() {
    let _env_lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let _trusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", PLUGIN_ID);
    let fx = boot_fixture().await;

    let page: Value = serde_json::from_str(
        &cli(
            &fx,
            &["tool", "ls", "--prefix", "plugin_gitforge_", "--json"],
        )
        .await,
    )
    .unwrap();
    let mut expected: Vec<Value> = read_manifest()
        .exposes_tools
        .iter()
        .map(|tool| {
            json!({
                "name": format!("plugin_gitforge_{}", tool.name),
                "cli": null,
                "listed": true,
                "plugin": PLUGIN_ID,
                "kind": "forge-action",
            })
        })
        .collect();
    expected.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    assert_eq!(
        page,
        json!({"tools": expected, "next_cursor": null}),
        "{page:#}"
    );

    let kernel: Value = serde_json::from_str(
        &cli(
            &fx,
            &["tool", "describe", "--name", "neige_track_ls", "--json"],
        )
        .await,
    )
    .unwrap();
    assert_eq!(
        (&kernel["plugin"], &kernel["kind"]),
        (&Value::Null, &Value::Null)
    );

    let text = cli(&fx, &["tool", "ls", "--prefix", COMMIT_TOOL]).await;
    assert!(
        text.starts_with(&format!(
            "{COMMIT_TOOL}  —  listed  plugin:{PLUGIN_ID}  forge-action\n"
        )),
        "{text}"
    );
}
