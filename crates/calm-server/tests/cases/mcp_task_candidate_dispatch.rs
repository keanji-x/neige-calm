//! The named consumer entry point; core candidate guards remain in their own suite.
use crate::mcp_task_dispatch::{args, boot, counts, dispatch, payload};
use serde_json::{Value, json};

pub(crate) fn candidate_args() -> Value {
    let mut a = args();
    a["workspace"] = json!("verified-candidate");
    a["input"] = json!({"producer":"release-2","slot":"release_bundle"});
    a
}

#[tokio::test]
async fn dispatch_candidate_maps_exact_nondefault_input_and_replays_without_writes() {
    let b = boot().await;
    crate::task_recovery::declare(&b, json!({
        "key":"release-2","kind":"codex","goal":"Produce release", "ready":false,
        "declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR, "no_gate_reason":"Candidate checks",
        "context":{"neige_execution":{"version":"isolated-codex-v1","workspace":"empty",
            "file_delivery":{"role":"candidate_producer","slot":"release_bundle","paths":["release.txt"],
                "policy":{"scope":"declared-checks-only","timeout_secs":20,"steps":[{"name":"check","cmd":"test -f release.txt"}]}}}}
    })).await;
    let before = payload(&b).await;
    let first = dispatch(&b, candidate_args()).await.unwrap();
    let after = payload(&b).await;
    assert_eq!(after.doc_rev, before.doc_rev + 1);
    let blocks = after.blocks.as_ref().unwrap();
    assert_eq!(blocks.len(), before.blocks.as_ref().unwrap().len() + 1);
    let block = blocks
        .iter()
        .find(|b| b.id == first["receipt"]["block_id"])
        .unwrap();
    assert_eq!(
        block.payload["context"],
        json!({"neige_execution":{
            "version":"isolated-codex-v1","workspace":"file-input",
            "file_delivery":{"role":"candidate_consumer","producer":"release-2","slot":"release_bundle","purpose":"verified-candidate-input"}
        }})
    );
    assert_eq!(first["current"]["contract_status"], "matches_dispatch");
    let saved = counts(&b).await;
    assert_eq!(saved.0, 1);
    assert_eq!(
        dispatch(&b, candidate_args()).await.unwrap()["receipt"],
        first["receipt"]
    );
    assert_eq!(counts(&b).await, saved);
    assert_eq!(payload(&b).await, after);
    for (field, value) in [("producer", "release-3"), ("slot", "other_bundle")] {
        let mut changed = candidate_args();
        changed["input"][field] = json!(value);
        assert_eq!(dispatch(&b, changed).await.unwrap_err().code, -32409);
        assert_eq!(counts(&b).await, saved);
        assert_eq!(payload(&b).await, after);
    }
}

#[tokio::test]
async fn dispatch_candidate_rejects_malformed_input_without_writes() {
    let b = boot().await;
    let initial = counts(&b).await;
    let mut cases = vec![];
    let mut absent = candidate_args();
    absent.as_object_mut().unwrap().remove("input");
    cases.push(absent);
    for input in [
        Value::Null,
        json!({}),
        json!({"producer":"release-2"}),
        json!({"slot":"release_bundle"}),
        json!({"producer":null,"slot":"release_bundle"}),
        json!({"producer":"release-2","slot":null}),
        json!({"producer":"release-2\n","slot":"release_bundle"}),
        json!({"producer":"release-2","slot":"release_bundle\n"}),
        json!({"producer":"../foreign","slot":"release_bundle"}),
        json!({"producer":"release-2","slot":"../path"}),
        json!({"producer":"release-2","slot":"release_bundle","track_id":"foreign"}),
    ] {
        let mut a = candidate_args();
        a["input"] = input;
        cases.push(a);
    }
    let mut empty_with_input = candidate_args();
    empty_with_input["workspace"] = json!("empty");
    cases.push(empty_with_input);
    for a in cases {
        assert_eq!(
            dispatch(&b, a.clone()).await.unwrap_err().code,
            -32602,
            "{a}"
        );
        assert_eq!(counts(&b).await, initial);
    }
}

#[tokio::test]
async fn dispatch_legacy_empty_json_payload_and_reply_remain_compatible() {
    const LEGACY: &str = r#"{"name":"Summarize release scope","goal":"Write a concise release scope summary","acceptance":"The completion report names the supported scope and exclusions","executor":"codex","workspace":"empty"}"#;
    let b = boot().await;
    let first = dispatch(&b, serde_json::from_str(LEGACY).unwrap())
        .await
        .unwrap();
    let after = payload(&b).await;
    let block = after
        .blocks
        .as_ref()
        .unwrap()
        .iter()
        .find(|b| b.id == first["receipt"]["block_id"])
        .unwrap();
    assert_eq!(
        block.payload,
        json!({
            "key":first["receipt"]["task_key"],"kind":"codex","goal":args()["goal"],
            "acceptance":args()["acceptance"],"ready":true,"declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,
            "no_gate_reason":"Semantic acceptance is reviewed from the completion report; it is not a machine gate or file candidate qualification.",
            "context":{"neige_execution":{"version":"isolated-codex-v1","workspace":"empty"}}
        })
    );
    sqlx::query("UPDATE planner_dispatch_receipts SET contract_json=?1")
        .bind(LEGACY)
        .execute(&b.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let saved = counts(&b).await;
    let replay = dispatch(&b, args()).await.unwrap();
    assert_eq!(replay["receipt"], first["receipt"]);
    assert_eq!(replay["current"]["contract_status"], "matches_dispatch");
    assert_eq!(counts(&b).await, saved);
    assert_eq!(payload(&b).await, after);
    assert_eq!(
        replay["current"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec![
            "allocation",
            "as_of_ms",
            "blocking_reason",
            "contract_status",
            "declaration_present",
            "declaration_unavailable",
            "declaration_withdrawn",
            "diagnostics",
            "executor_environment",
            "task",
            "track"
        ]
    );
    let stored: String = sqlx::query_scalar("SELECT contract_json FROM planner_dispatch_receipts")
        .fetch_one(&b.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(stored, LEGACY);
}
