//! Close through the actual REST and authorized MCP entry points.
use super::*;
use calm_server::mcp_server::tools::track_state::TOOL_TRACK_CLOSE;
use calm_types::task_recovery::TASK_IN_TRACK_ROUTE;

async fn running_worker(boot: &Boot, track: &str) -> Caller {
    let worker = boot.worker_on(track).await;
    boot.role_cache.insert(
        worker.identity.card_id.clone().into(),
        CardRole::Worker,
        track.to_owned().into(),
    );
    boot.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE cards SET session_id=?1 WHERE id=?2")
                .bind(&worker.identity.session_id)
                .bind(&worker.identity.card_id),
        )
        .await
        .unwrap();
    let turn = format!("turn-{}", worker.identity.session_id);
    boot.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE worker_sessions SET thread_id=?2,active_turn_id=?3 WHERE id=?1")
                .bind(&worker.identity.session_id)
                .bind(&worker.identity.thread_id)
                .bind(&turn),
        )
        .await
        .unwrap();
    boot.codex
        .set_active_turn_for_test(&worker.identity.thread_id, &turn);
    boot.repo.db().writer().execute(sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id,created_at_ms,updated_at_ms) \
         VALUES(?1,?2,'work','codex','work','{}','running',?3,1,1)",
    ).bind(format!("{track}:work")).bind(track).bind(&worker.identity.card_id)).await.unwrap();
    worker
}

async fn assert_canceled(boot: &Boot, track: &str, worker: &Caller) {
    let status: String = boot
        .scalar("SELECT status FROM tasks WHERE track_id=?1", track)
        .await;
    assert_eq!(status, "canceled");
    let marker: Option<String> = boot.scalar(
        "SELECT json_extract(handle_state_json,'$.timeout_cleanup.task_id') FROM worker_sessions WHERE id=?1",
        &worker.identity.session_id,
    ).await;
    let stopped: bool = boot
        .scalar(
            "SELECT state IN ('exited','failed','superseded') FROM worker_sessions WHERE id=?1",
            &worker.identity.session_id,
        )
        .await;
    assert!(marker.as_deref() == Some(format!("{track}:work").as_str()) || stopped);
}

