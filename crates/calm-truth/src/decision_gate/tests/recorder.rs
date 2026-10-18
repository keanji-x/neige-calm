use super::*;

/// One table for the recorder criterion, so each half is pinned by a row that
/// flips when that half is removed.
async fn recorder_grant_for(
    session: &str,
    card: Option<&str>,
    card_role: CardRole,
    card_track: &str,
    target_track: &str,
) -> bool {
    let mut tx = FakeWriteTx::with_worker_session(worker_session(session, card.map(CardId::from)));
    if let Some(card) = card {
        tx = tx.with_card(card, card_role, card_track);
    }
    PrincipalDecisionGate::new(agent(session))
        .recorder_grant(&mut tx, &TrackId::from(target_track))
        .await
        .expect("recorder grant computes")
}

#[tokio::test]
async fn recorder_grant_admits_planner_and_assistant_cards_of_the_target_track() {
    assert!(
        recorder_grant_for(
            "s-planner",
            Some("card-planner"),
            CardRole::Planner,
            "track-1",
            "track-1"
        )
        .await,
        "the track's planner card may record — this is the path the old \
         root-session criterion used to serve"
    );
    assert!(
        recorder_grant_for(
            "s-assistant",
            Some("card-assistant"),
            CardRole::Assistant,
            "track-1",
            "track-1"
        )
        .await,
        "#1189: an assistant card on the track records into the same report"
    );
}

#[tokio::test]
async fn recorder_grant_refuses_a_worker_card_session() {
    assert!(
        !recorder_grant_for(
            "s-worker",
            Some("card-worker"),
            CardRole::Worker,
            "track-1",
            "track-1"
        )
        .await
    );
    assert!(
        !recorder_grant_for(
            "s-report",
            Some("card-report"),
            CardRole::ReportCard,
            "track-1",
            "track-1"
        )
        .await
    );
}

#[tokio::test]
async fn recorder_grant_refuses_a_card_from_another_track() {
    assert!(
        !recorder_grant_for(
            "s-foreign-assistant",
            Some("card-foreign-assistant"),
            CardRole::Assistant,
            "track-2",
            "track-1"
        )
        .await,
        "another track's assistant must not record into this track's report"
    );
    assert!(
        !recorder_grant_for(
            "s-foreign-planner",
            Some("card-foreign-planner"),
            CardRole::Planner,
            "track-2",
            "track-1"
        )
        .await,
        "and neither may another track's planner — the role whitelist alone \
         is not containment"
    );
}

#[tokio::test]
async fn recorder_grant_denies_every_unresolvable_step() {
    let mut tx = FakeWriteTx::new();
    assert!(
        !PrincipalDecisionGate::new(agent("ghost"))
            .recorder_grant(&mut tx, &TrackId::from("track-1"))
            .await
            .expect("missing session row is a denial, not an error")
    );
    assert!(!recorder_grant_for("s-cardless", None, CardRole::Planner, "track-1", "track-1").await);
    let mut tx = FakeWriteTx::with_worker_session(worker_session(
        "s-dangling",
        Some(CardId::from("card-gone")),
    ));
    assert!(
        !PrincipalDecisionGate::new(agent("s-dangling"))
            .recorder_grant(&mut tx, &TrackId::from("track-1"))
            .await
            .expect("unknown card is a denial, not an error")
    );
}

/// Parameterised only on `worker_sessions.state`; `session_get_tx` has no
/// state filter, so only `is_active_authority` separates these rows.
async fn recorder_decision_for_state(state: WorkerSessionState) -> GateDecision {
    let mut session = worker_session("s-planner", Some(CardId::from("card-planner")));
    session.state = state;
    let mut tx = FakeWriteTx::with_worker_session(session).with_card(
        "card-planner",
        CardRole::Planner,
        "w-1",
    );
    PrincipalDecisionGate::new(agent("s-planner"))
        .decide_recorder(&mut tx, &TrackId::from("w-1"))
        .await
        .expect("recorder decision computes")
}

/// A supersede only flips the old row's state; it keeps its `card_id` on the
/// same track, so the card criterion alone would still admit it.
#[tokio::test]
async fn recorder_grant_refuses_a_session_that_is_no_longer_an_active_authority() {
    for state in [
        WorkerSessionState::Superseded,
        WorkerSessionState::Exited,
        WorkerSessionState::Failed,
    ] {
        assert!(
            matches!(
                recorder_decision_for_state(state).await,
                GateDecision::Deny(_)
            ),
            "a {state:?} session on this track's planner card must not record"
        );
    }
    for state in [
        WorkerSessionState::Starting,
        WorkerSessionState::Running,
        WorkerSessionState::Idle,
        WorkerSessionState::TurnPending,
    ] {
        assert_eq!(
            recorder_decision_for_state(state).await,
            GateDecision::Allow,
            "a {state:?} session on this track's planner card still records"
        );
    }
}

#[tokio::test]
async fn recorder_deny_distinguishes_a_missing_row_from_a_dead_one() {
    let mut tx = FakeWriteTx::new();
    let missing = PrincipalDecisionGate::new(agent("ghost"))
        .decide_recorder(&mut tx, &TrackId::from("w-1"))
        .await
        .expect("missing row is a denial, not an error");
    let GateDecision::Deny(missing) = missing else {
        panic!("a session with no row must be denied");
    };
    assert!(
        missing.contains("has no session row"),
        "missing-row denial should say the row is absent, got {missing:?}"
    );

    let dead = recorder_decision_for_state(WorkerSessionState::Superseded).await;
    let GateDecision::Deny(dead) = dead else {
        panic!("a superseded session must be denied");
    };
    assert!(
        dead.contains("is no longer an active authority") && dead.contains("Superseded"),
        "dead-row denial should name the state it found, got {dead:?}"
    );
    assert_ne!(
        missing, dead,
        "the two denials must not collapse into one message"
    );
}

/// Nothing in `session_mark_track_root_tx` checks the root card's role, so a
/// root-session allow-arm would bypass the role whitelist.
#[tokio::test]
async fn a_root_marked_worker_session_is_still_refused() {
    let mut tx = FakeWriteTx::with_worker_session(worker_session(
        "s-root",
        Some(CardId::from("card-worker")),
    ))
    .with_card("card-worker", CardRole::Worker, "track-1");
    tx.root_session_id = Some(WorkerSessionId::from("s-root"));

    assert!(
        !PrincipalDecisionGate::new(agent("s-root"))
            .recorder_grant(&mut tx, &TrackId::from("track-1"))
            .await
            .expect("root grant computes"),
        "being the track root must not buy a worker card recording rights"
    );
}
