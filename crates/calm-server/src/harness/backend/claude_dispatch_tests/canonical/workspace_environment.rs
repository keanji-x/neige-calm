//! Project evaluation belongs to the same live canonical source as the native user line.
use super::*;

fn authorize_project(fx: &Fixture) -> std::path::PathBuf {
    std::fs::write(
        fx.dir.path().join(".envrc"),
        "printf evaluated >> \"$PWD/evaluation-marker\"\n",
    )
    .unwrap();
    assert!(
        std::process::Command::new("direnv")
            .arg("allow")
            .arg(fx.dir.path())
            .env_clear()
            .envs(crate::workspace_service::environment::directory_base_environment())
            .output()
            .unwrap()
            .status
            .success()
    );
    fx.dir.path().join("evaluation-marker")
}

#[tokio::test]
async fn canonical_claude_project_eval_rechecks_source_after_local_resource_wait() {
    let fx = Arc::new(Fixture::new("exit").await);
    let marker = authorize_project(&fx);
    let source = crate::workspace_service::environment::source_for_track(
        fx.owner.repo.as_ref(),
        &fx.owner.track,
        fx.dir.path(),
    )
    .await
    .unwrap();
    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "workspace-environment-before-ready",
        source.to_str().unwrap(),
        pause.clone(),
    );
    let dispatch = tokio::spawn({
        let fx = fx.clone();
        async move { fx.dispatch().await }
    });
    let entered = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()).await;
    if entered.is_ok() {
        fx.auth.sessions.remove(&fx.login).unwrap();
    }
    // Always unblock the resource, including when an earlier preparation failed.
    pause.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(15), dispatch)
        .await
        .unwrap()
        .unwrap();
    fx.session.shutdown().await.unwrap();
    entered.expect("canonical dispatch must reach the actual project execution resource");
    if let Ok(report) = result {
        assert!(matches!(
            report.report.delivery(),
            InputDelivery::NotDispatched(_)
        ));
    }
    fx.no_stdin();
    assert!(
        !marker.exists(),
        "revoked canonical source evaluated .envrc before its native writer refused"
    );
    assert_eq!(fx.session.user_line_writes_for_test(), 0);
    assert!(matches!(
        fx.checkpoint().await.attempts[0].delivery,
        InputDeliveryFact::NotDispatched { .. }
    ));
}

#[tokio::test]
async fn canonical_claude_live_source_evaluates_project_and_completes_first_turn() {
    let fx = Fixture::new("exit").await;
    let marker = authorize_project(&fx);
    let report = fx.dispatch().await.unwrap();
    assert!(matches!(
        report.report.delivery(),
        InputDelivery::Unknown(_)
    ));
    fx.wait_end().await;
    fx.session.shutdown().await.unwrap();
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "evaluated");
    assert!(fx.dir.path().join("stdin").exists());
    assert_eq!(fx.session.user_line_writes_for_test(), 1);
}

#[tokio::test]
async fn canonical_claude_rewind_control_keeps_its_operator_environment_without_project_eval() {
    let fx = Fixture::new("exit").await;
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    let before = std::fs::read(fx.dir.path().join("stdin")).unwrap();
    let marker = authorize_project(&fx);
    let outcome = fx
        .session
        .prepare_edit_for_test(&fx.owner.thread)
        .await
        .unwrap();
    fx.session.shutdown().await.unwrap();
    assert!(matches!(
        outcome,
        provider::control::EditPreparation::Prepared { .. }
    ));
    assert!(
        !marker.exists(),
        "rewind dry run owns no project execution grant"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("stdin")).unwrap(),
        before,
        "a control dry run sends no user line"
    );
    let argv = std::fs::read_to_string(fx.dir.path().join("check-argv")).unwrap();
    assert!(argv.lines().any(|arg| arg == "--resume"));
    assert!(argv.lines().any(|arg| arg == fx.owner.thread));
    assert!(!argv.lines().any(|arg| arg == "--mcp-config"));
    assert_eq!(fx.session.user_line_writes_for_test(), 1);
}
