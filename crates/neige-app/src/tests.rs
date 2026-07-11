use super::*;

#[test]
fn systemd_unit_points_at_system_serve() {
    let unit = render_systemd_unit(
        "neige-app",
        &PathBuf::from("/opt/neige/bin/neige-app"),
        &PathBuf::from("/home/me/.config/neige-app/config.toml"),
        "/usr/local/bin:/usr/bin",
        SystemdScope::User,
        None,
    )
    .expect("render unit");

    assert!(!unit.contains("User="));
    assert!(unit.contains("ExecStart=/opt/neige/bin/neige-app system serve"));
    assert!(unit.contains("--config /home/me/.config/neige-app/config.toml"));
    assert!(!unit.contains("--child-bin"));
    assert!(unit.contains("Restart=always"));
    assert!(!unit.contains("Group="));
    assert!(!unit.contains("Environment=HOME="));
    assert!(!unit.contains("Delegate=yes"));
    assert!(!unit.contains("KillMode=mixed"));
    assert!(!unit.contains("WantedBy=multi-user.target"));
    assert!(unit.contains("WantedBy=default.target"));
}

#[test]
fn systemd_system_unit_includes_system_scope_directives() {
    let run_as = SystemdRunAs {
        user: "kenji".into(),
        home: PathBuf::from("/home/kenji"),
    };
    let unit = render_systemd_unit(
        "neige-app",
        &PathBuf::from("/opt/neige/bin/neige-app"),
        &PathBuf::from("/home/kenji/.config/neige-app/config.toml"),
        "/usr/local/bin:/usr/bin",
        SystemdScope::System,
        Some(&run_as),
    )
    .expect("render unit");

    assert!(unit.contains("Description=neige-app system service"));
    assert!(unit.contains("User=kenji"));
    assert!(unit.contains("Group=kenji"));
    assert!(unit.contains("Environment=HOME=/home/kenji"));
    assert!(unit.contains("ExecStart=/opt/neige/bin/neige-app system serve"));
    assert!(unit.contains("Restart=always"));
    assert!(unit.contains("Delegate=yes"));
    assert!(unit.contains("KillMode=mixed"));
    assert!(unit.contains("WantedBy=multi-user.target"));
    assert!(!unit.contains("WantedBy=default.target"));
}

#[test]
fn systemd_unit_rejects_unsafe_exec_paths() {
    let err = render_systemd_unit(
        "neige-app",
        &PathBuf::from("/opt/neige app/neige-app"),
        &PathBuf::from("/home/me/.config/neige-app/config.toml"),
        "/usr/local/bin:/usr/bin",
        SystemdScope::User,
        None,
    )
    .expect_err("unsafe path must fail");
    assert!(err.to_string().contains("whitespace"));
}

#[test]
fn systemd_unit_rejects_percent_specifier_paths() {
    let err = render_systemd_unit(
        "neige-app",
        &PathBuf::from("/opt/neige%h/neige-app"),
        &PathBuf::from("/home/me/.config/neige-app/config.toml"),
        "/usr/local/bin:/usr/bin",
        SystemdScope::User,
        None,
    )
    .expect_err("percent path must fail");
    assert!(err.to_string().contains("%"));
}

#[test]
fn systemd_unit_includes_path_env() {
    let unit = render_systemd_unit(
        "neige-app",
        &PathBuf::from("/opt/neige/bin/neige-app"),
        &PathBuf::from("/home/me/.config/neige-app/config.toml"),
        "/foo/bin:/usr/bin",
        SystemdScope::User,
        None,
    )
    .expect("render unit");

    assert_eq!(
        unit.matches("Environment=\"PATH=/foo/bin:/usr/bin\"")
            .count(),
        1
    );
    let env_pos = unit
        .find("Environment=\"PATH=/foo/bin:/usr/bin\"")
        .expect("PATH environment line");
    let exec_pos = unit.find("ExecStart=").expect("ExecStart line");
    assert!(env_pos < exec_pos);
}

#[test]
fn systemd_unit_quotes_space_in_path() {
    let unit = render_systemd_unit(
        "neige-app",
        &PathBuf::from("/opt/neige/bin/neige-app"),
        &PathBuf::from("/home/me/.config/neige-app/config.toml"),
        "/opt/ai tools/bin:/usr/bin",
        SystemdScope::User,
        None,
    )
    .expect("render unit");

    assert!(unit.contains("Environment=\"PATH=/opt/ai tools/bin:/usr/bin\"\n"));
}

#[test]
fn systemd_unit_preserves_literal_dollar_in_path() {
    let unit = render_systemd_unit(
        "neige-app",
        &PathBuf::from("/opt/neige/bin/neige-app"),
        &PathBuf::from("/home/me/.config/neige-app/config.toml"),
        "/opt/x$y/bin",
        SystemdScope::User,
        None,
    )
    .expect("render unit");

    assert!(
        unit.lines()
            .any(|line| line == "Environment=\"PATH=/opt/x$y/bin\"")
    );
}

