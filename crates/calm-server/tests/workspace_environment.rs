//! The installed kernel loads project environments inside admitted PTY executions.
use std::{collections::BTreeMap, ffi::OsString, path::Path, process::Command};

struct Fixture {
    _root: tempfile::TempDir,
    source: std::path::PathBuf,
    cwd: std::path::PathBuf,
    base: BTreeMap<OsString, OsString>,
}
impl Fixture {
    fn new(rc: &str) -> Self {
        std::fs::create_dir_all("_local/tmp").unwrap();
        let root = tempfile::tempdir_in("_local/tmp").unwrap();
        let path = std::fs::canonicalize(root.path()).unwrap();
        let source = path.join("source");
        let cwd = path.join("worktree");
        let home = path.join("home");
        for directory in [&source, &cwd, &home] {
            std::fs::create_dir(directory).unwrap();
        }
        std::fs::write(source.join(".envrc"), rc).unwrap();
        let base = BTreeMap::from([
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), home.as_os_str().to_owned()),
            (
                "XDG_CONFIG_HOME".into(),
                home.join("config").into_os_string(),
            ),
            ("XDG_DATA_HOME".into(), home.join("data").into_os_string()),
            ("TERM".into(), "original-terminal".into()),
            ("NEIGE_MCP_TOKEN".into(), "synthetic-kernel-runtime".into()),
        ]);
        Self {
            _root: root,
            source,
            cwd,
            base,
        }
    }
    fn allow(&self) {
        assert!(
            Command::new("direnv")
                .arg("allow")
                .arg(&self.source)
                .current_dir(&self.source)
                .env_clear()
                .envs(&self.base)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    fn loader(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_calm-server"));
        command
            .args(["--workspace-environment-exec", "--source"])
            .arg(&self.source)
            .current_dir(&self.cwd)
            .env_clear()
            .envs(&self.base);
        if let Some(config) = self.base.get(std::ffi::OsStr::new("DIRENV_CONFIG")) {
            command.arg("--direnv-config").arg(config);
        }
        command
    }
    fn command(&self, script: &str) -> Command {
        let mut command = self.loader();
        command
            .args([
                "--preserve-key",
                "HOME",
                "--preserve-key",
                "XDG_CONFIG_HOME",
                "--preserve-key",
                "XDG_DATA_HOME",
                "--preserve-prefix",
                "NEIGE_",
                "--preserve-prefix",
                "CLAUDE_",
                "--preserve-prefix",
                "ANTHROPIC_",
                "--path-prefix",
            ])
            .arg(self.source.join("kernel-bin"))
            .args(["--", "/bin/sh", "-c", script]);
        command
    }
}

#[test]
fn workspace_environment_binary_preserves_runtime_and_unsets_without_echoing_tokens() {
    let fixture = Fixture::new(
        "test \"${NEIGE_MCP_TOKEN-unset}\" = unset || exit 19\nexport GH_TOKEN=synthetic-loader-project\nexport NEIGE_MCP_TOKEN=must-not-override\nexport CLAUDE_CONFIG_DIR=/must-not-select-account\nunset TERM\nexport PATH=$PWD/project-bin:$PATH\n",
    );
    fixture.allow();
    let script = "printf '%s|%s|%s|%s' \"${GH_TOKEN-unset}\" \"${NEIGE_MCP_TOKEN-unset}\" \"${CLAUDE_CONFIG_DIR-unset}\" \"${TERM-unset}\" > environment-result; printf '%s' \"$PATH\" > environment-path";
    let output = fixture.command(script).output().unwrap();
    assert!(
        output.status.success(),
        "loader exited with {:?}",
        output.status.code()
    );
    assert_eq!(
        std::fs::read_to_string(fixture.cwd.join("environment-result")).unwrap(),
        "synthetic-loader-project|synthetic-kernel-runtime|unset|unset"
    );
    let path = std::fs::read_to_string(fixture.cwd.join("environment-path")).unwrap();
    let entries = std::env::split_paths(&path).collect::<Vec<_>>();
    assert_eq!(entries[0], fixture.source.join("kernel-bin"));
    assert!(entries.contains(&fixture.source.join("project-bin")));
    for stream in [output.stdout, output.stderr] {
        let text = String::from_utf8_lossy(&stream);
        for secret in [
            "synthetic-loader-project",
            "synthetic-kernel-runtime",
            "must-not-override",
        ] {
            assert!(!text.contains(secret));
        }
    }
}

#[test]
fn workspace_environment_binary_refuses_unapproved_source_before_target() {
    let fixture = Fixture::new("export GH_TOKEN=synthetic-unapproved-loader\n");
    let output = fixture.command("touch target-started").output().unwrap();
    assert_eq!(output.status.code(), Some(75));
    assert!(!fixture.cwd.join("target-started").exists());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-unapproved-loader"));
}

#[test]
fn workspace_environment_binary_without_source_rc_needs_no_direnv() {
    let mut fixture = Fixture::new("# unused fixture source\n");
    let root = fixture.source.ancestors().last().unwrap().to_owned();
    assert!(!Path::new(&root).join(".envrc").exists());
    fixture.source = root;
    fixture
        .base
        .insert("PATH".into(), "/nonexistent-direnv-bin".into());
    let output = fixture
        .command("printf '%s' \"${GH_TOKEN-unset}\" > no-config-result")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(fixture.cwd.join("no-config-result")).unwrap(),
        "unset"
    );
}

