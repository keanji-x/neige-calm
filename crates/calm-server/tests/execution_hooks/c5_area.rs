//! Area effects must prove the whole captured set safe before moving any directory.
use super::c5_admission::{busy, external_worktree, typed};
use super::*;
use calm_server::test_seams::{PausePoint, install_pause_for_test};

fn pause(point: &str, area: &str) -> PausePoint {
    let p = PausePoint::default();
    install_pause_for_test(point, area, p.clone());
    p
}
async fn entered(p: &PausePoint) {
    tokio::time::timeout(Duration::from_secs(10), p.entered.notified())
        .await
        .unwrap();
}
async fn admit(f: &Fixture, owner: &str, root: std::path::PathBuf) -> ExecutionId {
    let mut intent = crate::support::execution::occupancy_intent(root);
    intent.metadata.track = exec::TrackId(owner.into());
    let id = intent.metadata.id.clone();
    let mut tx = sqlite::begin_immediate_tx(f.repo.db()).await.unwrap();
    let _ticket = f.engine.admit(&mut tx, intent).await.unwrap();
    tx.commit().await.unwrap();
    id
}
fn branch_exists(track: &Track) -> bool {
    std::process::Command::new("git")
        .args([
            "show-ref",
            "--verify",
            &format!(
                "refs/heads/{}",
                workspace::worktree::track_branch_for(track.id.as_str()).unwrap()
            ),
        ])
        .current_dir(&track.workspace.path)
        .output()
        .unwrap()
        .status
        .success()
}
async fn retained(f: &Fixture, worktree: &Track) {
    assert!(f.root.is_dir());
    assert!(Path::new(worktree.workspace.agent_cwd()).is_dir());
    assert!(branch_exists(worktree));
    assert!(f.repo.area_get(&f.area).await.unwrap().is_some());
    for id in [&f.track, worktree.id.as_str()] {
        assert!(f.repo.track_get(id).await.unwrap().is_some());
    }
}

#[tokio::test]
async fn c5_area_full_proof_member_and_foreign_roots_precede_first_move() {
    // Admit after quiesce to exercise the fresh proof, including owning scope
    // with a root outside every location target.
    for variant in 0..4 {
        let f = Fixture::new().await;
        let worktree = external_worktree(&f).await;
        let p = pause("c5-area-before-proof", &f.area);
        let insert = async {
            entered(&p).await;
            let root = match variant {
                0 => f._temp.path().to_path_buf(),
                1 => f.root.clone(),
                2 => Path::new(worktree.workspace.agent_cwd()).to_path_buf(),
                _ => f.state.workspace_root().join(&f.area),
            };
            let owner = if variant == 0 {
                &f.track
            } else {
                "foreign-owner"
            };
            let id = admit(&f, owner, root).await;
            p.release.notify_one();
            id
        };
        let url = format!("/api/areas/{}", f.area);
        let ((status, body), id) = tokio::join!(f.request("DELETE", &url, None), insert);
        assert_eq!(status, StatusCode::CONFLICT, "variant {variant}: {body}");
        retained(&f, &worktree).await;
        assert!(
            !f.state.workspace_root().join(".trash").exists(),
            "no move, including no move followed by restore"
        );
        assert_eq!(f.desired(&id).await, "run");
        f.stop(&id).await;
    }
}