#[test]
fn systemd_unit_escapes_backslash_in_path() {
    let unit = render_systemd_unit(
        "neige-app",
        &PathBuf::from("/opt/neige/bin/neige-app"),
        &PathBuf::from("/home/me/.config/neige-app/config.toml"),
        "/a\\b/bin",
        SystemdScope::User,
        None,
    )
    .expect("render unit");

    assert!(unit.contains("Environment=\"PATH=/a\\\\b/bin\"\n"));
}

#[test]
fn systemd_unit_escapes_double_quote_in_path() {
    let unit = render_systemd_unit(
        "neige-app",
        &PathBuf::from("/opt/neige/bin/neige-app"),
        &PathBuf::from("/home/me/.config/neige-app/config.toml"),
        "/a\"b/bin",
        SystemdScope::User,
        None,
    )
    .expect("render unit");

    assert!(unit.contains("Environment=\"PATH=/a\\\"b/bin\"\n"));
}

#[test]
fn systemd_unit_rejects_empty_path() {
    let err = render_systemd_unit(
        "neige-app",
        &PathBuf::from("/opt/neige/bin/neige-app"),
        &PathBuf::from("/home/me/.config/neige-app/config.toml"),
        "",
        SystemdScope::User,
        None,
    )
    .expect_err("empty PATH must fail");
    assert!(err.to_string().contains("pass --path explicitly"));
}

#[test]
fn systemd_unit_rejects_newline_in_path() {
    let err = render_systemd_unit(
        "neige-app",
        &PathBuf::from("/opt/neige/bin/neige-app"),
        &PathBuf::from("/home/me/.config/neige-app/config.toml"),
        "/foo/bin\n/usr/bin",
        SystemdScope::User,
        None,
    )
    .expect_err("newline in PATH must fail");
    assert!(err.to_string().contains("control"));
}

#[test]
fn systemd_unit_rejects_tab_in_path() {
    let err = render_systemd_unit(
        "neige-app",
        &PathBuf::from("/opt/neige/bin/neige-app"),
        &PathBuf::from("/home/me/.config/neige-app/config.toml"),
        "/foo/bin\t/usr/bin",
        SystemdScope::User,
        None,
    )
    .expect_err("tab in PATH must fail");
    assert!(err.to_string().contains("control"));
}

#[test]
fn systemd_unit_rejects_percent_in_path() {
    let err = render_systemd_unit(
        "neige-app",
        &PathBuf::from("/opt/neige/bin/neige-app"),
        &PathBuf::from("/home/me/.config/neige-app/config.toml"),
        "/foo/%h/bin:/usr/bin",
        SystemdScope::User,
        None,
    )
    .expect_err("percent in PATH must fail");
    assert!(err.to_string().contains("%"));
}

#[test]
fn systemd_unit_rejects_nul_in_path() {
    let err = render_systemd_unit(
        "neige-app",
        &PathBuf::from("/opt/neige/bin/neige-app"),
        &PathBuf::from("/home/me/.config/neige-app/config.toml"),
        "/foo/bin\0/usr/bin",
        SystemdScope::User,
        None,
    )
    .expect_err("NUL in PATH must fail");
    assert!(err.to_string().contains("control"));
}

#[test]
fn child_state_strings_are_stable_wire_values() {
    assert_eq!(ChildState::Stopped.as_str(), "stopped");
    assert_eq!(ChildState::Starting.as_str(), "starting");
    assert_eq!(ChildState::Running.as_str(), "running");
    assert_eq!(ChildState::Stopping.as_str(), "stopping");
    assert_eq!(ChildState::Exited.as_str(), "exited");
}

#[test]
fn cli_shape_is_system_only() {
    assert!(Cli::try_parse_from(["neige-app", "system", "unit"]).is_ok());
    assert!(
        Cli::try_parse_from(["neige-app", "system", "print-unit", "--path", "/usr/bin"]).is_ok()
    );
    assert!(Cli::try_parse_from(["neige-app", "desktop", "serve"]).is_err());
    assert!(Cli::try_parse_from(["neige-app", "container", "serve"]).is_err());
}

