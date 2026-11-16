//! Source-owner interrupt checks; no native execution or receipt fixture.
#![cfg(test)]
use super::*;
use provider::control::{InterruptOutcome, SubmissionEvidence};

#[test]
fn m1_mapping_acp_interrupt_reports_watch_delivery_not_native_ack() {
    for receiver_alive in [false, true] {
        let (tx, rx) = tokio::sync::watch::channel(false);
        let mut receiver = Some(rx);
        if !receiver_alive {
            receiver.take();
        }
        match cancel_outcome(tx.send(true)) {
            InterruptOutcome::LocalSignalDelivered => assert!(receiver_alive),
            InterruptOutcome::Unconfirmed { error, evidence } => {
                assert!(!receiver_alive);
                assert!(error.0);
                assert_eq!(evidence, SubmissionEvidence::NotWritten);
            }
            other => panic!("wrong mapping: {other:?}"),
        }
        if let Some(rx) = receiver {
            assert!(*rx.borrow());
        }
    }
}

#[tokio::test]
async fn acp_identical_local_turns_keep_two_owner_cancellation_separate() {
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let host = Arc::new(AcpPlannerHost::unconfigured_scratch().unwrap());
    let seals = Arc::new(ThreadSeals::new());
    let make = |owner: &str| {
        AcpPlannerSession::open(SessionParams {
            engine: host.execution.engine().cloned(),
            host: host.clone(),
            provider: AgentProvider::OpenCode,
            repo: repo.clone(),
            seals: seals.clone(),
            worker_session_id: owner.into(),
            card_id: owner.into(),
            track_id: owner.into(),
            cwd: "/tmp".into(),
            instructions: String::new(),
        })
    };
    let a = make("a");
    let b = make("b");
    let (a_cancel, a_seen) = watch::channel(CancelRequest::Running);
    let (b_cancel, b_seen) = watch::channel(CancelRequest::Running);
    a.shared.state.lock().unwrap().active = Some(Active {
        thread: "a".into(),
        turn: "same-local-turn".into(),
        cancel: a_cancel,
        approvals: std::sync::Weak::new(),
        native_session_id: "a".into(),
        execution_id: Some(workspace_runtime::execution::ExecutionId::fresh().unwrap()),
    });
    b.shared.state.lock().unwrap().active = Some(Active {
        thread: "b".into(),
        turn: "same-local-turn".into(),
        cancel: b_cancel,
        approvals: std::sync::Weak::new(),
        native_session_id: "b".into(),
        execution_id: Some(workspace_runtime::execution::ExecutionId::fresh().unwrap()),
    });
    assert!(matches!(
        a.turn_interrupt("b", "same-local-turn").await,
        InterruptOutcome::NoTarget
    ));
    assert!(matches!(
        a.turn_interrupt("a", "other-turn").await,
        InterruptOutcome::NoTarget
    ));
    assert!(matches!(*a_seen.borrow(), CancelRequest::Running));
    assert!(matches!(*b_seen.borrow(), CancelRequest::Running));
    assert!(matches!(
        a.turn_interrupt("a", "same-local-turn").await,
        InterruptOutcome::LocalSignalDelivered
    ));
    assert!(matches!(*a_seen.borrow(), CancelRequest::Requested));
    assert!(
        matches!(*b_seen.borrow(), CancelRequest::Running),
        "identical local turn must not give authority over another owner"
    );
}

#[tokio::test]
async fn acp_token_reattach_reuses_without_revocation() {
    let (session, repo, _) = super::admission_tests::fixture().await;
    let first = mint_token(&session.shared.params).await.unwrap();
    let params = &session.shared.params;
    let host =
        Arc::new(AcpPlannerHost::scratch_with_tokens(params.host.managed_tokens.clone()).unwrap());
    let rebuilt = AcpPlannerSession::open(SessionParams {
        engine: params.engine.clone(),
        host,
        provider: params.provider.clone(),
        repo: params.repo.clone(),
        seals: params.seals.clone(),
        worker_session_id: params.worker_session_id.clone(),
        card_id: params.card_id.clone(),
        track_id: params.track_id.clone(),
        cwd: params.cwd.clone(),
        instructions: params.instructions.clone(),
    });
    let second = mint_token(&rebuilt.shared.params).await.unwrap();
    assert_eq!(
        first, second,
        "reattach must preserve existing shim credentials"
    );
    assert!(
        crate::db::sqlite::session_get_by_active_token_hash(
            repo.db().read_pool(),
            &crate::mcp_server::auth::hash_token(&first),
        )
        .await
        .unwrap()
        .is_some()
    );
}

#[tokio::test]
async fn acp_token_revoke_rejects_old_credential_and_remints() {
    let (session, repo, _) = super::admission_tests::fixture().await;
    let first = mint_token(&session.shared.params).await.unwrap();
    revoke(&session.shared.params).await.unwrap();
    assert!(
        crate::db::sqlite::session_get_by_active_token_hash(
            repo.db().read_pool(),
            &crate::mcp_server::auth::hash_token(&first),
        )
        .await
        .unwrap()
        .is_none()
    );
    let second = mint_token(&session.shared.params).await.unwrap();
    assert_ne!(first, second);
    assert!(
        crate::db::sqlite::session_get_by_active_token_hash(
            repo.db().read_pool(),
            &crate::mcp_server::auth::hash_token(&first),
        )
        .await
        .unwrap()
        .is_none()
    );
    assert!(
        crate::db::sqlite::session_get_by_active_token_hash(
            repo.db().read_pool(),
            &crate::mcp_server::auth::hash_token(&second),
        )
        .await
        .unwrap()
        .is_some()
    );
}
