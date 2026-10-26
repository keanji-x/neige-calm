//! Track deletion must win over a committed but not yet materialized locator.
use super::*;
use crate::test_seams::{PausePoint, install_pause_for_test};

async fn deleted_creation(human: bool) {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let a = area(&app, &cookie, "delete fence").await;
    let path = repo(&fixture);
    let mut body = borrowed(&a, "codex", &path, "worktree");
    if human {
        body["first_message"] = json!(FIRST_BODY);
    }
    let pause = PausePoint::default();
    install_pause_for_test(
        "canonical-workspace-committed",
        a["id"].as_str().unwrap(),
        pause.clone(),
    );
    let creating = create(&app, &cookie, "first", body);
    tokio::pin!(creating);
    tokio::select! {
        _ = pause.entered.notified() => {},
        result = &mut creating => panic!("missing postcommit seam: {result:?}"),
        _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("missing postcommit seam"),
    }
    let binding = crate::db::sqlite::track_create_idempotency_get_pool(
        fixture.repo.pool(),
        a["id"].as_str().unwrap(),
        "first",
    )
    .await
    .unwrap()
    .unwrap();
    let track = fixture
        .repo
        .track_get(&binding.track_id)
        .await
        .unwrap()
        .unwrap();
    let worktree = std::path::PathBuf::from(track.workspace.agent_cwd());
    let branch = workspace::worktree::track_branch_for(&binding.track_id).unwrap();
    assert_eq!(
        http(
            &app,
            &cookie,
            "DELETE",
            &format!("/api/tracks/{}", track.id)
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(
        fixture
            .repo
            .track_get(&binding.track_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!worktree.exists());
    pause.release.notify_one();
    let result = creating.await;
    let branch_exists = std::process::Command::new("git")
        .args(["show-ref", "--verify", &format!("refs/heads/{branch}")])
        .current_dir(&path)
        .output()
        .unwrap()
        .status
        .success();
    fixture.shutdown_process(&state).await;
    assert!(
        !worktree.exists(),
        "deleted Track worktree was recreated; POST {result:?}"
    );
    assert!(!branch_exists, "deleted Track branch was recreated");
    assert_ne!(
        result.0,
        StatusCode::CREATED,
        "deleted Track must not receive 201"
    );
}

#[tokio::test]
async fn u8c_fence_empty_post_after_delete_sweep() {
    deleted_creation(false).await;
}

#[tokio::test]
async fn u8c_fence_first_human_post_after_delete_sweep() {
    deleted_creation(true).await;
}

struct HeldPause(PausePoint);
impl HeldPause {
    fn arm(point: &str, key: &str) -> Self {
        let pause = PausePoint::default();
        install_pause_for_test(point, key, pause.clone());
        Self(pause)
    }
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(10), self.0.entered.notified())
            .await
            .expect("production rendezvous");
    }
}
impl Drop for HeldPause {
    fn drop(&mut self) {
        self.0.release.notify_one();
    }
}

#[tokio::test]
async fn u8c_fence_replay_repair_waits_for_clean() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let a = area(&app, &cookie, "clean fence").await;
    let path = repo(&fixture);
    let body = borrowed(&a, "codex", &path, "worktree");
    let created = post(&app, &cookie, "/api/tracks", "first", body.clone()).await;
    let track = created["id"].as_str().unwrap();
    // An idle closed candidate, through the same setup as C1's clean regressions.
    sqlx::query("UPDATE tracks SET closed_at=123 WHERE id=?")
        .bind(track)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    let held = HeldPause::arm("c1-clean-fenced", track);
    let cleaning = crate::test_seams::clean_idle_closed_track_worktrees_for_test(
        fixture.repo.db(),
        &state.operation_runtime,
    );
    tokio::pin!(cleaning);
    tokio::select! {
        _ = held.entered() => {},
        result = &mut cleaning => panic!("clean did not fence candidate: {result:?}"),
    }
    let arrival = HeldPause::arm("key-lock-waiting", track);
    let repairing = create(&app, &cookie, "first", body);
    tokio::pin!(repairing);
    tokio::select! {
        _ = arrival.entered() => {},
        result = &mut repairing => panic!("repair bypassed clean fence: {result:?}"),
    }
    drop(arrival);
    drop(held);
    assert_eq!(cleaning.await.unwrap(), 1);
    assert_eq!(repairing.await.0, StatusCode::CREATED);
    read(&app, &cookie, track, MARKER).await;
    fixture.shutdown_process(&state).await;
}

