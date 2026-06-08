use std::sync::Arc;

use calm_server::db::Repo;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::harness::{
    HarnessConfig, HarnessSnapshot, HookKind, Observation, SpecHarness, SpecHarnessParams,
};
use calm_server::ids::{CardId, WaveId};
use calm_server::model::new_id;
use calm_server::shared_codex_appserver::SharedCodexAppServer;

async fn harness_from_snapshot(snapshot: HarnessSnapshot) -> SpecHarness {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let daemon = SharedCodexAppServer::new_stub(repo_dyn.clone());
    let (harness, _rx) = SpecHarness::run_unstarted_for_test(
        SpecHarnessParams {
            runtime_id: new_id(),
            wave_id: WaveId::from("wave-backpressure"),
            card_id: CardId::from("card-backpressure"),
            thread_id: Some("thread-backpressure".into()),
            repo: repo_dyn,
            events: EventBus::new(),
            card_role_cache: calm_server::card_role_cache::CardRoleCache::new(),
            wave_cove_cache: calm_server::wave_cove_cache::WaveCoveCache::new(),
            daemon,
            config: HarnessConfig::default(),
            snapshot,
        },
        1024,
    );
    harness
}

fn worker_hook_stop(idempotency_key: impl Into<String>) -> Observation {
    let idempotency_key = idempotency_key.into();
    Observation::WorkerHookStop {
        wave_id: WaveId::from("wave-backpressure"),
        card_id: CardId::from(format!("worker-{idempotency_key}")),
        kind: HookKind::CodexStop,
        idempotency_key,
    }
}

#[tokio::test]
async fn wave_goal_backpressure_keeps_queue_bounded_and_last_text() {
    let harness = harness_from_snapshot(HarnessSnapshot::initial(0, vec![])).await;
    for i in 0..1000 {
        harness
            .observe_for_test(
                Observation::WaveGoal {
                    text: format!("g{i}"),
                },
                None,
            )
            .await;
    }

    let pending = harness.pending_queue_for_test().await;
    assert!(pending.len() <= 256, "pending len = {}", pending.len());
    assert!(matches!(
        pending.last(),
        Some(Observation::WaveGoal { text }) if text == "g999"
    ));
}

#[tokio::test]
async fn full_hard_queue_drops_new_soft_observations() {
    let harness = harness_from_snapshot(HarnessSnapshot::initial(0, vec![])).await;
    for i in 0..300 {
        harness
            .observe_for_test(worker_hook_stop(format!("hook-{i}")), Some(i))
            .await;
    }
    for i in 0..300 {
        harness
            .observe_for_test(
                Observation::WaveGoal {
                    text: format!("soft-{i}"),
                },
                None,
            )
            .await;
    }

    let pending = harness.pending_queue_for_test().await;
    assert_eq!(pending.len(), 256);
    assert!(
        pending
            .iter()
            .all(|obs| matches!(obs, Observation::WorkerHookStop { .. })),
        "pending = {pending:?}"
    );
}

#[tokio::test]
async fn full_hard_queue_then_incoming_hard_drops_new() {
    let observations = (0..256)
        .map(|i| worker_hook_stop(format!("hook-{i}")))
        .collect();
    let harness = harness_from_snapshot(HarnessSnapshot::initial(0, observations)).await;

    harness
        .observe_for_test(worker_hook_stop("hook-new"), Some(256))
        .await;

    let pending = harness.pending_queue_for_test().await;
    assert_eq!(pending.len(), 256);
    assert!(
        pending.iter().all(Observation::is_hard_fire),
        "pending = {pending:?}"
    );
    assert!(
        pending.iter().any(
            |obs| matches!(obs, Observation::WorkerHookStop { idempotency_key, .. } if idempotency_key == "hook-0")
        ),
        "old hard observations must be retained: {pending:?}"
    );
    assert!(
        !pending.iter().any(
            |obs| matches!(obs, Observation::WorkerHookStop { idempotency_key, .. } if idempotency_key == "hook-new")
        ),
        "incoming hard must be dropped as a last resort: {pending:?}"
    );
}

#[tokio::test]
async fn full_soft_queue_incoming_hard_preserves_hard_and_evicts_oldest_soft() {
    let observations = (0..256)
        .map(|i| Observation::WaveGoal {
            text: format!("soft-{i}"),
        })
        .collect();
    let harness = harness_from_snapshot(HarnessSnapshot::initial(0, observations)).await;

    harness
        .observe_for_test(worker_hook_stop("hard-after-soft"), Some(256))
        .await;

    let pending = harness.pending_queue_for_test().await;
    assert_eq!(pending.len(), 256);
    assert_eq!(
        pending.iter().filter(|obs| !obs.is_hard_fire()).count(),
        255
    );
    assert!(
        pending.iter().any(
            |obs| matches!(obs, Observation::WorkerHookStop { idempotency_key, .. } if idempotency_key == "hard-after-soft")
        ),
        "incoming hard must be preserved: {pending:?}"
    );
    assert!(
        !pending
            .iter()
            .any(|obs| matches!(obs, Observation::WaveGoal { text } if text == "soft-0")),
        "oldest soft must be evicted: {pending:?}"
    );
    assert!(
        pending
            .iter()
            .any(|obs| matches!(obs, Observation::WaveGoal { text } if text == "soft-1")),
        "newer soft observations should be retained: {pending:?}"
    );
}

#[tokio::test]
async fn oversized_snapshot_keeps_newest_pending_queue_tail() {
    let observations = (0..300)
        .map(|i| Observation::WaveGoal {
            text: format!("g{i}"),
        })
        .collect::<Vec<_>>();
    let mut snapshot = HarnessSnapshot::initial(0, observations);
    snapshot.pending_envelope_ids = (0..300).map(Some).collect();

    let harness = harness_from_snapshot(snapshot).await;
    let restored = harness.snapshot().await;

    assert_eq!(restored.pending_queue.len(), 256);
    assert_eq!(restored.pending_envelope_ids.len(), 256);
    assert!(matches!(
        restored.pending_queue.first(),
        Some(Observation::WaveGoal { text }) if text == "g44"
    ));
    assert!(matches!(
        restored.pending_queue.last(),
        Some(Observation::WaveGoal { text }) if text == "g299"
    ));
    assert_eq!(restored.pending_envelope_ids.first(), Some(&Some(44)));
    assert_eq!(restored.pending_envelope_ids.last(), Some(&Some(299)));
}
