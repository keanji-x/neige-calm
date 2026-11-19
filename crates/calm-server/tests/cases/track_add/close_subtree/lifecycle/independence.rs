//! Independent native Stop for one subtree Close.
use super::*;

async fn leased_worker(boot: &Boot, track: &str) -> Caller {
    let worker = running_worker(boot, track).await;
    calm_server::test_seams::acquire_workspace_lease_for_test(
        boot.repo.db(),
        &worker.identity.card_id,
        track,
        "legacy-owner",
        &boot
            ._tmp
            .path()
            .join("workers")
            .join(&worker.identity.card_id),
    )
    .await
    .unwrap();
    worker
}

#[tokio::test]
async fn single_subtree_close_does_not_serialize_workers_behind_unanswered_stop() {
    use std::time::Duration;
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_test_writer()
        .try_init();
    let boot = boot(16).await;
    let (root, _) = boot.user_track("worker-root").await;
    let (child, _) = child_of(&boot, &root, "worker-child").await;
    let a = leased_worker(&boot, &root).await;
    let b = leased_worker(&boot, &child).await;
    let (entered, release) =
        native(&boot).hold_stops_for(&[&a.identity.thread_id, &b.identity.thread_id]);
    assert_eq!(rest_close(&boot, &root).await, StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(4), entered.notified())
        .await
        .unwrap();
    let held_thread = interrupted(&boot)
        .into_iter()
        .find(|(t, _)| t == &a.identity.thread_id || t == &b.identity.thread_id)
        .unwrap()
        .0;
    let (held, other) = if held_thread == a.identity.thread_id {
        (&a, &b)
    } else {
        assert_eq!(held_thread, b.identity.thread_id);
        (&b, &a)
    };
    let retired = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let done: bool = boot.scalar(
                "SELECT state='failed' AND json_extract(handle_state_json,'$.timeout_cleanup') IS NULL FROM worker_sessions WHERE id=?1",
                &other.identity.session_id,
            ).await;
            if done { break; }
            tokio::task::yield_now().await;
        }
    }).await.is_ok();
    let runtime = &boot.native.as_ref().unwrap().state.operation_runtime;
    let drive_free = tokio::time::timeout(
        Duration::from_millis(300),
        runtime.lock_for_track_delete_for_test(),
    )
    .await
    .is_ok();
    let held_pending: bool = boot.scalar(
        "SELECT json_extract(handle_state_json,'$.timeout_cleanup') IS NOT NULL FROM worker_sessions WHERE id=?1",
        &held.identity.session_id,
    ).await;
    let lease_query =
        "SELECT state FROM workspace_leases WHERE card_id=?1 ORDER BY created_at_ms DESC LIMIT 1";
    let held_lease: String = boot.scalar(lease_query, &held.identity.card_id).await;
    let other_lease: String = boot.scalar(lease_query, &other.identity.card_id).await;
    let mut outside = Box::pin(boot.user_track("unrelated-progress"));
    // Track creation includes workspace and input admission. Allow the same
    // observer budget as the existing independence controls; A stays held.
    let unrelated_ready = tokio::time::timeout(Duration::from_secs(3), outside.as_mut())
        .await
        .is_ok();
    release.notify_one();
    if !unrelated_ready {
        tokio::time::timeout(Duration::from_secs(5), outside)
            .await
            .unwrap();
    }
    let converged = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let left = boot.count("SELECT count(*) FROM worker_sessions WHERE json_extract(handle_state_json,'$.timeout_cleanup') IS NOT NULL").await;
            if left == 0 { break; }
            tokio::task::yield_now().await;
        }
    }).await.is_ok();
    let pending_workers: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT id,state,json_extract(handle_state_json,'$.timeout_cleanup') FROM worker_sessions WHERE contract='executor'"
    ).fetch_all(boot.repo.pool()).await.unwrap();
    finish_planners(&boot).await;
    println!(
        "OBSERVATION {}",
        json!({"other_retired_before_release":retired,"drive_free_before_release":drive_free,"unrelated_track_created_before_release":unrelated_ready,"held_marker":held_pending,"held_lease":held_lease,"other_lease":other_lease,"interrupts":interrupted(&boot),"converged":converged,"workers_after":pending_workers})
    );
    assert!(
        held_pending && held_lease == "held",
        "the unconfirmed worker retains its exact marker"
    );
    assert!(
        retired && other_lease == "released" && drive_free && unrelated_ready && converged,
        "one slow worker must not block another worker or the operation drive"
    );
}

#[tokio::test]
async fn single_subtree_close_worker_stop_does_not_block_planner_cleanup() {
    use std::time::Duration;
    let boot = boot(16).await;
    let (root, _) = boot.user_track("worker-parent").await;
    let a = leased_worker(&boot, &root).await;
    let (child, planner, child_thread) = create_active(&boot, "planner-child").await;
    boot.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2")
                .bind(&root)
                .bind(&child),
        )
        .await
        .unwrap();
    // Make B eligible only once the real worker Stop is on the wire.
    let planner_admission = boot
        .ctx
        .tool_calls
        .fence(&planner.identity.session_id)
        .unwrap();
    let (entered, release) = native(&boot).hold_stops_for(&[&a.identity.thread_id]);
    assert_eq!(rest_close(&boot, &root).await, StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(4), entered.notified())
        .await
        .unwrap();
    assert!(
        interrupted(&boot)
            .iter()
            .any(|(t, _)| t == &a.identity.thread_id)
    );
    drop(planner_admission);
    let retired = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let left: i64 = boot
                .scalar(
                    "SELECT count(*) FROM track_close_cleanups WHERE worker_session_id=?1",
                    &planner.identity.session_id,
                )
                .await;
            if left == 0 && boot.harness.get(&planner.identity.session_id).is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    let before = interrupted(&boot);
    release.notify_one();
    finish_planners(&boot).await;
    println!(
        "OBSERVATION {}",
        json!({"planner_retired_before_release":retired,"interrupts_before":before})
    );
    assert!(
        retired && before.iter().any(|(t, _)| t == &child_thread),
        "a slow worker cannot retain the global drive needed by another Planner"
    );
}