async fn retained_clean_and_delete(cold: bool) {
    let fixture = Fixture::new().await;
    let (state, app, cookie, auth) = fixture.application_with_auth(false).await;
    let a = area(&app, &cookie, "retained fence").await;
    let path = repo(&fixture);
    let mut body = borrowed(&a, "codex", &path, "worktree");
    body["first_message"] = json!(FIRST_BODY);
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created = post(&app, &cookie, "/api/tracks", "first", body).await;
    let track = created["id"].as_str().unwrap();
    let turn = super::super::native_attempt_recovery_tests::initial_request(&mut peer).await;
    send(
        &mut peer,
        json!({"id":turn["id"],"result":{"turn":{"id":TURN}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":TURN,"status":"inProgress"}),
    )
    .await;
    fixture
        .wait_phase(track, HarnessPhaseTag::TurnRunning)
        .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":TURN,"status":"completed"}),
    )
    .await;
    fixture
        .wait_phase(track, HarnessPhaseTag::TurnCompleted)
        .await;
    let (card, worker, _) = fixture.carrier(track).await;
    let route = crate::state::RouteState::from_ref(&state);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(header::COOKIE, cookie.parse().unwrap());
    let (_, source) = crate::auth::authenticated_source(auth, headers).await;
    let source = source.unwrap();
    let fence = crate::operation::planner_start_fence::CardStartFence::lock(
        &route.planner_recovery_locks,
        &route.repo,
        &route.operation_runtime,
        &card.clone().into(),
    )
    .await;
    if cold {
        super::super::native_attempt_recovery_tests::shutdown(&state, &worker, &mut peer).await;
    }
    sqlx::query("UPDATE tracks SET closed_at=123 WHERE id=?")
        .bind(track)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    let held = HeldPause::arm("c1-clean-fenced", track);
    let cleaning = crate::test_seams::clean_idle_closed_track_worktrees_for_test(
        fixture.repo.db(),
        &state.operation_runtime,
    );
    tokio::pin!(cleaning);
    tokio::select! {
        _ = held.entered() => {},
        result = &mut cleaning => panic!("clean did not fence retained candidate: {result:?}"),
    }
    let worktree = std::path::PathBuf::from(created["workspace"]["worktree"].as_str().unwrap());
    // Both direct live wake and drive-owned recovery must refuse a busy Track
    // without holding up operation drive during clean's filesystem work.
    let refused = tokio::time::timeout(
        Duration::from_secs(10),
        fence.wake_retained(
            route.planner_start_adapter.clone(),
            source.clone(),
            route.workspace_root.clone(),
            route.events.clone(),
        ),
    )
    .await
    .expect("retained refusal must release operation drive");
    assert!(
        matches!(refused, Err(crate::error::CalmError::WorkspaceBusy(_))),
        "{refused:?}"
    );
    drop(held);
    assert_eq!(cleaning.await.unwrap(), 1);
    let fence = crate::operation::planner_start_fence::CardStartFence::lock(
        &route.planner_recovery_locks,
        &route.repo,
        &route.operation_runtime,
        &card.clone().into(),
    )
    .await;
    let wake = fence.wake_retained(
        route.planner_start_adapter.clone(),
        source.clone(),
        route.workspace_root.clone(),
        route.events.clone(),
    );
    tokio::pin!(wake);
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            tokio::select! {
                result = &mut wake => break result,
                call = request(&mut peer) => {
                    assert!(cold && matches!(call["method"].as_str(), Some("thread/read" | "thread/resume")));
                    send(&mut peer, json!({"id":call["id"],"result":{"thread":{"id":THREAD,"status":{"type":"idle"},"turns":[]}}})).await;
                }
            }
        }
    }).await.expect("repair completes after clean releases its lifecycle fence");
    result.unwrap();
    assert!(worktree.is_dir());
    // Deletion is the production lifecycle owner, including native quiesce and sweep.
    let uri = format!("/api/tracks/{track}");
    let deleting = http(&app, &cookie, "DELETE", &uri);
    tokio::pin!(deleting);
    let deleted = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            tokio::select! {
                result = &mut deleting => break result,
                call = request(&mut peer) => {
                    assert!(matches!(call["method"].as_str(), Some("turn/interrupt" | "thread/unsubscribe")), "{call}");
                    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
                }
            }
        }
    })
    .await
    .expect("retained repair releases drive before deletion");
    assert_eq!(deleted.0, StatusCode::NO_CONTENT);
    let rejected = crate::operation::planner_start_fence::CardStartFence::lock(
        &route.planner_recovery_locks,
        &route.repo,
        &route.operation_runtime,
        &card.into(),
    )
    .await
    .wake_retained(
        route.planner_start_adapter.clone(),
        source,
        route.workspace_root.clone(),
        route.events.clone(),
    )
    .await;
    fixture.shutdown_process(&state).await;
    assert!(rejected.is_err());
    assert!(!worktree.exists());
}

#[tokio::test]
async fn u8c_fence_retained_live_wake_clean_and_delete() {
    retained_clean_and_delete(false).await;
}

