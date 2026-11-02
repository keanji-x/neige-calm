use super::*;

async fn failed_child(boot: &Boot, key: &str) -> (String, String, String) {
    failed_child_for_provider(boot, key, AgentProvider::Codex).await
}

async fn failed_child_for_provider(
    boot: &Boot,
    key: &str,
    provider: AgentProvider,
) -> (String, String, String) {
    let (task, child) = seed_child_parent(boot, key, false, None).await;
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: child.clone().into(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"planner_provider":provider}),
        })
        .await
        .unwrap();
    let session = format!("{key}-planner");
    let db = boot.repo.sqlite_db().unwrap();
    db.writer()
        .execute(sqlx::query("UPDATE cards SET role='planner' WHERE id=?1").bind(card.id.as_str()))
        .await
        .unwrap();
    let card_id = card.id.to_string();
    let session_id = session.clone();
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            session_start_runtime_tx(
                tx,
                WorkerSessionInit {
                    id: session_id.clone(),
                    card_id,
                    kind: WorkerSessionKind::SharedPlanner,
                    agent_provider: Some(provider),
                    status: WorkerSessionState::Idle,
                    terminal_run_id: None,
                    thread_id: Some(format!("thread-{session_id}")),
                    session_id: None,
                    active_turn_id: None,
                    handle_state_json: None,
                    spawn_op_id: None,
                    now_ms: now_ms(),
                },
            )
            .await?;
            // This real production failure writer deliberately emits no live event.
            calm_truth::db::sqlite::session_fail_if_active_runtime_tx(tx, &session_id).await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    (task, child, session)
}

#[tokio::test]
async fn child_failure_notice_reverse_scan_without_live_event_survives_restart() {
    let boot = boot().await;
    let (task, child, _) = failed_child(&boot, "failed-child").await;
    let (_runtime, scheduler) =
        build_scheduler_unbooted(&boot, vec![], Arc::new(tokio::sync::Semaphore::new(0)));
    scheduler.mark_boot_sweep_complete();
    scheduler.sweep_all().await;
    let wakes = event_rows(&boot, "track.wake_requested").await;
    assert_eq!(
        wakes.len(),
        1,
        "a failed current child Planner must durably notify its parent"
    );
    assert_eq!(wakes[0].1["track_id"], boot.track_id.as_str());
    assert!(wakes[0].1["text"].as_str().unwrap().contains(&task));
    assert_eq!(
        boot.repo.task_get(&task).await.unwrap().unwrap().status,
        TaskStatus::Running
    );
    assert!(
        boot.repo
            .track_get(&child)
            .await
            .unwrap()
            .unwrap()
            .is_open()
    );
    let (_runtime2, scheduler2) = build_scheduler(&boot, vec![]);
    scheduler2.sweep_progress_obligations().await.unwrap();
    assert_eq!(event_rows(&boot, "track.wake_requested").await.len(), 1);
}

