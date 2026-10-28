//! Recover native identity from real, validated replay evidence after a lost init bind.
use super::*;

#[tokio::test]
async fn canonical_claude_identity_recovery_resumes_after_bind_failure_and_restart() {
    let mut fx = Fixture::persistent("exit").await;
    // Fail the production init bind without blocking the independent replay acknowledgement.
    sqlx::query(
        "CREATE TRIGGER fail_native_bind BEFORE UPDATE OF agent_session_id ON worker_sessions
        WHEN NEW.agent_session_id IS NOT NULL
        BEGIN SELECT RAISE(ABORT,'bind failed'); END",
    )
    .execute(fx.owner.repo.pool())
    .await
    .unwrap();
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    let before = fx.checkpoint().await;
    assert!(matches!(&before.attempts[0].delivery,
        InputDeliveryFact::Accepted { basis, .. } if basis == "claude_user_replay"));
    let bound: Option<String> =
        sqlx::query_scalar("SELECT agent_session_id FROM worker_sessions WHERE id=?")
            .bind(&fx.owner.worker)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert_eq!(bound, None);
    fx.session.shutdown().await.unwrap();
    sqlx::query("DROP TRIGGER fail_native_bind")
        .execute(fx.owner.repo.pool())
        .await
        .unwrap();
    let (backend, session) = super::recovery::reopen_session(&fx, fx.owner.repo.clone()).await;
    fx.backend = backend;
    fx.session = session;
    assert_eq!(
        fx.checkpoint().await,
        before,
        "identity recovery does not alter delivery facts"
    );

    next_input(&mut fx).await;
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    let argv = std::fs::read_to_string(fx.dir.path().join("argv")).unwrap();
    assert!(argv.lines().any(|arg| arg == "--resume"), "{argv}");
    assert!(!argv.lines().any(|arg| arg == "--session-id"), "{argv}");
    let bound: Option<String> =
        sqlx::query_scalar("SELECT agent_session_id FROM worker_sessions WHERE id=?")
            .bind(&fx.owner.worker)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert_eq!(bound.as_deref(), Some(fx.owner.thread.as_str()));
    fx.session.shutdown().await.unwrap();
}

async fn next_input(fx: &mut Fixture) {
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    let accepted = crate::conversation_inputs::accept_human_tx(
        &mut tx,
        &fx.source.current_principal().unwrap(),
        &mut carrier,
        HumanPayload {
            text: "following human input".into(),
            bound_attachments: vec![],
        },
    )
    .await
    .unwrap();
    let AcceptedInputRef::Human { message } = accepted.input_ref else {
        panic!("Human")
    };
    let claimed = crate::conversation_inputs::claim_initial_tx(
        &mut tx,
        &fx.source,
        &mut carrier,
        SubmissionRenderPlan::V1 {
            segments: vec![SubmissionRenderSegment::Input {
                input_position: 0,
                input: SubmissionRenderInput::Human {
                    message,
                    author: FrozenHumanAttribution {
                        registered_user_id: calm_types::organization::LEGACY_OWNER_USER_ID.into(),
                        display_name: "Owner".into(),
                    },
                },
            }],
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    fx.submission = claimed.submission_id;
}

async fn binding(fx: &Fixture) -> Option<String> {
    sqlx::query_scalar("SELECT agent_session_id FROM worker_sessions WHERE id=?")
        .bind(&fx.owner.worker)
        .fetch_one(fx.owner.repo.pool())
        .await
        .unwrap()
}

async fn fail_bind(fx: &Fixture) {
    sqlx::query(
        "CREATE TRIGGER fail_native_bind BEFORE UPDATE OF agent_session_id ON worker_sessions
        WHEN NEW.agent_session_id IS NOT NULL
        BEGIN SELECT RAISE(ABORT,'bind failed'); END",
    )
    .execute(fx.owner.repo.pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn canonical_claude_identity_recovery_preserves_later_unknown_without_retry() {
    let mut fx = Fixture::persistent("exit").await;
    fail_bind(&fx).await;
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    next_input(&mut fx).await;
    std::fs::write(fx.dir.path().join("scenario"), "no-input-replay").unwrap();
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    let before = fx.checkpoint().await;
    assert!(matches!(
        before.attempts[0].delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    assert!(matches!(
        before.attempts[1].delivery,
        InputDeliveryFact::Unknown
    ));
    assert_eq!(binding(&fx).await, None);
    fx.session.shutdown().await.unwrap();
    sqlx::query("DROP TRIGGER fail_native_bind")
        .execute(fx.owner.repo.pool())
        .await
        .unwrap();
    let spawns = std::fs::read_to_string(fx.dir.path().join("spawns")).unwrap();
    let (_backend, session) = super::recovery::reopen_session(&fx, fx.owner.repo.clone()).await;
    assert_eq!(
        binding(&fx).await.as_deref(),
        Some(fx.owner.thread.as_str())
    );
    assert_eq!(fx.checkpoint().await, before);
    assert_eq!(
        std::fs::read_to_string(fx.dir.path().join("spawns")).unwrap(),
        spawns,
        "identity recovery must not retry uncertain input"
    );
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_identity_recovery_requires_verified_replay() {
    for scenario in [
        "no-input-replay",
        "wrong-input-replay",
        "receipt-before-init",
        "wrong-replay-session",
    ] {
        let fx = Fixture::persistent(scenario).await;
        fail_bind(&fx).await;
        fx.dispatch().await.unwrap();
        fx.wait_end().await;
        let before = fx.checkpoint().await;
        assert!(
            matches!(before.attempts[0].delivery, InputDeliveryFact::Unknown),
            "{scenario}"
        );
        assert_eq!(binding(&fx).await, None);
        fx.session.shutdown().await.unwrap();
        sqlx::query("DROP TRIGGER fail_native_bind")
            .execute(fx.owner.repo.pool())
            .await
            .unwrap();
        let (_backend, session) = super::recovery::reopen_session(&fx, fx.owner.repo.clone()).await;
        assert_eq!(binding(&fx).await, None, "{scenario}");
        assert_eq!(fx.checkpoint().await, before, "{scenario}");
        session.shutdown().await.unwrap();
    }
}