#[tokio::test]
async fn c5_area_foreign_typed_container_blocks_empty_and_nonempty_area() {
    for empty in [false, true] {
        let mut f = Fixture::new().await;
        typed(&mut f).await;
        let (status, body) = f
            .request(
                "POST",
                "/api/areas",
                Some(json!({"name":"target","color":"#000000"})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let area = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let container = f.state.workspace_root().join(&area);
        std::fs::create_dir_all(&container).unwrap();
        let member = if empty {
            None
        } else {
            let track = calm_server::test_seams::create_track_with_workspace_for_test(
                f.repo.db(),
                NewTrack {
                    area_id: area.clone().into(),
                    title: "member".into(),
                    cwd: String::new(),
                    sort: None,
                    template_id: None,
                    plugin_scope: None,
                    template_input: None,
                    attach_folder: false,
                    theme: RequestTheme::default_dark(),
                },
                &sqlite::TrackWorkspacePlan::ManagedUnder(f.state.workspace_root().to_path_buf()),
                f.repo.track_area_cache(),
            )
            .await
            .unwrap();
            workspace_runtime::selection::materialize_workspace(
                &track.workspace,
                f.state.workspace_root(),
                track.id.as_str(),
            )
            .unwrap();
            Some(track)
        };
        let (status, body) =
            terminal_execution_create::create(&f, container.to_str().unwrap(), "exec sleep 120")
                .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let terminal = terminal_execution_create::terminal(&f, &body).await;
        let id = terminal_execution_create::execution_id(&f, &terminal).await;
        let (status, body) = f
            .request("DELETE", &format!("/api/areas/{area}"), None)
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "empty={empty}: {body}");
        assert!(container.is_dir());
        assert!(f.repo.area_get(&area).await.unwrap().is_some());
        if let Some(track) = member {
            assert!(Path::new(&track.workspace.path).is_dir());
        }
        assert_eq!(f.desired(&id).await, "run");
        f.stop(&id).await;
    }
}

#[tokio::test]
async fn c5_area_delete_lock_blocks_container_terminal_creation() {
    let mut f = Fixture::new().await;
    typed(&mut f).await;
    let target = external_worktree(&f).await;
    // The request owner is outside the deleting Area.
    let old_area = f.area.clone();
    let (status, body) = f
        .request(
            "POST",
            "/api/areas",
            Some(json!({"name":"owner","color":"#000000"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let owner_area = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    sqlx::query("UPDATE tracks SET area_id=? WHERE id=?")
        .bind(owner_area)
        .bind(&f.track)
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    f.repo.track_area_cache().remove(&f.track.clone().into());
    let p = pause("c5-area-before-quiesce", &old_area);
    let create = async {
        entered(&p).await;
        busy(&f, &f.state.workspace_root().join(&old_area)).await;
        p.release.notify_one();
    };
    let url = format!("/api/areas/{old_area}");
    let ((status, body), ()) = tokio::join!(f.request("DELETE", &url, None), create);
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(!Path::new(target.workspace.agent_cwd()).exists());
}

#[tokio::test]
async fn c5_area_writer_recheck_restores_all_despite_foreign_occupancy() {
    let f = Fixture::new().await;
    let worktree = external_worktree(&f).await;
    let hook = calm_server::routes::areas::AreaDeleteCommitHook {
        entered: Arc::default(),
        release: Arc::default(),
        fail_after_release: false,
        panic_after_release: false,
    };
    calm_server::routes::areas::install_area_delete_commit_hook_for_test(&f.area, hook.clone());
    let insert = async {
        tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        assert!(!f.root.exists());
        let id = admit(&f, "foreign-owner", f.state.workspace_root().join(&f.area)).await;
        hook.release.notify_one();
        id
    };
    let url = format!("/api/areas/{}", f.area);
    let ((status, body), id) = tokio::join!(f.request("DELETE", &url, None), insert);
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    retained(&f, &worktree).await;
    assert_eq!(f.desired(&id).await, "run");
    f.stop(&id).await;
}

#[tokio::test]
async fn c5_area_commit_failure_and_panic_restore_all_with_new_occupancy() {
    for panic_after_release in [false, true] {
        let f = Fixture::new().await;
        let worktree = external_worktree(&f).await;
        let hook = calm_server::routes::areas::AreaDeleteCommitHook {
            entered: Arc::default(),
            release: Arc::default(),
            fail_after_release: !panic_after_release,
            panic_after_release,
        };
        calm_server::routes::areas::install_area_delete_commit_hook_for_test(&f.area, hook.clone());
        let insert = async {
            tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
                .await
                .unwrap();
            assert!(!f.root.exists());
            let id = admit(&f, "foreign-owner", f.root.clone()).await;
            hook.release.notify_one();
            id
        };
        let url = format!("/api/areas/{}", f.area);
        let ((status, body), id) = tokio::join!(f.request("DELETE", &url, None), insert);
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        retained(&f, &worktree).await;
        assert_eq!(f.desired(&id).await, "run");
        f.stop(&id).await;
    }
}

#[tokio::test]
async fn c5_area_committed_worktree_before_sweep_admission_is_busy() {
    let mut f = Fixture::new().await;
    typed(&mut f).await;
    let target = external_worktree(&f).await;
    let (status, body) = f
        .request(
            "POST",
            "/api/areas",
            Some(json!({"name":"owner","color":"#000000"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let owner_area = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    sqlx::query("UPDATE tracks SET area_id=? WHERE id=?")
        .bind(owner_area)
        .bind(&f.track)
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    f.repo.track_area_cache().remove(&f.track.clone().into());
    let p = PausePoint::default();
    calm_server::test_seams::install_workspace_sweep_hook_for_test(
        target.id.as_str(),
        calm_server::test_seams::WorkspaceSweepHook {
            entered: p.entered.clone(),
            release: p.release.clone(),
        },
    );
    let create = async {
        entered(&p).await;
        assert!(f.repo.area_get(&f.area).await.unwrap().is_none());
        assert!(
            f.repo
                .track_get(target.id.as_str())
                .await
                .unwrap()
                .is_none()
        );
        busy(&f, Path::new(target.workspace.agent_cwd())).await;
        p.release.notify_one();
    };
    let url = format!("/api/areas/{}", f.area);
    let ((status, body), ()) = tokio::join!(f.request("DELETE", &url, None), create);
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(!Path::new(target.workspace.agent_cwd()).exists());
    assert!(!branch_exists(&target));
}
