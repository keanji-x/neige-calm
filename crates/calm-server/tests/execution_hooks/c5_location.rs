use super::*;

#[tokio::test]
async fn c5_location_occupancy_foreign_execution_keeps_track_without_stop() {
    for variant in 0..3 {
        let f = Fixture::new().await;
        let root = match variant {
            0 => f.root.clone(),
            1 => {
                let child = f.root.join("child");
                std::fs::create_dir(&child).unwrap();
                child
            }
            _ => {
                let alias = f._temp.path().join("alias");
                std::os::unix::fs::symlink(&f.root, &alias).unwrap();
                alias
            }
        };
        let mut intent = crate::support::execution::occupancy_intent(root);
        intent.metadata.track = exec::TrackId("foreign-owner".into());
        let id = intent.metadata.id.clone();
        let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
        let _ticket = f.engine.admit(&mut tx, intent).await.unwrap();
        tx.commit().await.unwrap();
        let (status, body) = f
            .request("DELETE", &format!("/api/tracks/{}", f.track), None)
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(f.root.is_dir());
        assert!(
            !f.state.workspace_root().join(".trash").exists(),
            "proof must precede the first move"
        );
        assert!(f.repo.track_get(&f.track).await.unwrap().is_some());
        assert_eq!(f.desired(&id).await, "run");
        f.stop(&id).await;
    }
}

#[tokio::test]
async fn c5_admission_foreign_managed_owner_busy_then_succeeds() {
    let mut f = Fixture::new().await;
    wait(|| async { f.engine.available() }).await;
    f.state = f.state.clone().with_terminal_execution(true);
    let owned = f.state.workspace_root().join(&f.area).join("other-track");
    std::fs::create_dir_all(&owned).unwrap();
    let held = calm_server::per_card_lock::lock_key(
        f.state.operation_runtime.track_locks_for_test(),
        "other-track",
    )
    .await;
    let (status, body) =
        terminal_execution_create::create(&f, owned.to_str().unwrap(), "exec sleep 120").await;
    drop(held);
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"],
        "workspace_busy"
    );
    let (status, body) =
        terminal_execution_create::create(&f, owned.to_str().unwrap(), "exec sleep 120").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let terminal = terminal_execution_create::terminal(&f, &body).await;
    let id = terminal_execution_create::execution_id(&f, &terminal).await;
    f.stop(&id).await;
}

