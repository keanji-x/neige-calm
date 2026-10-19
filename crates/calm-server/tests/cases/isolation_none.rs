use super::*;

async fn direct_track(b: &Boot, area: &str, path: &Path) -> String {
    b.repo
        .track_create(calm_server::model::NewTrack {
            area_id: area.to_string().into(),
            title: "direct".into(),
            sort: None,
            cwd: path.to_str().unwrap().into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap()
        .id
        .to_string()
}

#[tokio::test]
async fn isolation_none_track_area_delete_and_clean_preserve_source() {
    let b = boot().await;
    let source = user_repo(&b.tmp.path().join("source"));
    std::fs::write(source.join("untracked"), b"user bytes\0").unwrap();
    std::fs::write(source.join(".gitignore"), b"ignored\n").unwrap();
    std::fs::write(source.join("ignored"), b"ignored bytes\0").unwrap();
    let before = fingerprint(&source);
    for area_delete in [false, true] {
        let area = create_area(&b, "direct").await;
        let track = direct_track(&b, &area, &source).await;
        sqlx::query("UPDATE tracks SET closed_at=123 WHERE id=?1")
            .bind(&track)
            .execute(b.repo.pool())
            .await
            .unwrap();
        assert_eq!(
            calm_server::test_seams::clean_idle_closed_track_worktrees_for_test(b.repo.pool())
                .await
                .unwrap(),
            0
        );
        assert_eq!(fingerprint(&source), before);
        let (status, body) = if area_delete {
            delete_area(&b, &area).await
        } else {
            delete_track(&b, &track).await
        };
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert_eq!(fingerprint(&source), before);
        assert!(!source.join(".claude/worktrees").exists());
    }
}

#[tokio::test]
async fn isolation_none_area_compensation_preserves_source() {
    let b = boot().await;
    let area = create_area(&b, "compensate").await;
    let source = user_repo(&b.tmp.path().join("source"));
    std::fs::write(source.join("untracked"), b"keep").unwrap();
    std::fs::write(source.join(".gitignore"), b"ignored\n").unwrap();
    std::fs::write(source.join("ignored"), b"keep ignored").unwrap();
    let track = direct_track(&b, &area, &source).await;
    let (_, managed) = managed_track(&b, &area, "owned").await;
    let before = fingerprint(&source);
    let hook = calm_server::routes::areas::AreaDeleteCommitHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
        fail_after_release: true,
        panic_after_release: false,
    };
    calm_server::routes::areas::install_area_delete_commit_hook_for_test(&area, hook.clone());
    hook.release.notify_one();
    assert_eq!(
        delete_area(&b, &area).await.0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(b.repo.track_get(&track).await.unwrap().is_some());
    assert!(managed.is_dir());
    assert_eq!(fingerprint(&source), before);
    assert!(!source.join(".claude/worktrees").exists());
}

#[tokio::test]
async fn isolation_none_historical_lease_cleans_only_owned_refs() {
    let b = boot().await;
    let area = create_area(&b, "old lease").await;
    let source = user_repo(&b.tmp.path().join("source"));
    let track = direct_track(&b, &area, &source).await;
    let sha = head(&source).unwrap();
    sqlx::query(
        "INSERT INTO workspace_leases \
         (lease_id,card_id,track_id,path,state,lease_owner,created_at_ms,updated_at_ms, \
          released_at_ms,base_sha,base_source,canonical_path,git_common_dir) \
         VALUES ('old','old-card',?1,?2,'released','old-owner',1,2,2,?3,'head',?2,?4)",
    )
    .bind(&track)
    .bind(source.to_str().unwrap())
    .bind(&sha)
    .bind(source.join(".git").to_str().unwrap())
    .execute(b.repo.pool())
    .await
    .unwrap();
    let owned = [
        format!("refs/neige/candidates/{track}/card/delivery"),
        format!("refs/neige/gate-runs/{track}/attempt/run"),
    ];
    let other = "refs/neige/candidates/another-track/card/delivery";
    // Snapshot before the two refs: deleting them must restore these exact bytes.
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&source)
            .args(["update-ref", other, &sha])
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(source.join("untracked"), b"keep").unwrap();
    std::fs::write(source.join(".gitignore"), b"ignored\n").unwrap();
    std::fs::write(source.join("ignored"), b"keep ignored").unwrap();
    for name in &owned {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&source)
                .args(["update-ref", name, &sha])
                .status()
                .unwrap()
                .success()
        );
    }
    let mut before = fingerprint(&source);
    before.retain(|path, _| {
        !owned
            .iter()
            .any(|name| path == &PathBuf::from(".git").join(name))
    });
    assert_eq!(delete_track(&b, &track).await.0, StatusCode::NO_CONTENT);
    let after = fingerprint(&source);
    // Git removes empty ref parent directories as well as the refs themselves.
    before.retain(|path, bytes| {
        bytes.is_some()
            || after.contains_key(path)
            || !owned
                .iter()
                .any(|name| PathBuf::from(".git").join(name).starts_with(path))
    });
    assert_eq!(after, before);
    assert!(!source.join(".claude/worktrees").exists());
}
