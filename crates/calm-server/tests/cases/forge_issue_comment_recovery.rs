//! Verify comment recovery through the production boot recovery path.
use super::*;

#[tokio::test]
async fn git_forge_issue_comment_crash_recovers_without_reposting() {
    let _lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let path = short_tempdir("comment-recovery-gh").unwrap();
    write_gh_shim(path.path());
    let _path = EnvGuard::set("PATH", prepend_to_path(path.path()));
    let results = short_tempdir("comment-recovery-results").unwrap();
    let _results = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results.path());
    for outage in [false, true] {
        let fx = boot_fixture().await;
        let state = shim_state_dir(&fx.origin_repo);
        let block = ShimBlock::new(&state, "issue_comment");
        let response = call_tool(&fx, 30, "plugin.dev.neige.git-forge_gh.issue.comment",
            json!({"repo":fx.origin_repo.to_string_lossy(),"issue":810,"body":"Recovery update","idem":"recovery-1"})).await;
        assert_tool_succeeded(&response, "gh.issue.comment");
        let op_id = op_id_from_response(&response);
        wait_for_counter(&state.join("issue_comment_count"), 1).await;
        wait_for_operation_phase(&fx.repo, &op_id, "parked").await;
        let result_path = operation_result_path(&fx.repo, &op_id).await;
        assert_result_files_absent(&result_path);
        mark_parked_artifacts_dead(&fx.repo, &op_id).await;
        mark_workspace_lease_stale_for_boot(&fx.repo, &fx.lease_id).await;
        let failure = state.join("issues/810.comments.fail");
        if outage {
            std::fs::write(&failure, "outage").unwrap();
        }
        let recovery = boot_recovery_runtime(&fx).await;
        let plan = recovery.recover_on_boot().await.unwrap();
        recovery.apply_recovery(plan).await.unwrap();
        let result = wait_for_recovery_result(&recovery, &op_id).await;
        if outage {
            // Unknown is the existing runtime's gate-infra failure, never a repost.
            assert!(
                matches!(result.outcome, OperationOutcome::Failed { .. }),
                "{:?}",
                result.outcome
            );
            assert!(format!("{:?}", result.outcome).contains("gate-infra"));
            assert_eq!(operation_phase(&fx.repo, &op_id).await, "failed");
        } else {
            assert!(
                matches!(result.outcome, OperationOutcome::Succeeded { .. }),
                "{:?}",
                result.outcome
            );
            assert_eq!(operation_phase(&fx.repo, &op_id).await, "succeeded");
        }
        assert_eq!(shim_counter(&state.join("issue_comment_count")), 1);
        block.release();
        wait_for_result_code_file(&result_path).await;
        assert_eq!(shim_counter(&state.join("issue_comment_count")), 1);
        fx.plugin_host.stop(PLUGIN_ID).await.unwrap();
    }
}

#[tokio::test]
async fn git_forge_issue_comment_recovery_does_not_borrow_another_caller() {
    let _lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let path = short_tempdir("comment-scope-gh").unwrap();
    write_gh_shim(path.path());
    let _path = EnvGuard::set("PATH", prepend_to_path(path.path()));
    let results = short_tempdir("comment-scope-results").unwrap();
    let _results = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results.path());
    let first = boot_fixture().await;
    let second = boot_fixture().await;
    assert_ne!(first.track_id, second.track_id);
    assert_ne!(first.worker_card_id, second.worker_card_id);
    let tool = "plugin.dev.neige.git-forge_gh.issue.comment";
    let args = json!({"repo":first.origin_repo.to_string_lossy(),"issue":810,"body":"Identical update","idem":"plan-1"});
    let first_response = call_tool(&first, 30, tool, args.clone()).await;
    assert_tool_succeeded(&first_response, "gh.issue.comment");
    wait_for_operation_phase(
        &first.repo,
        &op_id_from_response(&first_response),
        "succeeded",
    )
    .await;
    let state = shim_state_dir(&first.origin_repo);
    assert_eq!(shim_counter(&state.join("issue_comment_count")), 1);

    // B reaches gh but dies before posting. A's complete comment already exists.
    let block = ShimBlock::new(&state, "issue_comment_before");
    let second_response = call_tool(&second, 31, tool, args).await;
    assert_tool_succeeded(&second_response, "gh.issue.comment");
    let op_id = op_id_from_response(&second_response);
    wait_for_operation_phase(&second.repo, &op_id, "parked").await;
    let process = parked_process_group_guard(&second.repo, &op_id).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !state.join("issue_comment_before_started").exists() {
        assert!(
            Instant::now() < deadline,
            "second command never reached the before-post fence"
        );
        sleep(Duration::from_millis(20)).await;
    }
    let result_path = operation_result_path(&second.repo, &op_id).await;
    assert_result_files_absent(&result_path);
    mark_parked_artifacts_dead(&second.repo, &op_id).await;
    mark_workspace_lease_stale_for_boot(&second.repo, &second.lease_id).await;
    let recovery = boot_recovery_runtime(&second).await;
    let plan = recovery.recover_on_boot().await.unwrap();
    recovery.apply_recovery(plan).await.unwrap();
    let result = wait_for_recovery_result(&recovery, &op_id).await;
    assert!(
        matches!(result.outcome, OperationOutcome::Failed { ref last_error_class, .. }
        if last_error_class.as_deref() == Some("action-not-landed")),
        "B cannot borrow A's landing: {:?}",
        result.outcome
    );
    assert_eq!(shim_counter(&state.join("issue_comment_count")), 1);
    drop(process);
    block.release();
    first.plugin_host.stop(PLUGIN_ID).await.unwrap();
    second.plugin_host.stop(PLUGIN_ID).await.unwrap();
}
