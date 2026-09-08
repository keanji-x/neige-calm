//! Actual Planner declaration regression; no Worker is needed to author it.
use super::*;

#[tokio::test]
async fn candidate_authoring_accepts_exact_captured_unicode_policy_without_rewriting_commands() {
    use sha2::{Digest, Sha256};
    let captured: Value = serde_json::from_str(include_str!(
        "../fixtures/candidate_planner_unicode_declaration.json"
    ))
    .unwrap();
    let payload = captured["arguments"]["payload"].clone();
    let steps = payload["context"]["neige_execution"]["file_delivery"]["policy"]["steps"]
        .as_array()
        .unwrap();
    let expected = [
        (
            677,
            "06de9ac924feaeaec4397b55fe8872a7f1d5d2db5eb0d6c06e469dcaa4644ee3",
        ),
        (
            400,
            "9cfca4da87c7773715887228a67d8296218d11df4e316ebb07fcba6f7a00ad9b",
        ),
    ];
    assert_eq!(steps.len(), expected.len());
    for (step, (length, digest)) in steps.iter().zip(expected) {
        let command = step["cmd"].as_str().unwrap();
        assert_eq!(command.len(), length);
        assert_eq!(format!("{:x}", Sha256::digest(command.as_bytes())), digest);
    }
    let boot = crate::mcp_track_report::boot().await;
    // Only the document revision precondition is obtained from this fresh report;
    // the captured declaration payload goes unchanged through native authoring.
    declare(&boot, payload.clone()).await;
    let task = current(&boot, "write-project").await;
    let stored: Value = serde_json::from_str(&task.context_json).unwrap();
    assert_eq!(stored, payload["context"]);
    assert_eq!(task.status, TaskStatus::Pending);
    assert!(task.worker_card_id.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_authoring_human_names_are_literal_in_real_gate_wrapper() {
    let fx = fixture("controlled").await;
    fx.state.dispatcher.abort_event_listener_for_test();
    let quoted = "中文 review's \"quoted\" $HOME $(touch label-injected)";
    let mut declaration = producer("true");
    declaration["context"]["neige_execution"]["file_delivery"]["policy"]["steps"] = json!([
        {"name":"六个独立测试全部通过","cmd":"true"},
        {"name":"直接调用规则验证","cmd":"true"},
        {"name":quoted,"cmd":"exit 7"}
    ]);
    declare(&fx.boot, declaration).await;
    schedule(&fx).await;
    let task = current(&fx.boot, "produce").await;
    let path = workspace(&fx, &task).await;
    for (name, bytes) in FILES {
        std::fs::write(path.join(name), bytes).unwrap();
    }
    settle(&fx, &task, true).await;
    let publication = publish(&fx, &task).await;
    schedule(&fx).await;
    let evidence = verified(&fx, &publication).await;
    assert_eq!(evidence["verdict"]["exit_code"], 7);
    assert_eq!(evidence["verdict"]["failing_step"], quoted);
    let log = evidence["verdict"]["log_tail"].as_str().unwrap();
    assert!(log.contains("六个独立测试全部通过") && log.contains("直接调用规则验证"));
    let workspace = PathBuf::from(evidence["verdict"]["log_path"].as_str().unwrap())
        .parent()
        .unwrap()
        .to_owned();
    assert!(!workspace.join("input/source/label-injected").exists());
}

#[tokio::test]
async fn candidate_authoring_reports_policy_paths_through_native_errors() {
    use crate::mcp_track_report::{call_tool, planner_identity};
    let boot = crate::mcp_track_report::boot().await;
    let report = call_tool(
        &boot,
        "calm.report.read",
        planner_identity(&boot),
        json!({}),
    )
    .await
    .unwrap();
    let cases = [
        ("/timeout_secs", json!(0), "policy.timeout_secs"),
        ("/steps", json!([]), "policy.steps"),
        ("/steps/1/name", json!("bad\0name"), "policy.steps[1].name"),
        ("/steps/1/cmd", json!(" "), "policy.steps[1].cmd"),
        ("/steps/1/name", json!("first"), "policy.steps[1].name"),
    ];
    for (path, replacement, expected) in cases {
        let mut payload = producer("true");
        let policy = &mut payload["context"]["neige_execution"]["file_delivery"]["policy"];
        policy["steps"] = json!([{"name":"first","cmd":"true"},{"name":"second","cmd":"true"}]);
        *policy.pointer_mut(path).unwrap() = replacement;
        let error = call_tool(
            &boot,
            "calm.report.blocks.upsert",
            planner_identity(&boot),
            json!({"if_doc_rev":report["docRev"],"kind":"task","payload":payload}),
        )
        .await
        .unwrap_err();
        assert!(error.message.contains(expected), "{path}: {error:?}");
        if path == "/steps/1/name" && error.message.contains("duplicates") {
            assert!(error.message.contains("policy.steps[0].name"));
        }
    }
}
