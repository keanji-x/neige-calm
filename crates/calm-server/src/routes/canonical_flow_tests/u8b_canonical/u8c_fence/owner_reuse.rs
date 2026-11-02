//! Clean's real entry point publishes the existing location owner through its fence.
use super::*;
use crate::per_card_lock::{lock_key, try_lock_key};

async fn closed(
    fixture: &Fixture,
    app: &axum::Router,
    cookie: &str,
    path: &std::path::Path,
) -> crate::model::Track {
    let a = area(app, cookie, "teardown ownership").await;
    let created = post(
        app,
        cookie,
        "/api/tracks",
        "first",
        borrowed(&a, "codex", path, "worktree"),
    )
    .await;
    let track = created["id"].as_str().unwrap();
    (fixture.repo.db())
        .writer()
        .execute(sqlx::query("UPDATE tracks SET closed_at=123 WHERE id=?").bind(track))
        .await
        .unwrap();
    fixture.repo.track_get(track).await.unwrap().unwrap()
}

fn clean_task(
    fixture: &Fixture,
    state: &crate::state::AppState,
) -> tokio::task::JoinHandle<crate::error::Result<usize>> {
    let db = fixture.repo.db().clone();
    let runtime = state.operation_runtime.clone();
    tokio::spawn(async move {
        crate::test_seams::clean_idle_closed_track_worktrees_for_test(&db, &runtime).await
    })
}

#[tokio::test]
async fn u8c_fence_clean_busy_track_returns_without_waiting_or_releasing_owner() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let path = repo(&fixture);
    let track = closed(&fixture, &app, &cookie, &path).await;
    let runtime = &state.operation_runtime;
    let targets = &runtime.location_admission().unwrap().teardown_targets;
    let held = lock_key(runtime.track_delete_locks(), track.id.as_str()).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), clean_task(&fixture, &state))
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        0
    );
    assert!(targets.snapshot().is_empty());
    assert!(try_lock_key(runtime.track_delete_locks(), track.id.as_str()).is_none());
    drop(
        tokio::time::timeout(Duration::from_secs(5), runtime.lock_for_track_delete())
            .await
            .unwrap(),
    );
    drop(held);
    assert!(try_lock_key(runtime.track_delete_locks(), track.id.as_str()).is_some());
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn u8c_fence_clean_duplicate_cancel_preserves_location_owner_union() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let path = repo(&fixture);
    let track = closed(&fixture, &app, &cookie, &path).await;
    let admission = state.operation_runtime.location_admission().unwrap();
    let targets = &admission.teardown_targets;
    let mut conn = fixture.repo.db().read_pool().acquire().await.unwrap();
    let cwd = std::path::Path::new(track.workspace.agent_cwd());
    // A caller holding its Track gets exactly the same owner set before and during clean.
    let fence = admission
        .try_fence(&mut conn, cwd, Some(track.id.as_str()))
        .await
        .unwrap();
    let held = HeldPause::arm("c1-clean-fenced", track.id.as_str());
    let cleaning = clean_task(&fixture, &state);
    held.entered().await;
    let write_db = (fixture.repo.db()).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async {
                let mut tx = write_session.begin_immediate().await.unwrap();
                fence.recheck(&mut tx).await.unwrap();
                tx.rollback().await.unwrap();
                assert!(matches!(
                    admission.try_fence(&mut conn, cwd, None).await,
                    Err(crate::error::CalmError::WorkspaceBusy(_))
                ));
                let unrelated = fixture.root.path().join("unrelated");
                std::fs::create_dir(&unrelated).unwrap();
                assert!(
                    admission
                        .try_fence(&mut conn, &unrelated, None)
                        .await
                        .is_ok()
                );
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(5), clean_task(&fixture, &state))
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap(),
                    0
                );
                assert_eq!(
                    targets.snapshot_with_owners(),
                    vec![(track.id.to_string(), cwd.into())]
                );
                let other = targets.registration();
                other.register_location(track.id.as_str(), cwd.into());
                cleaning.abort();
                assert!(cleaning.await.unwrap_err().is_cancelled());
                assert_eq!(
                    targets.snapshot_with_owners(),
                    vec![(track.id.to_string(), cwd.into())]
                );
                let maintenance = crate::workspace_service::retained_worktree(
                    fixture.repo.clone(),
                    state.operation_runtime.track_delete_locks(),
                    &track,
                    crate::workspace_service::RetainedAdmission::Live(targets.clone()),
                    crate::workspace_service::RetainedPreparation::Locator,
                    || async { Ok(()) },
                )
                .await;
                assert!(matches!(
                    maintenance,
                    Err(crate::error::CalmError::WorkspaceBusy(_))
                ));
                drop(other);
                assert!(targets.snapshot().is_empty());
                assert!(admission.try_fence(&mut conn, cwd, None).await.is_ok());
                drop(held);
                assert_eq!(clean_task(&fixture, &state).await.unwrap().unwrap(), 0);
                assert!(targets.snapshot().is_empty());
                let mut tx = write_session.begin_immediate().await.unwrap();
                fence.recheck(&mut tx).await.unwrap();
                tx.rollback().await.unwrap();
                fixture.shutdown_process(&state).await;
            })
        })
        .await
        .expect("independent write operation")
}