#[tokio::test]
async fn close_cancels_running_rest() {
    let boot = boot(16).await;
    let (track, _) = boot.user_track("close-rest").await;
    let worker = running_worker(&boot, &track).await;
    let (status, body) = send(
        boot.app.clone(),
        "PATCH",
        &format!("/api/tracks/{track}"),
        Some(json!({"closed":true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_canceled(&boot, &track, &worker).await;
}

#[tokio::test]
async fn close_cancels_running_own_mcp() {
    let boot = boot(16).await;
    let (track, planner) = boot.user_track("close-mcp").await;
    let worker = running_worker(&boot, &track).await;
    boot.registry.lookup(TOOL_TRACK_CLOSE).unwrap()(
        boot.ctx.clone(),
        planner.identity,
        json!({"message":"stop this work"}),
    )
    .await
    .unwrap();
    assert_canceled(&boot, &track, &worker).await;
}

async fn child_of(boot: &Boot, parent: &str, title: &str) -> (String, Caller) {
    let (child, planner) = boot.user_track_admin(title, false).await;
    boot.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2")
                .bind(parent)
                .bind(&child),
        )
        .await
        .unwrap();
    (child, planner)
}

async fn task(boot: &Boot, track: &str, key: &str, status: &str, spawn: &str, child: Option<&str>) {
    boot.repo.db().writer().execute(sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,spawn,child_track_id,created_at_ms,updated_at_ms) \
         VALUES(?1,?2,?3,'codex','goal','{}',?4,?5,?6,1,1)",
    ).bind(format!("{track}:{key}")).bind(track).bind(key).bind(status).bind(spawn).bind(child)).await.unwrap();
}

async fn rest_close(boot: &Boot, track: &str) -> StatusCode {
    send(
        boot.app.clone(),
        "PATCH",
        &format!("/api/tracks/{track}"),
        Some(json!({"closed":true})),
    )
    .await
    .0
}

async fn open(boot: &Boot, track: &str) -> bool {
    boot.repo.track_get(track).await.unwrap().unwrap().is_open()
}

#[tokio::test]
async fn close_closes_descendants_rest_through_closed_middle_without_touching_other_tracks() {
    let boot = boot(16).await;
    let (parent, _) = boot.user_track("outside-parent").await;
    let (root, _) = child_of(&boot, &parent, "root").await;
    let (middle, _) = child_of(&boot, &root, "middle").await;
    let (leaf, _) = child_of(&boot, &middle, "leaf").await;
    let (sibling, _) = child_of(&boot, &parent, "sibling").await;
    let parent_worker = running_worker(&boot, &parent).await;
    task(
        &boot,
        &sibling,
        "outside",
        "pending",
        TASK_IN_TRACK_ROUTE,
        None,
    )
    .await;
    let (created, _) = boot.user_track_admin("creator-is-not-parent", false).await;
    boot.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE tracks SET creator_track_id=?1,creator_key='separate-provenance' WHERE id=?2")
                .bind(&root)
                .bind(&created),
        )
        .await
        .unwrap();
    boot.repo
        .db()
        .writer()
        .execute(sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1").bind(&middle))
        .await
        .unwrap();
    task(
        &boot,
        &leaf,
        "pending",
        "pending",
        TASK_IN_TRACK_ROUTE,
        None,
    )
    .await;
    task(
        &boot,
        &parent,
        "assignment",
        "running",
        "sub-track",
        Some(&root),
    )
    .await;
    assert_eq!(rest_close(&boot, &root).await, StatusCode::OK);
    for member in [&root, &middle, &leaf] {
        assert!(!open(&boot, member).await, "{member}");
    }
    for outside in [&parent, &sibling, &created] {
        assert!(open(&boot, outside).await, "{outside}");
    }
    assert_eq!(
        boot.repo
            .task_get(&format!("{parent}:work"))
            .await
            .unwrap()
            .unwrap()
            .status,
        calm_server::model::TaskStatus::Running
    );
    let marker: Option<String> = boot.scalar(
        "SELECT json_extract(handle_state_json,'$.timeout_cleanup') FROM worker_sessions WHERE id=?1",
        &parent_worker.identity.session_id,
    ).await;
    assert_eq!(marker, None);
    assert_eq!(
        boot.repo
            .task_get(&format!("{sibling}:outside"))
            .await
            .unwrap()
            .unwrap()
            .status,
        calm_server::model::TaskStatus::Pending
    );
    for id in [format!("{leaf}:pending"), format!("{parent}:assignment")] {
        assert_eq!(
            boot.repo.task_get(&id).await.unwrap().unwrap().status,
            calm_server::model::TaskStatus::Canceled
        );
    }
    assert_eq!(
        boot.count("SELECT count(*) FROM events WHERE kind='task.completed'")
            .await,
        0
    );
}

#[tokio::test]
async fn close_closes_descendants_uses_parent_edges_and_spares_creator_only_tracks() {
    let boot = boot(16).await;
    let (root, _) = boot.user_track("root").await;
    let (created, _) = boot.user_track_admin("separate-created-track", false).await;
    boot.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE tracks SET creator_track_id=?1,creator_key='separate' WHERE id=?2")
                .bind(&root)
                .bind(&created),
        )
        .await
        .unwrap();
    task(
        &boot,
        &created,
        "outside",
        "pending",
        TASK_IN_TRACK_ROUTE,
        None,
    )
    .await;
    assert_eq!(rest_close(&boot, &root).await, StatusCode::OK);
    assert!(open(&boot, &created).await);
    assert_eq!(
        boot.repo
            .task_get(&format!("{created}:outside"))
            .await
            .unwrap()
            .unwrap()
            .status,
        calm_server::model::TaskStatus::Pending
    );
}

#[tokio::test]
async fn close_closes_descendants_own_mcp_cancels_every_level_and_refuses_late_handback() {
    let boot = boot(16).await;
    let (root, planner) = boot.user_track("root").await;
    let (child, child_planner) = child_of(&boot, &root, "child").await;
    let (leaf, _) = child_of(&boot, &child, "leaf").await;
    task(&boot, &root, "child", "running", "sub-track", Some(&child)).await;
    task(
        &boot,
        &child,
        "leaf",
        "dispatched",
        "sub-track",
        Some(&leaf),
    )
    .await;
    let root_worker = running_worker(&boot, &root).await;
    let child_worker = running_worker(&boot, &child).await;
    let worker = running_worker(&boot, &leaf).await;
    boot.registry.lookup(TOOL_TRACK_CLOSE).unwrap()(
        boot.ctx.clone(),
        planner.identity,
        json!({"message":"stop the subtree"}),
    )
    .await
    .unwrap();
    for member in [&root, &child, &leaf] {
        assert!(!open(&boot, member).await);
    }
    assert_canceled(&boot, &leaf, &worker).await;
    assert_canceled(&boot, &root, &root_worker).await;
    assert_canceled(&boot, &child, &child_worker).await;
    for name in ["neige_task_done", "neige_task_fail"] {
        let args = if name == "neige_task_done" {
            json!({"attempt_id":format!("{root}:child")})
        } else {
            json!({"attempt_id":format!("{root}:child"),"reason":"late"})
        };
        let error = boot.registry.lookup(name).unwrap()(
            boot.ctx.clone(),
            child_planner.identity.clone(),
            args,
        )
        .await
        .unwrap_err();
        assert!(matches!(error.code, -32403 | -32409), "{error:?}");
    }
    for id in [format!("{root}:child"), format!("{child}:leaf")] {
        assert_eq!(
            boot.repo.task_get(&id).await.unwrap().unwrap().status,
            calm_server::model::TaskStatus::Canceled
        );
    }
    assert_eq!(
        boot.count("SELECT count(*) FROM events WHERE kind='task.completed'")
            .await,
        0
    );
}