#[test]
fn workspace_environment_binary_uses_declared_direnv_configuration() {
    let mut fixture = Fixture::new("load_project_identity\n");
    let config = fixture.source.parent().unwrap().join("custom-direnv");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("direnvrc"),
        "load_project_identity() { export GH_TOKEN=synthetic-custom-config; }\n",
    )
    .unwrap();
    fixture
        .base
        .insert("DIRENV_CONFIG".into(), config.into_os_string());
    fixture.allow();
    let output = fixture
        .command("printf '%s' \"$GH_TOKEN\" > custom-config-result")
        .env("DIRENV_CONFIG", fixture.cwd.join("wrong-inherited-config"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(fixture.cwd.join("custom-config-result")).unwrap(),
        "synthetic-custom-config"
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-custom-config"));
}

#[test]
fn workspace_environment_binary_without_declared_config_ignores_inherited_selection() {
    let fixture = Fixture::new("load_project_identity\n");
    let config = Path::new(
        fixture
            .base
            .get(std::ffi::OsStr::new("XDG_CONFIG_HOME"))
            .unwrap(),
    )
    .join("direnv");
    let wrong = fixture.cwd.join("wrong-inherited-config");
    for (directory, value) in [
        (&config, "synthetic-default-config"),
        (&wrong, "synthetic-wrong-inherited-config"),
    ] {
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(
            directory.join("direnvrc"),
            format!("load_project_identity() {{ export GH_TOKEN={value}; }}\n"),
        )
        .unwrap();
    }
    fixture.allow();
    let output = fixture
        .command("printf '%s' \"$GH_TOKEN\" > default-config-result")
        .env("DIRENV_CONFIG", &wrong)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(fixture.cwd.join("default-config-result")).unwrap(),
        "synthetic-default-config"
    );
}

#[test]
fn workspace_environment_binary_relative_config_follows_the_selected_source() {
    let mut fixture = Fixture::new("load_project_identity\n");
    let config = fixture.source.join("relative-direnv");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("direnvrc"),
        "load_project_identity() { export GH_TOKEN=synthetic-relative-source-config; }\n",
    )
    .unwrap();
    fixture
        .base
        .insert("DIRENV_CONFIG".into(), "relative-direnv".into());
    fixture.allow();
    let output = fixture
        .command("printf '%s' \"$GH_TOKEN\" > relative-config-result")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(fixture.cwd.join("relative-config-result")).unwrap(),
        "synthetic-relative-source-config"
    );
}

#[test]
fn workspace_environment_binary_excludes_service_proxy_credentials_from_project_code() {
    let mut fixture = Fixture::new(
        "test \"${HTTP_PROXY-unset}\" = unset || exit 19\n\
         test \"${HTTPS_PROXY-unset}\" = http://proxy.example:8080 || exit 20\n\
         export HTTP_PROXY=http://project:synthetic-project-proxy@project.example:8080\n",
    );
    fixture.base.insert(
        "HTTP_PROXY".into(),
        "http://service:synthetic-service-proxy@service.example:8080".into(),
    );
    fixture
        .base
        .insert("HTTPS_PROXY".into(), "http://proxy.example:8080".into());
    fixture.allow();
    let output = fixture
        .command("printf '%s' \"$HTTP_PROXY\" > project-proxy-result")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "loader exited {:?}",
        output.status.code()
    );
    assert_eq!(
        std::fs::read_to_string(fixture.cwd.join("project-proxy-result")).unwrap(),
        "http://project:synthetic-project-proxy@project.example:8080"
    );
    for stream in [output.stdout, output.stderr] {
        let text = String::from_utf8_lossy(&stream);
        assert!(!text.contains("synthetic-service-proxy"));
        assert!(!text.contains("synthetic-project-proxy"));
    }
}