#[tokio::test]
async fn u8c_fence_retained_recovery_clean_and_delete() {
    retained_clean_and_delete(true).await;
}

#[tokio::test]
async fn u8c_fence_cancelled_post_holds_delete_through_filesystem() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let a = area(&app, &cookie, "owned filesystem fence").await;
    let path = repo(&fixture);
    let body = borrowed(&a, "codex", &path, "worktree");
    let fs = HeldPause::arm("track-metadata-before", path.to_str().unwrap());
    let creating = {
        let app = app.clone();
        let cookie = cookie.clone();
        tokio::spawn(async move { create(&app, &cookie, "first", body).await })
    };
    fs.entered().await;
    let binding = crate::db::sqlite::track_create_idempotency_get_pool(
        fixture.repo.pool(),
        a["id"].as_str().unwrap(),
        "first",
    )
    .await
    .unwrap()
    .unwrap();
    let track = fixture
        .repo
        .track_get(&binding.track_id)
        .await
        .unwrap()
        .unwrap();
    let arrival = HeldPause::arm("key-lock-waiting", &binding.track_id);
    creating.abort();
    assert!(creating.await.unwrap_err().is_cancelled());
    // Probe before polling DELETE: a queued mutex waiter could reserve a released
    // permit even while paused, masking repair's loss of the lifecycle guard.
    let route = crate::state::RouteState::from_ref(&state);
    assert!(
        crate::per_card_lock::try_lock_key(&route.track_delete_locks, &binding.track_id).is_none(),
        "cancelled POST released the Track fence before filesystem completion"
    );
    let uri = format!("/api/tracks/{}", track.id);
    let deleting = http(&app, &cookie, "DELETE", &uri);
    tokio::pin!(deleting);
    tokio::select! {
        _ = arrival.entered() => {},
        result = &mut deleting => panic!("delete passed in-flight filesystem repair: {result:?}"),
    }
    // Both DELETE's arrival and Git remain paused after cancellation.
    assert!(
        crate::per_card_lock::try_lock_key(&route.track_delete_locks, &binding.track_id).is_none(),
        "cancelled POST released the Track fence before filesystem completion"
    );
    drop(arrival);
    drop(fs);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), deleting)
            .await
            .unwrap()
            .0,
        StatusCode::NO_CONTENT
    );
    assert!(!std::path::Path::new(track.workspace.agent_cwd()).exists());
    let branch = workspace::worktree::track_branch_for(&binding.track_id).unwrap();
    assert!(
        !std::process::Command::new("git")
            .args(["show-ref", "--verify", &format!("refs/heads/{branch}")])
            .current_dir(&path)
            .output()
            .unwrap()
            .status
            .success()
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn u8c_fence_post_rejects_changed_workspace_after_commit() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let a = area(&app, &cookie, "changed selection").await;
    let path = repo(&fixture);
    let body = borrowed(&a, "codex", &path, "worktree");
    let sampled = HeldPause::arm("canonical-workspace-committed", a["id"].as_str().unwrap());
    let creating = create(&app, &cookie, "first", body);
    tokio::pin!(creating);
    tokio::select! {
        _ = sampled.entered() => {},
        result = &mut creating => panic!("missing committed selection: {result:?}"),
    }
    let binding = crate::db::sqlite::track_create_idempotency_get_pool(
        fixture.repo.pool(),
        a["id"].as_str().unwrap(),
        "first",
    )
    .await
    .unwrap()
    .unwrap();
    let track = fixture
        .repo
        .track_get(&binding.track_id)
        .await
        .unwrap()
        .unwrap();
    let route = crate::state::RouteState::from_ref(&state);
    let lifecycle =
        crate::per_card_lock::lock_key(&route.track_delete_locks, &binding.track_id).await;
    let arrival = HeldPause::arm("key-lock-waiting", &binding.track_id);
    drop(sampled);
    tokio::select! {
        _ = arrival.entered() => {},
        result = &mut creating => panic!("POST bypassed lifecycle fence: {result:?}"),
    }
    // Change the selection only after POST has reached the held lifecycle fence.
    // This is the boundary fact the owner must re-read, without requiring repoint's
    // separate migration/provider policy to permit an attached-to-attached change.
    let changed = fixture.root.path().join("changed-worktree");
    sqlx::query("UPDATE tracks SET workspace_worktree_path=? WHERE id=?")
        .bind(changed.to_str().unwrap())
        .bind(&binding.track_id)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    drop(lifecycle);
    drop(arrival);
    let result = creating.await;
    fixture.shutdown_process(&state).await;
    assert_eq!(result.0, StatusCode::CONFLICT, "{result:?}");
    assert!(!std::path::Path::new(track.workspace.agent_cwd()).exists());
    assert!(!changed.exists());
}

mod owner_reuse;
