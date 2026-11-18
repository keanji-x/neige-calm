use super::*;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::Write,
    process::{Command, Stdio},
};

fn launch(
    provider: &str,
    operator: serde_json::Value,
    workspace: &[(&str, &str)],
) -> Result<crate::provider_execution::launch::Release> {
    let config: super::super::config::AcpAgentConfig = serde_json::from_value(serde_json::json!({
        "provider":provider, "command":"/usr/bin/python3", "args":[], "env":operator,
        "min_agent_version":"1.0.0"
    }))
    .unwrap();
    let profile = super::super::profile::AcpAgentProfile::of(&config.provider).unwrap();
    Ok(process_launch(
        Registration {
            config: &config,
            profile,
        },
        profile.resident.map_or(
            profile.launch(crate::planner_permission_mode::PlannerPermissionMode::Never),
            |r| r.launch,
        ),
        [
            ("NEIGE_MCP_SOCKET", "/kernel/socket".into()),
            ("NEIGE_MCP_TOKEN", "kernel-test-token".into()),
        ],
        workspace
            .iter()
            .map(|(key, value)| (OsString::from(*key), OsString::from(*value)))
            .collect::<BTreeMap<_, _>>(),
        None,
    )?
    .1)
}

fn observe(release: crate::provider_execution::launch::Release) -> serde_json::Value {
    let mut process = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!(
            "{}exec /usr/bin/python3 -c 'import json,os; print(json.dumps(dict(os.environ)))'",
            release.read_prefix
        ))
        .env_clear()
        .envs(release.public.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    process
        .stdin
        .take()
        .unwrap()
        .write_all(&release.payload)
        .unwrap();
    let output = process.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "private environment release failed"
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn acp_workspace_environment_reaches_child_without_public_secret_values() {
    let secret = "workspace-test-private-value";
    let release = launch(
        "grok",
        serde_json::json!({"HOME":"/operator/home"}),
        &[
            ("GH_TOKEN", secret),
            ("HOME", "/project/home"),
            ("PATH", "/project/bin:/usr/bin"),
            ("PROJECT_MULTILINE", "line one\nline two\n"),
            ("NEIGE_MCP_SOCKET", "/project/wrong-socket"),
        ],
    )
    .unwrap();
    assert!(
        release
            .public
            .iter()
            .all(|(_, value)| !value.to_string_lossy().contains(secret))
    );
    assert!(!release.read_prefix.contains(secret));
    let observed = observe(release);
    assert_eq!(observed["GH_TOKEN"], secret);
    assert_eq!(observed["HOME"], "/operator/home");
    assert_eq!(observed["PROJECT_MULTILINE"], "line one\nline two\n");
    assert_eq!(observed["NEIGE_MCP_SOCKET"], "/kernel/socket");
    let path = observed["PATH"].as_str().unwrap();
    assert_eq!(
        path.split(':').skip(1).collect::<Vec<_>>(),
        ["/project/bin", "/usr/bin"]
    );
}

#[test]
fn acp_workspace_cannot_replace_profile_control_or_authentication() {
    let grok = observe(
        launch(
            "grok",
            serde_json::json!({}),
            &[
                ("XAI_API_KEY", "workspace-key"),
                ("GROK_CODE_XAI_API_KEY", "workspace-key"),
                ("GROK_HOME", "/workspace/other-provider-account"),
                ("HOME", "/workspace/other-home"),
                ("XDG_CONFIG_HOME", "/workspace/other-config"),
                ("XDG_DATA_HOME", "/workspace/other-data"),
                ("GH_TOKEN", "project-github"),
            ],
        )
        .unwrap(),
    );
    assert!(grok.get("XAI_API_KEY").is_none());
    assert!(grok.get("GROK_CODE_XAI_API_KEY").is_none());
    assert_eq!(grok["GH_TOKEN"], "project-github");
    assert!(grok.get("GROK_HOME").is_none());
    for key in ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME"] {
        assert_eq!(
            grok.get(key).and_then(|value| value.as_str()),
            std::env::var(key).ok().as_deref()
        );
    }
    let operator = observe(
        launch(
            "grok",
            serde_json::json!({"GROK_HOME":"/operator/provider-home"}),
            &[("GROK_HOME", "/workspace/provider-home")],
        )
        .unwrap(),
    );
    assert_eq!(operator["GROK_HOME"], "/operator/provider-home");
    let opencode = observe(
        launch(
            "opencode",
            serde_json::json!({}),
            &[("OPENCODE_CONFIG_CONTENT", "workspace-may-not-disable-asks")],
        )
        .unwrap(),
    );
    let policy: serde_json::Value =
        serde_json::from_str(opencode["OPENCODE_CONFIG_CONTENT"].as_str().unwrap()).unwrap();
    assert!(
        policy["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["action"] == "shell" && p["effect"] == "ask")
    );
}

#[test]
fn opencode_workspace_values_never_enter_provider_process() {
    let observed = observe(
        launch(
            "opencode",
            serde_json::json!({"OPENAI_API_KEY":"operator-model-key", "HOME":"/operator/home", "XDG_CONFIG_HOME":"/operator/config"}),
            &[
                ("OPENAI_API_KEY", "workspace-model-key"),
                ("ANTHROPIC_API_KEY", "workspace-other-model-key"),
                ("CUSTOM_MODEL_ACCOUNT", "workspace-custom-model-key"),
                ("OPENCODE_CONFIG", "/workspace/control.json"),
                ("GH_TOKEN", "workspace-tool-key"),
                ("HOME", "/workspace/home"),
                ("XDG_CONFIG_HOME", "/workspace/config"),
            ],
        )
        .unwrap(),
    );
    assert_eq!(observed["OPENAI_API_KEY"], "operator-model-key");
    assert_eq!(observed["HOME"], "/operator/home");
    assert_eq!(observed["XDG_CONFIG_HOME"], "/operator/config");
    for key in [
        "ANTHROPIC_API_KEY",
        "CUSTOM_MODEL_ACCOUNT",
        "OPENCODE_CONFIG",
        "GH_TOKEN",
    ] {
        assert!(
            observed.get(key).is_none(),
            "workspace variable {key} entered provider"
        );
    }
}

#[tokio::test]
async fn opencode_workspace_control_process_does_not_evaluate_project_configuration() {
    std::fs::create_dir_all("_local/tmp").unwrap();
    let root = tempfile::tempdir_in("_local/tmp").unwrap();
    // This unapproved project environment would fail if discovery evaluated it.
    std::fs::write(root.path().join(".envrc"), "exit 29\n").unwrap();
    let cwd = root.path().canonicalize().unwrap();
    std::fs::write(cwd.join("scenario"), "reply").unwrap();
    let config: super::super::config::AcpAgentConfig = serde_json::from_value(serde_json::json!({
        "provider":"opencode", "command":"/usr/bin/python3",
        "args":["-u",concat!(env!("CARGO_MANIFEST_DIR"),"/tests/fixtures/acp_planner/agent.py")],
        "env":{"ACP_FIXTURE_ROOT":cwd},
        "min_agent_version":"2.1.3"
    }))
    .unwrap();
    let host = super::super::config::AcpPlannerHost::new(
        Some(super::super::config::AcpPlannerConfig {
            agents: vec![config],
        }),
        &cwd,
        cwd.join("unused-shim"),
        cwd.join("mcp.sock"),
        Default::default(),
    )
    .unwrap();
    // Native session setup activates the metadata-only plugin and proves it ready.
    assert!(
        !host
            .discover_catalog(&calm_types::runtime::AgentProvider::OpenCode, Some(&cwd))
            .await
            .models
            .is_empty()
    );
}

#[test]
fn acp_workspace_payload_limit_is_an_explicit_failure() {
    let large = "large-test-value".repeat(16 * 1024);
    assert!(matches!(
        launch("grok", serde_json::json!({}), &[("GH_TOKEN", &large)]),
        Err(CalmError::BadRequest(_))
    ));
}
