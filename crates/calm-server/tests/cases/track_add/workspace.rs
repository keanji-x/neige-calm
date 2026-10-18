use super::*;

#[tokio::test]
async fn rebuild_explicit_workspace_claims_and_materialized_agent_cwd() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("workspaces").await;
    let first = crate::support::git_helpers::attached_repo_fixture("rebuild-first");
    let second = crate::support::git_helpers::attached_repo_fixture("rebuild-second");
    boot.repo
        .area_folder_create(&boot.area_id, &first)
        .await
        .unwrap();
    for (index, (cwd, attach)) in [(&first, false), (&second, true)].into_iter().enumerate() {
        let mut args = boot.args(&format!("workspace-{index}"));
        args["cwd"] = json!(cwd);
        args["attach_folder"] = json!(attach);
        let out = boot.add(&planner, args.clone()).await.unwrap();
        let id = out["track_id"].as_str().unwrap();
        let track = boot.repo.track_get(id).await.unwrap().unwrap();
        assert_eq!(track.workspace.path, *cwd);
        assert_ne!(track.workspace.agent_cwd(), cwd);
        assert!(std::path::Path::new(track.workspace.agent_cwd()).is_dir());
        let payloads: Vec<String> = sqlx::query_scalar(
            "SELECT payload_json FROM operations WHERE kind='planner-harness-start'",
        )
        .fetch_all(boot.repo.pool())
        .await
        .unwrap();
        let payload = payloads
            .iter()
            .map(|s| serde_json::from_str::<Value>(s).unwrap())
            .find(|p| p["first_message"] == args["text"])
            .unwrap();
        // Harness admission persists the materialized worktree, not the historical repository.
        assert!(
            payload.to_string().contains(track.workspace.agent_cwd()),
            "{payload}"
        );
        assert_eq!(boot.add(&planner, args.clone()).await.unwrap(), out);
        args["attach_folder"] = json!(!attach);
        assert_eq!(boot.add(&planner, args).await.unwrap_err().code, -32409);
        assert_eq!(boot.deliveries(id).await, 1);
    }
    let folders = boot.repo.area_folders_by_area(&boot.area_id).await.unwrap();
    assert_eq!(folders.len(), 2);
}

#[tokio::test]
async fn rebuild_workspace_refusals_leave_no_track_key_or_claim() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("workspace-errors").await;
    let unclaimed = crate::support::git_helpers::attached_repo_fixture("rebuild-unclaimed");
    let conflict = crate::support::git_helpers::attached_repo_fixture("rebuild-other-area");
    let other = boot
        .repo
        .area_create(
            LEGACY_OWNER_USER_ID,
            NewArea {
                name: "other".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let claim = boot
        .repo
        .area_folder_create(other.id.as_str(), &conflict)
        .await
        .unwrap();
    let before = boot.track_count().await;
    for (index, (cwd, attach, cross)) in [
        (unclaimed.as_str(), false, false),
        (conflict.as_str(), false, false),
        (conflict.as_str(), true, false),
        (conflict.as_str(), true, true),
        ("/this-path-does-not-exist-rebuild", true, false),
        (boot._tmp.path().to_str().unwrap(), true, false),
    ]
    .into_iter()
    .enumerate()
    {
        let mut args = boot.args(&format!("refused-{index}"));
        args["cwd"] = json!(cwd);
        args["attach_folder"] = json!(attach);
        if cross {
            args["allow_cross_area_cwd"] = json!({"area_id":other.id,"folder_id":claim.id});
        }
        assert!(boot.add(&planner, args).await.is_err(), "{cwd} / {attach}");
        assert_eq!(boot.track_count().await, before);
        assert_eq!(
            boot.count("SELECT COUNT(*) FROM track_create_idempotency")
                .await,
            0
        );
        assert!(
            boot.repo
                .area_folders_by_area(&boot.area_id)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            boot.repo
                .area_folders_by_area(other.id.as_str())
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
