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

fn none_body(area: &str, cwd: &Path) -> Value {
    json!({"planner_provider":"codex", "area_id":area, "title":"borrowed", "cwd":cwd,
           "isolation":"none", "attach_folder":true, "theme":theme()})
}

async fn assert_none_refused(b: &Boot, area: &str, cwd: &Path) {
    let (status, body) = request(
        b.app.clone(),
        "POST",
        "/api/tracks",
        Some(none_body(area, cwd)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}: {body}", cwd.display());
    assert!(body.contains("Neige-owned location"), "{body}");
}

#[tokio::test]
async fn isolation_none_http_create_has_no_effect_and_projects_all_track_reads() {
    let b = boot().await;
    let area = create_area(&b, "borrow").await;
    let source = user_repo(&b.tmp.path().join("source"));
    std::fs::create_dir(source.join("sub")).unwrap();
    // A branch namespace collision is irrelevant to none; worktree creation would refuse it.
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&source)
            .args(["branch", "neige"])
            .status()
            .unwrap()
            .success()
    );
    let before = fingerprint(&source);
    let selected = source.join("sub");
    let answer = create_track(&b, none_body(&area, &selected)).await;
    assert_eq!(answer["workspace"]["isolation"], "none");
    assert!(answer["workspace"].get("worktree").is_none());
    let id = answer["id"].as_str().unwrap();
    let row = b.repo.track_get(id).await.unwrap().unwrap();
    assert_eq!(row.workspace.agent_cwd(), selected.to_str().unwrap());
    let events: Vec<String> = sqlx::query_scalar(
        "SELECT payload FROM events WHERE kind='track.updated' AND scope_track=?1",
    )
    .bind(id)
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    assert!(!events.is_empty());
    for payload in events {
        let value: Value = serde_json::from_str(&payload).unwrap();
        assert!(
            value["workspace"].get("isolation").is_none(),
            "persistent event changed: {value}"
        );
    }

    let payload: String = sqlx::query_scalar(
        "SELECT payload_json FROM operations WHERE kind='planner-harness-start'",
    )
    .fetch_one(b.repo.pool())
    .await
    .unwrap();
    let payload: calm_server::operation::planner_harness_start_adapter::PlannerHarnessStartOperationPayload =
        serde_json::from_str(&payload).unwrap();
    assert_eq!(payload.track_id, id);
    assert_eq!(payload.cwd, selected.to_str().unwrap());
    for uri in [
        format!("/api/tracks/{id}"),
        format!("/api/areas/{area}/tracks"),
    ] {
        let (status, text) = request(b.app.clone(), "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let value: Value = serde_json::from_str(&text).unwrap();
        let track = if value.is_array() {
            &value[0]
        } else {
            &value["track"]
        };
        assert_eq!(track["workspace"]["isolation"], "none", "{value}");
    }
    for patch in [json!({}), json!({"title":"borrowed"})] {
        let (status, text) = request(
            b.app.clone(),
            "PATCH",
            &format!("/api/tracks/{id}"),
            Some(patch),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["workspace"]["isolation"], "none", "{value}");
    }
    assert_eq!(fingerprint(&source), before);
    assert_eq!(delete_track(&b, id).await.0, StatusCode::NO_CONTENT);
    assert_eq!(fingerprint(&source), before);
    for isolation in ["worktree", "none"] {
        let body = json!({"planner_provider":"codex", "area_id":area, "theme":theme(), "isolation":isolation});
        assert_eq!(
            request(b.app.clone(), "POST", "/api/tracks", Some(body))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    let plain = b.tmp.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    assert_eq!(
        request(
            b.app.clone(),
            "POST",
            "/api/tracks",
            Some(none_body(&area, &plain))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn isolation_none_owned_locations_and_symlinks_are_refused() {
    let b = boot().await;
    let area = create_area(&b, "owned").await;
    let (_, managed) = managed_track(&b, &area, "managed").await;
    // Make every containing path itself a Git work tree so the overlap guard is the refusal.
    user_repo(b.tmp.path());
    user_repo(&b.workspace_root);
    let system = b.workspace_root.join("system-area");
    let trash = b.workspace_root.join(".trash");
    user_repo(&system);
    user_repo(&trash);
    let source = user_repo(&b.tmp.path().join("source"));
    let wt = attached_track(&b, &area, "owned-worktree", &source).await;
    let worktree = PathBuf::from(
        b.repo
            .track_get(&wt)
            .await
            .unwrap()
            .unwrap()
            .workspace
            .worktree
            .unwrap(),
    );
    std::fs::create_dir(worktree.join("sub")).unwrap();
    let roots = [
        b.tmp.path().to_path_buf(),
        b.workspace_root.clone(),
        system,
        trash,
        managed,
        source,
        worktree.clone(),
        worktree.join("sub"),
    ];
    for (i, root) in roots.iter().enumerate() {
        assert_none_refused(&b, &area, root).await;
        let alias = b.tmp.path().join(format!("alias-{i}"));
        std::os::unix::fs::symlink(root, &alias).unwrap();
        assert_none_refused(&b, &area, &alias).await;
    }
}

async fn teardown_window(area_delete: bool) {
    let b = boot().await;
    let area = create_area(&b, "deleting").await;
    let other = create_area(&b, "other").await;
    let source = user_repo(&b.tmp.path().join("source"));
    let id = attached_track(&b, &area, "worktree", &source).await;
    let path = PathBuf::from(
        b.repo
            .track_get(&id)
            .await
            .unwrap()
            .unwrap()
            .workspace
            .worktree
            .unwrap(),
    );
    std::fs::create_dir(path.join("sub")).unwrap();
    let hook = calm_server::test_seams::WorkspaceSweepHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    calm_server::test_seams::install_workspace_sweep_hook_for_test(&id, hook.clone());
    let app = b.app.clone();
    let uri = if area_delete {
        format!("/api/areas/{area}")
    } else {
        format!("/api/tracks/{id}")
    };
    let deletion = tokio::spawn(async move { request(app, "DELETE", &uri, None).await });
    tokio::time::timeout(std::time::Duration::from_secs(10), hook.entered.notified())
        .await
        .unwrap();
    assert!(
        b.repo.track_get(&id).await.unwrap().is_none(),
        "pause must follow commit"
    );
    assert_eq!(b.teardown_targets.snapshot(), vec![path.clone()]);
    for target_area in [&area, &other] {
        for cwd in [&path, &path.join("sub")] {
            assert_none_refused(&b, target_area, cwd).await;
        }
    }
    hook.release.notify_one();
    let (status, body) = deletion.await.unwrap();
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(b.teardown_targets.snapshot().is_empty());
    assert!(!path.exists());
    user_repo(&path);
    std::fs::create_dir(path.join("sub")).unwrap();
    for cwd in [&path, &path.join("sub")] {
        let target_area = if area_delete { &other } else { &area };
        let track = create_track(&b, none_body(target_area, cwd)).await;
        assert_eq!(track["workspace"]["isolation"], "none");
    }
}

#[tokio::test]
async fn isolation_none_track_delete_commit_to_sweep_window() {
    teardown_window(false).await;
}

#[tokio::test]
async fn isolation_none_area_delete_commit_to_sweep_window() {
    teardown_window(true).await;
}

#[tokio::test]
async fn isolation_none_delete_rollback_removes_registration_and_keeps_live_fence() {
    for area_delete in [false, true] {
        let b = boot().await;
        let area = create_area(&b, "rollback").await;
        let source = user_repo(&b.tmp.path().join("source"));
        let id = attached_track(&b, &area, "owned", &source).await;
        let path = PathBuf::from(
            b.repo
                .track_get(&id)
                .await
                .unwrap()
                .unwrap()
                .workspace
                .worktree
                .unwrap(),
        );
        sqlx::query("CREATE TRIGGER refuse_delete BEFORE DELETE ON tracks BEGIN SELECT RAISE(ABORT, 'fixture rollback'); END")
        .execute(b.repo.pool()).await.unwrap();
        assert_eq!(
            if area_delete {
                delete_area(&b, &area).await.0
            } else {
                delete_track(&b, &id).await.0
            },
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert!(b.teardown_targets.snapshot().is_empty());
        assert!(b.repo.track_get(&id).await.unwrap().is_some());
        assert_none_refused(&b, &area, &path).await;
    }
}

async fn keyed_create(b: &Boot, key: &str, body: Value) -> (StatusCode, Value) {
    let response = b
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tracks")
                .header("content-type", "application/json")
                .header("Idempotency-Key", key)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn isolation_none_pre_upgrade_binding_replays_and_digest_bytes_are_unchanged() {
    for with_message in [false, true] {
        let b = boot().await;
        let area = create_area(&b, "old binding").await;
        let source = user_repo(&b.tmp.path().join("source"));
        let mut body = none_body(&area, &source);
        body.as_object_mut().unwrap().remove("isolation");
        if with_message {
            body["first_message"] = json!("hello");
        }
        let (status, first) = keyed_create(&b, "old-key", body.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{first}");
        assert_eq!(first["workspace"]["isolation"], "worktree");
        let old_payload = json!({"title":"borrowed", "sort":null, "cwd":source,
            "template_id":null, "recipe_id":null, "template_input":null, "attach_folder":true,
            "theme":theme(), "fork_report_from":null});
        let old_digest =
            calm_server::routes::idempotency_key::stable_payload_hash(&old_payload).unwrap();
        let actual: String = sqlx::query_scalar("SELECT create_request_sha256 FROM track_create_idempotency WHERE area_id=?1 AND idempotency_key='old-key'")
            .bind(&area).fetch_one(b.repo.pool()).await.unwrap();
        assert_eq!(actual.as_bytes(), old_digest.as_bytes());
        // Freeze the pre-upgrade value explicitly, independently of the new request decoder.
        sqlx::query(
            "UPDATE track_create_idempotency SET create_request_sha256=?1 WHERE area_id=?2",
        )
        .bind(&old_digest)
        .bind(&area)
        .execute(b.repo.pool())
        .await
        .unwrap();
        for explicit in [false, true] {
            let mut replay = body.clone();
            if explicit {
                replay["isolation"] = json!("worktree");
            }
            let (status, answer) = keyed_create(&b, "old-key", replay).await;
            assert_eq!(status, StatusCode::CREATED, "{answer}");
            assert_eq!(answer["id"], first["id"]);
        }
        body["isolation"] = json!("none");
        assert_eq!(
            keyed_create(&b, "old-key", body).await.0,
            StatusCode::CONFLICT
        );
    }
}

#[tokio::test]
async fn isolation_none_replay_after_directory_vanishes_keeps_selection_without_materializing() {
    for with_message in [false, true] {
        let b = boot().await;
        let area = create_area(&b, "replay").await;
        let source = user_repo(&b.tmp.path().join("source"));
        let mut body = none_body(&area, &source);
        if with_message {
            body["first_message"] = json!("hello");
        }
        let (status, first) = keyed_create(&b, "direct-key", body.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{first}");
        std::fs::remove_dir_all(&source).unwrap();
        let (status, answer) = keyed_create(&b, "direct-key", body).await;
        assert_eq!(status, StatusCode::CREATED, "{answer}");
        assert_eq!(answer, first);
        assert!(!source.exists());
    }
}

#[tokio::test]
async fn isolation_none_sibling_lifecycle_preserves_admitted_subdirectory() {
    let b = boot().await;
    let area = create_area(&b, "owners").await;
    let source = user_repo(&b.tmp.path().join("source"));
    let (status, body) = request(
        b.app.clone(),
        "POST",
        &format!("/api/areas/{area}/folders"),
        Some(json!({"path":source})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let borrowed = source.join("borrowed");
    std::fs::create_dir(&borrowed).unwrap();
    std::fs::write(borrowed.join(".gitignore"), b"ignored\n").unwrap();
    std::fs::write(borrowed.join("ignored"), b"ignored\0").unwrap();
    std::fs::write(borrowed.join("untracked"), b"untracked\0").unwrap();
    let before = fingerprint(&borrowed);
    create_track(&b, none_body(&area, &borrowed)).await;
    for managed in [false, true] {
        let owner = if managed {
            managed_track(&b, &area, "owned").await.0
        } else {
            attached_track(&b, &area, "owned", &source).await
        };
        let (status, body) = request(
            b.app.clone(),
            "PATCH",
            &format!("/api/tracks/{owner}"),
            Some(json!({"closed":true})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        calm_server::test_seams::clean_idle_closed_track_worktrees_for_test(b.repo.pool())
            .await
            .unwrap();
        assert_eq!(fingerprint(&borrowed), before);
        assert_eq!(delete_track(&b, &owner).await.0, StatusCode::NO_CONTENT);
        assert_eq!(fingerprint(&borrowed), before);
    }
    // The worktree created after none is admitted is explicitly outside guarantee G.
    let root_none = create_track(&b, none_body(&area, &source)).await;
    let owner = attached_track(&b, &area, "later worktree", &source).await;
    let worktree = PathBuf::from(
        b.repo
            .track_get(&owner)
            .await
            .unwrap()
            .unwrap()
            .workspace
            .worktree
            .unwrap(),
    );
    assert!(worktree.exists());
    assert_eq!(delete_track(&b, &owner).await.0, StatusCode::NO_CONTENT);
    assert!(!worktree.exists());
    assert!(
        !Command::new("git")
            .arg("-C")
            .arg(&source)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/neige/track-{owner}")
            ])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(root_none["workspace"]["isolation"], "none");
    assert_eq!(delete_area(&b, &area).await.0, StatusCode::NO_CONTENT);
    workspace_runtime::recycle::gc_trash_best_effort(&b.workspace_root, i64::MAX);
    assert_eq!(fingerprint(&borrowed), before);
}

#[tokio::test]
async fn isolation_none_http_missing_owned_root_under_symlinked_ancestor_is_refused() {
    let b = boot_with_workspace_root(|tmp| {
        let physical = user_repo(&tmp.join("physical"));
        let link = tmp.join("link");
        std::os::unix::fs::symlink(&physical, &link).unwrap();
        link.join("not-created-yet")
    })
    .await;
    let area = create_area(&b, "missing root").await;
    assert!(!b.workspace_root.exists());
    assert_none_refused(&b, &area, &b.tmp.path().join("physical")).await;
    assert!(!b.workspace_root.exists());
}

#[tokio::test]
async fn isolation_none_reset_response_projects_workspace() {
    let b = boot().await;
    let area = create_area(&b, "reset").await;
    let source = user_repo(&b.tmp.path().join("source"));
    let track = create_track(&b, none_body(&area, &source)).await;
    let card: String = sqlx::query_scalar("SELECT card_id FROM worker_sessions WHERE track_id=?1")
        .bind(track["id"].as_str().unwrap())
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    let before = fingerprint(&source);
    let (status, body) = request(
        b.app.clone(),
        "POST",
        &format!("/api/cards/{card}/planner/reset"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["track"]["workspace"]["isolation"], "none", "{value}");
    assert_eq!(fingerprint(&source), before);
}