#[tokio::test]
async fn close_p1_worker_without_lease_retires_a_confirmed_stop() {
    use std::time::Duration;
    let boot = boot(16).await;
    let (track, _) = boot.user_track("no-lease-worker").await;
    let worker = running_worker(&boot, &track).await;
    assert_eq!(rest_close(&boot, &track).await, StatusCode::OK);
    let retired = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let done: bool = boot.scalar(
                "SELECT state='failed' AND json_extract(handle_state_json,'$.timeout_cleanup') IS NULL FROM worker_sessions WHERE id=?1",
                &worker.identity.session_id,
            ).await;
            if done { break; }
            tokio::task::yield_now().await;
        }
    }).await.is_ok();
    assert!(
        interrupted(&boot)
            .iter()
            .any(|(t, _)| t == &worker.identity.thread_id)
    );
    assert!(
        retired,
        "confirmed Stop has a cleanup write even when no lease event is emitted"
    );
}

#[tokio::test]
async fn single_subtree_close_stops_the_other_planner_while_one_interrupt_is_unanswered() {
    subtree_stops(true).await;
}

#[tokio::test]
async fn single_subtree_close_settles_both_planners_when_interrupts_are_answered() {
    subtree_stops(false).await;
}

async fn create_active(native_boot: &Boot, key: &str) -> (String, Caller, String) {
    let request = Request::post("/api/tracks")
        .header("content-type", "application/json")
        .header("idempotency-key", key)
        .body(Body::from(
            json!({
                "area_id":native_boot.area_id,"title":"","planner_provider":"codex",
                "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":key
            })
            .to_string(),
        ))
        .unwrap();
    let response = native_boot.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let created: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let track = created["id"].as_str().unwrap().to_owned();
    let planner = native_boot.planner_of(&track).await;
    wait_native_turn(native_boot, &planner).await;
    let thread: String = native_boot
        .scalar(
            "SELECT thread_id FROM worker_sessions WHERE id=?1",
            &planner.identity.session_id,
        )
        .await;
    (track, planner, thread)
}

async fn subtree_stops(hold_first: bool) {
    use std::time::Duration;
    let boot = boot(16).await;
    let (root, p, parent_thread) = create_active(&boot, "cascade-root").await;
    let (child, c, child_thread) = create_active(&boot, "cascade-child").await;
    assert_ne!(parent_thread, child_thread);
    boot.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2")
                .bind(&root)
                .bind(&child),
        )
        .await
        .unwrap();
    task(&boot, &root, "child", "running", "sub-track", Some(&child)).await;
    let (entered, release) = native(&boot).hold_stop();
    assert_eq!(rest_close(&boot, &root).await, StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(4), entered.notified())
        .await
        .unwrap();
    assert!(!open(&boot, &root).await && !open(&boot, &child).await);
    // The first Stop on the wire depends on production scheduling, so derive its
    // identity from that request before observing the other Planner.
    let first = interrupted(&boot)[0].0.clone();
    let (held, other, other_thread) = if first == parent_thread {
        (&p, &c, &child_thread)
    } else {
        assert_eq!(first, child_thread);
        (&c, &p, &parent_thread)
    };
    let neutral =
        tokio::time::timeout(Duration::from_secs(2), boot.codex.thread_path(other_thread))
            .await
            .unwrap();
    assert!(neutral.is_ok(), "{neutral:?}");
    if !hold_first {
        release.notify_one();
    }
    let marker = "SELECT count(*) FROM track_close_cleanups WHERE worker_session_id=?1";
    let state_sql = "SELECT state FROM worker_sessions WHERE id=?1";
    let retired = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let count: i64 = boot.scalar(marker, &other.identity.session_id).await;
            let state: String = boot.scalar(state_sql, &other.identity.session_id).await;
            if count == 0
                && state == "idle"
                && boot.harness.get(&other.identity.session_id).is_none()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    let held_pending: i64 = boot.scalar(marker, &held.identity.session_id).await;
    let other_pending: i64 = boot.scalar(marker, &other.identity.session_id).await;
    let before = interrupted(&boot);
    if hold_first {
        release.notify_one();
    }
    finish_planners(&boot).await;
    let after = interrupted(&boot);
    assert!(
        after.iter().any(|(t, _)| t == &parent_thread)
            && after.iter().any(|(t, _)| t == &child_thread)
    );
    let assignment: String = boot
        .scalar(
            "SELECT status FROM tasks WHERE id=?1",
            &format!("{root}:child"),
        )
        .await;
    assert_eq!(assignment, "canceled");
    println!(
        "OBSERVATION {}",
        json!({"hold_first":hold_first,"root_closed":true,"child_closed":true,"assignment":assignment,"first":first,"other":other_thread,"neutral_rpc_ok":true,"other_retired_before_release":retired,"held_marker":held_pending,"other_marker":other_pending,"interrupts_before":before,"interrupts_after":after,"markers_after":boot.count("SELECT count(*) FROM track_close_cleanups").await})
    );
    if hold_first {
        assert_eq!(held_pending, 1);
    }
    assert!(
        retired,
        "one parent Close must let the other Planner retire before the held Stop returns"
    );
}
