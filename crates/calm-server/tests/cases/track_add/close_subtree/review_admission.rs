//! Regression cases from the two independent L2 reviews.
use super::*;

#[tokio::test]
async fn child_self_close_cannot_cancel_a_returned_assignment_under_verification() {
    let boot = boot(16).await;
    let (parent, _) = boot.user_track("parent").await;
    let (child, planner) = child_of(&boot, &parent, "child").await;
    task(
        &boot,
        &parent,
        "assignment",
        "running",
        "sub-track",
        Some(&child),
    )
    .await;
    boot.activate_caller(&planner).await;
    boot.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE tasks SET status='verifying' WHERE id=?1")
                .bind(format!("{parent}:assignment")),
        )
        .await
        .unwrap();
    boot.repo
        .db()
        .writer()
        .execute(sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1").bind(&child))
        .await
        .unwrap();
    let result = boot.registry.lookup(TOOL_TRACK_CLOSE).unwrap()(
        boot.ctx.clone(),
        planner.identity,
        json!({"message":"repeat close after return"}),
    )
    .await;
    assert!(
        result.is_err(),
        "a child cannot decide the Parent's verification outcome"
    );
    assert_eq!(
        boot.repo
            .task_get(&format!("{parent}:assignment"))
            .await
            .unwrap()
            .unwrap()
            .status,
        calm_server::model::TaskStatus::Verifying
    );
}

#[tokio::test]
async fn direct_child_close_refuses_ambiguous_external_assignments() {
    let boot = boot(16).await;
    let (parent, _) = boot.user_track("parent").await;
    let (child, _) = child_of(&boot, &parent, "child").await;
    for key in ["first", "second"] {
        task(&boot, &parent, key, "running", "sub-track", Some(&child)).await;
    }
    assert_eq!(rest_close(&boot, &child).await, StatusCode::CONFLICT);
    assert!(open(&boot, &child).await);
    for key in ["first", "second"] {
        assert_eq!(
            boot.repo
                .task_get(&format!("{parent}:{key}"))
                .await
                .unwrap()
                .unwrap()
                .status,
            calm_server::model::TaskStatus::Running
        );
    }
}

#[tokio::test]
async fn creator_provenance_cannot_grant_cross_area_close() {
    let boot = boot(16).await;
    let (admin, caller) = boot.user_track("admin").await;
    let (outside, _) = boot.user_track_admin("outside", false).await;
    let area = boot
        .repo
        .area_create(
            LEGACY_OWNER_USER_ID,
            NewArea {
                name: "outside".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    boot.repo.db().writer().execute(
        sqlx::query("UPDATE tracks SET area_id=?1,creator_track_id=?2,creator_key='created' WHERE id=?3")
            .bind(area.id.as_str()).bind(&admin).bind(&outside),
    ).await.unwrap();
    task(
        &boot,
        &outside,
        "pending",
        "pending",
        TASK_IN_TRACK_ROUTE,
        None,
    )
    .await;
    let result = boot.registry.lookup(TOOL_TRACK_CLOSE).unwrap()(
        boot.ctx.clone(),
        caller.identity,
        json!({"track_id":outside,"message":"close created target"}),
    )
    .await;
    let error = result.expect_err("creator provenance cannot authorize another Area");
    assert_eq!(error.code, -32404);
    assert!(open(&boot, &outside).await);
    assert_eq!(
        boot.repo
            .task_get(&format!("{outside}:pending"))
            .await
            .unwrap()
            .unwrap()
            .status,
        calm_server::model::TaskStatus::Pending
    );
}

#[tokio::test]
async fn close_refuses_a_live_planner_with_a_broken_reverse_binding() {
    let boot = boot(16).await;
    let (track, planner) = boot.user_track("broken-planner").await;
    boot.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE worker_sessions SET card_id=NULL WHERE id=?1")
                .bind(&planner.identity.session_id),
        )
        .await
        .unwrap();
    assert_eq!(rest_close(&boot, &track).await, StatusCode::CONFLICT);
    assert!(open(&boot, &track).await);
    assert_eq!(
        boot.count("SELECT count(*) FROM track_close_cleanups")
            .await,
        0
    );
}

#[tokio::test]
async fn close_refuses_a_cleanup_marker_with_a_different_card() {
    let boot = boot(16).await;
    let (track, planner) = boot.user_track("planner").await;
    let (_, outside) = boot.user_track("outside").await;
    boot.repo.db().writer().execute(sqlx::query(
        "INSERT INTO track_close_cleanups(worker_session_id,card_id,track_id,requested_at_ms) VALUES(?1,?2,?3,1)",
    ).bind(&planner.identity.session_id).bind(&outside.identity.card_id).bind(&track)).await.unwrap();
    assert_eq!(rest_close(&boot, &track).await, StatusCode::CONFLICT);
    assert!(open(&boot, &track).await);
}

#[tokio::test]
async fn close_refuses_a_worker_native_thread_owned_by_another_live_worker() {
    let boot = boot(16).await;
    let (root, _) = boot.user_track("root").await;
    let (outside, _) = boot.user_track("outside").await;
    let first = running_worker(&boot, &root).await;
    let second = running_worker(&boot, &outside).await;
    boot.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE worker_sessions SET thread_id=?1 WHERE id=?2")
                .bind(&second.identity.thread_id)
                .bind(&first.identity.session_id),
        )
        .await
        .unwrap();
    assert_eq!(rest_close(&boot, &root).await, StatusCode::CONFLICT);
    assert!(open(&boot, &root).await && open(&boot, &outside).await);
    for track in [&root, &outside] {
        assert_eq!(
            boot.repo
                .task_get(&format!("{track}:work"))
                .await
                .unwrap()
                .unwrap()
                .status,
            calm_server::model::TaskStatus::Running
        );
    }
}