#[tokio::test]
async fn status_route_returns_supervisor_identity_shape() {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    let cfg = AppConfig::starter(PathBuf::from("/tmp/neige-app/config.toml"));
    let state = AppState {
        cfg: Arc::new(cfg),
        supervisor: Supervisor::new(SupervisorConfig {
            name: "calm-server".into(),
            child_bin: PathBuf::from("calm-server"),
            child_cwd: None,
            child_args: Vec::new(),
            child_envs: vec![("CALM_LISTEN".into(), "127.0.0.1:4040".into())],
            restart_delay: Duration::from_millis(1),
            stop_grace: Duration::from_millis(1),
            calm_listen: Some("127.0.0.1:4040".into()),
            persist_identity_to: None,
        }),
        proc_supervisor: Supervisor::new(SupervisorConfig {
            name: "calm-proc-supervisor".into(),
            child_bin: PathBuf::from("calm-proc-supervisor"),
            child_cwd: None,
            child_args: Vec::new(),
            child_envs: Vec::new(),
            restart_delay: Duration::from_millis(1),
            stop_grace: Duration::from_millis(1),
            calm_listen: None,
            persist_identity_to: None,
        }),
        apply_lock: Arc::new(Mutex::new(())),
        admin_token: Some(Arc::from("test-token")),
    };
    let app = admin_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/status")
                .header(axum::http::header::AUTHORIZATION, "Bearer test-token")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("status response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let body: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert!(body["calmServer"].is_object());
    assert!(body["procSupervisor"].is_object());
    assert!(body["calmServer"]["identity"].is_null());
    assert!(body["procSupervisor"]["identity"].is_null());
}

#[tokio::test]
async fn adopted_supervisor_status_keeps_peer_pid() {
    let supervisor = Supervisor::new(SupervisorConfig {
        name: "calm-proc-supervisor".into(),
        child_bin: PathBuf::from("calm-proc-supervisor"),
        child_cwd: None,
        child_args: Vec::new(),
        child_envs: Vec::new(),
        restart_delay: Duration::from_millis(1),
        stop_grace: Duration::from_millis(1),
        calm_listen: None,
        persist_identity_to: None,
    });

    supervisor.adopt_identity(None, Some(12345)).await;
    let status = supervisor.process_status().await;

    assert_eq!(status.child_state, "running");
    assert_eq!(status.child_pid, Some(12345));
    assert!(!status.desired_running);
}

#[test]
fn bearer_gate_refuses_state_changes_when_token_is_not_configured() {
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        "Bearer anything".parse().expect("valid auth header"),
    );

    let err = require_bearer(&headers, None).expect_err("missing configured token is fatal");
    assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(err.code, "admin_token_not_configured");
}

#[test]
fn bearer_gate_accepts_matching_token() {
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        "Bearer expected".parse().expect("valid auth header"),
    );

    require_bearer(&headers, Some("expected")).expect("matching token");
}

#[test]
fn install_refuses_existing_unit_without_force() {
    let tmp = test_temp_dir("install-existing-unit");
    let config_path = tmp.join("config.toml");
    let unit_path = tmp.join("neige-app.service");
    let token_path = tmp.join("admin.token");
    std::fs::write(
        &config_path,
        format!(
            r#"
[admin]
token_file = "{}"

[systemd]
unit_path = "{}"
bin = "/usr/local/bin/neige-app"
"#,
            token_path.display(),
            unit_path.display()
        ),
    )
    .expect("write config");
    std::fs::write(&unit_path, "existing").expect("write unit");

    let err = run_install(SystemInstallArgs {
        config: Some(config_path),
        force: false,
        path: Some("/usr/local/bin:/usr/bin".into()),
        user: None,
    })
    .expect_err("existing unit must fail");

    assert!(err.to_string().contains("already exists"));
    assert!(!token_path.exists());
}

