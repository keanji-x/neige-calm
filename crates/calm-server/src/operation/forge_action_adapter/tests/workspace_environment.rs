use super::*;
use crate::db::RepoOutOfDomain as _;
#[cfg(feature = "fixtures")]
mod admission;

#[tokio::test]
async fn forge_workspace_environment_reaches_action_and_probes_privately() {
    if isolated_forge_environment(
        stringify!(forge_workspace_environment_reaches_action_and_probes_privately),
        &[],
    ) {
        return;
    }
    let fx = forge_runtime_fixture().await;
    let config = fx.cwd.path().join("project-gh-config");
    let certificate = fx.cwd.path().join("synthetic-project-certificate.pem");
    let git_config = fx.cwd.path().join("synthetic-project-gitconfig");
    let multiline = "synthetic-project-multiline\n\\c \\n $(touch injected) ' \"\né\n\n";
    std::fs::create_dir(&config).unwrap();
    std::fs::write(&certificate, "synthetic certificate\n").unwrap();
    std::fs::write(&git_config, "[user]\n\tname = Project Author\n").unwrap();
    std::fs::write(
        fx.cwd.path().join(".envrc"),
        format!(
            "export GH_TOKEN=synthetic-forge-project\n\
             export GH_HOST=project.example\n\
             export GH_CONFIG_DIR={}\n\
             export SSL_CERT_FILE={}\n\
             export GIT_CONFIG_GLOBAL={}\n\
             export GIT_AUTHOR_NAME=synthetic-project-author\n\
             export GIT_AUTHOR_EMAIL=synthetic-project-author@example.invalid\n\
             export GIT_COMMITTER_NAME=synthetic-project-committer\n\
             export GIT_COMMITTER_EMAIL=synthetic-project-committer@example.invalid\n\
             export FORGE_PROJECT_VALUE={}\n\
             export FORGE_PROJECT_EMPTY=''\n\
             unset LC_ALL\n\
             export NEIGE_FORGE_RESULT_PATH=/synthetic-forged-result\n\
             export NEIGE_PROJECT_FAKE=synthetic-forged-control\n",
            shell_quote(config.to_str().unwrap()),
            shell_quote(certificate.to_str().unwrap()),
            shell_quote(git_config.to_str().unwrap()),
            shell_quote(multiline),
        ),
    )
    .unwrap();
    allow_environment(fx.cwd.path());
    // Only the external command observes its own environment. Admission, both
    // recovery probes and their private stdin releases use the real runtime.
    let observe = r#"printf '%s\0' "${GH_TOKEN-unset}" "${GH_HOST-unset}" "${GH_CONFIG_DIR-unset}" \
        "${SSL_CERT_FILE-unset}" "${GIT_CONFIG_GLOBAL-unset}" \
        "${GIT_AUTHOR_NAME-unset}" "${GIT_AUTHOR_EMAIL-unset}" \
        "${GIT_COMMITTER_NAME-unset}" "${GIT_COMMITTER_EMAIL-unset}" \
        "${FORGE_PROJECT_VALUE-unset}" "${FORGE_PROJECT_EMPTY-unset}" \
        "${LC_ALL-unset}" "${FORGE_SERVICE_ONLY-unset}" "${NEIGE_PROJECT_FAKE-unset}""#;
    let payload = merge_payload(
        &fx,
        shell_probe(&format!("{observe} > action-environment; exit 7")),
        ProbeSpec {
            probe_argv: shell_probe(&format!("{observe} > verdict-environment")),
            output_probe_argv: Some(shell_probe(&format!(
                "{observe} > output-environment\nprintf '%s\\n' '{{\"headRefOid\":\"1111111111111111111111111111111111111111\",\"mergeCommit\":{{\"oid\":\"2222222222222222222222222222222222222222\"}}}}'"
            ))),
        },
    );
    let result = submit_and_wait(&fx, payload, "workspace-full-environment").await;
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));
    let expected = [
        "synthetic-forge-project",
        "project.example",
        config.to_str().unwrap(),
        certificate.to_str().unwrap(),
        git_config.to_str().unwrap(),
        "synthetic-project-author",
        "synthetic-project-author@example.invalid",
        "synthetic-project-committer",
        "synthetic-project-committer@example.invalid",
        multiline,
        "",
        "unset",
        "unset",
        "unset",
        "",
    ]
    .join("\0");
    for phase in ["action", "verdict", "output"] {
        assert_eq!(
            std::fs::read(fx.cwd.path().join(format!("{phase}-environment"))).unwrap(),
            expected.as_bytes(),
            "{phase} must receive the complete project environment"
        );
    }
    assert!(!fx.cwd.path().join("injected").exists());
    assert_eq!(event_count(&fx.repo, "forge.pr.merged").await, 1);
    let persisted: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT intent_json,secrets_json FROM workspace_executions")
            .fetch_all(fx.repo.pool())
            .await
            .unwrap();
    let mut documents = persisted
        .into_iter()
        .flat_map(|(intent, secrets)| [intent, secrets.unwrap_or_default()])
        .collect::<Vec<_>>();
    documents.extend(
        sqlx::query_scalar::<_, String>(
            "SELECT payload_json FROM operations \
             UNION ALL SELECT COALESCE(tx_output_json,'') FROM operations \
             UNION ALL SELECT payload FROM events",
        )
        .fetch_all(fx.repo.pool())
        .await
        .unwrap(),
    );
    for document in documents {
        for private_value in [
            "synthetic-forge-project",
            "synthetic-project-certificate",
            "synthetic-project-gitconfig",
            "synthetic-project-author",
            "synthetic-project-committer",
            "synthetic-project-multiline",
            "synthetic-forged-control",
        ] {
            assert!(
                !document.contains(private_value),
                "project environment values must stay out of persisted metadata"
            );
        }
    }
}

