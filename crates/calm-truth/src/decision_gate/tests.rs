#![cfg(test)]

mod native_hold_tests;
use super::*;
use crate::model::{Area, AreaKind, Track};
use crate::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSessionState,
};

struct FakeWriteTx {
    root_session_id: Option<WorkerSessionId>,
    worker_session: Option<WorkerSessionRow>,
    worker_session_reads: usize,
    worker_session_read_error: bool,
    /// `cards` rows this fake knows about: id → (role, home track).
    cards: Vec<(CardId, CardRole, TrackId)>,
    /// `tracks` rows this fake knows about: id → area.
    tracks: Vec<(TrackId, AreaId)>,
}

impl FakeWriteTx {
    fn new() -> Self {
        Self {
            root_session_id: None,
            worker_session: None,
            worker_session_reads: 0,
            worker_session_read_error: false,
            cards: Vec::new(),
            tracks: Vec::new(),
        }
    }

    fn with_card(mut self, card: &str, role: CardRole, track: &str) -> Self {
        self.cards
            .push((CardId::from(card), role, TrackId::from(track)));
        self
    }

    fn with_track(mut self, track: &str, area: &str) -> Self {
        self.tracks.push((TrackId::from(track), AreaId::from(area)));
        self
    }

    fn with_worker_session(worker_session: WorkerSessionRow) -> Self {
        Self {
            worker_session: Some(worker_session),
            ..Self::new()
        }
    }

    fn with_worker_session_read_error() -> Self {
        Self {
            worker_session_read_error: true,
            ..Self::new()
        }
    }
}

impl sealed::Sealed for FakeWriteTx {}

#[async_trait]
impl WriteTx for FakeWriteTx {
    async fn read_track_root_session_id(
        &mut self,
        _track: &TrackId,
    ) -> Result<Option<WorkerSessionId>> {
        Ok(self.root_session_id.clone())
    }

    async fn read_worker_session(
        &mut self,
        id: &WorkerSessionId,
    ) -> Result<Option<WorkerSessionRow>> {
        self.worker_session_reads += 1;
        if self.worker_session_read_error {
            return Err(crate::error::TruthError::Internal(
                "worker session read failed".into(),
            ));
        }
        Ok(self
            .worker_session
            .as_ref()
            .filter(|session| &session.id == id)
            .cloned())
    }

    async fn read_card_role(&mut self, card: &CardId) -> Result<Option<CardRole>> {
        Ok(self
            .cards
            .iter()
            .find(|(id, _, _)| id == card)
            .map(|(_, role, _)| *role))
    }

    async fn read_card_track(&mut self, card: &CardId) -> Result<Option<TrackId>> {
        Ok(self
            .cards
            .iter()
            .find(|(id, _, _)| id == card)
            .map(|(_, _, track)| track.clone()))
    }

    async fn read_card_binding(
        &mut self,
        _card: &CardId,
    ) -> Result<Option<(String, serde_json::Value, Option<String>)>> {
        Ok(None)
    }

    async fn read_track_area(&mut self, track: &TrackId) -> Result<Option<AreaId>> {
        Ok(self
            .tracks
            .iter()
            .find(|(id, _)| id == track)
            .map(|(_, area)| area.clone()))
    }
}

fn agent(session_id: &str) -> Principal {
    Principal::Agent {
        session_id: WorkerSessionId::from(session_id),
        track_id: TrackId::from("track-1"),
        area_id: AreaId::from("area-1"),
    }
}

fn worker_session(session_id: &str, card_id: Option<CardId>) -> WorkerSession {
    WorkerSession {
        id: WorkerSessionId::from(session_id),
        track_id: TrackId::from("w"),
        provider: WorkerProviderKind::Codex,
        mode: SessionMode::Resumable,
        contract: WorkerContract::Executor,
        parent_session_id: None,
        requester_session_id: None,
        state: WorkerSessionState::Running,
        mcp_token_hash: None,
        thread_id: None,
        agent_session_id: None,
        active_turn_id: None,
        terminal_run_id: None,
        card_id,
        handle_state_json: None,
        liveness: LivenessTag::Unknown,
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: None,
        last_activity_ms: None,
        last_thread_status: None,
        created_at_ms: 0,
        updated_at_ms: 0,
        completed_at_ms: None,
    }
}

fn track(id: &str, area: &str) -> Track {
    Track {
        id: TrackId::from(id),
        area_id: AreaId::from(area),
        title: "t".into(),
        sort: 1.0,
        pinned_at: None,
        closed_at: None,
        admin_granted_at: None,
        cwd_wire_alias: String::new(),
        template_id: None,
        plugin_scope: None,
        purpose: None,
        template_input: None,
        recipe_id: None,
        recipe_revision: None,
        creator_track_id: None,
        creator_key: None,
        workspace: Default::default(),
        created_at: 0,
        updated_at: 0,
    }
}

