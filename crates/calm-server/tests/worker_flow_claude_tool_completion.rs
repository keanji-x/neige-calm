mod support;

use calm_server::worker_flow::claude_normalizer::{
    ClaudeNormalizerState, normalize_record_with_state, record_starts_turn, record_type,
};
use calm_types::worker::{WorkerProviderKind, WorkerSessionId};
use calm_types::worker_flow::{
    ExecStatus, McpStatus, PatchStatus, RawRef, ToolError, WorkerFlowItem,
};
use serde_json::{Value, json};

use support::worker_flow as wf;

#[test]
fn claude_normalizer_completes_file_change_and_web_search_tool_results() {
    let items = normalize_lines(vec![
        wf::claude_user_string("user-1", "edit and search"),
        wf::claude_assistant(
            "assistant-edit",
            "/tmp/claude-tools",
            vec![wf::claude_tool_use(
                "toolu-edit",
                "Edit",
                json!({
                    "file_path": "src/main.rs",
                    "old_string": "old",
                    "new_string": "new"
                }),
            )],
        ),
        wf::claude_user_blocks(
            "result-edit",
            vec![wf::claude_tool_result("toolu-edit", "Applied edit", false)],
        ),
        wf::claude_assistant(
            "assistant-web",
            "/tmp/claude-tools",
            vec![wf::claude_tool_use(
                "toolu-web",
                "WebSearch",
                json!({ "query": "rust serde" }),
            )],
        ),
        wf::claude_user_blocks(
            "result-web",
            vec![wf::claude_tool_result(
                "toolu-web",
                "Found serde documentation",
                false,
            )],
        ),
        wf::claude_assistant(
            "assistant-final",
            "/tmp/claude-tools",
            vec![wf::claude_text("done")],
        ),
    ]);

    let file_changes = items
        .iter()
        .filter_map(|item| match item {
            WorkerFlowItem::FileChange {
                call_id,
                changes,
                status,
                ..
            } => Some((call_id.as_ref().map(|id| id.as_str()), changes, status)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(file_changes.len(), 2);
    assert_eq!(file_changes[0].0, Some("toolu-edit"));
    assert_eq!(file_changes[1].0, Some("toolu-edit"));
    assert_eq!(file_changes[0].1, file_changes[1].1);
    assert_eq!(file_changes[0].2, &PatchStatus::InProgress);
    assert_eq!(file_changes[1].2, &PatchStatus::Completed);

    let web_searches = items
        .iter()
        .filter_map(|item| match item {
            WorkerFlowItem::WebSearch {
                call_id,
                query,
                results_summary,
                ..
            } => Some((
                call_id.as_ref().map(|id| id.as_str()),
                query.as_deref(),
                results_summary.as_deref(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(web_searches.len(), 2);
    assert_eq!(
        web_searches[0],
        (Some("toolu-web"), Some("rust serde"), None)
    );
    assert_eq!(
        web_searches[1],
        (
            Some("toolu-web"),
            Some("rust serde"),
            Some("Found serde documentation")
        )
    );
}

#[test]
fn claude_normalizer_marks_file_change_failed_when_tool_result_is_error() {
    let items = normalize_lines(vec![
        wf::claude_user_string("user-1", "edit"),
        wf::claude_assistant(
            "assistant-edit",
            "/tmp/claude-tools",
            vec![wf::claude_tool_use(
                "toolu-edit",
                "Edit",
                json!({
                    "file_path": "src/main.rs",
                    "old_string": "old",
                    "new_string": "new"
                }),
            )],
        ),
        wf::claude_user_blocks(
            "result-edit",
            vec![wf::claude_tool_result("toolu-edit", "Edit failed", true)],
        ),
    ]);

    let statuses = items
        .iter()
        .filter_map(|item| match item {
            WorkerFlowItem::FileChange { status, .. } => Some(status),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        statuses,
        vec![&PatchStatus::InProgress, &PatchStatus::Failed]
    );
}

#[test]
fn claude_normalizer_completes_bash_from_tool_use_result_stdout() {
    let (items, state) = normalize_lines_with_state(vec![
        wf::claude_user_string("user-bash", "list files"),
        wf::claude_assistant(
            "assistant-bash",
            "/tmp/claude-tools",
            vec![wf::claude_tool_use(
                "toolu-bash",
                "Bash",
                json!({ "command": "ls" }),
            )],
        ),
        claude_user_blocks_with_tool_use_result(
            "result-bash",
            vec![wf::claude_tool_result(
                "toolu-bash",
                "file1\nfile2\n",
                false,
            )],
            json!({
                "stdout": "file1\nfile2\n",
                "stderr": "",
                "interrupted": false,
                "isImage": false,
                "sandbox": false
            }),
        ),
    ]);

    let commands = command_executions(&items);
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].call_id, Some("toolu-bash"));
    assert_eq!(commands[0].status, &ExecStatus::InProgress);
    assert_eq!(commands[0].exit_code, None);
    assert_eq!(commands[0].aggregated_output, None);
    assert_eq!(commands[1].call_id, Some("toolu-bash"));
    assert_eq!(commands[1].status, &ExecStatus::Completed);
    assert_eq!(commands[1].exit_code, Some(0));
    assert_eq!(commands[1].aggregated_output, Some("file1\nfile2\n"));
    assert_eq!(state.pending_commands_len(), 0);
}

#[test]
fn claude_normalizer_fails_bash_from_tool_use_result_stderr() {
    let (items, state) = normalize_lines_with_state(vec![
        wf::claude_user_string("user-bash", "read protected file"),
        wf::claude_assistant(
            "assistant-bash",
            "/tmp/claude-tools",
            vec![wf::claude_tool_use(
                "toolu-bash",
                "Bash",
                json!({ "command": "cat /root/secret" }),
            )],
        ),
        claude_user_blocks_with_tool_use_result(
            "result-bash",
            vec![wf::claude_tool_result(
                "toolu-bash",
                "permission denied",
                true,
            )],
            json!({
                "stdout": "",
                "stderr": "permission denied",
                "interrupted": false
            }),
        ),
    ]);

    let commands = command_executions(&items);
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].status, &ExecStatus::InProgress);
    assert_eq!(commands[1].status, &ExecStatus::Failed);
    assert_eq!(commands[1].exit_code, Some(-1));
    assert_eq!(commands[1].aggregated_output, Some("permission denied"));
    assert_eq!(state.pending_commands_len(), 0);
}

#[test]
fn claude_normalizer_emits_failed_bash_completion_when_result_shape_absent() {
    let (items, state) = normalize_lines_with_state(vec![
        wf::claude_user_string("user-bash", "list files"),
        wf::claude_assistant(
            "assistant-bash",
            "/tmp/claude-tools",
            vec![wf::claude_tool_use(
                "toolu-bash-degenerate",
                "Bash",
                json!({ "command": "ls" }),
            )],
        ),
        wf::claude_user_blocks(
            "result-bash",
            vec![wf::claude_tool_result("toolu-bash-degenerate", "", false)],
        ),
    ]);

    let commands = command_executions(&items);
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].call_id, Some("toolu-bash-degenerate"));
    assert_eq!(commands[0].status, &ExecStatus::InProgress);
    assert_eq!(commands[0].exit_code, None);
    assert_eq!(commands[0].aggregated_output, None);
    assert_eq!(commands[1].call_id, Some("toolu-bash-degenerate"));
    assert_eq!(commands[1].status, &ExecStatus::Failed);
    assert_eq!(commands[1].exit_code, Some(-1));
    assert_eq!(commands[1].aggregated_output, None);
    assert_eq!(state.pending_commands_len(), 0);
}

#[test]
fn claude_normalizer_pairs_mcp_tool_result_with_mcp_completion() {
    let result_content = json!([{ "type": "text", "text": "result body" }]);
    let (items, state) = normalize_lines_with_state(vec![
        wf::claude_user_string("user-mcp", "call mcp"),
        wf::claude_assistant(
            "assistant-mcp",
            "/tmp/claude-tools",
            vec![wf::claude_tool_use(
                "toolu-mcp",
                "mcp__plugin_foo__bar",
                json!({ "arg": "value" }),
            )],
        ),
        wf::claude_user_blocks(
            "result-mcp",
            vec![claude_tool_result_value(
                "toolu-mcp",
                result_content.clone(),
                false,
            )],
        ),
    ]);

    let mcp_calls = mcp_tool_calls(&items);
    assert_eq!(mcp_calls.len(), 2);
    assert_eq!(mcp_calls[0].call_id, "toolu-mcp");
    assert_eq!(mcp_calls[0].status, &McpStatus::InProgress);
    assert_eq!(mcp_calls[0].result, None);
    assert!(mcp_calls[0].error.is_none());
    assert_eq!(mcp_calls[1].call_id, "toolu-mcp");
    assert_eq!(mcp_calls[1].status, &McpStatus::Completed);
    assert_eq!(mcp_calls[1].result, Some(&result_content));
    assert!(mcp_calls[1].error.is_none());
    assert_no_tool_result(&items, "toolu-mcp");
    assert_eq!(state.pending_mcp_calls_len(), 0);
}

#[test]
fn claude_normalizer_marks_mcp_tool_result_failure() {
    let result_content = json!([{ "type": "text", "text": "failure body" }]);
    let (items, state) = normalize_lines_with_state(vec![
        wf::claude_user_string("user-mcp", "call mcp"),
        wf::claude_assistant(
            "assistant-mcp",
            "/tmp/claude-tools",
            vec![wf::claude_tool_use(
                "toolu-mcp",
                "mcp__plugin_foo__bar",
                json!({ "arg": "value" }),
            )],
        ),
        wf::claude_user_blocks(
            "result-mcp",
            vec![claude_tool_result_value(
                "toolu-mcp",
                result_content.clone(),
                true,
            )],
        ),
    ]);

    let mcp_calls = mcp_tool_calls(&items);
    assert_eq!(mcp_calls.len(), 2);
    assert_eq!(mcp_calls[1].status, &McpStatus::Failed);
    assert_eq!(mcp_calls[1].result, Some(&result_content));
    assert_eq!(
        mcp_calls[1]
            .error
            .as_ref()
            .map(|error| error.message.as_str()),
        Some("failure body")
    );
    assert_no_tool_result(&items, "toolu-mcp");
    assert_eq!(state.pending_mcp_calls_len(), 0);
}

fn normalize_lines(lines: Vec<Value>) -> Vec<WorkerFlowItem> {
    normalize_lines_with_state(lines).0
}

fn normalize_lines_with_state(lines: Vec<Value>) -> (Vec<WorkerFlowItem>, ClaudeNormalizerState) {
    let mut seq = 0_u64;
    let mut turn = 0_u32;
    let session_id = WorkerSessionId::from("sess-claude-tools");
    let mut state = ClaudeNormalizerState::default();
    let mut emitted = Vec::new();

    for (idx, record) in lines.into_iter().enumerate() {
        if record_starts_turn(&record) {
            turn += 1;
        }
        let raw_ref = RawRef {
            provider: WorkerProviderKind::Claude,
            source_path: Some("/tmp/claude-tools.jsonl".to_string()),
            line: Some(idx as u64),
            record_type: Some(record_type(&record)),
        };
        let items =
            normalize_record_with_state(&record, seq, turn, &session_id, raw_ref, &mut state);
        seq = seq.saturating_add(items.len() as u64);
        emitted.extend(items);
    }

    (emitted, state)
}

fn claude_user_blocks_with_tool_use_result(
    uuid: &str,
    blocks: Vec<Value>,
    tool_use_result: Value,
) -> Value {
    let mut record = wf::claude_user_blocks(uuid, blocks);
    record["toolUseResult"] = tool_use_result;
    record
}

fn claude_tool_result_value(tool_use_id: &str, content: Value, is_error: bool) -> Value {
    json!({
        "type": "tool_result",
        "tool_use_id": tool_use_id,
        "content": content,
        "is_error": is_error
    })
}

struct CommandExecutionView<'a> {
    call_id: Option<&'a str>,
    status: &'a ExecStatus,
    exit_code: Option<i32>,
    aggregated_output: Option<&'a str>,
}

fn command_executions(items: &[WorkerFlowItem]) -> Vec<CommandExecutionView<'_>> {
    items
        .iter()
        .filter_map(|item| match item {
            WorkerFlowItem::CommandExecution {
                call_id,
                status,
                exit_code,
                aggregated_output,
                ..
            } => Some(CommandExecutionView {
                call_id: call_id.as_ref().map(|call_id| call_id.as_str()),
                status,
                exit_code: *exit_code,
                aggregated_output: aggregated_output.as_deref(),
            }),
            _ => None,
        })
        .collect()
}

struct McpToolCallView<'a> {
    call_id: &'a str,
    status: &'a McpStatus,
    result: Option<&'a Value>,
    error: &'a Option<ToolError>,
}

fn mcp_tool_calls(items: &[WorkerFlowItem]) -> Vec<McpToolCallView<'_>> {
    items
        .iter()
        .filter_map(|item| match item {
            WorkerFlowItem::McpToolCall {
                call_id,
                status,
                result,
                error,
                ..
            } => Some(McpToolCallView {
                call_id: call_id.as_str(),
                status,
                result: result.as_ref(),
                error,
            }),
            _ => None,
        })
        .collect()
}

fn assert_no_tool_result(items: &[WorkerFlowItem], expected_call_id: &str) {
    assert!(
        !items.iter().any(|item| matches!(
            item,
            WorkerFlowItem::ToolResult { call_id, .. } if call_id.as_str() == expected_call_id
        )),
        "unexpected generic ToolResult for {expected_call_id}"
    );
}
