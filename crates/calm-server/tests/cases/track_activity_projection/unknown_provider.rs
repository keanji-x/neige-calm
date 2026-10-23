use super::*;

#[tokio::test]
async fn track_activity_unknown_provider_retains_task_and_live_harness_evidence() {
    let f = fx().await;
    let (t, planner, harness) = harness_track(&f, WorkerSessionState::TurnPending).await;
    let worker = f.card(&t, "known-worker", "codex", CardRole::Worker).await;
    let worker_session = f
        .session(
            &worker,
            "known-session",
            WorkerSessionKind::CodexCard,
            WorkerSessionState::Running,
            Some("known-thread"),
            None,
            1_000,
        )
        .await;
    let interactive = f
        .card(&t, "unknown-interactive", "codex", CardRole::Worker)
        .await;
    let interactive_session = f
        .session(
            &interactive,
            "unknown-session",
            WorkerSessionKind::CodexCard,
            WorkerSessionState::Running,
            Some("unknown-thread"),
            None,
            1_000,
        )
        .await;
    f.exit_session(&interactive_session, WorkerSessionState::Failed, 1_500)
        .await;
    // The released schema admits nonempty providers beyond this binary's codec.
    sqlx::query("UPDATE worker_sessions SET provider = 'unknown' WHERE id = ?1")
        .bind(&interactive_session)
        .execute(&f.pool)
        .await
        .unwrap();
    f.plan_tasks(&t, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&t, "build", 2_000).await;
    f.mark_running(&t, "build", &worker, 3_000).await;

    let p = f.recompute(&t).await;
    assert!(p.working);
    assert_eq!(card_state(&p, &worker), Some(CardState::Working));
    assert_eq!(
        card_state(&p, &interactive),
        None,
        "unknown failed session is not PTY failure evidence"
    );
    assert_eq!(f.stored(&t).await.unwrap(), p);

    // Task evidence also remains independent of its own session's provider.
    sqlx::query("UPDATE worker_sessions SET provider = 'unknown' WHERE id IN (?1, ?2)")
        .bind(&worker_session)
        .bind(&harness)
        .execute(&f.pool)
        .await
        .unwrap();
    let p = f.recompute(&t).await;
    assert_eq!(card_state(&p, &worker), Some(CardState::Working));
    let rows = f.projector.read_rows(&t).await.unwrap().unwrap();
    assert_eq!(rows.sessions.len(), 3, "unknown rows are retained");
    assert!(rows.sessions.iter().all(|row| !row.interactive_pty));

    f.complete(&t, "build", &worker, 4_000).await;
    let p = f.recompute(&t).await;
    assert!(!p.working, "unknown harness without a live handle is quiet");
    assert!(
        p.cards.is_empty(),
        "unknown failed non-harness row stays excluded"
    );
    assert_eq!(p.activity_at_ms, Some(4_000));

    f.install_live_harness(&t, &planner, &harness).await;
    let p = f.recompute(&t).await;
    assert!(p.working, "unknown provider retains live harness evidence");
    assert_eq!(card_state(&p, &planner), Some(CardState::Working));
    assert_eq!(card_state(&p, &interactive), None);
    assert_eq!(p.activity_at_ms, Some(4_000));
    assert_eq!(f.stored(&t).await.unwrap(), p);
}
