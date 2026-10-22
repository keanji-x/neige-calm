use super::completed_commit_tests::Fixture;
use super::*;
use calm_truth::db::RepoOutOfDomain as _;

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
async fn context_usage_historical_reuses_matching_budget_without_projection_slot() {
    let fx = Fixture::new().await;
    fx.enqueue(vec![QueueEntry::user_message("hello".into(), None, vec![])])
        .await;
    fx.daemon.fail_turn_start_for_test();
    fx.issue().await;
    let attempt = fx
        .harness
        .snapshot()
        .await
        .unwrap()
        .projection_client_id
        .unwrap();
    assert!(matches!(
        fx.harness.snapshot().await.unwrap().turn_budget,
        budget::FrozenBudget::Planner { budget, .. } if u16::from(budget) == 30
    ));
    // Without the pre-issuance slot, the budget rule samples both facts afresh,
    // even though the queued entry will restore the same attempt identity.
    fx.repo
        .settings_upsert(crate::planner_turn_budget::KEY, "15")
        .await
        .unwrap();
    *fx.harness.inner.projection_client_id.lock().await = None;
    *fx.harness.inner.token_usage.lock().await = Some(TokenUsage {
        used_tokens: 142000,
        total_tokens: 9999999,
        context_window: Some(272000),
        at_ms: 1,
    });
    *fx.harness.inner.issuance_retry_after.lock().await = None;
    fx.daemon.clear_turn_start_failure_for_test();
    fx.issue().await;
    let segments = fx.projected_segments().await;
    assert!(segments[0].text.contains("budget is 15 minutes"));
    assert_eq!(segments[1].text, KNOWN);
    assert!(matches!(
        fx.harness.snapshot().await.unwrap().turn_budget,
        budget::FrozenBudget::Planner {
            budget,
            start_attempt,
            context_usage: calm_types::harness::budget::FrozenContextUsage::Text(text),
            ..
        } if start_attempt == attempt && u16::from(budget) == 15 && text == KNOWN
    ));
}

#[tokio::test]
async fn context_usage_historical_pre_feature_attempt_is_sent_unchanged() {
    assert_restored_context_usage("missing_context_usage").await;
}

#[tokio::test]
async fn context_usage_historical_missing_turn_budget_samples() {
    assert_restored_context_usage("missing_turn_budget").await;
}

#[tokio::test]
async fn context_usage_historical_legacy_turn_budget_samples() {
    assert_restored_context_usage("legacy").await;
}

#[tokio::test]
async fn context_usage_historical_other_attempt_samples() {
    assert_restored_context_usage("other_attempt").await;
}

async fn assert_restored_context_usage(shape: &str) {
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
    let mut wire = serde_json::to_value(&snapshot).unwrap();
    assert!(wire.to_string().contains("Context window: 50% used"));
    let attempt = snapshot.projection_client_id.clone().unwrap();
    match shape {
        "missing_context_usage" => {
            wire["turn_budget"]
                .as_object_mut()
                .unwrap()
                .remove("context_usage");
        }
        "missing_turn_budget" => {
            wire.as_object_mut().unwrap().remove("turn_budget");
        }
        "legacy" => wire["turn_budget"] = serde_json::json!({"kind": "legacy"}),
        "other_attempt" => wire["turn_budget"]["start_attempt"] = "other-attempt".into(),
        _ => panic!("unknown snapshot shape"),
    }
    let restored = HarnessSnapshot::from_value_strict(wire);
    assert_eq!(restored.projection_client_id.as_ref(), Some(&attempt));
    // The restored latest reading is numeric, but it was never frozen for this attempt.
    assert!(restored.token_usage.is_some());
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
    let segments = fx.projected_segments().await;
    let expected_context = if shape == "missing_context_usage" {
        assert_eq!(segments.len(), 2);
        assert_eq!(
            segments[0].presentation,
            HarnessInputPresentation::SystemPlannerTurnBudget
        );
        assert_eq!(segments[1].text, "User says:\nhello");
        assert!(
            segments
                .iter()
                .all(|segment| !segment.text.contains("context usage")
                    && !segment.text.contains("not recorded"))
        );
        calm_types::harness::budget::FrozenContextUsage::LegacyNotRecorded
    } else {
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[1].text, KNOWN);
        calm_types::harness::budget::FrozenContextUsage::Text(KNOWN.into())
    };
    let stored: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id = ?")
            .bind(&recovered.inner.worker_session_id)
            .fetch_one(fx.repo.pool())
            .await
            .unwrap();
    let persisted = HarnessSnapshot::from_value_strict(serde_json::from_str(&stored).unwrap());
    for snapshot in [recovered.snapshot().await.unwrap(), persisted] {
        assert!(matches!(
            snapshot.turn_budget,
            budget::FrozenBudget::Planner {
                start_attempt,
                context_usage,
                ..
            } if start_attempt == attempt && context_usage == expected_context
        ));
    }
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
        (-1, Some(258400), false, "invalid reading"),
        (i64::MIN, Some(258400), false, "invalid reading"),
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

#[tokio::test]
async fn context_usage_historical_refuses_render_plan_ownership_without_projection_id() {
    use calm_types::harness::budget::FrozenContextUsage;
    let fx = Fixture::new().await;
    fx.enqueue(vec![QueueEntry::user_message("hello".into(), None, vec![])])
        .await;
    fx.daemon.fail_turn_start_for_test();
    fx.issue().await;
    {
        let mut frozen = fx.harness.inner.turn_budget.lock().await;
        let budget::FrozenBudget::Planner { context_usage, .. } = &mut *frozen else {
            panic!("issued planner budget")
        };
        *context_usage = FrozenContextUsage::RenderPlan;
    }
    *fx.harness.inner.projection_client_id.lock().await = None;
    fx.daemon.clear_turn_start_failure_for_test();
    *fx.harness.inner.issuance_retry_after.lock().await = None;
    fx.issue().await;
    assert_eq!(
        fx.harness.inner.issuance_block.lock().await.as_deref(),
        Some("Cannot retry this turn: its context usage belongs to a canonical render plan.")
    );
    assert_eq!(
        fx.harness
            .inner
            .historical_queue()
            .unwrap()
            .lock()
            .await
            .len(),
        1
    );
    assert!(matches!(
        *fx.harness.inner.turn_budget.lock().await,
        budget::FrozenBudget::Planner {
            context_usage: FrozenContextUsage::RenderPlan,
            ..
        }
    ));
}
