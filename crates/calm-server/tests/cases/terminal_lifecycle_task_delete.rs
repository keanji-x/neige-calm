use super::*;

// Deleting an execution card must settle its active task in the same transaction.
#[tokio::test]
async fn card_delete_settles_active_tasks_and_preserves_other_tasks() {
    let state = fresh_state().await;
    let raw = state.raw_repo();
    let area = raw
        .area_create(NewArea {
            name: "delete-task".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = raw
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "delete-task".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = raw
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    let pool = raw.sqlite_pool().unwrap();
    for (key, status, card_id) in [
        ("running", "running", card.id.as_str()),
        ("dispatched", "dispatched", card.id.as_str()),
        ("completed", "done", card.id.as_str()),
        ("failed", "failed", card.id.as_str()),
        ("pending", "pending", card.id.as_str()),
        ("verifying", "verifying", card.id.as_str()),
        ("canceled", "canceled", card.id.as_str()),
        ("other", "running", "another-card"),
    ] {
        sqlx::query("INSERT INTO tasks
            (id,track_id,key,kind,goal,context_json,status,worker_card_id,declared_by,created_at_ms,updated_at_ms)
            VALUES (?1,?2,?3,'codex','test','[]',?4,?5,'user',1,1)")
            .bind(format!("{}:{key}", track.id)).bind(track.id.as_str()).bind(key)
            .bind(status).bind(card_id).execute(&pool).await.unwrap();
    }
    // A released recovery allocation preserves its failed predecessor.
    use calm_types::task_recovery::{TaskAttemptOrigin, TaskRecoveryConstraint};
    let next_id = format!("{}:failed:next", track.id);
    let origin = TaskAttemptOrigin::Recovery {
        previous_attempt_id: format!("{}:failed", track.id),
        idempotency_key: "next-allocation".into(),
        request_fingerprint: "test".into(),
        reason: "historical allocation fixture".into(),
        actor: calm_server::ids::ActorId::Kernel,
        constraint: TaskRecoveryConstraint::V1 {
            spawn: calm_types::task_recovery::TASK_IN_TRACK_ROUTE.into(),
            declared_by: "user".into(),
            refs: vec![calm_types::event::TaskContextRef {
                track_id: track.id.clone(),
                block_id: "root".into(),
                rev: 1,
                hash: "a".repeat(64),
                is_root: true,
            }],
        },
    };
    sqlx::query(
        "INSERT INTO task_attempt_allocations
        (attempt_id,track_id,key,generation,origin_json,created_at_ms)
        VALUES (?1,?2,'failed',2,?3,2)",
    )
    .bind(&next_id)
    .bind(track.id.as_str())
    .bind(serde_json::to_string(&origin).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO tasks
        (id,track_id,key,kind,goal,context_json,status,worker_card_id,declared_by,created_at_ms,updated_at_ms)
        VALUES (?1,?2,'failed','codex','test','[]','running','another-card','user',2,2)")
        .bind(&next_id).bind(track.id.as_str()).execute(&pool).await.unwrap();
    let current = raw
        .task_current_get(track.id.as_str(), "failed")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.id, next_id);
    // The spawn operation owns the card before the scheduler stamps the task.
    for key in ["unstamped", "unrelated-unstamped"] {
        sqlx::query(
            "INSERT INTO tasks
            (id,track_id,key,kind,goal,context_json,status,declared_by,created_at_ms,updated_at_ms)
            VALUES (?1,?2,?3,'codex','test','[]','dispatched','user',1,1)",
        )
        .bind(format!("{}:{key}", track.id))
        .bind(track.id.as_str())
        .bind(key)
        .execute(&pool)
        .await
        .unwrap();
    }
    // #2493: each in-flight attempt of the card runs in a session of it, bound when its spawn
    // prepared (the bind stamps `worker_card_id` with it, so no row is unstamped any more).
    for key in ["running", "dispatched", "unstamped"] {
        calm_server::test_seams::bind_task_to_card_for_test(
            &pool,
            &format!("{}:{key}", track.id),
            card.id.as_str(),
        )
        .await
        .unwrap();
    }
    let app = build_app(state.clone());
    // A failed deletion must roll back task status and its events as well.
    sqlx::query("CREATE TRIGGER refuse_card_delete BEFORE DELETE ON cards BEGIN SELECT RAISE(ABORT, 'test delete failure'); END")
        .execute(&pool).await.unwrap();
    let refused = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/cards/{}", card.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tasks WHERE worker_card_id=?1 AND status IN ('running','dispatched')",
    )
    .bind(card.id.as_str())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active, 3);
    let events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM events WHERE kind IN ('task.failed','card.deleted')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(events, 0);
    sqlx::query("DROP TRIGGER refuse_card_delete")
        .execute(&pool)
        .await
        .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/cards/{}", card.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    for (key, expected) in [
        ("running", "failed"),
        ("dispatched", "failed"),
        ("completed", "done"),
        ("failed", "failed"),
        ("pending", "pending"),
        ("verifying", "verifying"),
        ("canceled", "canceled"),
        ("unstamped", "failed"),
        ("unrelated-unstamped", "dispatched"),
        ("other", "running"),
        ("failed:next", "running"),
    ] {
        let row: (String, Option<String>, Option<i64>) =
            sqlx::query_as("SELECT status,status_detail,finished_at_ms FROM tasks WHERE id=?1")
                .bind(format!("{}:{key}", track.id))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.0, expected, "task {key}");
        if matches!(key, "running" | "dispatched" | "unstamped") {
            assert!(row.1.unwrap().starts_with("worker-card-deleted"));
            assert!(row.2.is_some());
        } else {
            assert_eq!(row.1, None);
            assert_eq!(row.2, None);
        }
    }
    let failures: Vec<(String, String, String)> = sqlx::query_as("SELECT json_extract(payload,'$.idempotency_key'),actor,scope_track FROM events WHERE kind='task.failed' ORDER BY id")
        .fetch_all(&pool).await.unwrap();
    assert_eq!(failures.len(), 3);
    for (task_id, actor, scope) in failures {
        assert!(
            task_id.ends_with(":running")
                || task_id.ends_with(":dispatched")
                || task_id.ends_with(":unstamped")
        );
        assert_eq!(
            serde_json::from_str::<calm_server::ids::ActorId>(&actor).unwrap(),
            calm_server::ids::ActorId::KernelDispatcher
        );
        assert_eq!(scope, track.id.as_str());
    }
    let delete_event: i64 = sqlx::query_scalar("SELECT id FROM events WHERE kind='card.deleted'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let last_failure: i64 =
        sqlx::query_scalar("SELECT MAX(id) FROM events WHERE kind='task.failed'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(last_failure < delete_event);
    let repeat = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/cards/{}", card.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(repeat.status(), StatusCode::NOT_FOUND);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind='task.failed'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 3);
}
