//! Project environments must reach the real Claude PTY on every start, including resume.
use super::live_restart::starts;
use super::{Boot, RealClaude, body, boot_with_real_claude, post, post_restart};
use axum::http::StatusCode;
use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_truth::db::RepoRead;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const BUDGET: Duration = Duration::from_secs(120);

#[path = "workspace_environment/proxy.rs"]
mod proxy;

/// Run each real-PTY case with only its own HOME, direnv trust database and owner inputs.
fn isolated(test: &str, custom_config: bool) -> bool {
    isolated_with_environment(test, custom_config, &[])
}

fn isolated_with_environment(
    test: &str,
    custom_config: bool,
    environment: &[(&str, &str)],
) -> bool {
    if std::env::var_os("CLAUDE_PTY_ENV_CHILD").is_some() {
        return false;
    }
    std::fs::create_dir_all("_local/tmp").unwrap();
    let root = tempfile::tempdir_in("_local/tmp").unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap();
    let name = format!("{module}::{test}");
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args([name.as_str(), "--exact", "--nocapture", "--test-threads=1"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &root)
        .env("TMPDIR", &root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("LANG", "operator-locale")
        .env("CLAUDE_PTY_ENV_CHILD", "1")
        .envs(environment.iter().copied());
    if custom_config {
        child.env("DIRENV_CONFIG", root.join("selected-direnv"));
    }
    let output = child.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success()
            && stdout
                .lines()
                .any(|line| line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;")),
        "isolated regression must execute one passing test\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

struct EnvironmentCard {
    boot: Boot,
    _supervisor: Option<InProcessProcSupervisor>,
    _directory: tempfile::TempDir,
    source: PathBuf,
    capture: PathBuf,
    log: PathBuf,
    card: String,
    terminal: String,
}

impl EnvironmentCard {
    async fn create(rc: &str) -> Self {
        let supervisor = InProcessProcSupervisor::start().await.unwrap();
        let mut live = Self::create_with_socket(rc, supervisor.sock()).await;
        live._supervisor = Some(supervisor);
        live
    }

    async fn create_with_socket(rc: &str, socket: &Path) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("project");
        let capture = directory.path().join("capture");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&capture).unwrap();
        std::fs::write(source.join(".envrc"), rc).unwrap();
        allow(&source);
        let log = directory.path().join("starts");
        let bin = directory.path().join("fake-claude");
        let quote = |path: &Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = --version ]; then printf '2.1.259 (Claude Code)\\n'; exit 0; fi\n\
                 capture={}/$$\n\
                 mkdir \"$capture\"\n\
                 printf '%s' \"${{GH_TOKEN-unset}}\" > \"$capture/project\"\n\
                 printf '%s' \"${{LANG-unset}}\" > \"$capture/lang\"\n\
                 printf '%s' \"${{HTTP_PROXY-unset}}\" > \"$capture/http-upper\"\n\
                 printf '%s' \"${{http_proxy-unset}}\" > \"$capture/http-lower\"\n\
                 printf '%s' \"${{HTTPS_PROXY-unset}}\" > \"$capture/https-upper\"\n\
                 printf '%s' \"${{https_proxy-unset}}\" > \"$capture/https-lower\"\n\
                 printf '%s' \"${{ANTHROPIC_API_KEY-unset}}\" > \"$capture/model\"\n\
                 printf '%s' \"${{CLAUDE_CONFIG_DIR-unset}}\" > \"$capture/account\"\n\
                 printf '%s' \"${{NEIGE_CARD_ID-unset}}\" > \"$capture/card\"\n\
                 printf '%s' \"$HOME\" > \"$capture/home\"\n\
                 printf '%s' \"$PATH\" > \"$capture/path\"\n\
                 printf '%s' \"$PWD\" > \"$capture/cwd\"\n\
                 printf 'start %s %s\\n' \"$$\" \"$*\" >> {}\n\
                 trap 'exit 0' TERM\n\
                 sleep 600 &\n\
                 wait\n",
                quote(&capture),
                quote(&log),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let boot = boot_with_real_claude(RealClaude {
            sock: socket.to_owned(),
            claude_bin: bin.display().to_string(),
        })
        .await;
        let mut request = body(&boot, None);
        request["cwd"] = json!(source);
        let (status, created) = post(boot.app.clone(), &boot.track_id, request, None, None).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        Self {
            boot,
            _supervisor: None,
            _directory: directory,
            source,
            capture,
            log,
            card: created["id"].as_str().unwrap().to_owned(),
            terminal: created["payload"]["terminal_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        }
    }

    fn captured(&self, start: usize, key: &str) -> String {
        let pid = starts(&self.log)[start].0;
        std::fs::read_to_string(self.capture.join(pid.to_string()).join(key)).unwrap()
    }

    async fn wait_for_starts(&self, count: usize) {
        let deadline = tokio::time::Instant::now() + BUDGET;
        while starts(&self.log).len() < count {
            let term = self
                .boot
                .repo
                .terminal_get(&self.terminal)
                .await
                .unwrap()
                .unwrap();
            assert!(
                term.exit_code.is_none(),
                "Claude target did not start; PTY exited {:?}",
                term.exit_code
            );
            assert!(
                tokio::time::Instant::now() < deadline,
                "Claude target did not start"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_for_rejected_target(&self, prior_starts: usize) {
        let deadline = tokio::time::Instant::now() + BUDGET;
        loop {
            assert_eq!(
                starts(&self.log).len(),
                prior_starts,
                "rejected environment started Claude"
            );
            let term = self
                .boot
                .repo
                .terminal_get(&self.terminal)
                .await
                .unwrap()
                .unwrap();
            if let Some(code) = term.exit_code {
                assert_eq!(code, 75);
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "environment rejection was not recorded"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn assert_project_values_not_persisted(&self, values: &[&str]) {
        let terminal = self
            .boot
            .repo
            .terminal_get(&self.terminal)
            .await
            .unwrap()
            .unwrap();
        let metadata: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT payload_json, tx_output_json FROM operations WHERE kind IN ('claude-create', 'claude-restart')",
        )
        .fetch_all(self.boot.repo.pool())
        .await
        .unwrap();
        assert!(!metadata.is_empty());
        for value in values {
            assert!(!terminal.env.to_string().contains(value));
            for (payload, output) in &metadata {
                assert!(!payload.contains(value));
                assert!(!output.as_deref().unwrap_or_default().contains(value));
            }
        }
    }
}

fn allow(source: &Path) {
    assert!(
        Command::new("direnv")
            .arg("allow")
            .arg(source)
            .output()
            .unwrap()
            .status
            .success()
    );
}

fn project_rc(value: &str) -> String {
    format!(
        "test \"${{NEIGE_CARD_ID-unset}}\" = unset || exit 19\n\
         export GH_TOKEN={value}\n\
         export PATH=$PWD/project-bin:$PATH\n\
         unset LANG\n\
         export ANTHROPIC_API_KEY=synthetic-forbidden-model\n\
         export CLAUDE_CONFIG_DIR=/wrong-project-account\n\
         export NEIGE_CARD_ID=wrong-project-card\n\
         export HOME=/wrong-project-home\n"
    )
}

#[tokio::test]
async fn restart_reloads_full_project_environment_without_persisting_values() {
    if isolated(
        stringify!(restart_reloads_full_project_environment_without_persisting_values),
        false,
    ) {
        return;
    }
    let live = EnvironmentCard::create(&project_rc("synthetic-first-project")).await;
    live.wait_for_starts(1).await;
    assert_eq!(live.captured(0, "project"), "synthetic-first-project");
    std::fs::write(
        live.source.join(".envrc"),
        project_rc("synthetic-resumed-project"),
    )
    .unwrap();
    allow(&live.source);
    let (status, response) = post_restart(live.boot.app.clone(), &live.card).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    live.wait_for_starts(2).await;
    assert_eq!(live.captured(1, "project"), "synthetic-resumed-project");
    assert_eq!(live.captured(1, "lang"), "unset");
    assert_eq!(live.captured(1, "model"), "unset");
    assert_eq!(live.captured(1, "account"), "unset");
    assert_eq!(live.captured(1, "card"), live.card);
    assert_eq!(live.captured(1, "home"), std::env::var("HOME").unwrap());
    assert_eq!(live.captured(1, "cwd"), live.source.to_str().unwrap());
    let path = live.captured(1, "path");
    assert!(std::env::split_paths(&path).any(|entry| entry == live.source.join("project-bin")));
    assert!(starts(&live.log)[1].1.contains("--resume="));
    live.assert_project_values_not_persisted(&[
        "synthetic-first-project",
        "synthetic-resumed-project",
        "synthetic-forbidden-model",
    ])
    .await;
}

#[tokio::test]
async fn real_pty_uses_kernel_selected_direnvrc_and_authorization() {
    if isolated(
        stringify!(real_pty_uses_kernel_selected_direnvrc_and_authorization),
        true,
    ) {
        return;
    }
    let selected = PathBuf::from(std::env::var_os("DIRENV_CONFIG").unwrap());
    std::fs::create_dir_all(&selected).unwrap();
    std::fs::write(
        selected.join("direnvrc"),
        "load_test_project_identity() { export GH_TOKEN=synthetic-selected-direnv; }\n",
    )
    .unwrap();
    let wrong = selected.parent().unwrap().join("supervisor-direnv");
    std::fs::create_dir_all(&wrong).unwrap();
    std::fs::write(
        wrong.join("direnvrc"),
        "load_test_project_identity() { export GH_TOKEN=synthetic-wrong-supervisor; }\n",
    )
    .unwrap();
    let supervisor = crate::support::execution::Fixture::start_with_environment(vec![(
        "DIRENV_CONFIG".into(),
        wrong.into_os_string(),
    )]);
    tokio::time::timeout(BUDGET, async {
        loop {
            if calm_session::control::execution::connect_legacy(&supervisor.socket)
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the independently configured supervisor is ready");
    let live =
        EnvironmentCard::create_with_socket("load_test_project_identity\n", &supervisor.socket)
            .await;
    live.wait_for_starts(1).await;
    assert_eq!(live.captured(0, "project"), "synthetic-selected-direnv");
    live.assert_project_values_not_persisted(&["synthetic-selected-direnv"])
        .await;
}

#[tokio::test]
async fn unapproved_project_environment_never_starts_restart_target() {
    if isolated(
        stringify!(unapproved_project_environment_never_starts_restart_target),
        false,
    ) {
        return;
    }
    let live = EnvironmentCard::create("export GH_TOKEN=synthetic-approved-before-restart\n").await;
    live.wait_for_starts(1).await;
    std::fs::write(
        live.source.join(".envrc"),
        "export GH_TOKEN=synthetic-unapproved-at-restart\n",
    )
    .unwrap();
    let (status, response) = post_restart(live.boot.app.clone(), &live.card).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    live.wait_for_rejected_target(1).await;
    live.assert_project_values_not_persisted(&["synthetic-unapproved-at-restart"])
        .await;
}
