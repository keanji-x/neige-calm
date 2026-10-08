//! #2003 §4.5, #2289 D2/D3: `neige tool ls|describe` over a real kernel socket. The catalog is
//! every tool the session's role may call, each with its surfaces; `listed` is true exactly for
//! its `tools/list` set; describe tells an unknown tool, a tool served to other roles and an
//! MCP-only tool apart.

#![cfg(unix)]

use crate::support;

use calm_server::mcp_server::build_default_registry;
use calm_server::model::CardRole;
use serde_json::{Value, json};
use support::mcp::{
    CardBoot, boot_with_role, cli_output, connect, handshake, neige_cli_via_socket, recv_frame,
    send_frame, tools_list_frame,
};

/// The reviewed `COMMANDS` table: tool → command.
const CLI_COVERED: [(&str, &str); 16] = [
    ("neige_admin_gc", "neige admin gc"),
    ("neige_admin_vacuum", "neige admin vacuum"),
    ("neige_mail_cat", "neige mail cat"),
    ("neige_mail_ls", "neige mail ls"),
    ("neige_report_find", "neige report find"),
    ("neige_report_tag", "neige report tag"),
    ("neige_task_fail", "neige task fail"),
    ("neige_task_done", "neige task done"),
    ("neige_task_gate", "neige task gate"),
    ("neige_track_cat", "neige track cat"),
    ("neige_track_close", "neige track close"),
    ("neige_track_diff", "neige track diff"),
    ("neige_track_log", "neige track log"),
    ("neige_track_ls", "neige track ls"),
    ("neige_track_show", "neige track show"),
    ("neige_track_status", "neige track status"),
];

fn cli_of(name: &str) -> Option<&'static str> {
    CLI_COVERED
        .iter()
        .find(|(tool, _)| *tool == name)
        .map(|(_, cli)| *cli)
}

fn surfaces_of(name: &str) -> Value {
    match cli_of(name) {
        Some(_) => json!(["mcp", "cli"]),
        None => json!(["mcp"]),
    }
}

async fn cli(boot: &CardBoot, argv: &[&str]) -> (String, String, i64) {
    cli_output(&neige_cli_via_socket(&boot.socket_path, &boot.raw_token, argv).await)
}

/// The registry's kernel tools whose declared `roles` hold `role`, by name. No plugin runs in
/// these fixtures, so no compiled native is served.
fn kernel_tools_callable_by(role: CardRole) -> Vec<String> {
    let mut names: Vec<String> = build_default_registry()
        .descriptors()
        .into_iter()
        .filter(|tool| calm_server::builtin_plugins::owner(&tool.name).is_none())
        .filter(|tool| tool.roles.contains(&role))
        .map(|tool| tool.name)
        .collect();
    names.sort();
    names
}

/// Every `tool ls --all --json` row, following `next_cursor`, each page within the byte and row
/// limits.
async fn all_rows(boot: &CardBoot) -> Vec<Value> {
    let mut rows = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let mut args = vec!["tool", "ls", "--all", "--json"];
        if let Some(cursor) = next.as_deref() {
            args.extend(["--cursor", cursor]);
        }
        let (stdout, stderr, exit) = cli(boot, &args).await;
        assert_eq!((stderr, exit), (String::new(), 0));
        assert!(stdout.len() <= 4097);
        let page: Value = serde_json::from_str(&stdout).unwrap();
        let batch = page["tools"].as_array().unwrap();
        assert!(batch.len() <= 20);
        rows.extend(batch.iter().cloned());
        match page["next_cursor"].as_str() {
            Some(cursor) => {
                assert_ne!(next.as_deref(), Some(cursor));
                next = Some(cursor.into());
            }
            None => break,
        }
    }
    rows
}

fn row<'a>(rows: &'a [Value], name: &str) -> Option<&'a Value> {
    rows.iter().find(|row| row["name"] == name)
}

/// `tool describe --name <name> --json` → (stdout, parsed stderr error, exit).
async fn describe(boot: &CardBoot, name: &str) -> (String, Value, i64) {
    let (stdout, stderr, exit) = cli(boot, &["tool", "describe", "--name", name, "--json"]).await;
    let error = if stderr.is_empty() {
        Value::Null
    } else {
        serde_json::from_str::<Value>(&stderr).unwrap()["error"].clone()
    };
    (stdout, error, exit)
}