#[test]
fn install_creates_token_file() {
    let tmp = test_temp_dir("install-token");
    let config_path = tmp.join("config.toml");
    let unit_path = tmp.join("neige-app.service");
    let token_path = tmp.join("admin.token");
    std::fs::write(
        &config_path,
        format!(
            r#"
[admin]
token_file = "{}"

[systemd]
unit_path = "{}"
bin = "/usr/local/bin/neige-app"
"#,
            token_path.display(),
            unit_path.display()
        ),
    )
    .expect("write config");

    run_install(SystemInstallArgs {
        config: Some(config_path),
        force: false,
        path: Some("/usr/local/bin:/usr/bin".into()),
        user: None,
    })
    .expect("install");

    let token = std::fs::read_to_string(&token_path).expect("read token");
    assert_eq!(token.len(), 64);
    assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(unit_path.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&token_path)
            .expect("token metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn install_next_steps_are_scope_aware() {
    let user_steps = install_next_steps(SystemdScope::User, "neige-app");
    assert!(user_steps.iter().any(|step| step.contains("--user")));
    assert!(user_steps.contains(&"systemctl --user daemon-reload".to_owned()));
    assert!(
        user_steps
            .iter()
            .any(|step| step == "systemctl --user enable --now neige-app")
    );

    let system_steps = install_next_steps(SystemdScope::System, "neige-app");
    assert!(!system_steps.iter().any(|step| step.contains("--user")));
    assert!(system_steps.contains(&"systemctl daemon-reload".to_owned()));
    assert!(
        system_steps
            .iter()
            .any(|step| step == "systemctl enable --now neige-app")
    );
}

#[test]
fn systemd_run_as_prefers_sudo_user_over_user_env() {
    let tmp = test_temp_dir("passwd-sudo-user");
    let passwd_path = tmp.join("passwd");
    std::fs::write(
        &passwd_path,
        "root:x:0:0:root:/root:/bin/bash\nkenji:x:1000:1000:Kenji:/home/kenji:/bin/zsh\n",
    )
    .expect("write passwd");
    let env = SystemdInstallEnv {
        sudo_user: Some("kenji".into()),
        user: Some("root".into()),
    };

    let run_as =
        resolve_systemd_run_as(None, None, None, &env, &passwd_path).expect("resolve run user");

    assert_eq!(run_as.user, "kenji");
    assert_eq!(run_as.home, PathBuf::from("/home/kenji"));
}

#[test]
fn systemd_run_as_rejects_inferred_root() {
    let tmp = test_temp_dir("passwd-root");
    let passwd_path = tmp.join("passwd");
    std::fs::write(&passwd_path, "root:x:0:0:root:/root:/bin/bash\n").expect("write passwd");
    let env = SystemdInstallEnv {
        sudo_user: None,
        user: Some("root".into()),
    };

    let err = resolve_systemd_run_as(None, None, None, &env, &passwd_path)
        .expect_err("inferred root must fail");

    assert!(err.to_string().contains("refusing"));
    assert!(err.to_string().contains("systemd.user"));
    assert!(err.to_string().contains("--user"));
}

#[test]
fn systemd_run_as_config_override_wins_and_home_override_applies() {
    let tmp = test_temp_dir("passwd-config-user");
    let passwd_path = tmp.join("passwd");
    std::fs::write(
        &passwd_path,
        "deploy:x:1001:1001:Deploy:/srv/deploy:/bin/sh\nkenji:x:1000:1000:Kenji:/home/kenji:/bin/zsh\n",
    )
    .expect("write passwd");
    let env = SystemdInstallEnv {
        sudo_user: Some("kenji".into()),
        user: Some("root".into()),
    };

    let run_as = resolve_systemd_run_as(
        None,
        Some("deploy"),
        Some(Path::new("/custom/home")),
        &env,
        &passwd_path,
    )
    .expect("resolve run user");

    assert_eq!(run_as.user, "deploy");
    assert_eq!(run_as.home, PathBuf::from("/custom/home"));
}

#[test]
fn web_only_activation_next_steps_do_not_restart_backend() {
    let cfg = AppConfig::starter(PathBuf::from("/tmp/neige-app/config.toml"));
    let activation = upgrade::ActivationResult {
        activated: true,
        mode: "web-only".into(),
        release_id: "web-1".into(),
        restart_required: false,
        changed_symlinks: Vec::new(),
        db_backup: None,
    };

    let steps = upgrade_next_steps(&cfg, Some(&activation));

    assert_eq!(steps.len(), 1);
    assert!(steps[0].contains("No backend restart required"));
    assert!(!steps.iter().any(|step| step.contains("/restart")));
    assert!(!steps.iter().any(|step| step.contains("systemctl")));
}

#[test]
fn server_activation_next_steps_include_restart() {
    let cfg = AppConfig::starter(PathBuf::from("/tmp/neige-app/config.toml"));
    let activation = upgrade::ActivationResult {
        activated: true,
        mode: "server-only".into(),
        release_id: "server-1".into(),
        restart_required: true,
        changed_symlinks: Vec::new(),
        db_backup: None,
    };

    let steps = upgrade_next_steps(&cfg, Some(&activation));

    assert!(steps.iter().any(|step| step.contains("/restart")));
    assert!(
        steps
            .iter()
            .any(|step| step == "systemctl --user restart neige-app")
    );
}

#[test]
fn server_activation_next_steps_use_system_restart_for_system_scope() {
    let mut cfg = AppConfig::starter(PathBuf::from("/tmp/neige-app/config.toml"));
    cfg.systemd.scope = SystemdScope::System;
    let activation = upgrade::ActivationResult {
        activated: true,
        mode: "server-only".into(),
        release_id: "server-1".into(),
        restart_required: true,
        changed_symlinks: Vec::new(),
        db_backup: None,
    };

    let steps = upgrade_next_steps(&cfg, Some(&activation));

    assert!(steps.iter().any(|step| step.contains("/restart")));
    assert!(
        steps
            .iter()
            .any(|step| step == "systemctl restart neige-app")
    );
    assert!(!steps.iter().any(|step| step.contains("--user")));
}

fn test_temp_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("neige-app-{name}-{}", std::process::id()));
    if path.exists() {
        std::fs::remove_dir_all(&path).expect("remove stale temp dir");
    }
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}
