//! Exercise the actual maintenance entry and interactive routes, without copying GC policy.
use super::*;
use calm_server::test_seams::{PausePoint, install_pause_for_test};
use calm_server::workspace_service::trash::collect_once;
use calm_truth::db::RepoRead;
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(10);

fn expired_trash(root: &Path) -> PathBuf {
    let stamp =
        calm_server::model::now_ms() - workspace_runtime::recycle::TRASH_RETENTION_MS - 60_000;
    let stale = root.join(".trash").join(format!("old-track-{stamp}"));
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("payload"), b"old").unwrap();
    stale
}

struct ReleaseOnDrop(Option<Arc<tokio::sync::Notify>>);
impl ReleaseOnDrop {
    fn release(&mut self) {
        if let Some(release) = self.0.take() {
            release.notify_one();
        }
    }
}
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.release();
    }
}

#[tokio::test]
async fn workspace_trash_maintenance_collects_expired_and_preserves_fresh_unowned_and_symlink_entries()
 {
    let b = boot().await;
    let stale = expired_trash(&b.workspace_root);
    let fresh = b
        .workspace_root
        .join(".trash")
        .join(format!("fresh-{}", calm_server::model::now_ms()));
    let unknown = b.workspace_root.join(".trash").join("unrecognized");
    std::fs::create_dir_all(&fresh).unwrap();
    std::fs::create_dir_all(&unknown).unwrap();
    let external = b.tmp.path().join("external");
    std::fs::create_dir_all(&external).unwrap();
    std::fs::write(external.join("keep"), b"user data").unwrap();
    let old_stamp =
        calm_server::model::now_ms() - workspace_runtime::recycle::TRASH_RETENTION_MS - 60_000;
    let link = b
        .workspace_root
        .join(".trash")
        .join(format!("symlink-{old_stamp}"));
    std::os::unix::fs::symlink(&external, &link).unwrap();

    collect_once(
        b.workspace_root.clone(),
        b.operation_runtime.clone(),
        b.repo.db().clone(),
    )
    .await;
    assert!(!stale.exists());
    assert!(fresh.is_dir());
    assert!(unknown.is_dir());
    assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read(external.join("keep")).unwrap(), b"user data");
}

#[tokio::test]
async fn workspace_trash_maintenance_waits_for_lifecycle_fence_without_leasing_the_writer() {
    let b = boot().await;
    let stale = expired_trash(&b.workspace_root);
    let fence = b.operation_runtime.lock_for_track_delete_for_test().await;
    let pass = collect_once(
        b.workspace_root.clone(),
        b.operation_runtime.clone(),
        b.repo.db().clone(),
    );
    tokio::pin!(pass);
    let pending = futures::poll!(&mut pass).is_pending();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let untouched_while_fenced = stale.exists();
    let writer = tokio::time::timeout(LIMIT, b.repo.db().writer_pool().acquire()).await;
    let writer_available = writer.as_ref().is_ok_and(|lease| lease.is_ok());
    drop(fence);
    // Keep the sole writer lease while the actual filesystem collector completes.
    let completed = tokio::time::timeout(LIMIT, pass).await;
    drop(writer);
    assert!(pending, "maintenance bypassed an in-flight lifecycle saga");
    assert!(
        untouched_while_fenced,
        "trash moved before the lifecycle boundary"
    );
    assert!(writer_available);
    assert!(
        completed.is_ok(),
        "maintenance waited for the database writer"
    );
    assert!(!stale.exists());
}

#[tokio::test]
async fn workspace_trash_maintenance_does_not_hold_lifecycle_fence_or_delay_unrelated_deletion() {
    let b = boot().await;
    let area = create_area(&b, "maintenance").await;
    let (track, path) = managed_track(&b, &area, "unrelated deletion").await;
    let stale = expired_trash(&b.workspace_root);
    let pause = PausePoint::default();
    let mut release = ReleaseOnDrop(Some(pause.release.clone()));
    install_pause_for_test(
        "workspace-trash-collection",
        b.workspace_root.to_str().unwrap(),
        pause.clone(),
    );
    let root = b.workspace_root.clone();
    let runtime = b.operation_runtime.clone();
    let db = b.repo.db().clone();
    let collector = tokio::spawn(async move { collect_once(root, runtime, db).await });
    let entered = tokio::time::timeout(LIMIT, pause.entered.notified()).await;
    let fence =
        tokio::time::timeout(LIMIT, b.operation_runtime.lock_for_track_delete_for_test()).await;
    let fence_available = fence.is_ok();
    drop(fence);
    let writer = tokio::time::timeout(LIMIT, b.repo.db().writer_pool().acquire()).await;
    let writer_available = writer.as_ref().is_ok_and(|lease| lease.is_ok());
    drop(writer);
    let deletion = tokio::time::timeout(LIMIT, delete_track(&b, &track)).await;
    let collector_still_held = !collector.is_finished();
    // Always release and drain before asserting: a regression must fail, not hang
    // the runtime's blocking-pool shutdown (also true during mutation verification).
    release.release();
    let drained = tokio::time::timeout(LIMIT, collector).await;
    assert!(
        entered.is_ok(),
        "collector never reached the real blocking worker"
    );
    assert!(drained.as_ref().is_ok_and(|join| join.is_ok()));
    assert!(
        fence_available,
        "recursive GC retained the global lifecycle fence"
    );
    assert!(writer_available, "recursive GC leased the SQL writer");
    assert!(
        collector_still_held,
        "premise: collector must remain paused"
    );
    let (status, body) = deletion.expect("unrelated delete waited for trash collection");
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(b.repo.track_get(&track).await.unwrap().is_none());
    assert!(!path.exists());
    assert!(!stale.exists());
    assert!(trash_entry_for(&b.workspace_root, &track).is_some());
}