#[test]
fn workspace_environment_binary_without_rc_excludes_service_proxy_credentials() {
    let mut fixture = Fixture::new("# unused fixture source\n");
    fixture.source = fixture.source.ancestors().last().unwrap().to_owned();
    fixture.base.insert(
        "HTTP_PROXY".into(),
        "http://service:synthetic-service-proxy@service.example:8080".into(),
    );
    fixture
        .base
        .insert("HTTPS_PROXY".into(), "http://proxy.example:8080".into());
    let output = fixture
        .command("printf '%s|%s' \"${HTTP_PROXY-unset}\" \"$HTTPS_PROXY\" > no-rc-proxy-result")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(fixture.cwd.join("no-rc-proxy-result")).unwrap(),
        "unset|http://proxy.example:8080"
    );
    for stream in [output.stdout, output.stderr] {
        assert!(!String::from_utf8_lossy(&stream).contains("synthetic-service-proxy"));
    }
}

#[test]
fn workspace_environment_binary_exports_shell_values_for_the_actual_mapped_cwd() {
    let mut fixture = Fixture::new(
        "test \"${NEIGE_MCP_TOKEN-unset}\" = unset || exit 19\n\
         test \"${OPENAI_API_KEY-unset}\" = unset || exit 20\n\
         export OPENAI_API_KEY=synthetic-project-model\n\
         export CUSTOM_MODEL_ACCOUNT=synthetic-custom-model\n\
         export HOME=$PWD/project-home\n\
         export XDG_CONFIG_HOME=$PWD/project-config\n",
    );
    fixture
        .base
        .insert("OPENAI_API_KEY".into(), "synthetic-operator-model".into());
    fixture.allow();
    let nested = fixture.source.join("nested");
    let cwd = fixture.cwd.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(
        nested.join(".envrc"),
        "source_up\nexport GH_TOKEN=synthetic-nested-tool\nunset TERM\n",
    )
    .unwrap();
    assert!(
        Command::new("direnv")
            .args(["allow"])
            .arg(&nested)
            .env_clear()
            .envs(&fixture.base)
            .output()
            .unwrap()
            .status
            .success()
    );
    let output = fixture
        .loader()
        .args(["--print-environment", "--derived-root"])
        .arg(&fixture.cwd)
        .args(["--preserve-prefix", "NEIGE_", "--path-prefix"])
        .arg(fixture.source.join("kernel-bin"))
        .current_dir(&cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "loader exited {:?}",
        output.status.code()
    );
    let environment: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(environment["OPENAI_API_KEY"], "synthetic-project-model");
    assert_eq!(
        environment["CUSTOM_MODEL_ACCOUNT"],
        "synthetic-custom-model"
    );
    assert_eq!(environment["GH_TOKEN"], "synthetic-nested-tool");
    assert_eq!(environment["NEIGE_MCP_TOKEN"], "synthetic-kernel-runtime");
    assert_eq!(
        environment["HOME"],
        fixture.source.join("project-home").to_str().unwrap()
    );
    assert_eq!(
        environment["XDG_CONFIG_HOME"],
        fixture.source.join("project-config").to_str().unwrap()
    );
    assert!(environment.get("TERM").is_none());
    assert_eq!(
        std::env::split_paths(environment["PATH"].as_str().unwrap())
            .next()
            .unwrap(),
        fixture.source.join("kernel-bin")
    );
    for secret in [
        "synthetic-project-model",
        "synthetic-nested-tool",
        "synthetic-kernel-runtime",
        "synthetic-operator-model",
    ] {
        assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    }
}

#[test]
fn workspace_environment_binary_refuses_unapproved_environment_export_without_values() {
    let fixture = Fixture::new("export GH_TOKEN=synthetic-unapproved-export\n");
    let output = fixture
        .loader()
        .args(["--print-environment", "--derived-root"])
        .arg(&fixture.cwd)
        .current_dir(&fixture.cwd)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(75));
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-unapproved-export"));
}