fn card_scope(card: &str, track: &str, area: &str) -> EventScope {
    EventScope::Card {
        card: CardId::from(card),
        track: TrackId::from(track),
        area: AreaId::from(area),
    }
}

fn track_scope(track: &str, area: &str) -> EventScope {
    EventScope::Track {
        track: TrackId::from(track),
        area: AreaId::from(area),
    }
}

fn track_updated() -> Event {
    Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
        track("w", "c"),
        None,
    ))
}

fn area_updated() -> Event {
    Event::AreaUpdated(Area {
        id: AreaId::from("c"),
        name: "n".into(),
        color: "#fff".into(),
        sort: 1.0,
        kind: AreaKind::User,
        default_template_id: None,
        default_cwd: None,
        created_at: 0,
        updated_at: 0,
    })
}

fn seeded_caches(card: &CardId, role: CardRole) -> (CardRoleCache, TrackAreaCache) {
    let cache = CardRoleCache::new();
    cache.insert(card.clone(), role, TrackId::from("w"));
    let wcc = TrackAreaCache::new();
    wcc.insert(TrackId::from("w"), AreaId::from("c"));
    (cache, wcc)
}

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
    let mut tx = FakeWriteTx::with_worker_session(worker_session("s2", Some(planner_card.clone())));

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
    ));

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
// Equivalence matrix: both substrates are projected from one `World`, so
// (actor × event × scope) must agree exactly, including the violation text.
// States where cache and DB disagree cannot be expressed here.

/// The rows both substrates are built from.
struct World {
    cards: Vec<(&'static str, CardRole, &'static str)>,
    tracks: Vec<(&'static str, &'static str)>,
    session: Option<WorkerSessionRow>,
}

impl World {
    fn tx(&self) -> FakeWriteTx {
        let mut tx = match &self.session {
            Some(session) => FakeWriteTx::with_worker_session(session.clone()),
            None => FakeWriteTx::new(),
        };
        for (card, role, track) in &self.cards {
            tx = tx.with_card(card, *role, track);
        }
        for (track, area) in &self.tracks {
            tx = tx.with_track(track, area);
        }
        tx
    }

    fn caches(&self) -> (CardRoleCache, TrackAreaCache) {
        let cache = CardRoleCache::new();
        for (card, role, track) in &self.cards {
            cache.insert(CardId::from(*card), *role, TrackId::from(*track));
        }
        let area_cache = TrackAreaCache::new();
        for (track, area) in &self.tracks {
            area_cache.insert(TrackId::from(*track), AreaId::from(*area));
        }
        (cache, area_cache)
    }
}

fn matrix_world() -> World {
    World {
        cards: vec![
            ("planner", CardRole::Planner, "w"),
            ("worker", CardRole::Worker, "w"),
            ("report", CardRole::ReportCard, "w"),
            ("assistant", CardRole::Assistant, "w"),
            ("foreign-worker", CardRole::Worker, "w2"),
            // A card whose home track has no row: both substrates must deny with
            // `RoleLookupFailed`, not panic.
            ("orphan", CardRole::Worker, "w-gone"),
        ],
        tracks: vec![("w", "c"), ("w2", "c2")],
        session: Some(worker_session("s-live", Some(CardId::from("planner")))),
    }
}

// Pin each fixture dimension once; fixed arrays make missing rows a compile error.
const MATRIX_ACTOR_COUNT: usize = 21;
const MATRIX_EVENT_COUNT: usize = 9;
const MATRIX_SCOPE_COUNT: usize = 13;

fn matrix_actors() -> [ActorId; MATRIX_ACTOR_COUNT] {
    [
        ActorId::User,
        ActorId::Kernel,
        ActorId::KernelDispatcher,
        ActorId::Plugin("p".into()),
        ActorId::AiPlanner(CardId::from("planner")),
        ActorId::AiPlanner(CardId::from("worker")),
        ActorId::AiPlanner(CardId::from("")),
        ActorId::AiCodex(CardId::from("worker")),
        ActorId::AiCodex(CardId::from("planner")),
        ActorId::AiCodex(CardId::from("report")),
        ActorId::AiCodex(CardId::from("assistant")),
        ActorId::AiCodex(CardId::from("foreign-worker")),
        ActorId::AiCodex(CardId::from("ghost")),
        ActorId::AiCodex(CardId::from("orphan")),
        ActorId::AiCodex(CardId::from("")),
        ActorId::AiClaude(CardId::from("worker")),
        ActorId::AiCodexSession(WorkerSessionId::from("s-live")),
        ActorId::AiPlannerSession(WorkerSessionId::from("s-live")),
        ActorId::AiClaudeSession(WorkerSessionId::from("s-live")),
        ActorId::AiCodexSession(WorkerSessionId::from("s-ghost")),
        ActorId::AiCodexSession(WorkerSessionId::from("")),
    ]
}

