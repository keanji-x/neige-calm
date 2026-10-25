//! #2087 B4 (§4 closed input, §5 codes): every kernel tool and every built-in's compiled plugin
//! tool, called through the kernel socket by an authenticated Planner session, refuses an unknown
//! top-level key with `-32602`, led by its own name and naming every key its declared schema
//! accepts.

#![cfg(unix)]

use crate::support;

use calm_server::mcp_server::build_default_registry;
use calm_server::model::CardRole;
use calm_server::session_projection_repo::AgentProvider;
use serde_json::{Value, json};
use support::mcp::{boot_with_role, call_tool_via_socket};

/// The kernel registry's tools, each with the keys its declared schema accepts, by name.
fn kernel_tools() -> Vec<(String, Vec<String>, Value)> {
    let mut tools: Vec<_> = build_default_registry()
        .descriptors()
        .into_iter()
        .map(|descriptor| {
            let mut keys: Vec<String> = descriptor.input_schema["properties"]
                .as_object()
                .map(|properties| properties.keys().cloned().collect())
                .unwrap_or_default();
            keys.sort();
            (descriptor.name, keys, descriptor.input_schema)
        })
        .collect();
    tools.sort_by(|a, b| a.0.cmp(&b.0));
    tools
}

use crate::support::builtins as configured;

