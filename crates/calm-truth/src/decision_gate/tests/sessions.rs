use super::*;

#[tokio::test]
async fn session_resolver_allows_worker_self_scope() {
    let worker_card = CardId::from("worker-card");
    let (cache, wcc) = seeded_caches(&worker_card, CardRole::Worker);
    let mut tx = FakeWriteTx::with_worker_session(worker_session("s1", Some(worker_card.clone())));

    let res = enforce_role_resolving_session(
        &mut tx,
        &ActorId::AiCodexSession(WorkerSessionId::from("s1")),
        &area_updated(),
        &card_scope(worker_card.as_str(), "w", "c"),
        &cache,
        &wcc,
    )
    .await;

    assert!(res.is_ok(), "worker session in own scope: {res:?}");
    assert_eq!(tx.worker_session_reads, 1);
}

#[tokio::test]
async fn session_resolver_allows_planner_track_updated() {
    let planner_card = CardId::from("planner-card");
    let (cache, wcc) = seeded_caches(&planner_card, CardRole::Planner);
    let mut tx = FakeWriteTx::with_worker_session(worker_session("s2", Some(planner_card.clone())))
        .with_card("planner-card", CardRole::Planner, "w");
    tx.bindings.push((planner_card.clone(), "s2".into()));

    let res = enforce_role_resolving_session(
        &mut tx,
        &ActorId::AiPlannerSession(WorkerSessionId::from("s2")),
        &track_updated(),
        &track_scope("w", "c"),
        &cache,
        &wcc,
    )
    .await;

    assert!(res.is_ok(), "planner session should update track: {res:?}");
    assert_eq!(tx.worker_session_reads, 1);
}

#[tokio::test]
async fn session_resolver_denies_planner_session_bound_to_worker_card_on_ordinary_event() {
    let worker_card = CardId::from("worker-card");
    let (cache, wcc) = seeded_caches(&worker_card, CardRole::Worker);
    let mut tx = FakeWriteTx::with_worker_session(worker_session(
        "s-planner-worker",
        Some(worker_card.clone()),
    ));

    let err = enforce_role_resolving_session(
        &mut tx,
        &ActorId::AiPlannerSession(WorkerSessionId::from("s-planner-worker")),
        &area_updated(),
        &card_scope(worker_card.as_str(), "w", "c"),
        &cache,
        &wcc,
    )
    .await
    .expect_err("planner session bound to worker card must deny");

    match err {
        RoleViolation::SessionPlannerRoleMismatch { session, card } => {
            assert_eq!(session, WorkerSessionId::from("s-planner-worker"));
            assert_eq!(card, worker_card);
        }
        other => panic!("unexpected violation: {other:?}"),
    }
    assert_eq!(tx.worker_session_reads, 1);
}

#[tokio::test]
async fn session_resolver_denies_planner_session_bound_to_unknown_card_on_ordinary_event() {
    let cache = CardRoleCache::new();
    let wcc = TrackAreaCache::new();
    let ghost = CardId::from("ghost");
    let mut tx =
        FakeWriteTx::with_worker_session(worker_session("s-planner-ghost", Some(ghost.clone())));

    let err = enforce_role_resolving_session(
        &mut tx,
        &ActorId::AiPlannerSession(WorkerSessionId::from("s-planner-ghost")),
        &area_updated(),
        &card_scope(ghost.as_str(), "w", "c"),
        &cache,
        &wcc,
    )
    .await
    .expect_err("planner session bound to unknown card must deny");

    match err {
        RoleViolation::SessionPlannerRoleMismatch { session, card } => {
            assert_eq!(session, WorkerSessionId::from("s-planner-ghost"));
            assert_eq!(card, ghost);
        }
        other => panic!("unexpected violation: {other:?}"),
    }
    assert_eq!(tx.worker_session_reads, 1);
}

#[tokio::test]
async fn session_resolver_allows_planner_session_bound_to_planner_card_on_ordinary_event() {
    let planner_card = CardId::from("planner-card");
    let (cache, wcc) = seeded_caches(&planner_card, CardRole::Planner);
    let mut tx = FakeWriteTx::with_worker_session(worker_session(
        "s-planner-ordinary",
        Some(planner_card.clone()),
    ))
    .with_card("planner-card", CardRole::Planner, "w");
    tx.bindings
        .push((planner_card.clone(), "s-planner-ordinary".into()));

    let res = enforce_role_resolving_session(
        &mut tx,
        &ActorId::AiPlannerSession(WorkerSessionId::from("s-planner-ordinary")),
        &area_updated(),
        &card_scope(planner_card.as_str(), "w", "c"),
        &cache,
        &wcc,
    )
    .await;

    assert!(
        res.is_ok(),
        "planner session bound to planner card should pass ordinary event: {res:?}"
    );
    assert_eq!(tx.worker_session_reads, 1);
}