#[tokio::test]
async fn child_failure_notice_ignores_idle_closed_rebound_and_invalid_contract() {
    let boot = boot().await;
    let db = boot.repo.sqlite_db().unwrap();
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    for case in [
        "idle",
        "closed",
        "parent-closed",
        "card-rebound",
        "root-rebound",
        "provider",
        "contract",
        "canceled",
    ] {
        let (task, child, session) = failed_child(&boot, case).await;
        let query = match case {
            "idle" => sqlx::query("UPDATE worker_sessions SET state='idle' WHERE id=?1").bind(&session),
            "closed" => sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1").bind(&child),
            "parent-closed" => sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1").bind(boot.track_id.as_str()),
            "card-rebound" => sqlx::query("UPDATE cards SET session_id=NULL WHERE session_id=?1").bind(&session),
            "root-rebound" => sqlx::query("UPDATE tracks SET root_session_id=NULL WHERE id=?1").bind(&child),
            "provider" => sqlx::query("UPDATE cards SET payload=json_set(payload,'$.planner_provider','claude') WHERE session_id=?1").bind(&session),
            "contract" => sqlx::query("UPDATE worker_sessions SET contract='executor' WHERE id=?1").bind(&session),
            "canceled" => sqlx::query("UPDATE tasks SET status='canceled' WHERE id=?1").bind(&task),
            _ => unreachable!(),
        };
        db.writer().execute(query).await.unwrap();
        scheduler.sweep_progress_obligations().await.unwrap();
        assert!(
            event_rows(&boot, "track.wake_requested").await.is_empty(),
            "{case} has no wake authority"
        );
        if case == "parent-closed" {
            db.writer()
                .execute(
                    sqlx::query("UPDATE tracks SET closed_at=NULL WHERE id=?1")
                        .bind(boot.track_id.as_str()),
                )
                .await
                .unwrap();
            // Keep this previous child outside the scope of later loop iterations.
            db.writer()
                .execute(sqlx::query("UPDATE tasks SET status='canceled' WHERE id=?1").bind(&task))
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn child_failure_notice_event_failure_rolls_back_and_retries_once() {
    let boot = boot().await;
    let (_task, _, _) = failed_child(&boot, "rollback-failed-child").await;
    let db = boot.repo.sqlite_db().unwrap();
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    db.writer()
        .execute(sqlx::query(
            "CREATE TRIGGER reject_child_failure_notice BEFORE INSERT ON events \
        WHEN NEW.kind='track.wake_requested' BEGIN SELECT RAISE(ABORT,'notice unavailable'); END",
        ))
        .await
        .unwrap();
    assert!(scheduler.sweep_progress_obligations().await.is_err());
    db.writer()
        .execute(sqlx::query("DROP TRIGGER reject_child_failure_notice"))
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        scheduler.sweep_progress_obligations(),
        scheduler.sweep_progress_obligations()
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(event_rows(&boot, "track.wake_requested").await.len(), 1);
}

#[tokio::test]
async fn child_failure_notice_same_session_recovery_has_fresh_failure_identity() {
    let boot = boot().await;
    let (_, _, session) = failed_child(&boot, "recover-failed-child").await;
    let db = boot.repo.sqlite_db().unwrap();
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    scheduler.sweep_progress_obligations().await.unwrap();
    let first = event_rows(&boot, "track.wake_requested").await;
    assert_eq!(first.len(), 1);
    // Failed snapshots and timestamps are not new failures.
    db.writer().execute(sqlx::query("UPDATE worker_sessions SET state='failed',updated_at_ms=updated_at_ms+1000 WHERE id=?1").bind(&session)).await.unwrap();
    scheduler.sweep_progress_obligations().await.unwrap();
    assert_eq!(event_rows(&boot, "track.wake_requested").await.len(), 1);
    // Explicit recovery can reuse the carrier; the next failure must not re-arm its old obligation.
    db.writer()
        .execute(sqlx::query("UPDATE worker_sessions SET state='idle' WHERE id=?1").bind(&session))
        .await
        .unwrap();
    scheduler.sweep_progress_obligations().await.unwrap();
    let failed = session.clone();
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            calm_truth::db::sqlite::session_fail_if_active_runtime_tx(tx, &failed).await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    scheduler.sweep_progress_obligations().await.unwrap();
    let wakes = event_rows(&boot, "track.wake_requested").await;
    assert_eq!(wakes.len(), 2);
    assert_ne!(wakes[0].1["key"], wakes[1].1["key"]);
}

#[tokio::test]
async fn child_failure_notice_superseded_attempt_cannot_mint_for_successor() {
    let boot = boot().await;
    let (old, _, _) = failed_child(&boot, "superseded-child").await;
    let db = boot.repo.sqlite_db().unwrap();
    db.writer()
        .execute(sqlx::query("UPDATE tasks SET status='failed' WHERE id=?1").bind(&old))
        .await
        .unwrap();
    let origin = calm_types::task_recovery::TaskAttemptOrigin::Recovery {
        previous_attempt_id: old.clone(),
        idempotency_key: "new-owner".into(),
        request_fingerprint: "fingerprint".into(),
        reason: "new attempt".into(),
        actor: ActorId::Kernel,
        constraint: calm_types::task_recovery::TaskRecoveryConstraint::V1 {
            spawn: calm_types::task_recovery::TASK_IN_TRACK_ROUTE.into(),
            declared_by: "spec".into(),
            refs: vec![TaskContextRef {
                track_id: boot.track_id.clone(),
                block_id: "root".into(),
                rev: 1,
                hash: "0".repeat(64),
                is_root: true,
            }],
        },
    };
    db.writer().execute(sqlx::query("INSERT INTO task_attempt_allocations(attempt_id,track_id,key,generation,origin_json,created_at_ms) VALUES('successor',?1,'superseded-child',2,?2,2)")
        .bind(boot.track_id.as_str()).bind(serde_json::to_string(&origin).unwrap())).await.unwrap();
    // Late old state remains stored; current_tasks must exclude it even before a successor projection exists.
    db.writer()
        .execute(sqlx::query("UPDATE tasks SET status='running' WHERE id=?1").bind(&old))
        .await
        .unwrap();
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    scheduler.sweep_progress_obligations().await.unwrap();
    assert!(event_rows(&boot, "track.wake_requested").await.is_empty());
}

#[tokio::test]
async fn child_failure_notice_dormant_scope_repairs_without_rearming() {
    for dormant in ["closed", "binding"] {
        let boot = boot().await;
        let (_, child, session) = failed_child(&boot, &format!("dormant-{dormant}")).await;
        let db = boot.repo.sqlite_db().unwrap();
        let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
        // Enrollment commits, but the factual event/CAS must roll back.
        db.writer().execute(sqlx::query("CREATE TRIGGER reject_dormant_notice BEFORE INSERT ON events WHEN NEW.kind='track.wake_requested' BEGIN SELECT RAISE(ABORT,'held'); END")).await.unwrap();
        assert!(scheduler.sweep_progress_obligations().await.is_err());
        db.writer()
            .execute(sqlx::query("DROP TRIGGER reject_dormant_notice"))
            .await
            .unwrap();
        if dormant == "closed" {
            db.writer()
                .execute(sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1").bind(&child))
                .await
                .unwrap();
        } else {
            db.writer()
                .execute(
                    sqlx::query("UPDATE tracks SET root_session_id=NULL WHERE id=?1").bind(&child),
                )
                .await
                .unwrap();
        }
        scheduler.sweep_progress_obligations().await.unwrap();
        assert!(event_rows(&boot, "track.wake_requested").await.is_empty());
        if dormant == "closed" {
            db.writer()
                .execute(sqlx::query("UPDATE tracks SET closed_at=NULL WHERE id=?1").bind(&child))
                .await
                .unwrap();
        } else {
            db.writer()
                .execute(
                    sqlx::query("UPDATE tracks SET root_session_id=?1 WHERE id=?2")
                        .bind(&session)
                        .bind(&child),
                )
                .await
                .unwrap();
        }
        scheduler.sweep_progress_obligations().await.unwrap();
        scheduler.sweep_progress_obligations().await.unwrap();
        assert_eq!(
            event_rows(&boot, "track.wake_requested").await.len(),
            1,
            "{dormant} repair must preserve the original unnotified obligation"
        );
    }
}

#[tokio::test]
async fn child_failure_notice_active_retries_stop_but_committed_history_remains() {
    let boot = boot().await;
    let (_, _, session) = failed_child(&boot, "held-parent").await;
    let db = boot.repo.sqlite_db().unwrap();
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    scheduler.sweep_progress_obligations().await.unwrap();
    let pending =
        calm_truth::db::sqlite::task_child_failure::task_pending_child_failure_notices(&db)
            .await
            .unwrap();
    assert_eq!(pending.len(), 1);
    let event_id = pending[0].event_id;
    db.writer()
        .execute(sqlx::query("UPDATE worker_sessions SET state='idle' WHERE id=?1").bind(&session))
        .await
        .unwrap();
    assert!(
        calm_truth::db::sqlite::task_child_failure::task_pending_child_failure_notices(&db)
            .await
            .unwrap()
            .is_empty()
    );
    scheduler.sweep_progress_obligations().await.unwrap();
    let wakes = event_rows(&boot, "track.wake_requested").await;
    assert_eq!(wakes.len(), 1);
    let persisted = boot.repo.events_since(event_id - 1, 1).await.unwrap();
    assert_eq!(
        persisted[0].0, event_id,
        "a committed observation remains an immutable historical fact"
    );
}

#[tokio::test]
async fn child_failure_notice_uses_registered_planner_bindings_across_providers() {
    let boot = boot().await;
    let (_runtime, scheduler) = build_scheduler(&boot, vec![]);
    for provider in AgentProvider::ALL {
        let key = format!("provider-{}", provider.wire_name());
        failed_child_for_provider(&boot, &key, provider).await;
    }
    scheduler.sweep_progress_obligations().await.unwrap();
    assert_eq!(
        event_rows(&boot, "track.wake_requested").await.len(),
        AgentProvider::ALL.len()
    );
}