#[tokio::test]
async fn close_closes_descendants_already_closed_parent_and_is_idempotent() {
    let boot = boot(16).await;
    let (root, _) = boot.user_track("root").await;
    let (child, _) = child_of(&boot, &root, "child").await;
    boot.repo
        .db()
        .writer()
        .execute(sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1").bind(&root))
        .await
        .unwrap();
    assert_eq!(rest_close(&boot, &root).await, StatusCode::OK);
    assert!(!open(&boot, &child).await);
    let events = boot
        .count("SELECT count(*) FROM events WHERE kind IN ('track.updated','plan.updated')")
        .await;
    assert_eq!(rest_close(&boot, &root).await, StatusCode::OK);
    assert_eq!(
        boot.count("SELECT count(*) FROM events WHERE kind IN ('track.updated','plan.updated')")
            .await,
        events
    );
}

#[tokio::test]
async fn close_closes_descendants_concurrent_closes_have_one_event_per_member() {
    let boot = boot(16).await;
    let (root, _) = boot.user_track("root").await;
    let (child, _) = child_of(&boot, &root, "child").await;
    let before = boot
        .count("SELECT count(*) FROM events WHERE kind='track.updated'")
        .await;
    let (a, b) = tokio::join!(rest_close(&boot, &root), rest_close(&boot, &root));
    assert_eq!((a, b), (StatusCode::OK, StatusCode::OK));
    assert!(!open(&boot, &child).await);
    assert_eq!(
        boot.count("SELECT count(*) FROM events WHERE kind='track.updated'")
            .await,
        before + 2
    );
}

#[tokio::test]
async fn close_refuses_uncancellable_work_rolls_back_every_member_and_cleanup() {
    for (status, spawn) in [
        ("dispatched", TASK_IN_TRACK_ROUTE),
        ("verifying", TASK_IN_TRACK_ROUTE),
        ("running", TASK_IN_TRACK_ROUTE),
        ("running", "unknown"),
    ] {
        let boot = boot(16).await;
        let (root, _) = boot.user_track("root").await;
        let (child, _) = child_of(&boot, &root, "child").await;
        let worker = running_worker(&boot, &root).await;
        task(&boot, &child, "unsafe", status, spawn, None).await;
        let events = boot
            .count("SELECT count(*) FROM events WHERE kind IN ('track.updated','plan.updated')")
            .await;
        assert_eq!(
            rest_close(&boot, &root).await,
            StatusCode::CONFLICT,
            "{status}/{spawn}"
        );
        assert!(open(&boot, &root).await && open(&boot, &child).await);
        assert_eq!(
            boot.repo
                .task_get(&format!("{root}:work"))
                .await
                .unwrap()
                .unwrap()
                .status,
            calm_server::model::TaskStatus::Running
        );
        let marker: Option<String> = boot.scalar("SELECT json_extract(handle_state_json,'$.timeout_cleanup.task_id') FROM worker_sessions WHERE id=?1", &worker.identity.session_id).await;
        assert_eq!(marker, None);
        assert_eq!(
            boot.count("SELECT count(*) FROM track_close_cleanups")
                .await,
            0
        );
        assert_eq!(
            boot.count(
                "SELECT count(*) FROM events WHERE kind IN ('track.updated','plan.updated')"
            )
            .await,
            events
        );
    }
}

#[tokio::test]
async fn close_refuses_uncancellable_damaged_tree_without_partial_changes() {
    for damage in ["cycle", "deep", "wide", "area"] {
        let boot = boot(16).await;
        let (root, _) = boot.user_track("root").await;
        let (child, _) = child_of(&boot, &root, "child").await;
        match damage {
            "cycle" => {
                boot.repo
                    .db()
                    .writer()
                    .execute(
                        sqlx::query("UPDATE tracks SET parent_track_id=?1,admin_granted_at=NULL WHERE id=?2")
                            .bind(&child)
                            .bind(&root),
                    )
                    .await
                    .unwrap();
            }
            "deep" => {
                let (two, _) = child_of(&boot, &child, "two").await;
                let (three, _) = child_of(&boot, &two, "three").await;
                child_of(&boot, &three, "four").await;
            }
            "wide" => {
                boot.repo.db().writer().execute(sqlx::query(
                    "WITH RECURSIVE n(i) AS (VALUES(0) UNION ALL SELECT i+1 FROM n WHERE i<63) \
                     INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at,parent_track_id) \
                     SELECT ?1||'-wide-'||i,?2,'wide',0,1,1,?1 FROM n",
                ).bind(&root).bind(&boot.area_id)).await.unwrap();
            }
            "area" => {
                let area = boot
                    .repo
                    .area_create(
                        LEGACY_OWNER_USER_ID,
                        NewArea {
                            name: "other".into(),
                            color: "#000".into(),
                            sort: None,
                        },
                    )
                    .await
                    .unwrap();
                boot.repo
                    .db()
                    .writer()
                    .execute(
                        sqlx::query("UPDATE tracks SET area_id=?1 WHERE id=?2")
                            .bind(area.id.as_str())
                            .bind(&child),
                    )
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(3), rest_close(&boot, &root))
                .await
                .unwrap(),
            StatusCode::CONFLICT,
            "{damage}"
        );
        assert!(open(&boot, &root).await && open(&boot, &child).await);
    }
}

#[tokio::test]
async fn close_cancels_running_reopening_keeps_attempts_canceled() {
    let boot = boot(16).await;
    let (root, _) = boot.user_track("root").await;
    running_worker(&boot, &root).await;
    task(&boot, &root, "pending", "pending", "sub-track", None).await;
    assert_eq!(rest_close(&boot, &root).await, StatusCode::OK);
    assert_eq!(
        send(
            boot.app.clone(),
            "PATCH",
            &format!("/api/tracks/{root}"),
            Some(json!({"closed":false}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let states: Vec<String> = sqlx::query_scalar("SELECT status FROM tasks WHERE track_id=?1")
        .bind(&root)
        .fetch_all(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(states, vec!["canceled", "canceled"]);
}

#[tokio::test]
async fn close_cancels_running_child_gate_result_cannot_replace_canceled_assignment() {
    let boot = boot(16).await;
    let (parent, _) = boot.user_track("parent").await;
    let (child, _) = child_of(&boot, &parent, "child").await;
    task(
        &boot,
        &parent,
        "assignment",
        "verifying",
        "sub-track",
        Some(&child),
    )
    .await;
    assert_eq!(rest_close(&boot, &child).await, StatusCode::OK);
    let id = format!("{parent}:assignment");
    let late = id.clone();
    let moved =
        calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
            Box::pin(async move {
                Ok(calm_truth::db::sqlite::task_apply_gate_result_tx(
                    tx, &late, 0, true, None, "{}", 2,
                )
                .await?)
            })
        })
        .await
        .unwrap();
    assert_eq!(moved, 0);
    assert_eq!(
        boot.repo.task_get(&id).await.unwrap().unwrap().status,
        calm_server::model::TaskStatus::Canceled
    );
    assert!(open(&boot, &parent).await);
}

#[tokio::test]
async fn close_refuses_uncancellable_foreign_child_and_worker_bindings() {
    for binding in ["child", "worker", "child-worker"] {
        let boot = boot(16).await;
        let (root, _) = boot.user_track("root").await;
        let (outside, _) = boot.user_track("outside").await;
        if binding == "child" {
            task(
                &boot,
                &root,
                "foreign",
                "running",
                "sub-track",
                Some(&outside),
            )
            .await;
        } else {
            let worker = running_worker(&boot, &outside).await;
            let child = if binding == "child-worker" {
                Some(child_of(&boot, &root, "bound-child").await.0)
            } else {
                None
            };
            task(
                &boot,
                &root,
                "foreign",
                "running",
                if child.is_some() {
                    "sub-track"
                } else {
                    TASK_IN_TRACK_ROUTE
                },
                child.as_deref(),
            )
            .await;
            boot.repo
                .db()
                .writer()
                .execute(
                    sqlx::query("UPDATE tasks SET worker_card_id=?1 WHERE track_id=?2")
                        .bind(&worker.identity.card_id)
                        .bind(&root),
                )
                .await
                .unwrap();
        }
        assert_eq!(
            rest_close(&boot, &root).await,
            StatusCode::CONFLICT,
            "{binding}"
        );
        assert!(open(&boot, &root).await && open(&boot, &outside).await);
    }
}

#[path = "close_subtree/lifecycle.rs"]
mod lifecycle;
#[path = "close_subtree/workers.rs"]
mod workers;