#[tokio::test]
async fn area_delete_keeps_unrelated_expired_trash_for_maintenance() {
    let b = boot().await;
    let area = create_area(&b, "maintenance").await;
    let (track, path) = managed_track(&b, &area, "area deletion").await;
    let stale = expired_trash(&b.workspace_root);
    let (status, body) = delete_area(&b, &area).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(stale.is_dir(), "area deletion collected unrelated trash");
    assert!(b.repo.track_get(&track).await.unwrap().is_none());
    assert!(!path.exists());
    assert!(trash_entry_for(&b.workspace_root, &track).is_some());
}

async fn failed_rollback_gc(occupied: bool) {
    let b = boot_with_storage(|root| root.join("workspaces"), true).await;
    let area = create_area(&b, "retention").await;
    let (track, path) = managed_track(&b, &area, "restore blocked").await;
    let hook = calm_server::routes::tracks::TrackDeleteCommitHook {
        entered: Arc::default(),
        release: Arc::default(),
        panic_after_release: true,
    };
    calm_server::routes::tracks::install_track_delete_commit_hook_for_test(&track, hook.clone());
    let app = b.app.clone();
    let id = track.clone();
    let deletion =
        tokio::spawn(
            async move { request(app, "DELETE", &format!("/api/tracks/{id}"), None).await },
        );
    tokio::time::timeout(LIMIT, hook.entered.notified())
        .await
        .unwrap();
    let renamed = if occupied {
        std::fs::create_dir_all(&path).unwrap();
        None
    } else {
        // Inject rename failure: the recorded trash source disappears, while its
        // owned bytes remain in another correctly named trash entry.
        let trash = trash_entry_for(&b.workspace_root, &track).unwrap();
        let moved = b.workspace_root.join(".trash").join(format!("{track}-1"));
        std::fs::rename(trash, &moved).unwrap();
        Some(moved)
    };
    hook.release.notify_one();
    let (status, body) = deletion.await.unwrap();
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(b.repo.track_get(&track).await.unwrap().is_some());
    assert_eq!(path.exists(), occupied);
    let trash = renamed.unwrap_or_else(|| trash_entry_for(&b.workspace_root, &track).unwrap());
    let stamp =
        calm_server::model::now_ms() - workspace_runtime::recycle::TRASH_RETENTION_MS - 60_000;
    let expired = b
        .workspace_root
        .join(".trash")
        .join(format!("{track}-{stamp}"));
    std::fs::rename(trash, &expired).unwrap();
    collect_once(
        b.workspace_root.clone(),
        b.operation_runtime.clone(),
        b.repo.db().clone(),
    )
    .await;
    assert!(expired.is_dir());
    // A fresh lifecycle runtime has no in-memory rollback state; the durable owner still protects it.
    let reboot = boot().await;
    let runtime = reboot.operation_runtime.clone();
    let reopened = SqlxRepo::open(&format!(
        "sqlite:{}",
        b.tmp.path().join("state.db").display()
    ))
    .await
    .unwrap();
    collect_once(b.workspace_root.clone(), runtime, reopened.db().clone()).await;
    assert!(expired.is_dir());
    let (status, body) = delete_track(&b, &track).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    collect_once(
        b.workspace_root.clone(),
        b.operation_runtime.clone(),
        b.repo.db().clone(),
    )
    .await;
    assert!(!expired.exists());
}

#[tokio::test]
async fn c5_trash_restore_occupied_owner_protects_until_delete() {
    failed_rollback_gc(true).await;
}

#[tokio::test]
async fn c5_trash_restore_rename_failure_owner_protects_until_delete() {
    failed_rollback_gc(false).await;
}
