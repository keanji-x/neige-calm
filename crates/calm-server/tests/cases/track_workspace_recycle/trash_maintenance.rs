//! Online trash maintenance is deferred without an enforced filesystem boundary.
use super::*;
use calm_server::workspace_service::trash::collect_once;
use std::time::Duration;

fn expired_trash(root: &Path) -> PathBuf {
    let stamp =
        calm_server::model::now_ms() - workspace_runtime::recycle::TRASH_RETENTION_MS - 60_000;
    let path = root.join(".trash").join(format!("old-track-{stamp}"));
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("payload"), b"old").unwrap();
    path
}

#[tokio::test]
async fn retirement_trash_retention_age_does_not_authorize_online_deletion() {
    let b = boot().await;
    let stale = expired_trash(&b.workspace_root);
    let external = b.tmp.path().join("external");
    std::fs::create_dir(&external).unwrap();
    std::fs::write(external.join("keep"), b"user data").unwrap();
    let link = b.workspace_root.join(".trash").join("alias");
    std::os::unix::fs::symlink(&external, &link).unwrap();
    collect_once(
        b.workspace_root.clone(),
        b.operation_runtime.clone(),
        b.repo.db().clone(),
    )
    .await;
    assert_eq!(std::fs::read(stale.join("payload")).unwrap(), b"old");
    assert_eq!(std::fs::read(external.join("keep")).unwrap(), b"user data");
    assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
}

#[tokio::test]
async fn retirement_deferred_trash_collection_does_not_wait_for_lifecycle_or_writer() {
    let b = boot().await;
    let stale = expired_trash(&b.workspace_root);
    let fence = b.operation_runtime.lock_for_track_delete_for_test().await;
    let writer = b.repo.db().hold_writer_for_test().await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        collect_once(
            b.workspace_root.clone(),
            b.operation_runtime.clone(),
            b.repo.db().clone(),
        ),
    )
    .await
    .expect("deferred garbage must not wait for normal lifecycle resources");
    assert!(stale.exists());
    drop(writer);
    drop(fence);
    let area = create_area(&b, "independent").await;
    let (track, path) = managed_track(&b, &area, "independent").await;
    let (status, body) = delete_track(&b, &track).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_retired_path(&b, &path).await;
    assert!(stale.exists());
}