#[tokio::test]
async fn u8c_fence_clean_does_not_wait_for_git_metadata() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let path = repo(&fixture);
    let track = closed(&fixture, &app, &cookie, &path).await;
    std::fs::write(path.join(".git/info/exclude"), "target/\n").unwrap();
    let ignored = std::path::Path::new(track.workspace.agent_cwd()).join("target/output");
    std::fs::create_dir_all(ignored.parent().unwrap()).unwrap();
    std::fs::write(&ignored, "output").unwrap();
    // A second production materializer holds the common Git metadata lock.
    let metadata = HeldPause::arm("track-metadata-entered", path.to_str().unwrap());
    let a = json!({"id":track.area_id});
    let body = borrowed(&a, "codex", &path, "worktree");
    let mut holder = {
        let app = app.clone();
        let cookie = cookie.clone();
        tokio::spawn(async move { create(&app, &cookie, "holder", body).await })
    };
    tokio::select! {
        _ = metadata.entered() => {},
        result = &mut holder => panic!("metadata holder returned before locking: {result:?}"),
    }
    // No physical cleanup is admitted, so the metadata holder cannot block
    // maintenance, and completing maintenance must not release that holder.
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), clean_task(&fixture, &state))
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        0
    );
    assert!(!holder.is_finished());
    let runtime = &state.operation_runtime;
    let targets = &runtime.location_admission().unwrap().teardown_targets;
    assert!(targets.snapshot().is_empty());
    assert!(try_lock_key(runtime.track_delete_locks(), track.id.as_str()).is_some());
    assert!(ignored.is_file());
    drop(metadata);
    assert_eq!(holder.await.unwrap().0, StatusCode::CREATED);
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn u8c_fence_clean_filesystem_error_releases_owner_for_later_pass() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let path = repo(&fixture);
    let track = closed(&fixture, &app, &cookie, &path).await;
    let cwd = std::path::Path::new(track.workspace.agent_cwd());
    std::fs::write(path.join(".git/info/exclude"), "target/\n").unwrap();
    let ignored = cwd.join("target/output");
    std::fs::create_dir_all(ignored.parent().unwrap()).unwrap();
    std::fs::write(&ignored, "output").unwrap();
    // Ask Git for its index location rather than reconstructing private worktree layout.
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-path", "index"])
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(output.status.success());
    let index = std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let original = std::fs::read(&index).unwrap();
    std::fs::write(&index, "invalid index").unwrap();
    let held = HeldPause::arm("c1-clean-fenced", track.id.as_str());
    let cleaning = clean_task(&fixture, &state);
    held.entered().await;
    let targets = &state
        .operation_runtime
        .location_admission()
        .unwrap()
        .teardown_targets;
    assert_eq!(
        targets.snapshot_with_owners(),
        vec![(track.id.to_string(), cwd.into())]
    );
    drop(held);
    assert_eq!(cleaning.await.unwrap().unwrap(), 0);
    assert!(ignored.exists());
    assert!(targets.snapshot().is_empty());
    assert!(
        try_lock_key(
            state.operation_runtime.track_delete_locks(),
            track.id.as_str()
        )
        .is_some()
    );
    std::fs::write(&index, original).unwrap();
    assert_eq!(clean_task(&fixture, &state).await.unwrap().unwrap(), 0);
    assert!(ignored.is_file());
    assert!(targets.snapshot().is_empty());
    fixture.shutdown_process(&state).await;
}
