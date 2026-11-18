use super::*;
use std::{
    io::Write,
    process::{Command, Stdio},
    sync::Arc,
};

#[tokio::test]
async fn claude_workspace_environment_reaches_real_child_privately() {
    if std::env::var_os("CLAUDE_ENV_TEST_CHILD").is_none() {
        std::fs::create_dir_all("_local/tmp").unwrap();
        let home = tempfile::tempdir_in("_local/tmp").unwrap();
        let home_path = std::fs::canonicalize(home.path()).unwrap();
        let name = crate::test_support::exact_test_name(
            module_path!(),
            stringify!(claude_workspace_environment_reaches_real_child_privately),
        );
        let output = Command::new(std::env::current_exe().unwrap())
            .args([name.as_str(), "--exact", "--nocapture", "--test-threads=1"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home_path)
            .env("XDG_CONFIG_HOME", home_path.join("config"))
            .env("XDG_DATA_HOME", home_path.join("data"))
            .env("TMPDIR", &home_path)
            .env("CLAUDE_ENV_TEST_CHILD", "1")
            .output()
            .unwrap();
        crate::test_support::assert_isolated_test_passed(&output);
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join(".envrc"),
        r#"export GH_TOKEN=synthetic-claude-project
export ANTHROPIC_API_KEY=must-not-select-inference
export CLAUDE_CONFIG_DIR=/wrong-config
export NEIGE_MCP_SOCKET=/wrong-socket
export HTTP_PROXY=http://project-proxy:8181
export http_proxy=http://project-proxy:8181
"#,
    )
    .unwrap();
    assert!(
        Command::new("direnv")
            .arg("allow")
            .arg(root.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    let repo: Arc<dyn crate::db::Repo> = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            crate::model::NewArea {
                name: "environment".into(),
                color: "#000000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(crate::model::NewTrack {
            area_id: area.id,
            title: "environment".into(),
            cwd: root.path().display().to_string(),
            sort: None,
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: crate::model::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let host =
        Arc::new(crate::claude_planner::config::ClaudePlannerHost::unconfigured_scratch().unwrap());
    let mut params = ClaudePlannerSessionParams {
        engine: None,
        host: host.clone(),
        worker_session_id: "environment-worker".into(),
        card_id: "environment-card".into(),
        track_id: track.id.to_string(),
        cwd: root.path().to_owned(),
        instructions: String::new(),
        calm_tools: host.calm_tools.clone(),
        proxy: vec![],
        prior_total_tokens: 0,
        repo,
        seals: Arc::new(crate::thread_seals::ThreadSeals::new()),
    };
    let admission = crate::harness::native_admission::NativePromptAdmission::new(
        || Ok(()),
        || Box::pin(async { Ok(()) }),
    );
    for configured in [false, true] {
        let expected_proxy = if configured {
            "http://operator-user:operator-secret@operator-proxy:8282"
        } else {
            "http://project-proxy:8181"
        };
        params.proxy = if configured {
            vec![(
                "HTTP_PROXY".into(),
                "http_proxy".into(),
                expected_proxy.into(),
            )]
        } else {
            Vec::new()
        };
        let logs = root
            .path()
            .join(if configured { "configured" } else { "project" });
        std::fs::create_dir(&logs).unwrap();
        let source_env =
            crate::claude_planner::spawn::session_env(&params, Path::new("/operator-config"))
                .unwrap();
        let script = r#"printf '%s' "${GH_TOKEN-unset}" > child-token
printf '%s' "${HTTP_PROXY-unset}" > child-proxy
printf '%s' "${http_proxy-unset}" > child-proxy-lower
test "${ANTHROPIC_API_KEY-unset}" = unset
test "$CLAUDE_CONFIG_DIR" = /operator-config
test "$NEIGE_MCP_SOCKET" = "$1"
"#;
        let args = vec![
            "-c".into(),
            script.into(),
            "observe".into(),
            params.host.mcp_socket.as_os_str().to_owned(),
        ];
        let (intent, payload) = intent(
            &params,
            &ExecutionId::fresh().unwrap(),
            &logs,
            Path::new("/bin/sh"),
            args,
            source_env,
            Some("synthetic-kernel-token".into()),
            &admission,
        )
        .await
        .unwrap();
        let serialized = serde_json::to_string(&intent.metadata).unwrap();
        for secret in [
            "synthetic-claude-project",
            "must-not-select-inference",
            "synthetic-kernel-token",
            "operator-secret",
        ] {
            assert!(!serialized.contains(secret));
        }
        let mut child = Command::new(&intent.metadata.program.program)
            .args(&intent.metadata.program.args)
            .env_clear()
            .envs(intent.metadata.program.environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&payload).unwrap();
        assert!(child.wait_with_output().unwrap().status.success());
        assert_eq!(
            std::fs::read_to_string(root.path().join("child-token")).unwrap(),
            "synthetic-claude-project"
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("child-proxy")).unwrap(),
            expected_proxy
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("child-proxy-lower")).unwrap(),
            expected_proxy
        );
    }
}