/// The rows of every role are exactly the registry tools whose `roles` hold it, and `listed` is
/// true exactly for the session's `tools/list` set; describe gives that declaration, a role
/// refusal for a tool served to other roles, and `no tool` for a name nobody serves. A stale
/// session is refused.
#[tokio::test]
async fn cli_tool_lookup_matches_scoped_mcp_listing_and_rejects_stale_sessions() {
    for role in [CardRole::Planner, CardRole::Assistant, CardRole::Worker] {
        let boot = boot_with_role(role).await;
        let (mut read, mut write) = connect(&boot.socket_path).await;
        handshake(&mut read, &mut write, &boot.raw_token).await;
        send_frame(&mut write, tools_list_frame(90, &boot.thread_id)).await;
        let listed = recv_frame(&mut read).await;
        let declarations = listed["result"]["tools"].as_array().unwrap();
        assert!(!declarations.is_empty());
        let visible: Vec<&str> = declarations
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        let callable = kernel_tools_callable_by(role);
        assert!(
            visible
                .iter()
                .all(|name| callable.iter().any(|c| c == name)),
            "{role:?}: tools/list shows a tool its role may not call"
        );
        let expected: Vec<Value> = callable
            .iter()
            .map(|name| {
                json!({
                    "name": name,
                    "surfaces": surfaces_of(name),
                    "cli": cli_of(name),
                    "listed": visible.contains(&name.as_str()),
                    "plugin": null,
                    "kind": null,
                })
            })
            .collect();
        assert_eq!(all_rows(&boot).await, expected, "{role:?}");

        let (text, stderr, exit) = cli(&boot, &["tool", "ls", "--prefix", "neige_track_"]).await;
        assert_eq!((stderr, exit), (String::new(), 0));
        assert!(
            text.ends_with("listing is not a grant; the tool's role gate decides\n"),
            "{text}"
        );
        let selected = &declarations[0];
        let name = selected["name"].as_str().unwrap();
        let (stdout, error, exit) = describe(&boot, name).await;
        assert_eq!((error, exit), (Value::Null, 0));
        let mut want = selected.clone();
        want["surfaces"] = surfaces_of(name);
        want["cli"] = json!(cli_of(name));
        want["listed"] = json!(true);
        want["plugin"] = Value::Null;
        want["kind"] = Value::Null;
        assert_eq!(serde_json::from_str::<Value>(&stdout).unwrap(), want);

        // `neige_admin_vacuum` is the Planner's; any other role is told whose it is.
        let (stdout, error, exit) = describe(&boot, "neige_admin_vacuum").await;
        if role == CardRole::Planner {
            assert_eq!((error, exit), (Value::Null, 0));
            let vacuum: Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(
                (&vacuum["surfaces"], &vacuum["cli"], &vacuum["listed"]),
                (
                    &json!(["mcp", "cli"]),
                    &json!("neige admin vacuum"),
                    &json!(visible.contains(&"neige_admin_vacuum"))
                )
            );
        } else {
            assert_eq!((stdout.as_str(), exit), ("", 4));
            assert_eq!(
                error["detail"],
                json!({"kind": "role", "roles": ["Planner"], "role": format!("{role:?}")})
            );
        }

        let (stdout, stderr, exit) = cli(&boot, &["tool", "ls", "--prefix", name, "--json"]).await;
        assert_eq!((stderr, exit), (String::new(), 0));
        let prefix: Value = serde_json::from_str(&stdout).unwrap();
        assert!(
            prefix["tools"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["name"].as_str().unwrap().starts_with(name))
        );
        let (stdout, error, exit) = describe(&boot, "neige_hidden_nonexistent").await;
        assert_eq!((stdout.as_str(), exit), ("", 4));
        assert_eq!(error["detail"], json!({"kind": "catalog"}));
        let (_, _, exit) = cli(&boot, &["tool", "ls", "--prefix", "neige_*", "--json"]).await;
        assert_eq!(exit, 1);
        sqlx::query("UPDATE worker_sessions SET state = 'exited' WHERE card_id = ?")
            .bind(&boot.card_id)
            .execute(boot.sqlx.pool())
            .await
            .unwrap();
        send_frame(&mut write, json!({"jsonrpc":"2.0","id":91,"method":"neige/cli","params":{"argv":["tool","ls","--all","--json"]}})).await;
        let rejected = recv_frame(&mut read).await;
        assert_eq!(rejected["result"]["exit"], 4);
        assert_eq!(rejected["result"]["stdout"], "");
    }
}