#[tokio::test]
async fn c5_repoint_internal_trash_refuses_before_lifecycle() {
    let f = Fixture::new().await;
    let target = f.state.workspace_root().join(".trash/retired");
    std::fs::create_dir_all(&target).unwrap();
    git(&target, &["init", "-q"]);
    let before = f.repo.track_get(&f.track).await.unwrap().unwrap().workspace;
    let (status, body) = f
        .request(
            "PATCH",
            &format!("/api/tracks/{}", f.track),
            Some(json!({"workspace":{"kind":"attached", "path":target,"attach_folder":true}})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        f.repo.track_get(&f.track).await.unwrap().unwrap().workspace,
        before
    );
    assert!(f.root.is_dir());
}

#[tokio::test]
async fn c5_location_repoint_foreign_without_stop() {
    let f = Fixture::new().await;
    let mut intent = crate::support::execution::occupancy_intent(f.root.clone());
    intent.metadata.track = exec::TrackId("foreign-owner".into());
    let id = intent.metadata.id.clone();
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    let _ticket = f.engine.admit(&mut tx, intent).await.unwrap();
    tx.commit().await.unwrap();
    let target = f._temp.path().join("attached");
    std::fs::create_dir(&target).unwrap();
    git(&target, &["init", "-q"]);
    let before = f.repo.track_get(&f.track).await.unwrap().unwrap().workspace;
    let (status, body) = f
        .request(
            "PATCH",
            &format!("/api/tracks/{}", f.track),
            Some(json!({"workspace":{"kind":"attached","path":target,"attach_folder":true}})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        f.repo.track_get(&f.track).await.unwrap().unwrap().workspace,
        before
    );
    assert!(f.root.is_dir());
    assert_eq!(f.desired(&id).await, "run");
    f.stop(&id).await;
}

#[tokio::test]
async fn c5_location_malformed_roots_fail_closed_delete_and_repoint() {
    for source in [json!({}), json!({"root":null}), json!({"root":42})] {
        let f = Fixture::new().await;
        let id = calm_server::model::new_id();
        sqlx::query("INSERT INTO workspace_executions \
            (execution_id,track_id,alias,intent_json,secrets_json,desired) VALUES (?,?,?,?,?,'run')")
            .bind(&id).bind("foreign-owner").bind(format!("bad:{id}"))
            .bind(json!({"source":source}).to_string()).bind("{}").execute(f.repo.db().writer_pool()).await.unwrap();
        let target = f._temp.path().join("attached");
        std::fs::create_dir(&target).unwrap();
        git(&target, &["init", "-q"]);
        for (method, payload) in [
            (
                "PATCH",
                Some(json!({"workspace":{"kind":"attached","path":target,"attach_folder":true}})),
            ),
            ("DELETE", None),
        ] {
            let (status, body) = f
                .request(method, &format!("/api/tracks/{}", f.track), payload)
                .await;
            assert_eq!(status, StatusCode::CONFLICT, "{body}");
            assert!(body.contains(&id), "{body}");
            assert!(f.root.is_dir());
            assert!(f.repo.track_get(&f.track).await.unwrap().is_some());
            assert!(!f.state.workspace_root().join(".trash").exists());
        }
    }
}

#[tokio::test]
async fn c5_location_ancestor_root_does_not_block_delete() {
    let f = Fixture::new().await;
    let mut intent =
        crate::support::execution::occupancy_intent(f.state.workspace_root().to_path_buf());
    intent.metadata.track = exec::TrackId("foreign-owner".into());
    let id = intent.metadata.id.clone();
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    let _ticket = f.engine.admit(&mut tx, intent).await.unwrap();
    tx.commit().await.unwrap();
    let (status, body) = f
        .request("DELETE", &format!("/api/tracks/{}", f.track), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(f.desired(&id).await, "run");
    f.stop(&id).await;
}

#[tokio::test]
async fn c5_track_positive_proof_after_quiesce_precedes_move() {
    let f = Fixture::new().await;
    let pause = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "c5-track-before-proof",
        &f.track,
        pause.clone(),
    );
    let insert = async {
        tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
            .await
            .unwrap();
        let mut intent = crate::support::execution::occupancy_intent(f.root.clone());
        intent.metadata.track = exec::TrackId(f.track.clone());
        let id = intent.metadata.id.clone();
        let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
        let _ticket = f.engine.admit(&mut tx, intent).await.unwrap();
        tx.commit().await.unwrap();
        pause.release.notify_one();
        id
    };
    let url = format!("/api/tracks/{}", f.track);
    let ((status, body), id) = tokio::join!(f.request("DELETE", &url, None), insert);
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(f.root.is_dir());
    assert!(
        !f.state.workspace_root().join(".trash").exists(),
        "positive proof must prevent the move, rather than restore it"
    );
    assert_eq!(f.desired(&id).await, "run");
    f.stop(&id).await;
}

#[tokio::test]
async fn c5_track_writer_rechecks_foreign_location() {
    let f = Fixture::new().await;
    let hook = calm_server::routes::tracks::TrackDeleteCommitHook {
        entered: Arc::default(),
        release: Arc::default(),
        panic_after_release: false,
    };
    calm_server::routes::tracks::install_track_delete_commit_hook_for_test(&f.track, hook.clone());
    let insert = async {
        tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        assert!(!f.root.exists());
        let mut intent = crate::support::execution::occupancy_intent(f.root.clone());
        intent.metadata.track = exec::TrackId("foreign-owner".into());
        let id = intent.metadata.id.clone();
        let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
        let _ticket = f.engine.admit(&mut tx, intent).await.unwrap();
        tx.commit().await.unwrap();
        hook.release.notify_one();
        id
    };
    let url = format!("/api/tracks/{}", f.track);
    let ((status, body), id) = tokio::join!(f.request("DELETE", &url, None), insert);
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(f.root.is_dir());
    assert!(f.repo.track_get(&f.track).await.unwrap().is_some());
    assert_eq!(f.desired(&id).await, "run");
    f.stop(&id).await;
}

#[tokio::test]
async fn c5_repoint_writer_rechecks_foreign_location_without_stop() {
    let f = Fixture::new().await;
    let hook = calm_server::routes::tracks::WorkspaceRepointRaceHook {
        entered: Arc::default(),
        release: Arc::default(),
    };
    calm_server::routes::tracks::install_workspace_repoint_race_hook_for_test(
        &f.track,
        hook.clone(),
    );
    let insert = async {
        tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        let mut intent = crate::support::execution::occupancy_intent(f.root.clone());
        intent.metadata.track = exec::TrackId("foreign-owner".into());
        let id = intent.metadata.id.clone();
        let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
        let _ticket = f.engine.admit(&mut tx, intent).await.unwrap();
        tx.commit().await.unwrap();
        hook.release.notify_one();
        id
    };
    let target = f._temp.path().join("attached");
    std::fs::create_dir(&target).unwrap();
    git(&target, &["init", "-q"]);
    let before = f.repo.track_get(&f.track).await.unwrap().unwrap().workspace;
    let url = format!("/api/tracks/{}", f.track);
    let ((status, body), id) = tokio::join!(
        f.request(
            "PATCH",
            &url,
            Some(json!({"workspace":{"kind":"attached","path":target,"attach_folder":true}}))
        ),
        insert
    );
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        f.repo.track_get(&f.track).await.unwrap().unwrap().workspace,
        before
    );
    assert!(f.root.is_dir());
    assert!(!f.state.workspace_root().join(".trash").exists());
    assert_eq!(f.desired(&id).await, "run");
    f.stop(&id).await;
}

#[tokio::test]
async fn c5_trash_repoint_attached_owner_does_not_pin_retired_directory() {
    let f = Fixture::new().await;
    let target = f._temp.path().join("attached");
    std::fs::create_dir(&target).unwrap();
    git(&target, &["init", "-q"]);
    let (status, body) = f
        .request(
            "PATCH",
            &format!("/api/tracks/{}", f.track),
            Some(json!({"workspace":{"kind":"attached","path":target,"attach_folder":true}})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let trash_root = f.state.workspace_root().join(".trash");
    let trash = std::fs::read_dir(&trash_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let expired = trash_root.join(format!("{}-1", f.track));
    std::fs::rename(trash, &expired).unwrap();
    calm_server::workspace_service::trash::collect_once(
        f.state.workspace_root().to_path_buf(),
        f.state.operation_runtime.clone(),
        f.repo.db().clone(),
    )
    .await;
    assert!(!expired.exists());
    assert!(f.repo.track_get(&f.track).await.unwrap().is_some());
    assert!(target.is_dir());
}
