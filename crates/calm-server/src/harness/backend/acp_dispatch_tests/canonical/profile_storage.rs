//! Canonical Planner input is the ACP profile's claim; storage must admit every claimant.
#![cfg(test)]
use super::*;

/// The claimants come from the profile registry, so a profile that turns canonical input on
/// without storage for its provider fails here before any Planner reaches it.
#[tokio::test]
async fn canonical_acp_every_claiming_profile_stores_its_turn() {
    let claimants: Vec<_> = AgentProvider::ALL
        .into_iter()
        .filter(crate::acp_planner::wiring::canonical_planner_inputs)
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