async fn scope_track(boot: &support::mcp::CardBoot, scope: Option<&str>) {
    let pool = boot.repo.sqlite_db().expect("sqlite-backed boot");
    sqlx::query("UPDATE tracks SET plugin_scope = ?1 WHERE id = ?2")
        .bind(scope)
        .bind(boot.track_id.as_str())
        .execute(pool.writer_pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn every_kernel_tool_refuses_unknown_arguments() {
    let boot = boot_with_role(CardRole::Planner).await;
    let _builtins = configured::running(&boot).await;

    let tools = kernel_tools();
    assert!(
        tools.len() >= 40,
        "anti-vacuity: {} kernel tools",
        tools.len()
    );
    // The built-ins' compiled plugin tools sit in the kernel registry too (#2227): dispatch reaches
    // them before any manifest route, so they refuse under their minted names here.
    for native in [
        "plugin_gitforge_publish",
        "plugin_calendar_add",
        "plugin_calendar_ls",
        "plugin_calendar_set",
        "plugin_calendar_rm",
    ] {
        assert!(tools.iter().any(|(name, ..)| name == native), "{native}");
    }
    for (name, keys, schema) in &tools {
        assert_eq!(
            schema["additionalProperties"],
            json!(false),
            "{name}: the declared schema is closed like its input"
        );
        // A native tool is reachable on a Track its plugin owns.
        let scope = calm_server::builtin_plugins::owner(name).map(|p| p.manifest().id.clone());
        scope_track(&boot, scope.as_deref()).await;

        let resp = call_tool_via_socket(
            &boot.socket_path,
            &boot.raw_token,
            &boot.thread_id,
            7,
            name,
            json!({ "zz": 1 }),
        )
        .await;
        let error = &resp["error"];
        let valid = if keys.is_empty() {
            "none".to_string()
        } else {
            keys.join(", ")
        };
        assert_eq!(
            (&error["code"], error["message"].as_str()),
            (
                &json!(-32602),
                Some(format!("{name}: unknown argument `zz`; valid: {valid}").as_str())
            ),
            "{name}: {resp}"
        );
    }
}

/// #2227: dispatch finds a built-in's compiled tool by its minted name in the kernel registry,
/// before any manifest route, and the call reaches the compiled handler: Calendar lists, publish
/// answers its own refusal. The old `neige_` names are unknown; no alias serves them.
#[tokio::test]
async fn compiled_plugin_tools_dispatch_under_their_minted_names() {
    let boot = boot_with_role(CardRole::Planner).await;
    let _builtins = configured::running(&boot).await;
    let call = |name: &'static str, args: Value| {
        let boot = &boot;
        async move {
            call_tool_via_socket(
                &boot.socket_path,
                &boot.raw_token,
                &boot.thread_id,
                9,
                name,
                args,
            )
            .await
        }
    };

    scope_track(&boot, Some("calendar")).await;
    let listed = call(
        "plugin_calendar_ls",
        json!({"from": "2026-10-01", "to": "2026-10-08", "timezone": "UTC"}),
    )
    .await;
    assert_eq!(
        listed["result"]["structuredContent"],
        json!({"entries": []}),
        "{listed}"
    );

    scope_track(&boot, Some("gitforge")).await;
    let refused = call(
        "plugin_gitforge_publish",
        json!({"idempotency_key": "k", "title": "T", "body": "B"}),
    )
    .await;
    assert_eq!(refused["error"]["code"], json!(-32409), "{refused}");
    assert!(
        refused["error"]["message"].as_str().is_some_and(
            |m| m.starts_with("plugin_gitforge_publish: refused: publish-needs-track-worktree")
        ),
        "{refused}"
    );

    for (old, scope) in [
        ("neige_dev_publish", "gitforge"), // retired-name: rejection input
        ("neige_calendar_ls", "calendar"), // retired-name: rejection input
    ] {
        scope_track(&boot, Some(scope)).await;
        let unknown = call(old, json!({})).await;
        assert_eq!(unknown["error"]["code"], json!(-32601), "{old}: {unknown}");
    }
}

/// #2087 C1: gitforge's manifest tools are not kernel tools (its compiled `publish` is, and
/// `every_kernel_tool_refuses_unknown_arguments` covers it); they reach the plugin dispatch, which
/// refuses an unknown key from each closed manifest schema the same way, under the minted name,
/// before the forge runtime is asked.
#[tokio::test]
async fn every_gitforge_tool_refuses_unknown_arguments() {
    let boot = boot_with_role(CardRole::Planner).await;
    let _builtins = configured::running(&boot).await;
    scope_track(&boot, Some("gitforge")).await;
    let manifest = calm_server::builtin_plugins::catalog()
        .iter()
        .map(|plugin| plugin.manifest().clone())
        .find(|manifest| manifest.id == "gitforge")
        .expect("the gitforge built-in");
    assert!(!manifest.exposes_tools.is_empty(), "anti-vacuity");
    for tool in &manifest.exposes_tools {
        let schema = tool.input_schema.as_ref().expect("a declared schema");
        assert_eq!(
            schema["additionalProperties"],
            json!(false),
            "{}: the manifest schema is closed",
            tool.name
        );
        let mut keys: Vec<&str> = schema["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        let name = calm_server::plugin_results::registry_name(&manifest.id, &tool.name);
        let resp = call_tool_via_socket(
            &boot.socket_path,
            &boot.raw_token,
            &boot.thread_id,
            7,
            &name,
            json!({ "zz": 1 }),
        )
        .await;
        assert_eq!(
            (&resp["error"]["code"], resp["error"]["message"].as_str()),
            (
                &json!(-32602),
                Some(format!("{name}: unknown argument `zz`; valid: {}", keys.join(", ")).as_str())
            ),
            "{name}: {resp}"
        );
    }
}

#[tokio::test]
async fn planner_diagnostics_regression_codex_error_code_visible() {
    kernel_refusal_reaches_model(AgentProvider::Codex).await;
}

#[tokio::test]
async fn planner_diagnostics_regression_claude_first_text_code_visible() {
    kernel_refusal_reaches_model(AgentProvider::Claude).await;
}

/// Real provider-attributed session → authenticated kernel → RPC framing → stdio shim.
/// Both production providers use the default shim mode; ACP also projects successful results.
async fn kernel_refusal_reaches_model(provider: AgentProvider) {
    use std::process::Stdio;
    use support::mcp::TEST_BUDGET;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::time::timeout;

    let boot = if provider == AgentProvider::Codex {
        support::mcp::boot_shared_daemon_with_planner_thread().await
    } else {
        boot_with_role(CardRole::Planner).await
    };
    // A shared Planner's payload and session must declare the same provider. Reuse
    // the production payload factory; card kind alone does not identify its provider.
    let payload = calm_server::track_creation::planner_harness_card_payload(
        &calm_server::acp_planner::config::AcpPlannerHost::unconfigured_scratch().unwrap(),
        None,
        provider.clone(),
    );
    sqlx::query("UPDATE cards SET payload = ?1 WHERE id = ?2")
        .bind(payload.to_string())
        .bind(&boot.card_id)
        .execute(boot.sqlx.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET provider = ?1 WHERE id = ?2")
        .bind(provider.wire_name())
        .bind(&boot.session_id)
        .execute(boot.sqlx.pool())
        .await
        .unwrap();
    let state = projection_raw_call(&boot, 2, "neige_track_status", json!({})).await;
    let caller = state["result"]["structuredContent"]["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["id"].as_str() == Some(boot.card_id.as_str()))
        .unwrap();
    assert_eq!(
        caller["provider"],
        provider.wire_name(),
        "production session attribution"
    );
    let report_card = boot
        .repo
        .card_create(calm_server::model::NewCard {
            track_id: boot.track_id.clone(),
            title: None,
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(calm_server::track_report::TrackReportPayload::initial())
                .unwrap(),
        })
        .await
        .unwrap();
    let arguments = json!({"message":"declare", "ops":[{"op":"upsert", "kind":"task", "payload":{
        "key":"oversized", "kind":"claude", "ready":false, "declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR, "goal":"中".repeat(calm_types::report_blocks::MAX_STRING_CHARS + 1)
    }}]});
    let wire = projection_raw_call(&boot, 7, "neige_report_commit", arguments.clone()).await;
    assert_eq!(wire["error"]["code"], -32602, "{wire}");
    let read = projection_raw_call(&boot, 8, "neige_report_read", json!({})).await;
    assert!(read.get("error").is_none(), "{read}");
    // A foreign user edit goes through the production persist boundary; it must not
    // advance the Planner session's read anchors.
    calm_server::track_report::persist_report(
        boot.repo.as_ref(),
        &boot.events,
        &calm_server::state::WriteContext::new(
            boot.card_role_cache.clone(),
            boot.track_area_cache.clone(),
        ),
        calm_server::ids::ActorId::User,
        calm_server::event::EditAuthor::User,
        boot.repo
            .track_get(boot.track_id.as_str())
            .await
            .unwrap()
            .unwrap(),
        report_card,
        calm_server::track_report::TrackReportPayload::initial(),
        calm_server::track_report::TrackReportPayload::new("", "# 概要\n\npeer edit\n"),
        read["result"]["structuredContent"]["doc_rev"]
            .as_u64()
            .unwrap(),
        None,
    )
    .await
    .unwrap();
    let stale_arguments = json!({"body":"# 概要\n\nplanner edit\n", "message":"merge needed"});
    let stale = projection_raw_call(&boot, 11, "neige_report_write", stale_arguments.clone()).await;
    assert_eq!(stale["error"]["code"], -32409, "{stale}");
    assert!(stale["error"]["data"]["doc_rev"].is_number(), "{stale}");

    // Production Codex authenticates the shared daemon and supplies threadId;
    // Claude uses the card-bound token. Both use the default content mode.
    let (credential_name, credential) = boot
        .daemon_token
        .as_deref()
        .map_or(("NEIGE_MCP_TOKEN", boot.raw_token.as_str()), |token| {
            ("NEIGE_MCP_DAEMON_TOKEN", token)
        });
    for shim_args in [&[][..], &["--structured-content-as-text"][..]] {
        let shim = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("neige-mcp-stdio-shim");
        let mut child = tokio::process::Command::new(&shim)
            .args(shim_args)
            .env("NEIGE_MCP_SOCKET", &boot.socket_path)
            .env_remove("NEIGE_MCP_TOKEN")
            .env_remove("NEIGE_MCP_DAEMON_TOKEN")
            .env(credential_name, credential)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("build neige-mcp-stdio-shim before this test");
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        for mut request in [
            json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
                "protocolVersion":"2024-11-05", "capabilities":{}, "clientInfo":{"name":provider.wire_name(),"version":"1"}
            }}),
            json!({"jsonrpc":"2.0", "id":7, "method":"tools/call", "params":{"name":"neige_report_commit", "arguments":arguments}}),
            json!({"jsonrpc":"2.0", "id":11, "method":"tools/call", "params":{"name":"neige_report_write", "arguments":stale_arguments}}),
        ] {
            if request["method"] == "tools/call" && boot.daemon_token.is_some() {
                request["params"]["_meta"] = json!({"threadId": boot.thread_id});
            }
            timeout(
                TEST_BUDGET,
                stdin.write_all(format!("{request}\n").as_bytes()),
            )
            .await
            .unwrap()
            .unwrap();
            let mut line = String::new();
            timeout(TEST_BUDGET, stdout.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            let response: Value = serde_json::from_str(&line).unwrap();
            if request["id"] == 1 {
                assert!(response.get("error").is_none(), "{response}");
            } else {
                let expected = if request["id"] == 7 {
                    &wire["error"]
                } else {
                    &stale["error"]
                };
                assert_eq!(response["id"], request["id"]);
                assert_eq!(
                    response["result"]["structuredContent"]["code"], expected["code"],
                    "Codex must see the original error code: {response}"
                );
                assert_eq!(response["result"]["isError"], true, "{response}");
                assert_eq!(response["result"]["content"][0]["type"], "text");
                let text = response["result"]["content"][0]["text"].as_str().unwrap();
                let visible: Value = serde_json::from_str(text).unwrap();
                assert_eq!(
                    visible["code"], expected["code"],
                    "Claude first text must contain the original error code"
                );
                assert_eq!(visible, *expected);
                assert_eq!(response["result"]["structuredContent"], *expected);
            }
        }
        drop(stdin);
        assert!(
            timeout(TEST_BUDGET, child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
}

/// Raw framing probe through each provider's production authentication route.
async fn projection_raw_call(
    boot: &support::mcp::CardBoot,
    id: i64,
    name: &str,
    arguments: Value,
) -> Value {
    use support::mcp::{connect, handshake, recv_frame, send_frame};
    if let Some(token) = boot.daemon_token.as_deref() {
        return call_tool_via_socket(
            &boot.socket_path,
            token,
            &boot.thread_id,
            id,
            name,
            arguments,
        )
        .await;
    }
    let (mut reader, mut writer) = connect(&boot.socket_path).await;
    handshake(&mut reader, &mut writer, &boot.raw_token).await;
    send_frame(&mut writer, json!({"jsonrpc":"2.0", "id":id, "method":"tools/call", "params":{"name":name,"arguments":arguments}})).await;
    recv_frame(&mut reader).await
}
