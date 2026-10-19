use super::*;

// Equivalence matrix: both substrates are projected from one `World`, so
// (actor × event × scope) must agree exactly, including the violation text.
// States where cache and DB disagree cannot be expressed here.

/// The rows both substrates are built from.
struct World {
    cards: Vec<(&'static str, CardRole, &'static str)>,
    tracks: Vec<(&'static str, &'static str)>,
    session: Option<WorkerSessionRow>,
    bindings: Vec<(CardId, String)>,
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
        tx.bindings = self.bindings.clone();
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
        bindings: vec![(CardId::from("planner"), "s-live".into())],
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
