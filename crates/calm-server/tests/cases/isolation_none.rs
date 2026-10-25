use super::*;
use calm_truth::db::RepoRead;

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
            calm_server::test_seams::clean_idle_closed_track_worktrees_for_test(
                b.repo.db(),
                &b.operation_runtime
            )
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

    let (card, worker, worker_track): (String, String, String) = sqlx::query_as(
        "SELECT c.id, ws.id, ws.track_id FROM cards c \
         JOIN worker_sessions ws ON ws.id=c.session_id AND ws.card_id=c.id \
         WHERE c.track_id=?1 AND c.role='planner'",
    )
    .bind(id)
    .fetch_one(b.repo.pool())
    .await
    .unwrap();
    assert_eq!(worker_track, id);
    let mut tx = b.repo.pool().begin().await.unwrap();
    let carrier =
        calm_truth::db::sqlite::conversation::current_input_carrier_tx(&mut tx, &card, &worker)
            .await
            .unwrap();
    assert_eq!(
        carrier.checkpoint(),
        &calm_types::conversation::ConversationInputCheckpoint::empty()
    );
    tx.rollback().await.unwrap();
    let worker_workspace = b
        .repo
        .track_get(&worker_track)
        .await
        .unwrap()
        .unwrap()
        .workspace;
    assert_eq!(worker_workspace, row.workspace);
    assert_eq!(worker_workspace.agent_cwd(), selected.to_str().unwrap());
    let historical_starts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM operations WHERE kind='planner-harness-start'")
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(historical_starts, 0);
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

