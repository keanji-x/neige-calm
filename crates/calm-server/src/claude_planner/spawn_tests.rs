//! The spawn contract, pinned by exact equality (design #1791 §5.2, §5.3).

use std::path::Path;

use serde_json::{Value, json};
use uuid::Uuid;

use crate::planner_model::TurnModelSelection;
use crate::planner_permission_mode::PlannerPermissionMode;

use super::spawn::{EnvInputs, SessionStart, argv, base_env, passthrough_keys, settings_json};

const THREAD: &str = "5d0b2694-9ccc-4543-ab84-611aa4287dbe";

fn args(start: SessionStart, cwd: &str) -> Vec<String> {
    args_with(start, &TurnModelSelection::inherit(), cwd)
}

fn args_with(start: SessionStart, selection: &TurnModelSelection, cwd: &str) -> Vec<String> {
    args_in(PlannerPermissionMode::Never, start, selection, cwd)
}

fn args_in(
    mode: PlannerPermissionMode,
    start: SessionStart,
    selection: &TurnModelSelection,
    cwd: &str,
) -> Vec<String> {
    argv(
        Uuid::parse_str(THREAD).unwrap(),
        start,
        None,
        selection,
        mode,
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
    let mcp = json!({"mcpServers":{"neige":{"type":"stdio","command":"/opt/neige/neige-mcp-stdio-shim",
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
        "--include-partial-messages",
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
        "Bash Read ToolSearch WebFetch WebSearch mcp__neige Edit(//ws/track/**)",
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
    for flag in ["--model", "--effort", "--permission-mode"] {
        assert!(
            !got.iter().any(|arg| arg == flag),
            "{flag} must not be passed"
        );
    }
}

/// #1810, #1822: a chosen model rides verbatim as the one token `--model=<value>` right after the
/// session, and a chosen effort as `--effort=<level>` after it; without either the argv is exactly
/// the contract above. A value that looks like a flag stays inside its token.
#[test]
fn a_chosen_model_and_effort_are_passed_as_flags_and_nothing_else_changes() {
    let pick = |model: Option<&str>, effort: Option<&str>| TurnModelSelection {
        model: model.map(str::to_string),
        effort: effort.map(str::to_string),
    };
    for start in [SessionStart::New, SessionStart::Resume] {
        let without = args(start, "/ws/track");
        for (selection, flags) in [
            (
                pick(Some("claude-fable-5-1[1m]"), Some("high")),
                vec!["--model=claude-fable-5-1[1m]", "--effort=high"],
            ),
            (pick(Some("haiku"), None), vec!["--model=haiku"]),
            (pick(None, Some("max")), vec!["--effort=max"]),
            (
                pick(Some("-x"), Some("--dangerously-skip-permissions")),
                vec!["--model=-x", "--effort=--dangerously-skip-permissions"],
            ),
        ] {
            let mut expected = without.clone();
            expected.splice(10..10, flags.into_iter().map(str::to_string));
            assert_eq!(args_with(start, &selection, "/ws/track"), expected);
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
    assert!(got.contains(
        &"Bash Read ToolSearch WebFetch WebSearch mcp__neige Edit(//ws/track/**)".to_string()
    ));
}

#[test]
fn the_sandbox_settings_are_exactly_the_owner_decision() {
    let settings: Value = serde_json::from_str(&settings_json()).unwrap();
    assert_eq!(
        settings,
        json!({
            "attribution": { "commit": "", "pr": "" },
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

/// #2348: `ask` asks before a command leaves the sandbox, through the CLI's stdio prompt tool. It
/// differs from `never` in exactly the prompt flag, the rule list (no Bash rule) and the settings,
/// and still loads the project setting source.
#[test]
fn the_ask_argv_is_exactly_the_spawn_contract() {
    let never = args(SessionStart::New, "/ws/track");
    let ask = args_in(
        PlannerPermissionMode::Ask,
        SessionStart::New,
        &TurnModelSelection::inherit(),
        "/ws/track",
    );
    let mut expected = never.clone();
    let settings = expected.iter().position(|arg| arg == "--settings").unwrap() + 1;
    expected[settings] = ask[settings].clone();
    let prompts = expected
        .iter()
        .position(|arg| arg == "--permission-prompts")
        .unwrap();
    expected.splice(
        prompts..prompts + 2,
        ["--permission-prompt-tool", "stdio"].map(String::from),
    );
    let rules = expected
        .iter()
        .position(|arg| arg == "--allowedTools")
        .unwrap()
        + 1;
    expected[rules] = "Read ToolSearch WebFetch WebSearch mcp__neige Edit(//ws/track/**)".into();
    assert_eq!(ask, expected);
    assert_eq!(
        serde_json::from_str::<Value>(&ask[settings]).unwrap(),
        json!({
            "attribution": { "commit": "", "pr": "" },
            "permissions": {
                "allow": ["WebFetch(domain:*)"],
                "deny": [
                    "Edit(//ws/track/.claude/settings.json)",
                    "Edit(//ws/track/.claude/settings.local.json)",
                ],
            },
            "sandbox": {
                "enabled": true,
                "failIfUnavailable": true,
                "allowUnsandboxedCommands": true,
                "autoAllowBashIfSandboxed": true,
                "network": { "allowAllUnixSockets": true },
            },
        })
    );
    let sources = ask
        .iter()
        .position(|arg| arg == "--setting-sources")
        .unwrap();
    assert_eq!(ask[sources + 1], "project");
}

/// #2441: `full` bypasses every permission check with the sandbox off, and asks nothing. It
/// differs from `never` in exactly the bypass flag and the sandbox; its rules are `never`'s.
#[test]
fn the_full_argv_is_exactly_the_spawn_contract() {
    let never = args(SessionStart::New, "/ws/track");
    let full = args_in(
        PlannerPermissionMode::Full,
        SessionStart::New,
        &TurnModelSelection::inherit(),
        "/ws/track",
    );
    let mut expected = never.clone();
    let settings = expected.iter().position(|arg| arg == "--settings").unwrap() + 1;
    expected[settings] = full[settings].clone();
    let prompts = expected
        .iter()
        .position(|arg| arg == "--permission-prompts")
        .unwrap();
    expected.splice(
        prompts..prompts,
        ["--permission-mode", "bypassPermissions"].map(String::from),
    );
    assert_eq!(full, expected);
    assert_eq!(
        serde_json::from_str::<Value>(&full[settings]).unwrap(),
        json!({
            "attribution": { "commit": "", "pr": "" },
            "permissions": { "allow": ["WebFetch(domain:*)"] },
            "sandbox": {
                "enabled": false,
                "failIfUnavailable": true,
                "allowUnsandboxedCommands": false,
                "network": { "allowAllUnixSockets": true },
            },
        })
    );
}

/// The safety invariant of #2348, over every mode and both session starts: a spawn whose commands
/// may leave the sandbox has no Bash rule (a bare `Bash` would let them leave unasked), runs
/// sandboxed commands without asking, and denies editing the workspace's Claude settings, spelled
/// from the same workspace root its Edit rule is. A spawn without a sandbox (#2441) is exactly one
/// that bypasses every check and asks nothing; every sandboxed spawn keeps the default mode.
#[test]
fn a_spawn_that_may_leave_the_sandbox_asks_first_and_cannot_edit_its_settings() {
    let mut unsandboxed = 0;
    let mut without_sandbox = 0;
    for mode in [
        PlannerPermissionMode::Never,
        PlannerPermissionMode::Ask,
        PlannerPermissionMode::Full,
    ] {
        for start in [SessionStart::New, SessionStart::Resume] {
            for cwd in ["/ws/track", "/ws/track/"] {
                let got = args_in(mode, start, &TurnModelSelection::inherit(), cwd);
                let after = |flag: &str| {
                    let at = got.iter().position(|arg| arg == flag);
                    at.map(|at| got[at + 1].clone())
                };
                let settings: Value =
                    serde_json::from_str(&after("--settings").expect("--settings")).unwrap();
                if settings["sandbox"]["enabled"] != json!(true) {
                    without_sandbox += 1;
                    assert_eq!(
                        after("--permission-mode").as_deref(),
                        Some("bypassPermissions"),
                        "{mode:?}"
                    );
                    assert_eq!(after("--permission-prompts").as_deref(), Some("none"));
                    assert_eq!(after("--permission-prompt-tool"), None, "{mode:?}");
                    continue;
                }
                assert_eq!(after("--permission-mode"), None, "{mode:?}");
                if settings["sandbox"]["allowUnsandboxedCommands"] != json!(true) {
                    continue;
                }
                unsandboxed += 1;
                let rules = after("--allowedTools").expect("--allowedTools");
                assert!(
                    !rules
                        .split(' ')
                        .any(|rule| rule == "Bash" || rule.starts_with("Bash(")),
                    "{mode:?}: {rules}"
                );
                assert!(rules.split(' ').any(|rule| rule == "Edit(//ws/track/**)"));
                assert_eq!(
                    settings["sandbox"]["autoAllowBashIfSandboxed"],
                    json!(true),
                    "{mode:?}"
                );
                let deny = settings["permissions"]["deny"]
                    .as_array()
                    .expect("deny rules");
                for file in ["settings.json", "settings.local.json"] {
                    let rule = json!(format!("Edit(//ws/track/.claude/{file})"));
                    assert!(deny.contains(&rule), "{mode:?}: {deny:?}");
                }
                assert_eq!(after("--permission-prompt-tool").as_deref(), Some("stdio"));
                assert_eq!(after("--setting-sources").as_deref(), Some("project"));
            }
        }
    }
    assert_eq!(unsandboxed, 4, "only ask lets a command leave the sandbox");
    assert_eq!(without_sandbox, 4, "only full runs without the sandbox");
}

#[test]
fn a_workspace_that_would_split_a_rule_is_refused() {
    for (cwd, mode) in ["/ws/my track", "/ws/a,b", "/ws/(x)", "relative/ws"]
        .into_iter()
        .flat_map(|cwd| {
            [
                PlannerPermissionMode::Never,
                PlannerPermissionMode::Ask,
                PlannerPermissionMode::Full,
            ]
            .map(|m| (cwd, m))
        })
    {
        let result = argv(
            Uuid::parse_str(THREAD).unwrap(),
            SessionStart::New,
            None,
            &TurnModelSelection::inherit(),
            mode,
            Path::new(cwd),
            Path::new("/opt/shim"),
            Path::new("/data/x.md"),
        );
        assert!(result.is_err(), "{cwd} {mode:?}");
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