fn matrix_scopes() -> [EventScope; MATRIX_SCOPE_COUNT] {
    [
        EventScope::System,
        EventScope::Area {
            area: AreaId::from("c"),
        },
        track_scope("w", "c"),
        track_scope("w2", "c2"),
        card_scope("worker", "w", "c"),
        card_scope("report", "w", "c"),
        card_scope("assistant", "w", "c"),
        card_scope("planner", "w", "c"),
        // Spoof shapes: right card, wrong track / wrong area.
        card_scope("worker", "w2", "c2"),
        card_scope("worker", "w", "c2"),
        card_scope("foreign-worker", "w2", "c2"),
        card_scope("ghost", "w", "c"),
        card_scope("orphan", "w-gone", "c"),
    ]
}

fn matrix_events() -> [(&'static str, Event); MATRIX_EVENT_COUNT] {
    [
        ("track.updated", track_updated()),
        ("area.updated", area_updated()),
        (
            "plan.updated",
            Event::PlanUpdated {
                track_id: TrackId::from("w"),
                changed_keys: Vec::new(),
                agent_message: None,
            },
        ),
        (
            "task.dispatched",
            Event::TaskDispatched {
                idempotency_key: "k".into(),
                kind: "codex".into(),
                agent_message: None,
            },
        ),
        (
            "task.context_frozen",
            Event::TaskContextFrozen {
                track_id: TrackId::from("w"),
                task_key: "k".into(),
                idempotency_key: "k".into(),
                task_id: "t".into(),
                refs: Vec::new(),
                doc_revs: Default::default(),
                truncated: false,
            },
        ),
        (
            "ask.requested",
            Event::AskRequested {
                action: None,
                track_id: TrackId::from("w"),
                questions: vec![calm_types::event::AskQuestion {
                    detail: None,
                    title: "Merge?".into(),
                    options: Vec::new(),
                }],
                delivery: calm_types::event::AskDelivery::Wake,
                source_item_id: None,
            },
        ),
        (
            "ask.answered",
            Event::AskAnswered {
                ask_id: 1,
                track_id: TrackId::from("w"),
                answers: vec![calm_types::event::AskAnswer::Text("yes".into())],
            },
        ),
        (
            "ask.withdrawn",
            Event::AskWithdrawn {
                ask_id: 1,
                track_id: TrackId::from("w"),
            },
        ),
        (
            "hook.codex",
            Event::CodexHook {
                card_id: CardId::from("worker"),
                kind: "hook.codex.pre_tool_use".into(),
                hook_idempotency_key: "k".into(),
                payload: serde_json::json!({}),
            },
        ),
    ]
}

/// Separate strings per denial reason — `is_err()` alone would let a
/// divergence in *why* slip through.
fn verdict(result: std::result::Result<(), RoleViolation>) -> String {
    match result {
        Ok(()) => "allow".to_string(),
        Err(violation) => format!("deny: {violation}"),
    }
}

#[tokio::test]
async fn tx_read_gate_agrees_with_cache_gate_on_the_whole_matrix() {
    let world = matrix_world();
    let (cache, area_cache) = world.caches();
    let mut compared = 0usize;
    let mut denials = 0usize;
    let mut lookup_failures = 0usize;

    for actor in matrix_actors() {
        for (event_label, event) in matrix_events() {
            for scope in matrix_scopes() {
                let mut tx_a = world.tx();
                let from_tx = verdict(
                    enforce_role_resolving_session_from_tx(&mut tx_a, &actor, &event, &scope).await,
                );
                let mut tx_b = world.tx();
                let from_cache = verdict(
                    enforce_role_resolving_session(
                        &mut tx_b,
                        &actor,
                        &event,
                        &scope,
                        &cache,
                        &area_cache,
                    )
                    .await,
                );
                assert_eq!(
                    from_tx, from_cache,
                    "substrate divergence for actor={actor:?} event={event_label} scope={scope:?}"
                );
                compared += 1;
                if from_tx.starts_with("deny") {
                    denials += 1;
                }
                if from_tx.contains("role lookup failed") {
                    lookup_failures += 1;
                }
            }
        }
    }

    // Guard against the matrix silently collapsing to nothing (an empty
    // or all-allow matrix would agree trivially and prove nothing).
    assert_eq!(
        compared,
        MATRIX_ACTOR_COUNT * MATRIX_EVENT_COUNT * MATRIX_SCOPE_COUNT,
        "matrix size changed unexpectedly"
    );
    assert!(
        denials > compared / 4,
        "matrix is too permissive to be evidence: only {denials} of {compared} rows deny"
    );
    // The `orphan` / `w-gone` rows exist so both substrates meet an empty
    // track→area lookup; without a row reaching it the agreement is vacuous.
    assert!(
        lookup_failures > 0,
        "no matrix row reached the track→area lookup miss"
    );
}
