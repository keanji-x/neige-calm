//! Admission races exercise the production terminal, creation and repoint endpoints.
use super::*;
use calm_server::test_seams::{PausePoint, install_pause_for_test};

pub(super) async fn typed(f: &mut Fixture) {
    wait(|| async { f.engine.available() }).await;
    f.state = f
        .state
        .clone()
        .with_terminal_execution(true)
        .with_objective_checkout_root(f._temp.path().join("objective-checks"));
}
async fn count(f: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM workspace_executions")
        .fetch_one(f.repo.pool())
        .await
        .unwrap()
}
pub(super) async fn busy(f: &Fixture, cwd: &Path) {
    let before = count(f).await;
    let (status, body) = tokio::time::timeout(
        Duration::from_secs(5),
        terminal_execution_create::create(f, cwd.to_str().unwrap(), "true"),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"],
        "workspace_busy"
    );
    assert_eq!(count(f).await, before);
}
async fn succeeds(f: &Fixture, cwd: &Path) {
    let (status, body) = terminal_execution_create::create(f, cwd.to_str().unwrap(), "true").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let term = terminal_execution_create::terminal(f, &body).await;
    let id = terminal_execution_create::execution_id(f, &term).await;
    f.stop(&id).await;
}
fn pause(point: &str, key: &str) -> PausePoint {
    let pause = PausePoint {
        entered: Arc::default(),
        release: Arc::default(),
    };
    install_pause_for_test(point, key, pause.clone());
    pause
}
async fn entered(p: &PausePoint) {
    tokio::time::timeout(Duration::from_secs(10), p.entered.notified())
        .await
        .unwrap();
}
pub(super) async fn external_worktree(f: &Fixture) -> Track {
    let repo = f._temp.path().join(calm_server::model::new_id());
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ],
    );
    calm_server::test_seams::create_track_with_workspace_for_test(
        f.repo.db(),
        NewTrack {
            area_id: f.area.clone().into(),
            title: "foreign worktree".into(),
            cwd: repo.to_str().unwrap().into(),
            sort: None,
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        &sqlite::TrackWorkspacePlan::AttachedWithTrackWorktree(repo),
        f.repo.track_area_cache(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn c5_admission_own_cwd_and_free_foreign_alias_succeed() {
    let mut f = Fixture::new().await;
    typed(&mut f).await;
    succeeds(&f, &f.root).await;
    let other = f.state.workspace_root().join(&f.area).join("other");
    std::fs::create_dir_all(&other).unwrap();
    let alias = f._temp.path().join("alias");
    std::os::unix::fs::symlink(&other, &alias).unwrap();
    let held = calm_server::per_card_lock::lock_key(
        f.state.operation_runtime.track_locks_for_test(),
        "other",
    )
    .await;
    busy(&f, &alias).await;
    drop(held);
    succeeds(&f, &alias).await;
}

#[tokio::test]
async fn c5_admission_area_container_and_busy_release_all_guards() {
    let mut f = Fixture::new().await;
    typed(&mut f).await;
    let area = f.state.workspace_root().join(&f.area);
    let held = calm_server::per_card_lock::lock_key(
        f.state.operation_runtime.area_locks_for_test(),
        &f.area,
    )
    .await;
    busy(&f, &area).await;
    drop(held);
    let other = area.join("other");
    std::fs::create_dir(&other).unwrap();
    let held = calm_server::per_card_lock::lock_key(
        f.state.operation_runtime.track_locks_for_test(),
        "other",
    )
    .await;
    busy(&f, &other).await;
    // The attempted area lock was acquired before the busy foreign track lock.
    let area_guard = calm_server::per_card_lock::try_lock_key(
        f.state.operation_runtime.area_locks_for_test(),
        &f.area,
    )
    .expect("Busy must release every guard");
    drop(area_guard);
    drop(held);
    succeeds(&f, &other).await;
}

#[tokio::test]
async fn c5_admission_live_worktree_physical_owner_busy_then_succeeds() {
    let mut f = Fixture::new().await;
    typed(&mut f).await;
    let other = external_worktree(&f).await;
    let cwd = Path::new(other.workspace.agent_cwd());
    let alias = f._temp.path().join("worktree-alias");
    std::os::unix::fs::symlink(cwd, &alias).unwrap();
    let held = calm_server::per_card_lock::lock_key(
        f.state.operation_runtime.track_locks_for_test(),
        other.id.as_str(),
    )
    .await;
    busy(&f, &alias).await;
    drop(held);
    succeeds(&f, &alias).await;
}

#[tokio::test]
async fn c5_admission_deleted_worktree_before_sweep_is_busy() {
    let mut f = Fixture::new().await;
    typed(&mut f).await;
    let other = external_worktree(&f).await;
    let cwd = std::path::PathBuf::from(other.workspace.agent_cwd());
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    calm_server::test_seams::install_workspace_sweep_hook_for_test(
        other.id.as_str(),
        calm_server::test_seams::WorkspaceSweepHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    let deleting_url = format!("/api/tracks/{}", other.id);
    let deleting = f.request("DELETE", &deleting_url, None);
    tokio::pin!(deleting);
    tokio::select! {
        result = &mut deleting => panic!("delete passed sweep pause: {result:?}"),
        _ = tokio::time::timeout(Duration::from_secs(10), entered.notified()) => {}
    }
    assert!(f.repo.track_get(other.id.as_str()).await.unwrap().is_none());
    assert!(cwd.is_dir());
    busy(&f, &cwd).await;
    release.notify_one();
    let (status, body) = deleting.await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(!cwd.exists());
    let output = std::process::Command::new("git")
        .args([
            "show-ref",
            "--verify",
            &format!(
                "refs/heads/{}",
                workspace::worktree::track_branch_for(other.id.as_str()).unwrap()
            ),
        ])
        .current_dir(&other.workspace.path)
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[tokio::test]
async fn c5_admission_repoint_committed_old_managed_is_busy() {
    let mut f = Fixture::new().await;
    typed(&mut f).await;
    let other = external_worktree(&f).await;
    // Use the worktree track as caller A while B is the fixture's pristine managed track.
    let caller = f.track.clone();
    f.track = other.id.to_string();
    let target = f._temp.path().join("external");
    std::fs::create_dir(&target).unwrap();
    git(&target, &["init", "-q"]);
    let p = pause("c5-repoint-committed", &caller);
    let patch_url = format!("/api/tracks/{caller}");
    let patch = f.request(
        "PATCH",
        &patch_url,
        Some(json!({"workspace":{"kind":"attached","path":target,"attach_folder":true}})),
    );
    tokio::pin!(patch);
    tokio::select! {
        result = &mut patch => panic!("repoint passed pause: {result:?}"),
        _ = entered(&p) => {}
    }
    assert_eq!(
        f.repo
            .track_get(&caller)
            .await
            .unwrap()
            .unwrap()
            .workspace
            .path,
        target.to_str().unwrap()
    );
    busy(&f, &f.root).await;
    p.release.notify_one();
    let (status, body) = patch.await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!f.root.exists());
}

#[tokio::test]
async fn c5_admission_writer_rechecks_changed_physical_owner() {
    let mut f = Fixture::new().await;
    typed(&mut f).await;
    let other = external_worktree(&f).await;
    let external = f._temp.path().join("external-cwd");
    std::fs::create_dir(&external).unwrap();
    let alias = f._temp.path().join("changing-alias");
    std::os::unix::fs::symlink(&external, &alias).unwrap();
    let p = pause("c5-terminal-location-fenced", &f.track);
    let create = terminal_execution_create::create(&f, alias.to_str().unwrap(), "true");
    tokio::pin!(create);
    tokio::select! {
        result = &mut create => panic!("admission passed pause: {result:?}"),
        _ = entered(&p) => {}
    }
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(other.workspace.agent_cwd(), &alias).unwrap();
    p.release.notify_one();
    let (status, body) = create.await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(count(&f).await, 0);
}

#[tokio::test]
async fn c5_internal_cwd_objective_trash_and_physical_aliases_bad_request() {
    for is_objective in [false, true] {
        for alias in [false, true] {
            for typed_terminal in [false, true] {
                let mut f = Fixture::new().await;
                typed(&mut f).await;
                f.state = f.state.clone().with_terminal_execution(typed_terminal);
                let root = if is_objective {
                    f.state.objective_checkout_root().to_path_buf()
                } else {
                    f.state.workspace_root().join(".trash")
                };
                let cwd = root.join("nested");
                std::fs::create_dir_all(&cwd).unwrap();
                let target = if alias {
                    let link = f._temp.path().join("alias");
                    std::os::unix::fs::symlink(&cwd, &link).unwrap();
                    link
                } else {
                    cwd
                };
                let (status, body) =
                    terminal_execution_create::create(&f, target.to_str().unwrap(), "true").await;
                assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
                assert_eq!(count(&f).await, 0);
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM terminals")
                        .fetch_one(f.repo.pool())
                        .await
                        .unwrap(),
                    0
                );
            }
        }
    }
}

#[tokio::test]
async fn c5_repoint_internal_owned_locations_refuse_before_lifecycle_matrix() {
    for objective in [false, true] {
        for nested in [false, true] {
            for alias in [false, true] {
                let mut f = Fixture::new().await;
                typed(&mut f).await;
                let root = if objective {
                    f.state.objective_checkout_root().to_path_buf()
                } else {
                    f.state.workspace_root().join(".trash")
                };
                let root = if nested { root.join("repo") } else { root };
                std::fs::create_dir_all(&root).unwrap();
                git(&root, &["init", "-q"]);
                let target = if alias {
                    let link = f._temp.path().join("alias");
                    std::os::unix::fs::symlink(&root, &link).unwrap();
                    link
                } else {
                    root
                };
                let before = f.repo.track_get(&f.track).await.unwrap().unwrap().workspace;
                let sessions: Vec<(String, String)> =
                    sqlx::query_as("SELECT id,state FROM worker_sessions ORDER BY id")
                        .fetch_all(f.repo.pool())
                        .await
                        .unwrap();
                let cards = f.repo.cards_by_track(&f.track).await.unwrap();
                let trash = f.state.workspace_root().join(".trash");
                let entries = || {
                    if trash.exists() {
                        std::fs::read_dir(&trash).unwrap().count()
                    } else {
                        0
                    }
                };
                let before_entries = entries();
                let (status, body) = f.request("PATCH", &format!("/api/tracks/{}",f.track),
                    Some(json!({"workspace":{"kind":"attached","path":target,"attach_folder":true}}))).await;
                assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
                assert_eq!(
                    f.repo.track_get(&f.track).await.unwrap().unwrap().workspace,
                    before
                );
                assert_eq!(
                    sqlx::query_as::<_, (String, String)>(
                        "SELECT id,state FROM worker_sessions ORDER BY id"
                    )
                    .fetch_all(f.repo.pool())
                    .await
                    .unwrap(),
                    sessions
                );
                assert_eq!(
                    serde_json::to_value(f.repo.cards_by_track(&f.track).await.unwrap()).unwrap(),
                    serde_json::to_value(cards).unwrap()
                );
                assert_eq!(entries(), before_entries);
                assert!(f.root.is_dir());
            }
        }
    }
}

#[tokio::test]
async fn c5_objective_checkout_track_creation_refuses_none_and_worktree() {
    let mut f = Fixture::new().await;
    f.state = f.state.clone().with_shared_codex_appserver(
        calm_server::shared_codex_appserver::SharedCodexAppServer::new_fake_running_with_pending(
            f.repo.clone(),
            None,
        ),
    );
    typed(&mut f).await;
    let root = f.state.objective_checkout_root();
    std::fs::create_dir_all(root.join("child")).unwrap();
    git(root, &["init", "-q"]);
    git(
        root,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ],
    );
    for (cwd, isolation) in [
        (root.to_path_buf(), "none"),
        (root.join("child"), "none"),
        (root.to_path_buf(), "worktree"),
    ] {
        let (status, body) = f.request("POST", "/api/tracks", Some(json!({
            "planner_provider":"codex","area_id":f.area,"title":"refused","cwd":cwd,"isolation":isolation,"attach_folder":true,
            "theme":RequestTheme::default_dark()
        }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("Neige-owned location"), "{body}");
    }
    assert_eq!(f.repo.tracks_by_area(&f.area).await.unwrap().len(), 1);
}

#[tokio::test]
async fn c5_admission_reciprocal_requests_terminate() {
    let mut f = Fixture::new().await;
    typed(&mut f).await;
    let other = external_worktree(&f).await;
    let url_a = format!("/api/tracks/{}/terminal-cards", f.track);
    let url_b = format!("/api/tracks/{}/terminal-cards", other.id);
    let requests = async {
        tokio::join!(
            f.request("POST", &url_a, Some(json!({"cwd":other.workspace.agent_cwd(),"program":"true","theme":RequestTheme::default_dark()}))),
            f.request("POST", &url_b, Some(json!({"cwd":f.root,"program":"true","theme":RequestTheme::default_dark()})))
        )
    };
    let (a, b) = tokio::time::timeout(Duration::from_secs(5), requests)
        .await
        .unwrap();
    for (status, body) in [a, b] {
        assert!(
            [StatusCode::CREATED, StatusCode::CONFLICT].contains(&status),
            "{status} {body}"
        );
        if status == StatusCode::CREATED {
            let term = terminal_execution_create::terminal(&f, &body).await;
            f.stop(&terminal_execution_create::execution_id(&f, &term).await)
                .await;
        } else {
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"],
                "workspace_busy"
            );
        }
    }
}

#[tokio::test]
async fn c5_admission_failed_delete_releases_registration_and_locks() {
    use axum::extract::FromRef;
    let mut f = Fixture::new().await;
    typed(&mut f).await;
    let other = external_worktree(&f).await;
    sqlx::query("CREATE TRIGGER c5_refuse_track_delete BEFORE DELETE ON tracks BEGIN SELECT RAISE(ABORT,'fixture commit failure'); END")
        .execute(f.repo.pool()).await.unwrap();
    let (status, body) = f
        .request("DELETE", &format!("/api/tracks/{}", other.id), None)
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(f.repo.track_get(other.id.as_str()).await.unwrap().is_some());
    assert!(Path::new(other.workspace.agent_cwd()).is_dir());
    let route = calm_server::state::RouteState::from_ref(&f.state);
    assert!(route.teardown_targets.snapshot_with_owners().is_empty());
    succeeds(&f, Path::new(other.workspace.agent_cwd())).await;
}

#[tokio::test]
async fn c5_admission_legacy_busy_does_not_leave_pending_launch() {
    let f = Fixture::new().await;
    let owned = f.state.workspace_root().join(&f.area).join("legacy-owner");
    std::fs::create_dir_all(&owned).unwrap();
    let held = calm_server::per_card_lock::lock_key(
        f.state.operation_runtime.track_locks_for_test(),
        "legacy-owner",
    )
    .await;
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM operations")
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    busy(&f, &owned).await;
    drop(held);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM operations")
            .fetch_one(f.repo.pool())
            .await
            .unwrap(),
        before
    );
}

#[tokio::test]
async fn c5_admission_holds_owner_until_execution_commit() {
    let mut f = Fixture::new().await;
    typed(&mut f).await;
    let other = external_worktree(&f).await;
    let cwd = other.workspace.agent_cwd();
    let p = pause("c5-terminal-location-fenced", &f.track);
    let create = terminal_execution_create::create(&f, cwd, "exec sleep 120");
    tokio::pin!(create);
    tokio::select! {
        result = &mut create => panic!("admission passed pause: {result:?}"),
        _ = entered(&p) => {}
    }
    let owner_wait = pause("key-lock-waiting", other.id.as_str());
    let proof = pause("c5-track-before-proof", other.id.as_str());
    let url = format!("/api/tracks/{}", other.id);
    let deleting = f.request("DELETE", &url, None);
    tokio::pin!(deleting);
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            _ = owner_wait.entered.notified() => {},
            _ = proof.entered.notified() => panic!("owner lifecycle crossed the admission fence"),
            result = &mut deleting => panic!("owner lifecycle completed before admission: {result:?}"),
        }
    }).await.unwrap();
    assert_eq!(count(&f).await, 0);
    owner_wait.release.notify_one();
    p.release.notify_one();
    let (status, body) = create.await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // The committed execution can refuse deletion during preflight, before the
    // positive-proof hook. Allow either refusal point while polling the route.
    proof.release.notify_one();
    let (deleted, detail) = deleting.await;
    assert_eq!(deleted, StatusCode::CONFLICT, "{detail}");
    assert!(Path::new(cwd).is_dir());
    let term = terminal_execution_create::terminal(&f, &body).await;
    let id = terminal_execution_create::execution_id(&f, &term).await;
    assert_eq!(f.desired(&id).await, "run");
    f.stop(&id).await;
}