#[tokio::test]
async fn session_resolver_denies_empty_card_id_via_worker_variant_sync_gate() {
    let cache = CardRoleCache::new();
    let wcc = TrackAreaCache::new();
    let mut tx =
        FakeWriteTx::with_worker_session(worker_session("s-empty-card", Some(CardId::from(""))));

    let err = enforce_role_resolving_session(
        &mut tx,
        &ActorId::AiCodexSession(WorkerSessionId::from("s-empty-card")),
        &area_updated(),
        &EventScope::System,
        &cache,
        &wcc,
    )
    .await
    .expect_err("worker session resolving to empty card id must deny");

    assert!(matches!(err, RoleViolation::EmptyAiCardId));
    assert_eq!(tx.worker_session_reads, 1);
}

#[tokio::test]
async fn session_resolver_denies_missing_row() {
    let cache = CardRoleCache::new();
    let wcc = TrackAreaCache::new();
    let mut tx = FakeWriteTx::new();

    let err = enforce_role_resolving_session(
        &mut tx,
        &ActorId::AiCodexSession(WorkerSessionId::from("missing")),
        &area_updated(),
        &EventScope::System,
        &cache,
        &wcc,
    )
    .await
    .expect_err("missing session row must deny");

    assert!(matches!(err, RoleViolation::SessionRowMissing { .. }));
    assert_eq!(tx.worker_session_reads, 1);
}

#[tokio::test]
async fn session_resolver_denies_cardless_row() {
    let cache = CardRoleCache::new();
    let wcc = TrackAreaCache::new();
    let mut tx = FakeWriteTx::with_worker_session(worker_session("cardless", None));

    let err = enforce_role_resolving_session(
        &mut tx,
        &ActorId::AiCodexSession(WorkerSessionId::from("cardless")),
        &area_updated(),
        &EventScope::System,
        &cache,
        &wcc,
    )
    .await
    .expect_err("cardless session must deny");

    assert!(matches!(err, RoleViolation::CardlessSessionDenied { .. }));
    assert_eq!(tx.worker_session_reads, 1);
}

#[tokio::test]
async fn session_resolver_denies_terminal_session_rows_before_card_delegation() {
    let worker_card = CardId::from("worker-card");
    let (cache, wcc) = seeded_caches(&worker_card, CardRole::Worker);

    for (session_id, state) in [
        ("s-exited", WorkerSessionState::Exited),
        ("s-failed", WorkerSessionState::Failed),
        ("s-superseded", WorkerSessionState::Superseded),
    ] {
        let mut session = worker_session(session_id, Some(worker_card.clone()));
        session.state = state;
        let mut tx = FakeWriteTx::with_worker_session(session);

        let err = enforce_role_resolving_session(
            &mut tx,
            &ActorId::AiCodexSession(WorkerSessionId::from(session_id)),
            &area_updated(),
            &card_scope(worker_card.as_str(), "w", "c"),
            &cache,
            &wcc,
        )
        .await
        .expect_err("terminal session row must deny before card delegation");

        match err {
            RoleViolation::SessionNotActive { session } => {
                assert_eq!(session, WorkerSessionId::from(session_id));
            }
            other => panic!("unexpected violation for {state:?}: {other:?}"),
        }
        assert_eq!(tx.worker_session_reads, 1);
    }
}

#[tokio::test]
async fn session_resolver_denies_read_error() {
    let cache = CardRoleCache::new();
    let wcc = TrackAreaCache::new();
    let mut tx = FakeWriteTx::with_worker_session_read_error();

    let err = enforce_role_resolving_session(
        &mut tx,
        &ActorId::AiCodexSession(WorkerSessionId::from("s-error")),
        &area_updated(),
        &EventScope::System,
        &cache,
        &wcc,
    )
    .await
    .expect_err("worker_sessions read error must deny");

    assert!(matches!(err, RoleViolation::SessionResolutionError { .. }));
    assert_eq!(tx.worker_session_reads, 1);
}

