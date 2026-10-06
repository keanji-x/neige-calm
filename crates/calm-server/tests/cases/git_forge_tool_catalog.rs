//! #2227: through the real MCP socket and the running git-forge plugin, a Worker of a git-forge
//! Track sees every git-forge row of `neige tool ls` name its plugin and declared kind, and a
//! kernel tool name none. #2289 D3: a running plugin outside the Track's scope stays
//! undiscoverable, while an in-scope native served to another role is described as theirs.

use super::*;

async fn cli_frame(fx: &Fixture, argv: &[&str]) -> Value {
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.raw_token).await;
    send_frame(
        &mut wr,
        json!({"jsonrpc":"2.0","id":1,"method":"neige/cli","params":{"argv":argv}}),
    )
    .await;
    recv_frame(&mut rd).await["result"].clone()
}

async fn cli(fx: &Fixture, argv: &[&str]) -> String {
    let result = cli_frame(fx, argv).await;
    assert_eq!(result["exit"], 0, "{result:#}");
    result["stdout"].as_str().unwrap().to_string()
}

/// `tool describe --name <name> --json` refused: its exit and the error's detail.
async fn describe_refusal(fx: &Fixture, name: &str) -> (Value, Value) {
    let result = cli_frame(fx, &["tool", "describe", "--name", name, "--json"]).await;
    assert_eq!(result["stdout"], "", "{result:#}");
    let stderr: Value = serde_json::from_str(result["stderr"].as_str().unwrap()).unwrap();
    (result["exit"].clone(), stderr["error"]["detail"].clone())
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
                "surfaces": ["mcp"],
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
            "{COMMIT_TOOL}  mcp  —  listed  plugin:{PLUGIN_ID}  forge-action\n"
        )),
        "{text}"
    );

    // The compiled `publish` is in scope and running, and declared for the Planner only.
    assert_eq!(
        describe_refusal(&fx, "plugin_gitforge_publish").await,
        (
            json!(4),
            json!({"kind": "role", "roles": ["Planner"], "role": "Worker"})
        )
    );
    // The calendar runs too, but this Track is bound to git-forge: its tools are no tool here,
    // never another role's.
    fx.plugin_host
        .spawn("calendar")
        .await
        .expect("spawn calendar");
    assert!(
        fx.plugin_host
            .running_plugin_ids()
            .await
            .contains("calendar")
    );
    assert_eq!(
        describe_refusal(&fx, "plugin_calendar_ls").await,
        (json!(4), json!({"kind": "catalog"}))
    );
}
