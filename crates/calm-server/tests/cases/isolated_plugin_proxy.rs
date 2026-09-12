//! Real isolated process -> native shim -> platform -> authenticated HTTP connector.
use crate::isolated_codex_smoke::fixture_with_plugin;
use crate::task_recovery::{current, declare};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn isolated_worker_queries_http_plugin_with_network_disabled_policy() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = calls.clone();
    let app = axum::Router::new().route("/mcp", axum::routing::post(move |headers: axum::http::HeaderMap, axum::Json(request): axum::Json<Value>| {
        let recorded=recorded.clone();
        async move {
            assert_eq!(headers.get("authorization").unwrap(), "Bearer fixture-only-secret");
            let result=match request["method"].as_str().unwrap() {
                "initialize" => json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"research-fixture","version":"1"}}),
                "notifications/initialized" => return axum::Json(json!({})),
                "tools/list" => json!({"tools":[
                    {"name":"search","inputSchema":{"type":"object"}},
                    {"name":"detail","inputSchema":{"type":"object"}},
                    {"name":"ungranted","inputSchema":{"type":"object"}}]}),
                "tools/call" => {
                    let name=request["params"]["name"].as_str().unwrap();
                    recorded.lock().unwrap().push(name.to_string());
                    let data=if name=="search" {json!({"id":"source-42"})}
                        else {assert_eq!(name,"detail");assert_eq!(request["params"]["arguments"]["id"],"source-42");json!({"answer":42,"port":port})};
                    json!({"content":[{"type":"text","text":data.to_string()}],"isError":false})
                },
                other=>panic!("unexpected connector method {other}"),
            };
            axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
        }
    }));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let manifest = json!({"manifest_version":1,"kind":"mcp-http","display_name":"Research fixture","id":"research","version":"0.1.0","min_kernel_version":"0.0.1",
        "mcp_http":{"url":format!("http://127.0.0.1:{port}/mcp"),"api_key_secret":"key","api_key_in":"bearer","tools_allow":["search","detail","ungranted"],"request_timeout_ms":3000}});
    let fx = fixture_with_plugin("plugin-proxy", Some(manifest)).await;
    let declaration = json!({"key":"plugin-research","kind":"codex","goal":"Query source through granted plugin tools.","declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,"ready":true,"no_gate_reason":"Read-only research fixture.",
        "context":{"neige_execution":{"version":"isolated-codex-v1","workspace":"empty","plugin_tools":["plugin.research_search","plugin.research_detail"]}}});
    declare(&fx.boot, declaration).await;
    let scheduler = fx.state.dispatcher.scheduler();
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();
    tokio::time::timeout(
        Duration::from_secs(30),
        scheduler.schedule_track(fx.boot.track_id.clone()),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let task = current(&fx.boot, "plugin-research").await;
            if task.status == calm_server::model::TaskStatus::Done {
                break;
            }
            if task.status == calm_server::model::TaskStatus::Failed {
                let op: String = sqlx::query_scalar("SELECT id FROM operations WHERE kind='codex-isolated-worker' AND idempotency_key=?1")
                    .bind(&task.id).fetch_one(&fx.boot.repo.sqlite_pool().unwrap()).await.unwrap();
                let stderr=std::fs::read_to_string(fx.root.path().join("runtime").join(op).join("provider.stderr")).unwrap_or_default();
                panic!("worker failed {:?}; fixture provider stderr: {stderr}",task.status_detail);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["search", "detail"],
        "ungranted call must never reach the external service"
    );
    let task = current(&fx.boot, "plugin-research").await;
    tokio::time::timeout(Duration::from_secs(20),async {
        loop {
            let (phase,raw):(String,String)=sqlx::query_as("SELECT phase,tx_output_json FROM operations WHERE kind='codex-isolated-worker' AND idempotency_key=?1")
                .bind(&task.id).fetch_one(&fx.boot.repo.sqlite_pool().unwrap()).await.unwrap();
            if phase=="succeeded" {
                let record:Value=serde_json::from_str(&raw).unwrap();
                let workspace=record["data"]["isolated_execution"]["request"]["workspace"].as_str().unwrap();
                let data:Value=serde_json::from_str(&std::fs::read_to_string(std::path::Path::new(workspace).join("report-result.json")).unwrap()).unwrap();
                assert_eq!(data,json!({"source":"source-42","answer":42,"command_network_policy":false}));
                break;
            }
            assert_ne!(phase,"failed");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.expect("owned execution must be stopped and settled");
    server.abort();
}
