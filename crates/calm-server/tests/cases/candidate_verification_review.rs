//! Bounded A1 review reproductions through production entry points.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_normal_exit_cleans_background_group_before_settlement() {
    let (fx, _, _, publication) = source("sleep 600 & echo $! > background.pid").await;
    schedule(&fx).await;
    let evidence = verified(&fx, &publication).await;
    let pgid = evidence["process"]["pgid"].as_i64().unwrap() as i32;
    let live = calm_server::proc_identity::scan_process_group_members(pgid)
        .into_iter()
        .filter(|m| !m.is_zombie)
        .collect::<Vec<_>>();
    // Always clean this test's exact observed children, including on RED.
    for member in &live {
        if calm_server::proc_identity::read_proc_start_time(member.pid) == Some(member.start_time) {
            unsafe {
                libc::kill(member.pid, libc::SIGKILL);
            }
        }
    }
    assert!(live.is_empty(), "settled with live descendants: {live:?}");
    assert_eq!(evidence["verdict"]["passed"], true);
    assert_eq!(evidence["verdict"]["exit_code"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_withdrawn_green_explains_current_qualification() {
    for change in ["withdrawal", "release"] {
        let (fx, task, _, publication) = source("true").await;
        schedule(&fx).await;
        assert_eq!(verified(&fx, &publication).await["verdict"]["passed"], true);
        let pool = fx.boot.repo.sqlite_pool().unwrap();
        if change == "withdrawal" {
            sqlx::query("UPDATE tasks SET context_stale_at_ms=1 WHERE id=?1")
                .bind(&task.id)
                .execute(&pool)
                .await
                .unwrap();
        } else {
            sqlx::query("UPDATE tracks SET automation_policy='declare-and-wait' WHERE id=?1")
                .bind(&task.track_id)
                .execute(&pool)
                .await
                .unwrap();
        }
        let view = listed(&fx).await;
        let producer = view["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["key"] == "produce")
            .unwrap();
        let delivery = &producer["file_delivery"];
        assert_eq!(delivery["verification"]["passed"], true, "{view}");
        assert_eq!(delivery["qualified"], false);
        assert!(
            delivery["qualification"]["reason"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "{change}: {view}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_track_delete_refuses_unsubmitted_reservation() {
    let (fx, task, _, publication) = source("true").await;
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    sqlx::query("INSERT INTO task_candidate_verification_allocations VALUES(?1,?2,?3)")
        .bind(&publication)
        .bind(&task.track_id)
        .bind(calm_server::model::new_id())
        .execute(&pool)
        .await
        .unwrap();
    let result = fx.boot.repo.track_delete(&task.track_id).await;
    assert!(
        result
            .as_ref()
            .err()
            .is_some_and(|e| e.to_string().contains("candidate verification")),
        "{result:?}"
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM task_candidate_verification_allocations")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    sqlx::query("UPDATE tasks SET context_stale_at_ms=1 WHERE id=?1")
        .bind(&task.id)
        .execute(&pool)
        .await
        .unwrap();
    schedule(&fx).await;
    let op = verification(&fx, &publication).await;
    let result = fx.state.operation_runtime.wait(&op.id).await.unwrap();
    assert!(matches!(result.outcome, OperationOutcome::Failed { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_recovery_retains_capacity_when_leader_missing_group_live() {
    // Start a real owned candidate and retain its actual recorded group identity.
    let (fx, _, _, publication) = source("sleep 600 & echo $! > background.pid; sleep 600").await;
    schedule(&fx).await;
    let op = verification(&fx, &publication).await;
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    let artifacts = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let raw: Option<String> = sqlx::query_scalar(
                "SELECT spawn_artifacts_json FROM operations WHERE id=?1 AND phase='parked'",
            )
            .bind(&op.id)
            .fetch_optional(&pool)
            .await
            .unwrap()
            .flatten();
            if let Some(raw) = raw {
                let a: calm_server::operation::SpawnArtifacts = serde_json::from_str(&raw).unwrap();
                if calm_server::proc_identity::scan_process_group_members(a.pgid).len() >= 3 {
                    break a;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    // Reap the leader from outside its observer, as a restart's new parent can.
    // waitpid competes with the old observer, so hold the journal non-parked
    // until the kernel has actually removed the identity.
    sqlx::query("UPDATE operations SET phase='spawn_started' WHERE id=?1")
        .bind(&op.id)
        .execute(&pool)
        .await
        .unwrap();
    unsafe {
        libc::kill(artifacts.pid, libc::SIGKILL);
        libc::waitpid(artifacts.pid, std::ptr::null_mut(), 0);
    }
    let survivors = calm_server::proc_identity::scan_process_group_members(artifacts.pgid);
    struct Cleanup(Vec<calm_server::proc_identity::GroupMember>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for member in &self.0 {
                if calm_server::proc_identity::read_proc_start_time(member.pid)
                    == Some(member.start_time)
                {
                    unsafe {
                        libc::kill(member.pid, libc::SIGKILL);
                    }
                }
            }
        }
    }
    let _cleanup = Cleanup(survivors.clone());
    assert!(
        survivors.iter().any(|m| !m.is_zombie),
        "must retain a live orphan group"
    );
    sqlx::query(
        "UPDATE operations SET phase='parked',lease_owner=NULL,lease_until_ms=NULL WHERE id=?1",
    )
    .bind(&op.id)
    .execute(&pool)
    .await
    .unwrap();
    fx.state
        .operation_runtime
        .apply_recovery(fx.state.operation_runtime.recover_on_boot().await.unwrap())
        .await
        .unwrap();
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM task_candidate_verification_allocations a LEFT JOIN operations o ON o.operation_key=a.operation_key AND o.kind='candidate-verify' WHERE o.id IS NULL OR o.phase NOT IN ('succeeded','failed')")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(active, 1, "boot must retain unresolved group capacity");
    let live = calm_server::proc_identity::scan_process_group_members(artifacts.pgid);
    assert!(
        live.iter().any(|m| !m.is_zombie),
        "must not signal an unowned group"
    );
    fx.state
        .operation_runtime
        .cancel_parked(&op.id, "test cancellation")
        .await
        .unwrap();
    let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id=?1")
        .bind(&op.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        !matches!(phase.as_str(), "succeeded" | "failed"),
        "cleanup falsely resolved: {phase}"
    );
    assert!(
        calm_server::proc_identity::scan_process_group_members(artifacts.pgid)
            .iter()
            .any(|m| !m.is_zombie)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_consumer_recovery_describes_file_set() {
    let (fx, _, _, publication) = source("true").await;
    declare(&fx.boot, consumer()).await;
    schedule(&fx).await;
    verified(&fx, &publication).await;
    schedule(&fx).await;
    let task = current(&fx.boot, "consume").await;
    assert_eq!(task.status, TaskStatus::Running);
    settle(&fx, &task, false).await;
    let view = listed(&fx).await;
    let consumer = view["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == "consume")
        .unwrap();
    assert_eq!(consumer["recovery"]["allowed"], true, "{view}");
    let reason = consumer["recovery"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("file-set") && !reason.contains("JSON"),
        "{reason}"
    );
}

async fn delete_http(fx: &Fixture, path: &str) -> (axum::http::StatusCode, String) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let app = calm_server::routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(fx.state.clone());
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("DELETE")
                .uri(path)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_deletion_fences_track_area_routes_repos_and_cascades() {
    let (fx, task, _, publication) = source("true").await;
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    sqlx::query("INSERT INTO task_candidate_verification_allocations VALUES(?1,?2,?3)")
        .bind(&publication)
        .bind(&task.track_id)
        .bind(calm_server::model::new_id())
        .execute(&pool)
        .await
        .unwrap();
    for path in [
        format!("/api/tracks/{}", task.track_id),
        format!("/api/areas/{}", fx.boot.area_id),
    ] {
        let (status, body) = delete_http(&fx, &path).await;
        assert_eq!(status, axum::http::StatusCode::CONFLICT, "{path}: {body}");
        assert!(body.contains("candidate verification"), "{body}");
    }
    assert!(
        fx.boot
            .repo
            .track_delete(&task.track_id)
            .await
            .unwrap_err()
            .to_string()
            .contains("candidate verification")
    );
    assert!(
        fx.boot
            .repo
            .area_delete(fx.boot.area_id.as_str())
            .await
            .unwrap_err()
            .to_string()
            .contains("candidate verification")
    );
    for (query, id) in [
        ("DELETE FROM tracks WHERE id=?1", task.track_id.as_str()),
        ("DELETE FROM areas WHERE id=?1", fx.boot.area_id.as_str()),
        (
            "DELETE FROM task_candidate_verification_allocations WHERE track_id=?1",
            task.track_id.as_str(),
        ),
    ] {
        let error = sqlx::query(query)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("candidate verification"),
            "{query}: {error}"
        );
    }
    schedule(&fx).await;
    assert_eq!(verified(&fx, &publication).await["verdict"]["passed"], true);
    // With cleanup resolved, the same route may perform its normal teardown.
    let (status, body) = delete_http(&fx, &format!("/api/tracks/{}", task.track_id)).await;
    assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_canceled_unsubmitted_reservation_replays_without_current_done_task()
{
    let (fx, task, _, publication) = source("true").await;
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    sqlx::query("INSERT INTO task_candidate_verification_allocations VALUES(?1,?2,?3)")
        .bind(&publication)
        .bind(&task.track_id)
        .bind(calm_server::model::new_id())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tasks SET status='canceled' WHERE id=?1")
        .bind(&task.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET lifecycle='canceled' WHERE id=?1")
        .bind(&task.track_id)
        .execute(&pool)
        .await
        .unwrap();
    let scheduler = fx.state.dispatcher.scheduler();
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();
    scheduler.sweep_all().await;
    let op = verification(&fx, &publication).await;
    let result = fx.state.operation_runtime.wait(&op.id).await.unwrap();
    assert!(
        matches!(result.outcome, OperationOutcome::Failed { .. }),
        "{result:?}"
    );
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM task_candidate_verification_allocations a LEFT JOIN operations o ON o.operation_key=a.operation_key AND o.kind='candidate-verify' WHERE o.id IS NULL OR o.phase NOT IN ('succeeded','failed')")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(active, 0);
    let artifacts: Option<String> =
        sqlx::query_scalar("SELECT spawn_artifacts_json FROM operations WHERE id=?1")
            .bind(&op.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        artifacts.is_none(),
        "rejected reservation must not launch checks"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_malformed_evidence_is_read_error_not_unqualified() {
    let (fx, _, _, publication) = source("true").await;
    schedule(&fx).await;
    verified(&fx, &publication).await;
    let op = verification(&fx, &publication).await;
    sqlx::query("UPDATE operations SET tx_output_json='{}' WHERE id=?1")
        .bind(&op.id)
        .execute(&fx.boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let result = crate::mcp_track_report::call_tool(
        &fx.boot,
        "calm.plan.list",
        crate::mcp_track_report::planner_identity(&fx.boot),
        json!({}),
    )
    .await;
    assert!(
        result.is_err(),
        "malformed evidence must surface: {result:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_real_wait_status_overrides_forged_completion_file() {
    let (fx, _, _, publication) = source("printf '0\\n' > ../../gate.exit; kill -KILL $$").await;
    schedule(&fx).await;
    let evidence = verified(&fx, &publication).await;
    assert_eq!(evidence["verdict"]["passed"], false, "{evidence}");
    assert_eq!(evidence["verdict"]["status_detail"], "gate-infra");
    assert!(evidence["verdict"]["exit_code"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_timeout_stops_group_before_releasing_capacity() {
    // A bounded sleeper also self-cleans if a regression prevents observation.
    let (fx, _, _, publication) = source("sleep 25 & wait").await;
    schedule(&fx).await;
    let evidence = verified(&fx, &publication).await;
    assert_eq!(evidence["verdict"]["passed"], false, "{evidence}");
    assert_eq!(evidence["verdict"]["status_detail"], "gate-timeout");
    let pgid = evidence["process"]["pgid"].as_i64().unwrap() as i32;
    assert!(
        calm_server::proc_identity::scan_process_group_members(pgid)
            .iter()
            .all(|m| m.is_zombie)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_verification_parked_delete_preserves_global_count_and_owned_process() {
    let (fx, task, _, publication) = source("sleep 25").await;
    schedule(&fx).await;
    let op = verification(&fx, &publication).await;
    let pool = fx.boot.repo.sqlite_pool().unwrap();
    let artifacts = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let raw: Option<String> = sqlx::query_scalar(
                "SELECT spawn_artifacts_json FROM operations WHERE id=?1 AND phase='parked'",
            )
            .bind(&op.id)
            .fetch_optional(&pool)
            .await
            .unwrap()
            .flatten();
            if let Some(raw) = raw {
                break serde_json::from_str::<calm_server::operation::SpawnArtifacts>(&raw)
                    .unwrap();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    struct Stop(calm_server::operation::SpawnArtifacts);
    impl Drop for Stop {
        fn drop(&mut self) {
            if calm_server::proc_identity::verify_owned_pid(
                self.0.pid,
                self.0.start_time,
                &self.0.boot_id,
            ) {
                calm_server::proc_identity::signal_process_group(self.0.pgid, libc::SIGKILL);
            }
        }
    }
    let stop = Stop(artifacts);
    let (status, body) = delete_http(&fx, &format!("/api/tracks/{}", task.track_id)).await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT, "{body}");
    assert!(calm_server::proc_identity::verify_owned_pid(
        stop.0.pid,
        stop.0.start_time,
        &stop.0.boot_id
    ));
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM task_candidate_verification_allocations a LEFT JOIN operations o ON o.operation_key=a.operation_key AND o.kind='candidate-verify' WHERE o.id IS NULL OR o.phase NOT IN ('succeeded','failed')")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(active, 1);
    fx.state
        .operation_runtime
        .cancel_parked(&op.id, "test owned cancellation")
        .await
        .unwrap();
    let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id=?1")
        .bind(&op.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(phase, "failed");
    assert!(
        calm_server::proc_identity::scan_process_group_members(stop.0.pgid)
            .iter()
            .all(|m| m.is_zombie)
    );
}
