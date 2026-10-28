//! The worker argv and init contract, pinned by exact equality (§3.4).
use super::{WorkerArgs, WorkerInitRecord, WorkerRole, WorkerStart};
use crate::claude_planner::role::{LaunchRole, ProtocolProfile};
use provider::claude::protocol::SystemInit;
use std::path::Path;
use uuid::Uuid;

const THREAD: &str = "5d0b2694-9ccc-4543-ab84-611aa4287dbe";
const MCP: &str = r#"{"mcpServers":{"neige":{"args":[],"command":"/opt/neige/neige-mcp-stdio-shim","env":{"NEIGE_MCP_SOCKET":"${NEIGE_MCP_SOCKET}","NEIGE_MCP_TOKEN":"${NEIGE_MCP_TOKEN}"},"type":"stdio"}}}"#;

fn thread() -> Uuid {
    Uuid::parse_str(THREAD).unwrap()
}

fn argv(start: WorkerStart) -> Vec<String> {
    WorkerArgs {
        start,
        settings: Path::new("/data/claude-settings/card-1/settings.json"),
        mcp_shim: Path::new("/opt/neige/neige-mcp-stdio-shim"),
        system_prompt: "You are a worker.",
    }
    .argv()
    .unwrap()
    .into_iter()
    .map(|arg| arg.into_string().unwrap())
    .collect()
}

fn expected(flag: &str) -> Vec<String> {
    [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--replay-user-messages",
        "--include-partial-messages",
        flag,
        THREAD,
        "--permission-mode",
        "auto",
        "--settings",
        "/data/claude-settings/card-1/settings.json",
        "--strict-mcp-config",
        "--mcp-config",
        MCP,
        "--allowedTools",
        "mcp__neige",
        "--append-system-prompt",
        "You are a worker.",
    ]
    .map(String::from)
    .to_vec()
}

fn without_mcp_json(mut args: Vec<String>) -> (Vec<String>, serde_json::Value) {
    let at = args.iter().position(|arg| arg == "--mcp-config").unwrap() + 1;
    let json = serde_json::from_str(&args[at]).unwrap();
    args[at] = String::new();
    (args, json)
}

fn assert_argv(actual: Vec<String>, flag: &str) {
    let (actual, actual_mcp) = without_mcp_json(actual);
    let (expected, expected_mcp) = without_mcp_json(expected(flag));
    assert_eq!(actual, expected);
    assert_eq!(actual_mcp, expected_mcp);
}

#[test]
fn w_claude_worker_argv_first_turn_uses_session_id() {
    let args = argv(WorkerStart::New(thread()));
    assert!(!args.iter().any(|arg| arg == "--resume"));
    assert_argv(args, "--session-id");
}

#[test]
fn w_claude_worker_argv_later_turn_resumes() {
    let recorded = WorkerInitRecord {
        session: thread(),
        claude_code_version: "2.1.280".into(),
    };
    let start = recorded.resume();
    assert_eq!(start.thread(), thread());
    let args = argv(start);
    assert!(!args.iter().any(|arg| arg == "--session-id"));
    assert_argv(args, "--resume");
}

#[test]
fn w_claude_worker_argv_keeps_owner_settings_and_commands() {
    let args = argv(WorkerStart::New(thread()));
    for flag in [
        "--setting-sources",
        "--disable-slash-commands",
        "--tools",
        "--permission-prompts",
        "--append-system-prompt-file",
    ] {
        assert!(!args.iter().any(|arg| arg == flag), "{flag}");
    }
}

fn init(json: serde_json::Value) -> SystemInit {
    let mut init = serde_json::json!({"type":"system","subtype":"init","session_id":THREAD,
        "claude_code_version":"9.9.9","model":"m","capabilities":["interrupt_receipt_v1"],
        "mcp_servers":[{"name":"neige","status":"connected"}],"slash_commands":["review"],
        "skills":["navi","finance:journal-entry"],
        "plugins":[{"name":"finance","source":"finance@synced"}]});
    for (key, value) in json.as_object().unwrap() {
        init[key] = value.clone();
    }
    match provider::claude::protocol::decode(&init.to_string()).unwrap() {
        provider::claude::protocol::Record::SystemInit(init) => init,
        _ => panic!("not an init"),
    }
}

#[test]
fn w_claude_worker_init_check_accepts_owner_skills_plugins_and_any_version() {
    let role = WorkerRole { thread: thread() };
    assert_eq!(role.init_check(&init(serde_json::json!({}))), Ok(()));
}

#[test]
fn w_claude_worker_init_check_refuses_session_capability_and_mcp() {
    let role = WorkerRole { thread: thread() };
    for broken in [
        serde_json::json!({"session_id": Uuid::new_v4()}),
        serde_json::json!({"capabilities": []}),
        serde_json::json!({"mcp_servers": [{"name":"neige","status":"failed"}]}),
        serde_json::json!({"mcp_servers": []}),
    ] {
        assert!(role.init_check(&init(broken.clone())).is_err(), "{broken}");
    }
}
