//! Real mixed-batch delivery pins receipt ownership across kernel prefixes.
use super::*;

#[tokio::test]
async fn receipt_mixed_user_and_results_keep_exact_segment_ownership() {
    mixed_batch(false).await;
}

#[tokio::test]
async fn receipt_mixed_results_with_budget_and_timeout_keep_exact_segment_ownership() {
    mixed_batch(true).await;
}

async fn mixed_batch(with_timeout: bool) {
    let fx = Fixture::new().await;
    if with_timeout {
        fx.harness
            .observe_user_message_durable(
                "previous".into(),
                vec![],
                crate::harness::SendKey::unique_for_test(),
            )
            .await
            .unwrap();
        maybe_issue_turn(&fx.harness.inner).await.unwrap();
        let inner = &fx.harness.inner;
        let turn_id = inner.issued_turn_id.lock().await.clone().unwrap();
        on_notification(
            inner,
            &inner.live_claim.writer(),
            PlannerEvent {
                thread_id: inner.thread_id.read().await.clone(),
                kind: PlannerEventKind::TurnStarted {
                    turn_id: turn_id.clone(),
                },
            },
        )
        .await
        .unwrap();
        if let HarnessState::TurnRunning { watchdog_from, .. } =
            &mut *fx.harness.inner.state.lock().await
        {
            *watchdog_from = Instant::now() - Duration::from_secs(1801);
        }
        issue_interrupt_for_turn(
            &fx.harness.inner,
            turn_id.clone(),
            "max_turn_duration".into(),
        )
        .await
        .unwrap();
        let inner = &fx.harness.inner;
        let thread = inner.thread_id.read().await.clone().unwrap();
        let outcome = json!({"id":turn_id,"status":"interrupted"});
        crate::harness::turn_outcome::record(
            fx.repo.as_ref(),
            &inner.worker_session_id,
            inner.card_id.as_str(),
            inner.track_id.as_str(),
            &thread,
            &turn_id,
            &outcome,
            crate::harness::turn_outcome::Confirmation::ProviderTerminal,
        )
        .await
        .unwrap();
        on_notification(
            inner,
            &inner.live_claim.writer(),
            PlannerEvent {
                thread_id: Some(thread),
                kind: PlannerEventKind::TurnCompleted { turn: outcome },
            },
        )
        .await
        .unwrap();
    }
    fx.harness
        .observe_user_message_durable(
            "user-only".into(),
            vec![],
            crate::harness::SendKey::unique_for_test(),
        )
        .await
        .unwrap();
    let completed = enqueue_event(
        &fx,
        Event::TaskCompleted {
            idempotency_key: "mixed-completed".into(),
            result: json!("completion"),
            artifacts: vec![],
            agent_message: None,
        },
    )
    .await;
    let failed = enqueue_event(
        &fx,
        Event::TaskFailed {
            idempotency_key: "mixed-failed".into(),
            reason: "failure".into(),
            details: None,
            agent_message: None,
        },
    )
    .await;
    maybe_issue_turn(&fx.harness.inner).await.unwrap();
    let segments = fx.projected_segments().await;
    assert_eq!(segments.len(), if with_timeout { 6 } else { 5 });
    let budget_index = usize::from(with_timeout);
    assert_eq!(
        segments[budget_index].presentation,
        crate::model::HarnessInputPresentation::SystemPlannerTurnBudget
    );
    assert_eq!(segments[budget_index].origin, None);
    assert_eq!(
        segments[budget_index + 1].presentation,
        crate::model::HarnessInputPresentation::SystemPlannerContextUsage
    );
    assert_eq!(segments[budget_index + 1].origin, None);
    if with_timeout {
        assert_eq!(
            segments[0].presentation,
            crate::model::HarnessInputPresentation::System
        );
        assert_eq!(segments[0].origin, None);
        assert!(segments[0].text.contains("Trusted previous turn timeout"));
    }
    let sent = fx.daemon.started_turns_for_test();
    assert_eq!(sent.len(), if with_timeout { 2 } else { 1 });
    let InputItem::Text { text } = &sent.last().unwrap().1[0] else {
        panic!("text")
    };
    assert!(
        text.contains(
            &segments
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        )
    );
    let user = segments
        .iter()
        .find(|s| s.presentation == crate::model::HarnessInputPresentation::User)
        .unwrap();
    assert_eq!(user.origin.as_ref().unwrap().observation, "user_message");
    assert_eq!(user.origin.as_ref().unwrap().event_id, None);
    assert_eq!(user.text, "User says:\nuser-only");
    for (observation, identity, kind, id, presentation) in [
        (
            "task_completed",
            "mixed-completed",
            "completed",
            completed,
            crate::model::HarnessInputPresentation::SystemTaskCompleted,
        ),
        (
            "task_failed",
            "mixed-failed",
            "failed",
            failed,
            crate::model::HarnessInputPresentation::SystemTaskFailed,
        ),
    ] {
        let segment = segments
            .iter()
            .find(|s| s.origin.as_ref().is_some_and(|o| o.event_id == Some(id)))
            .unwrap();
        assert_eq!(segment.presentation, presentation);
        assert_eq!(segment.origin.as_ref().unwrap().observation, observation);
        assert_only_detail(&segment.text, RECORDED_WITH_EVENT, identity, kind, id);
        let details = read_details(&fx, &segment.text).await;
        assert_eq!(details["attempt_id"], identity);
        assert_eq!(details["events"][kind]["event_id"], id);
    }
}
