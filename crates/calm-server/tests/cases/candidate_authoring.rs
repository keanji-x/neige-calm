//! Captured execution-context regression through an explicitly authored task.
use super::*;

#[tokio::test]
async fn candidate_authoring_accepts_captured_execution_context_without_rewriting_commands() {
    use sha2::{Digest, Sha256};
    let context: Value = serde_json::from_str(include_str!(
        "../fixtures/candidate_planner_unicode_execution_context.json"
    ))
    .unwrap();
    let steps = context["neige_execution"]["file_delivery"]["policy"]["steps"]
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
    use crate::mcp_track_report::{call_tool, planner_identity};
    let boot = crate::mcp_track_report::boot().await;
    // The fixture is only the captured execution context, not a partial task.
    // Spell out the complete declaration and native request here; provenance is
    // the existing author constant, never a missing-field fallback.
    let payload = json!({
        "key": "write-project",
        "kind": "codex",
        "goal": "在隔离工作区 /workspace 创建并交付三个真实文件，且只交付 src/score.py、README.md、tests/test_score.py。实现 score(values)：对非负整数列表求和，空列表返回 0；布尔值（True 与 False）或负整数抛 ValueError。README 用中文说明规则和从项目根运行的 Python 标准库 unittest 命令。tests/test_score.py 恰好六个独立 unittest 测试方法，命名 test_empty、test_single、test_multiple、test_zero、test_negative、test_boolean，分别覆盖空列表、单值、多值、零值、负整数拒绝、布尔值拒绝；禁止跳过、预期失败或模拟 score。真实运行测试。必须遵守已声明候选检查。不得联网、安装依赖、修改 Neige 源码、其他任务或服务；不得创建额外业务任务、计算 hash、手工搬运或猜宿主机目录。结果中报告这三个相对路径和测试结果。平台负责封存与交付，无需你提供主机路径。",
        "acceptance": "三个真实文件组成候选；平台对其准确版本发现恰好六个独立测试并全部通过，直接验证 score([2,3,5])==10 以及负整数、布尔值抛 ValueError。仅机器验收。",
        "no_gate_reason": "使用原生 candidate_producer 冻结的 declared-checks-only 检查替代普通 gate：六个测试全部通过并直接验证求和与异常；检查通过后平台交付同版本。不要求独立 Reviewer。",
        "ready": true,
        "declared_by": calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,
        "context": context.clone()
    });
    let report = call_tool(
        &boot,
        "calm.report.read",
        planner_identity(&boot),
        json!({}),
    )
    .await
    .unwrap();
    call_tool(
        &boot,
        "calm.report.blocks.upsert",
        planner_identity(&boot),
        json!({
            "if_doc_rev": report["docRev"], "kind":"task", "payload":payload
        }),
    )
    .await
    .unwrap();
    let task = current(&boot, "write-project").await;
    let stored: Value = serde_json::from_str(&task.context_json).unwrap();
    assert_eq!(stored, context);
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
