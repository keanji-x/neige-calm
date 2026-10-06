//! REST races across independent RouteStates, including a linked checkout.
use super::*;
use calm_server::test_seams::{PausePoint, install_pause_for_test};
use std::time::Duration;

struct Pause(PausePoint);
impl Pause {
    fn arm(point: &str, repo: &Path) -> Self {
        let hook = PausePoint {
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
        };
        install_pause_for_test(point, repo.to_str().unwrap(), hook.clone());
        Self(hook)
    }
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(10), self.0.entered.notified())
            .await
            .expect("production metadata boundary reached");
    }
    fn release(&self) {
        self.0.release.notify_one();
    }
    async fn contended_before(&self, entered: &Self) {
        tokio::select! {
            _ = self.entered() => {},
            _ = entered.0.entered.notified() => panic!("entered without competing for the held metadata lock"),
        }
        use futures::FutureExt;
        assert!(entered.0.entered.notified().now_or_never().is_none());
    }
}
impl Drop for Pause {
    fn drop(&mut self) {
        self.release();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metadata_root_linked_create_serializes_independent_states() {
    let first = Arc::new(boot().await);
    let second = Arc::new(boot().await);
    let up = upstream(first.tmp.path());
    let linked = first.tmp.path().join("linked");
    run_git(
        &up.clone,
        ["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    run_git(&linked, ["branch", "--set-upstream-to=origin/main"]);
    let holder = Pause::arm("track-metadata-entered", &up.clone);
    let create = {
        let b = first.clone();
        let root = up.clone.clone();
        tokio::spawn(async move { b.create_at(&root, Some("metadata-first"), None).await })
    };
    holder.entered().await;
    let before = Pause::arm("track-metadata-before", &linked);
    let contended = Pause::arm("track-metadata-contended", &linked);
    let entered = Pause::arm("track-metadata-entered", &linked);
    let waiting = {
        let b = second.clone();
        let root = linked.clone();
        tokio::spawn(async move { b.create_at(&root, Some("metadata-second"), None).await })
    };
    before.entered().await;
    // Delay the waiter at the pre-lock boundary while another repository progresses.
    let third = boot().await;
    let other = upstream(third.tmp.path());
    let (status, body) = tokio::time::timeout(
        Duration::from_secs(10),
        third.create_at(&other.clone, None, None),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::CREATED, "{body}");
    before.release();
    contended.contended_before(&entered).await;
    holder.release();
    contended.release();
    entered.entered().await;
    entered.release();
    let (status, first_body) = create.await.unwrap();
    assert_eq!(status, StatusCode::CREATED, "{first_body}");
    let (status, second_body) = waiting.await.unwrap();
    assert_eq!(status, StatusCode::CREATED, "{second_body}");
    for (b, root, body, key) in [
        (&first, &up.clone, &first_body, "metadata-first"),
        (&second, &linked, &second_body, "metadata-second"),
    ] {
        let id = body["id"].as_str().unwrap();
        assert_eq!(
            upstream_of(&expected_worktree(root, id)),
            Some("origin/main".into())
        );
        let (status, retry) = b.create_at(root, Some(key), None).await;
        assert!(status.is_success(), "{retry}");
        assert_eq!(retry["id"], body["id"]);
        b.shutdown_harnesses().await;
    }
    third.shutdown_harnesses().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metadata_create_delete_serializes_and_survives_request_cancellation() {
    let first = Arc::new(boot().await);
    let second = Arc::new(boot().await);
    let up = upstream(first.tmp.path());
    let (status, body) = second.create_at(&up.clone, None, None).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let deleting_id = body["id"].as_str().unwrap().to_owned();
    second.shutdown_harnesses().await;
    let holder = Pause::arm("track-metadata-entered", &up.clone);
    let create = {
        let b = first.clone();
        let root = up.clone.clone();
        tokio::spawn(async move { b.create_at(&root, Some("metadata-cancel"), None).await })
    };
    holder.entered().await;
    create.abort();
    let _ = create.await;
    let before = Pause::arm("track-metadata-before", &up.clone);
    let contended = Pause::arm("track-metadata-contended", &up.clone);
    let entered = Pause::arm("track-metadata-entered", &up.clone);
    let deletion = {
        let b = second.clone();
        let id = deleting_id.clone();
        tokio::spawn(async move { b.delete_track(&id).await })
    };
    before.entered().await;
    before.release();
    contended.contended_before(&entered).await;
    holder.release();
    contended.release();
    entered.entered().await;
    // The owned postcommit task must survive cancellation of the delete request too.
    deletion.abort();
    let _ = deletion.await;
    entered.release();
    tokio::time::timeout(Duration::from_secs(10), async {
        while git_ref_exists(&up.clone, &format!("refs/heads/neige/track-{deleting_id}")) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!expected_worktree(&up.clone, &deleting_id).exists());
    let (status, retry) = first
        .create_at(&up.clone, Some("metadata-cancel"), None)
        .await;
    assert!(status.is_success(), "{retry}");
    assert!(expected_worktree(&up.clone, retry["id"].as_str().unwrap()).is_dir());
    first.shutdown_harnesses().await;
}
