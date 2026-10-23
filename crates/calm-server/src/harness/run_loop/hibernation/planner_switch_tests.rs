use super::super::completed_commit_tests::Fixture;
use super::*;
use crate::harness::HarnessRegistry;
use crate::harness::registry::CarrierRetirementSlot;
use crate::model::CardRole;
use calm_truth::db::sqlite::conversation::CarrierReplacement;
use calm_truth::planner_binding::PlannerBinding;
use calm_truth::session_repo::SessionRepo;

#[tokio::test]
async fn planner_switch_owner_typed_refusals() {
    let f = Fixture::new().await;
    let inner = &f.harness.inner;
    async fn check(inner: &Arc<Inner>, ingress: bool, expected: Option<OwnerRefusal>) {
        assert_eq!(
            idle_for_lifecycle_guarded(inner, ingress, false)
                .await
                .unwrap(),
            expected
        );
    }
    check(inner, true, None).await;
    *inner.state.lock().await = HarnessState::Compacting {
        since: Instant::now(),
    };
    check(inner, true, Some(OwnerRefusal::TurnInFlight)).await;
    *inner.state.lock().await = HarnessState::Idle;
    check(inner, false, Some(OwnerRefusal::PendingObservations)).await;
    inner
        .inputs
        .historical_queue()
        .unwrap()
        .lock()
        .await
        .push_back(QueueEntry::user_message("pending".into(), None, vec![]));
    check(inner, true, Some(OwnerRefusal::PendingMessages)).await;
    inner
        .inputs
        .historical_queue()
        .unwrap()
        .lock()
        .await
        .clear();
    inner
        .steered_into_running_turn
        .lock()
        .await
        .push(SteeredEntry {
            entry: QueueEntry::user_message("steer".into(), None, vec![]),
            turn_id: "turn".into(),
            row_id: None,
        });
    check(inner, true, Some(OwnerRefusal::PendingMessages)).await;
    inner.steered_into_running_turn.lock().await.clear();
    // No native reader/session proof: absence must never count as idle.
    assert!(matches!(
        f.harness.idle_carrier_owner_typed().await,
        Err(OwnerProofError::Refused {
            reason: OwnerRefusal::SessionUnavailable,
            ..
        })
    ));
    inner.shutting_down.store(true, Ordering::SeqCst);
    assert!(matches!(
        f.harness.lock_if_idle_typed().await,
        Err(OwnerProofError::Refused {
            reason: OwnerRefusal::SessionUnavailable,
            ..
        })
    ));
    inner.shutting_down.store(false, Ordering::SeqCst);
}

async fn replacement(f: &Fixture, role: CardRole) -> CarrierReplacement {
    let inner = &f.harness.inner;
    let worker = f
        .repo
        .session_get(&inner.worker_session_id.clone().into())
        .await
        .unwrap()
        .unwrap();
    let binding = PlannerBinding {
        profile: if role == CardRole::Planner {
            crate::harness::profile::HarnessProfile::Planner
        } else {
            crate::harness::profile::HarnessProfile::Assistant
        },
        provider: crate::session_projection_repo::AgentProvider::Codex,
    };
    CarrierReplacement {
        card_id: inner.card_id.to_string(),
        track_id: inner.track_id.to_string(),
        old_worker: worker.clone(),
        new_worker: worker,
        old_binding: binding.clone(),
        new_binding: binding,
        watermark: 0,
        history_boundary: 0,
        root_session_id: None,
        revoked_card_tokens: 0,
        revoked_session_mirrors: 0,
    }
}

#[tokio::test]
async fn planner_switch_pair_cache_publication() {
    let a = Fixture::new().await;
    let b = Fixture::for_role(CardRole::Assistant).await;
    let old = replacement(&a, CardRole::Assistant).await;
    let new = replacement(&b, CardRole::Planner).await;
    let registry = HarnessRegistry::new();
    let roles = CardRoleCache::new();
    roles.insert(
        old.card_id.clone().into(),
        CardRole::Planner,
        old.track_id.clone().into(),
    );
    roles.insert(
        new.card_id.clone().into(),
        CardRole::Assistant,
        new.track_id.clone().into(),
    );
    let reservations = [
        registry.try_reserve(old.old_worker.id.to_string()).unwrap(),
        registry.try_reserve(new.old_worker.id.to_string()).unwrap(),
    ];
    let slots = [
        registry.carrier_retirement_slot(old.old_worker.id.as_str()),
        registry.carrier_retirement_slot(new.old_worker.id.as_str()),
    ];
    registry
        .publish_pair([(&old, &slots[0]), (&new, &slots[1])], &roles)
        .unwrap();
    assert_eq!(
        roles.get(&old.card_id.clone().into()),
        Some(CardRole::Assistant)
    );
    assert_eq!(
        roles.get(&new.card_id.clone().into()),
        Some(CardRole::Planner)
    );
    for worker in [&old.old_worker.id, &new.old_worker.id] {
        assert!(matches!(
            registry.carrier_retirement_slot(worker.as_str()),
            CarrierRetirementSlot::Absent
        ));
    }
    drop(reservations);
    // The single-card API shares the same retire/cache/cleanup owners.
    registry
        .publish_carrier_replacement(&old, CarrierRetirementSlot::Absent, &roles, None)
        .await
        .unwrap();
    assert_eq!(
        roles.get(&old.card_id.clone().into()),
        Some(CardRole::Assistant)
    );
}
