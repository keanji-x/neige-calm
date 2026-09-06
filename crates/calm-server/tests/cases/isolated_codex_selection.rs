//! Explicit single-task selection through the production report and dispatch codec.
use crate::mcp_track_report::{boot, call_tool, planner_identity};
use crate::task_recovery::{current, declare};
use calm_server::mcp_server::tools::track_report_blocks::TOOL_REPORT_BLOCKS_UPSERT;
use calm_server::scheduler::build_worker_payload;
use serde_json::{Value, json};

fn declaration(key: &str) -> Value {
    json!({"key":key,"kind":"codex","goal":"Create result.txt and report the result.",
        "no_gate_reason":"Single-task report-driven execution; no machine verification.",
        "declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,
        "ready":true,"context":{"neige_execution":{"version":"isolated-codex-v1","workspace":"empty"}}})
}

#[tokio::test]
async fn isolated_codex_authored_context_selects_distinct_operation() {
    let boot = boot().await;
    declare(&boot, declaration("isolated")).await;
    let task = current(&boot, "isolated").await;
    let (kind, payload) = build_worker_payload(&task).unwrap();
    assert_eq!(kind, "codex-isolated-worker");
    assert_eq!(
        payload,
        json!({"version":"isolated-worker-v1","actor":calm_server::ids::ActorId::KernelDispatcher,
        "track_id":boot.track_id,"task_id":task.id,"idempotency_key":task.id})
    );
    let mut legacy = declaration("ordinary");
    legacy["context"] = json!({"ordinary":"context"});
    declare(&boot, legacy).await;
    let task = current(&boot, "ordinary").await;
    assert_eq!(build_worker_payload(&task).unwrap().0, "codex-worker");
}

#[tokio::test]
async fn isolated_codex_authoring_rejects_invalid_and_unsupported_selection() {
    let boot = boot().await;
    let mut cases = Vec::new();
    for tag in [
        Value::Null,
        json!({"version":"future","workspace":"empty"}),
        json!({"version":"isolated-codex-v1","workspace":"repository"}),
        json!({"version":"isolated-codex-v1","workspace":"empty","fallback":true}),
    ] {
        let mut value = declaration("invalid");
        value["context"]["neige_execution"] = tag;
        cases.push(value);
    }
    for (field, value) in [
        ("depends_on", json!(["other"])),
        ("gate", json!({"steps":[{"name":"check","cmd":"true"}]})),
        ("kind", json!("claude")),
        ("spawn", json!("sub-wave")),
    ] {
        let mut payload = declaration("invalid");
        payload[field] = value;
        cases.push(payload);
    }
    for payload in cases {
        let report = call_tool(
            &boot,
            "calm.report.read",
            planner_identity(&boot),
            json!({}),
        )
        .await
        .unwrap();
        let result = call_tool(
            &boot,
            TOOL_REPORT_BLOCKS_UPSERT,
            planner_identity(&boot),
            json!({"kind":"task","payload":payload,"if_doc_rev":report["docRev"]}),
        )
        .await;
        assert!(
            result.is_err(),
            "unsupported selection must not persist or fall back: {payload}"
        );
    }
    assert!(
        boot.repo
            .tasks_by_track(boot.track_id.as_str())
            .await
            .unwrap()
            .is_empty()
    );
}