/// #2289: the owner's report. An Assistant may call `neige_report_read`, an MCP-only tool, so its
/// catalog lists and describes it; a tool whose declared `roles` exclude the Assistant is absent
/// from the listing and described as another role's.
#[tokio::test]
async fn an_assistant_catalog_is_what_it_may_call() {
    let boot = boot_with_role(CardRole::Assistant).await;
    let rows = all_rows(&boot).await;
    assert_eq!(
        row(&rows, "neige_report_read"),
        Some(&json!({
            "name": "neige_report_read",
            "surfaces": ["mcp"],
            "cli": null,
            "listed": true,
            "plugin": null,
            "kind": null,
        }))
    );
    let refused: Vec<String> = build_default_registry()
        .descriptors()
        .into_iter()
        .filter(|tool| !tool.roles.contains(&CardRole::Assistant))
        .map(|tool| tool.name)
        .collect();
    assert!(refused.iter().any(|name| name == "neige_admin_gc"));
    assert!(refused.iter().any(|name| name == "neige_track_close"));
    for name in &refused {
        assert!(row(&rows, name).is_none(), "{name} is not the Assistant's");
    }

    let (stdout, error, exit) = describe(&boot, "neige_report_read").await;
    assert_eq!((error, exit), (Value::Null, 0));
    let read: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        (
            &read["name"],
            &read["surfaces"],
            &read["cli"],
            &read["listed"]
        ),
        (
            &json!("neige_report_read"),
            &json!(["mcp"]),
            &Value::Null,
            &json!(true)
        )
    );

    let (stdout, error, exit) = describe(&boot, "neige_admin_gc").await;
    assert_eq!((stdout.as_str(), exit), ("", 4));
    assert_eq!(
        error,
        json!({
            "message": "`neige_admin_gc` is for roles [Planner]; this session is Assistant",
            "detail": {"kind": "role", "roles": ["Planner"], "role": "Assistant"},
        })
    );
    let (_, stderr, exit) = cli(&boot, &["tool", "describe", "--name", "neige_admin_gc"]).await;
    assert_eq!(
        (stderr.as_str(), exit),
        (
            "neige: `neige_admin_gc` is for roles [Planner]; this session is Assistant\n",
            4
        )
    );

    let (stdout, error, exit) = describe(&boot, "neige_nope_x").await;
    assert_eq!((stdout.as_str(), exit), ("", 4));
    assert_eq!(
        error,
        json!({
            "message": "no tool `neige_nope_x` in this session; `neige tool ls --all` lists them",
            "detail": {"kind": "catalog"},
        })
    );
}

/// #2289 D2/D3: a Planner's CLI-covered hidden tools are rows served over both surfaces and not
/// listed; a kernel tool without a command is refused on the CLI as MCP-only, whether its object
/// has commands (`report`) or none (`workspace`).
#[tokio::test]
async fn a_planner_sees_hidden_cli_tools_and_is_told_which_tools_are_mcp_only() {
    let boot = boot_with_role(CardRole::Planner).await;
    let rows = all_rows(&boot).await;
    assert_eq!(
        row(&rows, "neige_track_cat"),
        Some(&json!({
            "name": "neige_track_cat",
            "surfaces": ["mcp", "cli"],
            "cli": "neige track cat",
            "listed": false,
            "plugin": null,
            "kind": null,
        }))
    );
    let (text, _, exit) = cli(&boot, &["tool", "ls", "--prefix", "neige_track_cat"]).await;
    assert_eq!(exit, 0);
    assert!(
        text.starts_with("neige_track_cat  mcp+cli  neige track cat  hidden  kernel  —\n"),
        "{text}"
    );
    let (text, _, exit) = cli(&boot, &["tool", "ls", "--prefix", "neige_report_read"]).await;
    assert_eq!(exit, 0);
    assert!(
        text.starts_with("neige_report_read  mcp  —  listed  kernel  —\n"),
        "{text}"
    );

    for (argv, tool) in [
        (
            &["report", "read", "--path", "report.md"][..],
            "neige_report_read",
        ),
        (&["workspace", "ls"][..], "neige_workspace_ls"),
    ] {
        let (stdout, stderr, exit) = cli(&boot, argv).await;
        assert_eq!(
            (stdout.as_str(), stderr, exit),
            (
                "",
                format!(
                    "neige: `{tool}` has no CLI command; call it as an MCP tool (`neige tool describe --name {tool}` shows it)\n"
                ),
                1
            ),
            "{argv:?}"
        );
    }
    let (_, stderr, exit) = cli(&boot, &["--json", "report", "read"]).await;
    assert_eq!(exit, 1);
    let error: Value = serde_json::from_str(&stderr).unwrap();
    assert_eq!(error["error"]["detail"]["kind"], json!("usage"));
}