// Pre-cutover data uses Truth allocation and the released binding shape, never a
// canonical create followed by a forbidden rewrite of its immutable binding.
async fn historical_seed(
    b: &Boot,
    area: &str,
    source: &Path,
    worktree: bool,
    first: bool,
    digest: &str,
) -> Value {
    use calm_server::db::sqlite::*;
    use calm_server::model::{CardRole, NewCard, NewTrack};
    let mut tx = b.repo.pool().begin().await.unwrap();
    let track = track_create_tx(
        &mut tx,
        NewTrack {
            area_id: area.into(),
            title: "borrowed".into(),
            sort: None,
            cwd: source.to_str().unwrap().into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: true,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        },
        None,
        &if worktree {
            TrackWorkspacePlan::AttachedWithTrackWorktree(source.into())
        } else {
            TrackWorkspacePlan::AttachedFromCwd
        },
        None,
        &b.tracks,
    )
    .await
    .unwrap();
    let host = calm_server::acp_planner::config::AcpPlannerHost::unconfigured_scratch().unwrap();
    let planner = calm_server::model::new_id();
    let report = calm_server::model::new_id();
    for (id, kind, role, payload) in [
        (
            &planner,
            "codex",
            CardRole::Planner,
            calm_server::track_creation::planner_harness_card_payload(
                &host,
                None,
                calm_server::session_projection_repo::AgentProvider::Codex,
            ),
        ),
        (
            &report,
            "track-report",
            CardRole::ReportCard,
            serde_json::to_value(calm_types::track_report::TrackReportPayload::initial()).unwrap(),
        ),
    ] {
        card_create_with_id_tx(
            &mut tx,
            id.clone(),
            NewCard {
                title: None,
                track_id: track.id.clone(),
                kind: kind.into(),
                sort: None,
                payload,
            },
            role,
            false,
            &b.roles,
        )
        .await
        .unwrap();
    }
    track_create_idempotency_claim_tx(
        &mut tx,
        area,
        "old-key",
        &TrackCreateBindingClaim {
            track_id: track.id.to_string(),
            planner_card_id: planner,
            report_card_id: report,
            create_request_sha256: digest.into(),
            first_message_sha256: first
                .then(|| "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".into()),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    serde_json::to_value(calm_types::track_wire::TrackWire(track)).unwrap()
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
        let old_payload = json!({"title":"borrowed", "sort":null, "cwd":source,
            "template_id":null, "recipe_id":null, "template_input":null, "attach_folder":true,
            "theme":theme(), "fork_report_from":null});
        let old_digest =
            calm_server::routes::idempotency_key::stable_payload_hash(&old_payload).unwrap();
        let first = historical_seed(&b, &area, &source, true, with_message, &old_digest).await;
        assert_eq!(first["workspace"]["isolation"], "worktree");
        let actual: String = sqlx::query_scalar("SELECT create_request_sha256 FROM track_create_idempotency WHERE area_id=?1 AND idempotency_key='old-key'")
            .bind(&area).fetch_one(b.repo.pool()).await.unwrap();
        assert_eq!(actual.as_bytes(), old_digest.as_bytes());
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
        calm_server::test_seams::clean_idle_closed_track_worktrees_for_test(
            b.repo.db(),
            &b.operation_runtime,
        )
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
    workspace_runtime::recycle::gc_trash_best_effort(
        &b.workspace_root,
        i64::MAX,
        &Default::default(),
    );
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
    let original_card = b.repo.card_get(&card).await.unwrap().unwrap();
    let persisted = b
        .repo
        .track_get(track["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    let workers: Vec<(String, String)> =
        sqlx::query_as("SELECT id,handle_state_json FROM worker_sessions WHERE track_id=?1")
            .bind(persisted.id.as_str())
            .fetch_all(b.repo.pool())
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
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        json!(
            b.repo
                .track_get(persisted.id.as_str())
                .await
                .unwrap()
                .unwrap()
        ),
        json!(persisted)
    );
    let after: Vec<(String, String)> =
        sqlx::query_as("SELECT id,handle_state_json FROM worker_sessions WHERE track_id=?1")
            .bind(persisted.id.as_str())
            .fetch_all(b.repo.pool())
            .await
            .unwrap();
    assert_eq!(after, workers);
    assert_eq!(
        json!(b.repo.card_get(&card).await.unwrap().unwrap()),
        json!(original_card)
    );
    assert_eq!(fingerprint(&source), before);
    let digest = calm_server::routes::idempotency_key::stable_payload_hash(&json!({
        "title":"borrowed", "sort":null, "cwd":source, "isolation":"none",
        "template_id":null, "recipe_id":null, "template_input":null, "attach_folder":true,
        "theme":theme(), "fork_report_from":null
    }))
    .unwrap();
    let old = historical_seed(&b, &area, &source, false, false, &digest).await;
    let card: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(old["id"].as_str().unwrap())
            .fetch_one(b.repo.pool())
            .await
            .unwrap();
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

#[tokio::test]
async fn worktree_admission_refuses_owned_locations_and_symlinks() {
    let b = boot().await;
    let area = create_area(&b, "owned-worktree-admission").await;
    let (_, managed) = managed_track(&b, &area, "managed").await;
    user_repo(&b.workspace_root);
    let system = user_repo(&b.workspace_root.join("system-area"));
    let trash = user_repo(&b.workspace_root.join(".trash"));
    let source = user_repo(&b.tmp.path().join("source"));
    let owner = attached_track(&b, &area, "owner", &source).await;
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
    let managed_sub = managed.join("sub");
    let worktree_sub = worktree.join("sub");
    std::fs::create_dir(&managed_sub).unwrap();
    std::fs::create_dir(&worktree_sub).unwrap();
    // The lexical fence must also refuse a symlink pointing OUT of an owned root.
    let outward = managed.join("outside-alias");
    std::os::unix::fs::symlink(&source, &outward).unwrap();
    for (i, root) in [
        b.workspace_root.clone(),
        managed,
        managed_sub,
        system,
        trash,
        worktree,
        worktree_sub,
    ]
    .iter()
    .enumerate()
    {
        let alias = b.tmp.path().join(format!("worktree-alias-{i}"));
        std::os::unix::fs::symlink(root, &alias).unwrap();
        for cwd in [root, &alias] {
            assert_worktree_refused(&b, &area, cwd).await;
        }
    }
    assert_worktree_refused(&b, &area, &outward).await;
}

async fn assert_worktree_refused(b: &Boot, area: &str, cwd: &Path) {
    let mut body = none_body(area, cwd);
    body["isolation"] = json!("worktree");
    let (status, error) = request(b.app.clone(), "POST", "/api/tracks", Some(body)).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "{}: {error}",
        cwd.display()
    );
    assert!(error.contains("Neige-owned location"), "{error}");
}

#[tokio::test]
async fn worktree_admission_allows_repository_containing_owned_worktrees() {
    let b = boot().await;
    let area = create_area(&b, "repository-worktree-admission").await;
    let source = user_repo(&b.tmp.path().join("source"));
    let first = attached_track(&b, &area, "first", &source).await;
    let alias = b.tmp.path().join("repository-alias");
    std::os::unix::fs::symlink(&source, &alias).unwrap();
    let second = attached_track(&b, &area, "second", &alias).await;
    assert_ne!(first, second);
    for id in [first, second] {
        let track = b.repo.track_get(&id).await.unwrap().unwrap();
        let root = PathBuf::from(track.workspace.worktree.unwrap());
        assert_eq!(
            root,
            workspace::worktree::track_worktree_path_for(&source, &id)
        );
        assert_eq!(std::fs::canonicalize(&root).unwrap(), root);
        assert!(root.join(".git").is_file());
    }
}

async fn assert_redirect_admission(i: usize) {
    let b = boot().await;
    let area = create_area(&b, "redirect-admission").await;
    let (_, managed) = managed_track(&b, &area, "managed").await;
    let owner_source = user_repo(&b.tmp.path().join("owner-source"));
    let owner = attached_track(&b, &area, "owner", &owner_source).await;
    let owned_worktree = PathBuf::from(
        b.repo
            .track_get(&owner)
            .await
            .unwrap()
            .unwrap()
            .workspace
            .worktree
            .unwrap(),
    );
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    let claims: i64 = sqlx::query_scalar("SELECT count(*) FROM area_folders")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    let destinations = [managed, b.workspace_root.clone(), owned_worktree];
    let destination = &destinations[i];
    let source = user_repo(&b.tmp.path().join(format!("redirect-source-{i}")));
    if i == 0 {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&source)
                .args(["config", "core.worktree", destination.to_str().unwrap()])
                .status()
                .unwrap()
                .success()
        );
    } else {
        std::fs::create_dir_all(source.join(".claude")).unwrap();
        std::os::unix::fs::symlink(destination, source.join(".claude/worktrees")).unwrap();
    }
    assert_worktree_refused(&b, &area, &source).await;
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    let after_claims: i64 = sqlx::query_scalar("SELECT count(*) FROM area_folders")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(after_claims, claims);
}

#[tokio::test]
async fn worktree_admission_refuses_redirected_repository_without_claims() {
    assert_redirect_admission(0).await;
}

#[tokio::test]
async fn worktree_admission_refuses_redirected_target_managed_root_without_claims() {
    assert_redirect_admission(1).await;
}

#[tokio::test]
async fn worktree_admission_refuses_redirected_target_owned_worktree_without_claims() {
    assert_redirect_admission(2).await;
}
