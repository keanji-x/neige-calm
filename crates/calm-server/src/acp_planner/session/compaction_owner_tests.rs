//! Maintenance completion must never enter the input/receipt completion path.
use super::*;
use crate::provider_execution::{CompletionEvidence, KnownEnd, ProviderExecutionOwner, Purpose};
use calm_truth::db::RepoOutOfDomain;
use calm_truth::db::sqlite::provider_execution as claims;

async fn admitted(session: &AcpPlannerSession, purpose: Purpose) -> claims::Claim {
    let params = &session.shared.params;
    let pool = params.repo.sqlite_db().unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await.unwrap();
    let claims::Cas::Applied(claim) = claims::admit(
        &mut tx,
        &params.worker_session_id,
        crate::model::new_id(),
        purpose,
        claims::InitialRelease::Held,
    )
    .await
    .unwrap() else {
        panic!("admission");
    };
    tx.commit().await.unwrap();
    sqlx::query("UPDATE worker_sessions SET mcp_token_hash='current-token' WHERE id=?")
        .bind(&params.worker_session_id)
        .execute(pool.writer_pool())
        .await
        .unwrap();
    claim
}

async fn token(session: &AcpPlannerSession) -> Option<String> {
    sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?")
        .bind(&session.shared.params.worker_session_id)
        .fetch_one(session.shared.params.repo.sqlite_db().unwrap().read_pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn acp_compaction_owner_known_is_receipt_free() {
    receipt_free(Some(KnownEnd::AcpCompaction)).await;
}

#[tokio::test]
async fn acp_compaction_owner_recovery_is_receipt_free() {
    receipt_free(None).await;
}

async fn receipt_free(known: Option<KnownEnd>) {
    let (session, repo, thread) = admission_tests::fixture().await;
    let claim = admitted(
        &session,
        Purpose::Compaction {
            turn_id: crate::model::new_id(),
        },
    )
    .await;
    let Purpose::Compaction { turn_id } = &claim.record().purpose else {
        unreachable!()
    };
    // A colliding receipt must be untouched: purpose, not receipt absence, selects maintenance.
    sqlx::query(concat!(
        "INSERT INTO acp_submissions(",
        "worker_session_id,client_id,turn_id,thread_id,native_session_id,",
        "input_json,state,created_at_ms) ",
        "VALUES (?,'collision',?,?,'native','{}','sending',0)",
    ))
    .bind(&session.shared.params.worker_session_id)
    .bind(turn_id)
    .bind(&thread)
    .execute(repo.pool())
    .await
    .unwrap();
    sqlx::query(concat!(
        "CREATE TRIGGER no_input_recovery BEFORE UPDATE ON acp_submissions ",
        "BEGIN SELECT RAISE(FAIL, 'maintenance touched input'); END",
    ))
    .execute(repo.pool())
    .await
    .unwrap();
    let owner = super::super::execution::Owner::default();
    owner.bind(repo.clone());
    owner
        .complete(
            &session.shared.params.worker_session_id,
            claim.record(),
            CompletionEvidence::Unconfirmed,
            known,
        )
        .await
        .unwrap();
    assert_eq!(token(&session).await, None);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM acp_submissions")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    let state: String = sqlx::query_scalar("SELECT state FROM acp_submissions")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(state, "sending");
    assert!(
        repo.transcript_rows_of_thread(&session.shared.params.card_id, &thread)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn acp_compaction_owner_mismatch_refused_before_credential_revocation() {
    for (purpose, known) in [
        (
            Purpose::Turn {
                turn_id: crate::model::new_id(),
            },
            KnownEnd::AcpCompaction,
        ),
        (
            Purpose::Compaction {
                turn_id: crate::model::new_id(),
            },
            KnownEnd::AcpRefused,
        ),
        (
            Purpose::Compaction {
                turn_id: crate::model::new_id(),
            },
            KnownEnd::RewindCheck,
        ),
    ] {
        let (session, repo, _) = admission_tests::fixture().await;
        let claim = admitted(&session, purpose).await;
        let owner = super::super::execution::Owner::default();
        owner.bind(repo);
        assert!(
            owner
                .complete(
                    &session.shared.params.worker_session_id,
                    claim.record(),
                    CompletionEvidence::Unconfirmed,
                    Some(known)
                )
                .await
                .is_err()
        );
        assert_eq!(token(&session).await.as_deref(), Some("current-token"));
    }
}

#[tokio::test]
async fn acp_compaction_owner_stale_completion_preserves_successor() {
    let (session, repo, _) = admission_tests::fixture().await;
    let current = admitted(
        &session,
        Purpose::Compaction {
            turn_id: crate::model::new_id(),
        },
    )
    .await;
    let mut stale = current.record().clone();
    stale.execution_id = crate::model::new_id();
    let owner = super::super::execution::Owner::default();
    owner.bind(repo.clone());
    owner
        .complete(
            &session.shared.params.worker_session_id,
            &stale,
            CompletionEvidence::Unconfirmed,
            None,
        )
        .await
        .unwrap();
    assert_eq!(token(&session).await.as_deref(), Some("current-token"));
    assert_eq!(
        claims::load(repo.pool(), &session.shared.params.worker_session_id)
            .await
            .unwrap(),
        Some(current)
    );
}

#[tokio::test]
async fn acp_compaction_owner_turn_result_refused_before_credential_revocation() {
    let (session, repo, thread) = admission_tests::fixture().await;
    let turn = crate::model::new_id();
    let claim = admitted(
        &session,
        Purpose::Compaction {
            turn_id: turn.clone(),
        },
    )
    .await;
    let owner = super::super::execution::Owner::default();
    owner.bind(repo);
    // The result has the right identities; its Turn purpose is the mismatch.
    let known = KnownEnd::AcpTurn(Box::new(super::super::execution::End {
        shared: session.shared.clone(),
        receipt: AcpSubmission {
            worker_session_id: session.shared.params.worker_session_id.clone(),
            client_id: "not-issued".into(),
            turn_id: turn.clone(),
            thread_id: thread,
            native_session_id: "native".into(),
            input_json: "{}".into(),
            state: crate::db::sqlite::AcpSubmissionState::Sending,
            outcome_json: None,
            kept_cleanup: None,
        },
        events: Vec::new(),
        turn: json!({"id":turn,"status":"completed"}),
    }));
    let error = owner
        .complete(
            &session.shared.params.worker_session_id,
            claim.record(),
            CompletionEvidence::Unconfirmed,
            Some(known),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("wrong ACP completion result"));
    assert_eq!(token(&session).await.as_deref(), Some("current-token"));
}

#[tokio::test]
async fn acp_compaction_owner_failed_join_is_retired_without_turn_event() {
    use crate::harness::backend::{MaintenanceObservation, MaintenanceTarget};
    let (session, _repo, thread) = admission_tests::fixture().await;
    let params = &session.shared.params;
    let target = MaintenanceTarget {
        card_id: params.card_id.clone(),
        track_id: params.track_id.clone(),
        worker_session_id: params.worker_session_id.clone(),
        thread_id: thread.clone(),
        operation_id: crate::model::new_id(),
    };
    let id = workspace_runtime::execution::ExecutionId::fresh().unwrap();
    let turn = crate::model::new_id();
    let driver = tokio::spawn(async { panic!("lost maintenance driver") });
    while !driver.is_finished() {
        tokio::task::yield_now().await;
    }
    execution_end::install(
        &session.shared,
        DriverSlot::maintenance(
            &session.shared,
            driver,
            target.clone(),
            turn.clone(),
            "native".into(),
        )
        .with_execution(id.clone()),
    );
    let (cancel, _) = watch::channel(CancelRequest::Running);
    session.shared.state.lock().unwrap().active = Some(Active {
        thread,
        turn: turn.clone(),
        cancel,
        approvals: std::sync::Weak::new(),
        native_session_id: "native".into(),
        execution_id: id.clone(),
    });
    let mut events = session.shared.events.subscribe();
    assert_eq!(
        session.lost_maintenance_target(&id, &turn).await.unwrap(),
        Some(target.clone())
    );
    assert!(
        session
            .lost_maintenance_target(&id, "other-turn")
            .await
            .is_err()
    );
    session
        .complete_lost_execution(
            &id,
            &turn,
            &Purpose::Compaction {
                turn_id: turn.clone(),
            },
            Value::Null,
        )
        .await
        .unwrap();
    assert!(session.shared.state.lock().unwrap().active.is_none());
    assert!(
        events.try_recv().is_err(),
        "maintenance loss must not emit TurnCompleted"
    );
    assert!(!execution_end::maintenance_attempted(&session.shared, &target).await);
    assert!(matches!(
        execution_end::observe_maintenance(&session.shared, &target, Some("native")).await,
        MaintenanceObservation::Unconfirmed
    ));
    execution_end::join_driver(&session.shared).await.unwrap();
    session.shutdown().await.unwrap();
    // Another completion may already have retired the old claim and installed a
    // successor. Even reusing the local turn ID cannot authorize clearing it.
    let successor = workspace_runtime::execution::ExecutionId::fresh().unwrap();
    let (cancel, _) = watch::channel(CancelRequest::Running);
    session.shared.state.lock().unwrap().active = Some(Active {
        thread: target.thread_id.clone(),
        turn: turn.clone(),
        cancel,
        approvals: std::sync::Weak::new(),
        native_session_id: "native".into(),
        execution_id: successor.clone(),
    });
    session
        .complete_lost_execution(
            &id,
            &turn,
            &Purpose::Compaction {
                turn_id: turn.clone(),
            },
            Value::Null,
        )
        .await
        .unwrap_err();
    assert_eq!(
        session
            .shared
            .state
            .lock()
            .unwrap()
            .active
            .as_ref()
            .unwrap()
            .execution_id,
        successor
    );
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn acp_compaction_owner_absence_allows_restart_but_active_successor_refuses_loss() {
    let (session, _repo, thread) = admission_tests::fixture().await;
    let id = workspace_runtime::execution::ExecutionId::fresh().unwrap();
    let turn = crate::model::new_id();
    assert_eq!(
        session.lost_maintenance_target(&id, &turn).await.unwrap(),
        None
    );
    let successor = workspace_runtime::execution::ExecutionId::fresh().unwrap();
    let (cancel, _) = watch::channel(CancelRequest::Running);
    session.shared.state.lock().unwrap().active = Some(Active {
        thread,
        turn: turn.clone(),
        cancel,
        approvals: std::sync::Weak::new(),
        native_session_id: "native".into(),
        execution_id: successor,
    });
    assert!(session.lost_maintenance_target(&id, &turn).await.is_err());
}