#[tokio::test]
async fn forge_workspace_owner_proxies_reach_children_without_rc_exposure() {
    const SERVICE_PROXY: &str = "http://service-user:service-secret@service-proxy:8282";
    const SETTINGS_PROXY: &str = "http://settings-user:settings-secret@settings-proxy:8383";
    if isolated_forge_environment(
        stringify!(forge_workspace_owner_proxies_reach_children_without_rc_exposure),
        &[
            ("HTTP_PROXY", SERVICE_PROXY),
            ("http_proxy", SERVICE_PROXY),
            ("HTTPS_PROXY", SERVICE_PROXY),
            ("https_proxy", SERVICE_PROXY),
        ],
    ) {
        return;
    }
    let fx = forge_runtime_fixture().await;
    fx.repo
        .settings_upsert("http_proxy", SETTINGS_PROXY)
        .await
        .unwrap();
    std::fs::write(fx.cwd.path().join(".envrc"), r#"
printf '%s\0' "${HTTP_PROXY-unset}" "${http_proxy-unset}" "${HTTPS_PROXY-unset}" "${https_proxy-unset}" > rc-proxies
export HTTP_PROXY=http://project-proxy:8181
export http_proxy=http://project-proxy:8181
export HTTPS_PROXY=http://project-proxy:8181
export https_proxy=http://project-proxy:8181
export ALL_PROXY=http://project-only-proxy:8484
"#).unwrap();
    allow_environment(fx.cwd.path());
    let observe =
        r#"printf '%s\0' "$HTTP_PROXY" "$http_proxy" "$HTTPS_PROXY" "$https_proxy" "$ALL_PROXY""#;
    let payload = merge_payload(
        &fx,
        shell_probe(&format!("{observe} > action-proxies; exit 7")),
        ProbeSpec {
            probe_argv: shell_probe(&format!("{observe} > verdict-proxies")),
            output_probe_argv: Some(shell_probe(&format!(
                "{observe} > output-proxies\nprintf '%s\\n' '{{\"headRefOid\":\"1111111111111111111111111111111111111111\",\"mergeCommit\":{{\"oid\":\"2222222222222222222222222222222222222222\"}}}}'"
            ))),
        },
    );
    let result = submit_and_wait(&fx, payload, "workspace-owner-proxies").await;
    assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));
    let expected = [
        SETTINGS_PROXY,
        SETTINGS_PROXY,
        "http://project-proxy:8181",
        "http://project-proxy:8181",
        "http://project-only-proxy:8484",
        "",
    ]
    .join("\0");
    for phase in ["action", "verdict", "output"] {
        assert_eq!(
            std::fs::read(fx.cwd.path().join(format!("{phase}-proxies"))).unwrap(),
            expected.as_bytes(),
            "{phase} must keep owner proxy precedence"
        );
    }
    assert_eq!(
        std::fs::read(fx.cwd.path().join("rc-proxies")).unwrap(),
        b"unset\0unset\0unset\0unset\0"
    );
    let persisted: Vec<String> = sqlx::query_scalar(
        "SELECT intent_json FROM workspace_executions \
         UNION ALL SELECT COALESCE(secrets_json,'') FROM workspace_executions \
         UNION ALL SELECT payload_json FROM operations \
         UNION ALL SELECT COALESCE(tx_output_json,'') FROM operations \
         UNION ALL SELECT payload FROM events",
    )
    .fetch_all(fx.repo.pool())
    .await
    .unwrap();
    for document in persisted {
        assert!(!document.contains("service-secret"));
        assert!(!document.contains("settings-secret"));
    }
}

