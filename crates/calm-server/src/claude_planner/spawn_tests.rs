//! The spawn contract, pinned by exact equality (design #1791 §5.2, §5.3).

use std::path::Path;

use serde_json::{Value, json};
use uuid::Uuid;

use super::models::{ClaudeModel, MODELS};
use super::spawn::{EnvInputs, SessionStart, argv, base_env, passthrough_keys, settings_json};

const THREAD: &str = "5d0b2694-9ccc-4543-ab84-611aa4287dbe";

fn args(start: SessionStart, cwd: &str) -> Vec<String> {
    args_with(start, None, cwd)
}

fn args_with(start: SessionStart, model: Option<&ClaudeModel>, cwd: &str) -> Vec<String> {
    argv(
        Uuid::parse_str(THREAD).unwrap(),
        start,
        model,
        Path::new(cwd),
        Path::new("/opt/neige/neige-mcp-stdio-shim"),
        Path::new("/data/claude-planner/tmp/ws1-turn1.md"),
    )
    .expect("argv")
    .into_iter()
    .map(|arg| arg.into_string().expect("utf-8"))
    .collect()
}

#[test]
fn argv_is_exactly_the_spawn_contract() {
    let mcp = json!({"mcpServers":{"calm":{"type":"stdio","command":"/opt/neige/neige-mcp-stdio-shim",
        "args":[],"env":{"NEIGE_MCP_SOCKET":"${NEIGE_MCP_SOCKET}","NEIGE_MCP_TOKEN":"${NEIGE_MCP_TOKEN}"}}}});
    let got = args(SessionStart::New, "/ws/track");
    let expected: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--replay-user-messages",
        "--session-id",
        THREAD,
        "--setting-sources",
        "project",
        "--disable-slash-commands",
        "--tools",
        "Bash,Read,Edit,Write,ToolSearch,WebFetch,WebSearch",
        "--strict-mcp-config",
        "--mcp-config",
        "<mcp>",
        "--settings",
        "<settings>",
        "--permission-prompts",
        "none",
        "--allowedTools",
        "Bash Read ToolSearch WebFetch WebSearch mcp__calm Edit(//ws/track/**) Write(//ws/track/**)",
        "--append-system-prompt-file",
        "/data/claude-planner/tmp/ws1-turn1.md",
    ]
    .map(String::from)
    .to_vec();
    assert_eq!(got.len(), expected.len(), "{got:?}");
    for (index, (got, want)) in got.iter().zip(&expected).enumerate() {
        match want.as_str() {
            "<mcp>" => assert_eq!(serde_json::from_str::<Value>(got).unwrap(), mcp),
            "<settings>" => assert_eq!(got, &settings_json()),
            _ => assert_eq!(got, want, "argv[{index}]"),
        }
    }
    for flag in [
        "--model",
        "--effort",
        "--permission-mode",
        "--include-partial-messages",
    ] {
        assert!(
            !got.iter().any(|arg| arg == flag),
            "{flag} must not be passed"
        );
    }
}

/// #1810: a chosen alias rides as `--model <alias>` right after the session; without one the
/// argv is exactly the contract above.
#[test]
fn a_chosen_alias_is_passed_as_model_and_nothing_else_changes() {
    for model in MODELS {
        for start in [SessionStart::New, SessionStart::Resume] {
            let without = args(start, "/ws/track");
            let mut expected = without.clone();
            expected.splice(9..9, ["--model".to_string(), model.alias.to_string()]);
            assert_eq!(args_with(start, Some(model), "/ws/track"), expected);
        }
    }
}

#[test]
fn a_resumed_session_names_the_thread_with_resume() {
    let got = args(SessionStart::Resume, "/ws/track/");
    let at = got
        .iter()
        .position(|arg| arg == "--resume")
        .expect("--resume");
    assert_eq!(got[at + 1], THREAD);
    assert!(!got.iter().any(|arg| arg == "--session-id"));
    assert!(got.contains(&"Bash Read ToolSearch WebFetch WebSearch mcp__calm Edit(//ws/track/**) Write(//ws/track/**)".to_string()));
}

#[test]
fn the_sandbox_settings_are_exactly_the_owner_decision() {
    let settings: Value = serde_json::from_str(&settings_json()).unwrap();
    assert_eq!(
        settings,
        json!({
            "permissions": { "allow": ["WebFetch(domain:*)"] },
            "sandbox": {
                "enabled": true,
                "failIfUnavailable": true,
                "allowUnsandboxedCommands": false,
                "network": { "allowAllUnixSockets": true },
            },
        })
    );
}

#[test]
fn a_workspace_that_would_split_a_rule_is_refused() {
    for cwd in ["/ws/my track", "/ws/a,b", "/ws/(x)", "relative/ws"] {
        let result = argv(
            Uuid::parse_str(THREAD).unwrap(),
            SessionStart::New,
            None,
            Path::new(cwd),
            Path::new("/opt/shim"),
            Path::new("/data/x.md"),
        );
        assert!(result.is_err(), "{cwd}");
    }
}

#[test]
fn the_ambient_allowlist_drops_codex_openai_and_rust_keys() {
    let keys: Vec<&str> = passthrough_keys().collect();
    assert_eq!(
        keys,
        [
            "HOME",
            "USER",
            "LOGNAME",
            "SHELL",
            "LANG",
            "LANGUAGE",
            "LC_ALL",
            "LC_CTYPE",
            "TERM",
            "TZ",
            "TMPDIR",
            "TEMP",
            "TMP",
            "NO_PROXY",
            "no_proxy",
            "ALL_PROXY",
            "all_proxy",
            "SSL_CERT_FILE",
        ]
    );
}

/// #1814: the config dir is the owner's, so its auto-memory is the owner's; a Planner neither
/// reads nor writes it. The `--version` check runs on the same env, so it carries it too.
#[test]
fn the_spawn_env_disables_claude_auto_memory() {
    let env = base_env(&EnvInputs {
        path: "/usr/bin".into(),
        config_dir: Path::new("/home/owner/.claude"),
        mcp_socket: Path::new("/run/calm/mcp.sock"),
        marker: "marker".into(),
        proxy: &[],
    });
    let values: Vec<&std::ffi::OsString> = env
        .iter()
        .filter(|(key, _)| key == "CLAUDE_CODE_DISABLE_AUTO_MEMORY")
        .map(|(_, value)| value)
        .collect();
    assert_eq!(values, ["1"], "{env:?}");
}