#[tokio::test]
async fn session_resolver_denies_empty_session_id() {
    let cache = CardRoleCache::new();
    let wcc = TrackAreaCache::new();
    let mut tx = FakeWriteTx::new();

    let err = enforce_role_resolving_session(
        &mut tx,
        &ActorId::AiCodexSession(WorkerSessionId::from("")),
        &area_updated(),
        &EventScope::System,
        &cache,
        &wcc,
    )
    .await
    .expect_err("empty session id must deny");

    assert!(matches!(err, RoleViolation::SessionRowMissing { .. }));
    assert_eq!(tx.worker_session_reads, 0);
}

#[tokio::test]
async fn session_resolver_denies_unknown_card_via_sync_gate() {
    let cache = CardRoleCache::new();
    let wcc = TrackAreaCache::new();
    let ghost = CardId::from("ghost");
    let mut tx = FakeWriteTx::with_worker_session(worker_session("s-ghost", Some(ghost.clone())));

    let err = enforce_role_resolving_session(
        &mut tx,
        &ActorId::AiCodexSession(WorkerSessionId::from("s-ghost")),
        &area_updated(),
        &card_scope(ghost.as_str(), "w", "c"),
        &cache,
        &wcc,
    )
    .await
    .expect_err("unknown resolved card must deny");

    assert!(matches!(err, RoleViolation::UnknownCard { .. }));
    assert_eq!(tx.worker_session_reads, 1);
}

#[tokio::test]
async fn session_resolver_denies_out_of_scope_worker_via_sync_gate() {
    let worker_card = CardId::from("worker-card");
    let (cache, wcc) = seeded_caches(&worker_card, CardRole::Worker);
    let mut tx =
        FakeWriteTx::with_worker_session(worker_session("s-out-of-scope", Some(worker_card)));

    let err = enforce_role_resolving_session(
        &mut tx,
        &ActorId::AiCodexSession(WorkerSessionId::from("s-out-of-scope")),
        &area_updated(),
        &card_scope("other-card", "w", "c"),
        &cache,
        &wcc,
    )
    .await
    .expect_err("worker session outside own card scope must deny");

    assert!(matches!(err, RoleViolation::WorkerOutOfScope { .. }));
    assert_eq!(tx.worker_session_reads, 1);
}

#[tokio::test]
async fn session_resolver_denies_worker_track_updated_via_sync_gate() {
    let worker_card = CardId::from("worker-card");
    let (cache, wcc) = seeded_caches(&worker_card, CardRole::Worker);
    let mut tx =
        FakeWriteTx::with_worker_session(worker_session("s-worker-track", Some(worker_card)));

    let err = enforce_role_resolving_session(
        &mut tx,
        &ActorId::AiCodexSession(WorkerSessionId::from("s-worker-track")),
        &track_updated(),
        &track_scope("w", "c"),
        &cache,
        &wcc,
    )
    .await
    .expect_err("worker session must not update track");

    assert!(matches!(err, RoleViolation::NotPlannerForTrack { .. }));
    assert_eq!(tx.worker_session_reads, 1);
}

#[tokio::test]
async fn session_resolver_passthrough_keeps_card_actor_on_sync_path() {
    let worker_card = CardId::from("worker-card");
    let (cache, wcc) = seeded_caches(&worker_card, CardRole::Worker);
    let actor = ActorId::AiCodex(worker_card.clone());
    let event = area_updated();
    let scope = card_scope(worker_card.as_str(), "w", "c");
    let direct = enforce_role(&actor, &event, &scope, &cache, &wcc);
    let mut tx = FakeWriteTx::new();

    let routed =
        enforce_role_resolving_session(&mut tx, &actor, &event, &scope, &cache, &wcc).await;

    assert!(direct.is_ok(), "direct sync gate should allow: {direct:?}");
    assert!(routed.is_ok(), "async passthrough should allow: {routed:?}");
    assert_eq!(tx.worker_session_reads, 0);
}

#[tokio::test]
async fn tx_read_gate_denies_when_home_track_row_is_missing() {
    // The gate runs inside the caller's write transaction, so a missing track row
    // must come back as `Err`, never a panic unwinding out of a half-written write.
    let card = CardId::from("orphan");
    let mut tx = FakeWriteTx::new().with_card("orphan", CardRole::Worker, "w-gone");

    let err = enforce_role_resolving_session_from_tx(
        &mut tx,
        &ActorId::AiCodex(card),
        &area_updated(),
        &card_scope("orphan", "w-gone", "c"),
    )
    .await
    .expect_err("a card with no home track row must be denied, not panic");

    assert!(
        matches!(err, RoleViolation::RoleLookupFailed { ref subject }
            if subject == "tracks.area_id(w-gone)"),
        "unexpected violation: {err:?}"
    );
}
