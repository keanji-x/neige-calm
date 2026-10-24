use super::*;

#[tokio::test]
async fn git_worktree_remove_executes_and_records_success() {
    let _lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let results = short_tempdir("gfr").unwrap();
    let _trusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", PLUGIN_ID);
    let _results = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results.path());
    let fx = boot_fixture().await;
    let target = fx._tmp.path().join("extra");
    let added = call_tool(
        &fx,
        30,
        WORKTREE_TOOL,
        json!({"target":target,"branch":"extra"}),
    )
    .await;
    assert_eq!(added["result"]["isError"], false, "{added:#}");
    assert!(target.exists());
    let removed = call_tool(
        &fx,
        31,
        "plugin_gitforge_git_worktree_remove",
        json!({"target":target,"idem":"remove-1"}),
    )
    .await;
    assert_eq!(removed["result"]["isError"], false, "{removed:#}");
    assert!(!target.exists());
    let rows = event_rows(&fx.repo, "worktree.removed").await;
    assert_eq!(rows.len(), 1);
    assert_worktree_event(
        &rows[0],
        &fx.track_id,
        &fx.card_id,
        target.to_str().unwrap(),
    );
    run_git(&fx.lease_abs, ["show-ref", "--verify", "refs/heads/extra"]);
    fx.plugin_host.stop(PLUGIN_ID).await.unwrap();
}

#[tokio::test]
async fn git_worktree_remove_idempotency_generations_and_failure_receipts() {
    let _lock = FORGE_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let results = short_tempdir("gfr").unwrap();
    let _trusted = EnvGuard::set("NEIGE_TRUSTED_FORGE_PLUGINS", PLUGIN_ID);
    let _results = EnvGuard::set("NEIGE_FORGE_RESULTS_DIR", results.path());
    let fx = boot_fixture().await;
    let target = fx._tmp.path().join("extra");
    for generation in [1, 2] {
        let added = call_tool(&fx, 40, WORKTREE_TOOL, json!({"target":target,"branch":format!("extra-{generation}"),"idem":format!("add-{generation}")})).await;
        assert_eq!(added["result"]["isError"], false, "{added:#}");
        assert!(target.exists());
        std::fs::write(target.join("untracked"), "keep").unwrap();
        let refused = call_tool(
            &fx,
            41,
            "plugin_gitforge_git_worktree_remove",
            json!({"target":target,"idem":format!("dirty-{generation}")}),
        )
        .await;
        assert_eq!(refused["result"]["isError"], true, "{refused:#}");
        assert_eq!(
            event_rows(&fx.repo, "worktree.removed").await.len(),
            generation - 1
        );
        assert_eq!(
            std::fs::read_to_string(target.join("untracked")).unwrap(),
            "keep"
        );
        std::fs::remove_file(target.join("untracked")).unwrap();
        let args = json!({"target":target,"idem":format!("remove-{generation}")});
        for _ in 0..2 {
            let removed =
                call_tool(&fx, 42, "plugin_gitforge_git_worktree_remove", args.clone()).await;
            assert_eq!(removed["result"]["isError"], false, "{removed:#}");
            assert!(!target.exists());
        }
        assert_eq!(
            event_rows(&fx.repo, "worktree.removed").await.len(),
            generation
        );
        run_git(
            &fx.lease_abs,
            [
                "show-ref",
                "--verify",
                &format!("refs/heads/extra-{generation}"),
            ],
        );
    }
    fx.plugin_host.stop(PLUGIN_ID).await.unwrap();
}
