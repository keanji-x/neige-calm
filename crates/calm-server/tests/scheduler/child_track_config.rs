//! Child planner declaration admission through Report, scheduler and operation runtime.
use super::*;

#[tokio::test]
async fn child_track_config_invalid_provider_fails_parent_task_readably() {
    let boot = boot().await;
    let mut task = plan_task(
        &boot.track_id,
        "invalid-child-provider",
        TaskKind::Codex,
        &[],
    );
    task.spawn = "sub-track".into();
    task.child_track_json = Some(json!({"provider":"invalid-provider"}).to_string());
    let task_id = task.id.clone();
    seed_projected_task(&boot, task).await;
    let adapter = Arc::new(ChildTrackAdapter::new(
        Arc::new(calm_server::acp_planner::config::AcpPlannerHost::unconfigured_scratch().unwrap()),
        boot.card_role_cache.clone(),
        boot.track_area_cache.clone(),
        child_track_workspace_root(),
        std::path::PathBuf::from("/unused-objective-checkouts"),
        workspace_runtime::sweep::TeardownTargets::default(),
    )) as Arc<dyn ProviderAdapter>;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![adapter]);
    scheduler.sweep_all().await;
    let task = boot.repo.task_get(&task_id).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Failed);
    let detail = task.status_detail.unwrap();
    assert!(detail.contains("child_track.provider"), "{detail}");
}

/// A bound parent must have a committed HEAD, as production child preparation requires.
pub(super) async fn boot_git_parent() -> (Boot, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        &["init", "-q"][..],
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "parent",
        ][..],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let boot = boot_at_workspace(dir.path().to_str().unwrap()).await;
    (boot, dir)
}