#[cfg(feature = "fixtures")]
#[tokio::test]
async fn forge_workspace_release_rechecks_names_and_preserves_token_cache() {
    if isolated_forge_environment(
        stringify!(forge_workspace_release_rechecks_names_and_preserves_token_cache),
        &[],
    ) {
        return;
    }
    for change in ["value", "add", "remove"] {
        let fx = forge_runtime_fixture().await;
        let envrc = fx.cwd.path().join(".envrc");
        let original =
            "export GH_TOKEN=synthetic-token-without-config\nexport FORGE_PROJECT_VALUE=before\n";
        std::fs::write(&envrc, original).unwrap();
        allow_environment(fx.cwd.path());
        let guard =
            crate::per_card_lock::lock_key(fx.runtime.track_locks_for_test(), &fx.track_id).await;
        let payload = no_event_payload(
            &fx,
            "release-keys",
            shell_probe(
                "printf '%s\\0%s' \"${FORGE_PROJECT_VALUE-unset}\" \"${GH_CONFIG_DIR-unset}\" > child-environment",
            ),
            "release-keys",
        );
        let id = submit_forge(&fx, payload, "release-keys").await.unwrap();
        let pause = crate::test_seams::PausePoint::default();
        crate::test_seams::install_pause_for_test("forge-observer-before-read", &id, pause.clone());
        drop(guard);
        tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
            .await
            .expect("admitted Forge child waits for its post-park release");
        assert!(!fx.cwd.path().join("child-environment").exists());
        let updated = match change {
            "value" => original.replace("=before", "=after"),
            "add" => format!("{original}export FORGE_ADDED_AFTER_ADMISSION=added\n"),
            "remove" => "export GH_TOKEN=synthetic-token-without-config\n".into(),
            _ => unreachable!(),
        };
        std::fs::write(&envrc, updated).unwrap();
        allow_environment(fx.cwd.path());
        pause.release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(10), fx.runtime.wait(&id))
            .await
            .expect("release either executes the child or refuses before sending bytes")
            .unwrap();
        if change == "value" {
            assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));
            let observed = std::fs::read(fx.cwd.path().join("child-environment")).unwrap();
            let (value, config) =
                observed.split_at(observed.iter().position(|byte| *byte == 0).unwrap());
            assert_eq!(value, b"after");
            let config = Path::new(std::str::from_utf8(&config[1..]).unwrap());
            assert!(config.is_absolute() && config.is_dir());
            assert!(std::fs::read_dir(config).unwrap().next().is_none());
        } else {
            assert!(matches!(
                result.outcome,
                OperationOutcome::Failed { last_error_class, .. }
                    if last_error_class.as_deref() == Some("gate-infra")
            ));
            assert!(!fx.cwd.path().join("child-environment").exists());
            let op = fx.runtime.repo.get_operation(&id).await.unwrap().unwrap();
            let record = execution::record(&op).unwrap();
            assert_eq!(record.release, execution::Release::Held);
            assert_eq!(
                record.decision,
                execution::Decision::Stopping {
                    reason: execution::StopReason::ReleaseRefused,
                }
            );
        }
    }
}

#[tokio::test]
async fn forge_workspace_project_proxy_bypass_overrides_and_unsets_service_defaults() {
    if isolated_forge_environment(
        stringify!(forge_workspace_project_proxy_bypass_overrides_and_unsets_service_defaults),
        &[
            ("NO_PROXY", "service-upper.invalid"),
            ("no_proxy", "service-lower.invalid"),
        ],
    ) {
        return;
    }
    for (label, declaration, expected) in [
        (
            "override",
            "export NO_PROXY=project-upper.invalid\nexport no_proxy=project-lower.invalid\n",
            "project-upper.invalid\0project-lower.invalid",
        ),
        ("unset", "unset NO_PROXY no_proxy\n", "unset\0unset"),
    ] {
        let fx = forge_runtime_fixture().await;
        std::fs::write(fx.cwd.path().join(".envrc"), declaration).unwrap();
        allow_environment(fx.cwd.path());
        let observe = r#"printf '%s\0%s' "${NO_PROXY-unset}" "${no_proxy-unset}""#;
        let payload = merge_payload(
            &fx,
            shell_probe(&format!("{observe} > action-bypass; exit 7")),
            ProbeSpec {
                probe_argv: shell_probe(&format!("{observe} > verdict-bypass")),
                output_probe_argv: Some(shell_probe(&format!(
                    "{observe} > output-bypass\nprintf '%s\\n' '{{\"headRefOid\":\"1111111111111111111111111111111111111111\",\"mergeCommit\":{{\"oid\":\"2222222222222222222222222222222222222222\"}}}}'"
                ))),
            },
        );
        let result = submit_and_wait(&fx, payload, label).await;
        assert!(matches!(result.outcome, OperationOutcome::Succeeded { .. }));
        for phase in ["action", "verdict", "output"] {
            assert_eq!(
                std::fs::read(fx.cwd.path().join(format!("{phase}-bypass"))).unwrap(),
                expected.as_bytes(),
                "{label}: {phase} must retain the project's proxy bypass decision"
            );
        }
    }
}

