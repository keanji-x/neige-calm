//! Both authorization substrates consume the same source facts and native policy.
use super::*;
use calm_types::event::{AskDelivery, AskQuestion};

fn hold() -> Event {
    Event::AskRequested {
        action: None,
        track_id: "w".into(),
        questions: vec![AskQuestion {
            detail: None,
            title: "Run?".into(),
            options: vec!["Allow".into(), "Deny".into()],
        }],
        delivery: AskDelivery::Hold,
        source_item_id: None,
    }
}
fn world(
    role: CardRole,
) -> (
    FakeWriteTx,
    CardRoleCache,
    TrackAreaCache,
    ActorId,
    EventScope,
) {
    let mut session = worker_session("native", Some("owner".into()));
    session.contract = if role == CardRole::Planner {
        WorkerContract::Planner
    } else {
        WorkerContract::Assistant
    };
    let tx = FakeWriteTx::with_worker_session(session)
        .with_card("owner", role, "w")
        .with_card("other", CardRole::Assistant, "w")
        .with_card("report", CardRole::ReportCard, "w")
        .with_track("w", "c");
    let roles = CardRoleCache::new();
    for (card, role, track) in &tx.cards {
        roles.insert(card.clone(), *role, track.clone());
    }
    let areas = TrackAreaCache::new();
    areas.insert("w".into(), "c".into());
    let actor = if role == CardRole::Planner {
        ActorId::AiPlannerSession("native".into())
    } else {
        ActorId::AiManagedSession("native".into())
    };
    let scope = card_scope("owner", "w", "c");
    (tx, roles, areas, actor, scope)
}
async fn both(
    tx: &mut FakeWriteTx,
    roles: &CardRoleCache,
    areas: &TrackAreaCache,
    actor: &ActorId,
    event: &Event,
    scope: &EventScope,
    allowed: bool,
) {
    let cached = enforce_role_resolving_session(tx, actor, event, scope, roles, areas).await;
    let live = enforce_role_resolving_session_from_tx(tx, actor, event, scope).await;
    assert_eq!(
        cached.as_ref().err().map(std::mem::discriminant),
        live.as_ref().err().map(std::mem::discriminant),
        "substrates differ: {actor:?} {scope:?}"
    );
    assert_eq!(
        cached.is_ok(),
        allowed,
        "{actor:?} {scope:?} {event:?}: {cached:?}"
    );
}

#[tokio::test]
async fn conversations_share_hold_wake_plan_and_track_capabilities_on_both_substrates() {
    for role in [CardRole::Planner, CardRole::Assistant] {
        let (mut tx, roles, areas, actor, scope) = world(role);
        let mut wake = hold();
        if let Event::AskRequested { delivery, .. } = &mut wake {
            *delivery = AskDelivery::Wake;
        }
        for event in [
            hold(),
            wake,
            track_updated(),
            Event::PlanUpdated {
                track_id: "w".into(),
                changed_keys: vec![],
                agent_message: None,
            },
        ] {
            both(&mut tx, &roles, &areas, &actor, &event, &scope, true).await;
        }
        let wrong = if role == CardRole::Planner {
            ActorId::AiManagedSession("native".into())
        } else {
            ActorId::AiPlannerSession("native".into())
        };
        both(&mut tx, &roles, &areas, &wrong, &hold(), &scope, false).await;
    }
}
