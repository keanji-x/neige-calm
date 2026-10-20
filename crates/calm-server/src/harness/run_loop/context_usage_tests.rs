use super::completed_commit_tests::Fixture;
use super::*;

const UNKNOWN: &str = "[Trusted Planner context usage]\nContext usage is unknown (no reading yet).";
const KNOWN: &str = "[Trusted Planner context usage]\nContext window: 50% used (142000 of 272000 tokens) as of the latest response before this turn. Record important state in the Report before compacting; conversation text after compaction is not guaranteed to survive.";

#[tokio::test]
async fn context_usage_historical_is_after_budget_and_unknown_without_reading() {
    let fx = Fixture::new().await;
    fx.enqueue(vec![QueueEntry::user_message("hello".into(), None, vec![])])
        .await;
    fx.issue().await;
    let segments = fx.projected_segments().await;
    assert_eq!(
        segments[0].presentation,
        HarnessInputPresentation::SystemPlannerTurnBudget
    );
    assert_eq!(segments[1].text, UNKNOWN);
    assert_eq!(segments.len(), 3);
}

#[tokio::test]
async fn context_usage_historical_uses_percent_policy_and_freezes_failed_attempt() {
    let fx = Fixture::new().await;
    *fx.harness.inner.token_usage.lock().await = Some(TokenUsage {
        used_tokens: 142000,
        total_tokens: 9999999,
        context_window: Some(272000),
        at_ms: 1,
    });
    fx.enqueue(vec![QueueEntry::user_message("hello".into(), None, vec![])])
        .await;
    fx.daemon.fail_turn_start_for_test();
    fx.issue().await;
    let snapshot = fx.harness.snapshot().await.unwrap();
    let wire = serde_json::to_value(&snapshot).unwrap();
    assert!(wire.to_string().contains("Context window: 50% used"));
    let mut restored = HarnessSnapshot::from_value_strict(wire);
    restored.token_usage = None; // A later reading cannot rewrite the issued attempt.
    let inner = &fx.harness.inner;
    let (recovered, _receiver) = PlannerHarness::run_unstarted_for_test(
        PlannerHarnessParams {
            track_delete_locks: Default::default(),
            worker_session_id: inner.worker_session_id.clone(),
            track_id: inner.track_id.clone(),
            card_id: inner.card_id.clone(),
            thread_id: restored.last_thread_id.clone(),
            repo: fx.repo.clone(),
            events: inner.events.clone(),
            card_role_cache: inner.card_role_cache.clone(),
            track_area_cache: inner.track_area_cache.clone(),
            backend: inner.backend.clone(),
            registry: crate::harness::HarnessRegistry::new(),
            config: inner.config,
            snapshot: restored.try_into().unwrap(),
        },
        8,
    );
    fx.daemon.clear_turn_start_failure_for_test();
    maybe_issue_turn(&recovered.inner).await.unwrap();
    assert_eq!(fx.projected_segments().await[1].text, KNOWN);
}

#[tokio::test]
async fn context_usage_worker_has_no_trusted_usage() {
    let fx = Fixture::for_role(crate::model::CardRole::Worker).await;
    fx.enqueue(vec![QueueEntry::user_message("hello".into(), None, vec![])])
        .await;
    fx.issue().await;
    assert_eq!(fx.projected_segments().await.len(), 1);
}

#[tokio::test]
async fn context_usage_historical_unknown_never_renders_a_number() {
    for (used_tokens, context_window, invalidated, reason) in [
        (142000, Some(272000), true, "reset by compaction"),
        (142000, None, false, "window unknown"),
        (1, Some(12000), false, "window unknown"),
        (272001, Some(272000), false, "reading exceeds window"),
    ] {
        let fx = Fixture::new().await;
        *fx.harness.inner.token_usage.lock().await = Some(TokenUsage {
            used_tokens,
            total_tokens: 9999999,
            context_window,
            at_ms: 1,
        });
        if invalidated {
            compact_command::invalidate_usage(&fx.harness.inner, None).await;
        }
        fx.enqueue(vec![QueueEntry::user_message("hello".into(), None, vec![])])
            .await;
        fx.issue().await;
        assert_eq!(
            fx.projected_segments().await[1].text,
            format!("[Trusted Planner context usage]\nContext usage is unknown ({reason}).")
        );
    }
}