fn isolated_forge_environment(test: &str, extra: &[(&str, &str)]) -> bool {
    if std::env::var_os("FORGE_ENV_TEST_CHILD").is_some() {
        return false;
    }
    std::fs::create_dir_all("_local/tmp").unwrap();
    let home = tempfile::tempdir_in("_local/tmp").unwrap();
    let path = std::fs::canonicalize(home.path()).unwrap();
    let name = crate::test_support::exact_test_name(module_path!(), test);
    let output = Command::new(std::env::current_exe().unwrap())
        .args([name.as_str(), "--exact", "--nocapture", "--test-threads=1"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &path)
        .env("XDG_CONFIG_HOME", path.join("config"))
        .env("XDG_DATA_HOME", path.join("data"))
        .env("TMPDIR", &path)
        .env("FORGE_ENV_TEST_CHILD", "1")
        .env("GH_TOKEN", "synthetic-wrong-service-account")
        .env("GH_HOST", "wrong-service.example")
        .env("GH_CONFIG_DIR", path.join("wrong-service-cache"))
        .env("LC_ALL", "C")
        .env("FORGE_SERVICE_ONLY", "synthetic-service-only")
        .env("FORGE_PROJECT_VALUE", "synthetic-wrong-service-value")
        .envs(extra.iter().copied())
        .output()
        .unwrap();
    crate::test_support::assert_isolated_test_passed(&output);
    true
}

fn allow_environment(cwd: &Path) {
    assert!(
        Command::new("direnv")
            .arg("allow")
            .arg(cwd)
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn forge_gh_without_project_identity_refuses_default_cache() {
    let output = Command::new("/bin/sh")
        .args(["-c", &format!("{FORGE_SHELL_PRELUDE}\nneige_gh --version")])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(78));
}

#[test]
fn forge_git_does_not_receive_project_github_configuration() {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all("_local/tmp").unwrap();
    let root = tempfile::tempdir_in("_local/tmp").unwrap();
    let git = root.path().join("git");
    std::fs::write(
        &git,
        "#!/bin/sh\nprintf '%s|%s' \"${GH_TOKEN-unset}\" \"${GH_CONFIG_DIR-unset}\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::env::join_paths([
        std::fs::canonicalize(root.path()).unwrap(),
        std::path::PathBuf::from("/usr/bin"),
        std::path::PathBuf::from("/bin"),
    ])
    .unwrap();
    let output = Command::new("/bin/sh")
        .args(["-c", &format!("{FORGE_SHELL_PRELUDE}\nneige_git status")])
        .env_clear()
        .env("PATH", path)
        .env("GH_TOKEN", "synthetic-project")
        .env("GH_CONFIG_DIR", "/synthetic-project-config")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "unset|unset");
}

#[tokio::test]
async fn forge_v1_wrapper_still_reads_its_released_six_key_payload() {
    use std::{io::Write, process::Stdio};
    std::fs::create_dir_all("_local/tmp").unwrap();
    let root = tempfile::tempdir_in("_local/tmp").unwrap();
    let result = std::fs::canonicalize(root.path())
        .unwrap()
        .join("legacy-result");
    let wrapper = render_forge_wrapper(&shell_probe(
        "printf '%s|%s' \"${GH_TOKEN-unset}\" \"${GH_CONFIG_DIR-unset}\"",
    ))
    .unwrap();
    let payload = credentials::render_payload(|key| {
        (key == "GH_TOKEN").then(|| "synthetic-legacy-token".into())
    })
    .unwrap();
    let mut child = Command::new("/bin/sh")
        .args(["-c", &wrapper])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("NEIGE_FORGE_RESULT_PATH", &result)
        .env("GH_CONFIG_DIR", "legacy-config")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&payload).unwrap();
    assert!(child.wait_with_output().unwrap().status.success());
    let output = read_result_file(&result).await.unwrap();
    assert_eq!(output.stdout, "synthetic-legacy-token|legacy-config");
}
