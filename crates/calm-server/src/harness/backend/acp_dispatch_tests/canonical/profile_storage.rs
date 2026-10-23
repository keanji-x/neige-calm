//! Canonical Planner input is the ACP profile's claim; storage must admit every claimant.
#![cfg(test)]
use super::*;

use calm_types::worker::WorkerContract;

/// Every worker contract. The match is exhaustive, so a new contract stops this compiling until
/// it is listed and its canonical claim is covered.
fn every_contract() -> [WorkerContract; 4] {
    let all = [
        WorkerContract::Planner,
        WorkerContract::Assistant,
        WorkerContract::Executor,
        WorkerContract::Validator,
    ];
    for contract in all {
        match contract {
            WorkerContract::Planner
            | WorkerContract::Assistant
            | WorkerContract::Executor
            | WorkerContract::Validator => {}
        }
    }
    all
}

/// The claimants come from the profile registry, so a profile that turns canonical input on for
/// any contract without storage for its provider fails here before any worker reaches it. The
/// Turn's provider CHECK does not depend on the contract, so one Planner turn per claimant
/// proves its storage.
#[tokio::test]
async fn canonical_acp_every_claiming_profile_stores_its_turn() {
    let claimants: Vec<_> = AgentProvider::ALL
        .into_iter()
        .filter(|provider| {
            every_contract()
                .into_iter()
                .any(|contract| crate::acp_planner::wiring::canonical_inputs(provider, contract))
        })
        .collect();
    assert!(!claimants.is_empty());
    for provider in claimants {
        let fx = Fixture::with_provider(provider.clone(), "reply", false).await;
        let stored: String = sqlx::query_scalar(
            "SELECT t.provider FROM conversation_turns t
             JOIN conversation_input_submissions s ON s.logical_turn_id=t.id WHERE s.id=?1",
        )
        .bind(fx.submission.as_str())
        .fetch_one(fx.owner.repo.pool())
        .await
        .unwrap();
        assert_eq!(stored, provider.wire_name());
        fx.session.shutdown().await.unwrap();
    }
}
