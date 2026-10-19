//! Exact identity survives batching; queue ownership, rather than equal text, decides presence.
use super::completed_commit_tests::Fixture;
use super::input_inspection::InputInspection;
use super::input_inspection_recovery_tests::confirm_projection;
use super::*;
use calm_types::conversation_receipt::InitialInputUnavailableReason as Reason;

#[tokio::test]
async fn initial_input_inspection_names_each_equal_text_entry_in_one_batch() {
    let fx = Fixture::new().await;
    let first = QueueEntry::user_message("same words".into(), None, vec![]);
    let second = QueueEntry::user_message("same words".into(), None, vec![]);
    let first_id = first.id().unwrap().clone();
    let second_id = second.id().unwrap().clone();
    // A system entry ahead of either user must not become their identity.
    let system = QueueEntry::system(
        Observation::TrackGoal {
            text: "context".into(),
        },
        None,
    )
    .unwrap();
    fx.enqueue(vec![system.clone(), first, system, second])
        .await;
    fx.issue().await;
    confirm_projection(&fx.harness, &fx.daemon, &first_id).await;
    let first = fx.harness.inspect_input(first_id).await.unwrap();
    let second = fx.harness.inspect_input(second_id).await.unwrap();
    let InputInspection::Projected {
        item_id,
        segment_index,
        ..
    } = first
    else {
        panic!("{first:?}");
    };
    let InputInspection::Projected {
        item_id: other_row,
        segment_index: other_segment,
        ..
    } = second
    else {
        panic!("{second:?}");
    };
    assert_eq!(item_id, other_row);
    assert_eq!(other_segment, segment_index + 2);
    assert_eq!(
        fx.harness
            .inspect_input(QueueEntryId::from_wire("not-the-equal-text-entry".into()))
            .await
            .unwrap(),
        InputInspection::Unavailable(Reason::RuntimeUnavailable)
    );
}

#[tokio::test]
async fn initial_input_inspection_reports_current_edit_then_real_deletion() {
    let fx = Fixture::new().await;
    let entry = QueueEntry::user_message("original".into(), None, vec![]);
    let id = entry.id().unwrap().clone();
    fx.enqueue(vec![entry]).await;
    assert_eq!(
        fx.harness.inspect_input(id.clone()).await.unwrap(),
        InputInspection::Queued {
            entry_id: id.to_string(),
            rev: 0,
            text: "original".into()
        }
    );
    fx.harness
        .mutate_pending_entry(
            QueueMutation::Edit {
                entry_id: id.clone(),
                text: "edited elsewhere".into(),
                if_entry_rev: 0,
            },
            ActorId::User,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fx.harness.inspect_input(id.clone()).await.unwrap(),
        InputInspection::Queued {
            entry_id: id.to_string(),
            rev: 1,
            text: "edited elsewhere".into()
        }
    );
    fx.harness
        .mutate_pending_entry(
            QueueMutation::Delete {
                entry_id: id.clone(),
                if_entry_rev: 1,
            },
            ActorId::User,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fx.harness.inspect_input(id).await.unwrap(),
        InputInspection::Unavailable(Reason::RuntimeUnavailable)
    );
}

#[tokio::test]
async fn initial_input_inspection_prefers_rebuffered_queue_to_stale_projection() {
    let fx = Fixture::new().await;
    let entry = QueueEntry::user_message("original".into(), None, vec![]);
    let id = entry.id().unwrap().clone();
    fx.enqueue(vec![entry.clone()]).await;
    fx.issue().await;
    confirm_projection(&fx.harness, &fx.daemon, &id).await;
    assert!(matches!(
        fx.harness.inspect_input(id.clone()).await.unwrap(),
        InputInspection::Projected { .. }
    ));
    fx.enqueue(vec![entry]).await;
    assert!(matches!(
        fx.harness.inspect_input(id).await.unwrap(),
        InputInspection::Queued { .. }
    ));
}

#[tokio::test]
async fn initial_input_inspection_never_infers_absence_from_unsettled_or_legacy_projection() {
    let fx = Fixture::new().await;
    let id = QueueEntryId::from_wire("missing".into());
    *fx.harness.inner.projection_client_id.lock().await = Some(id.clone());
    assert_eq!(
        fx.harness.inspect_input(id.clone()).await.unwrap(),
        InputInspection::Unavailable(Reason::RuntimeUnavailable)
    );
    *fx.harness.inner.projection_client_id.lock().await = None;
    let legacy = HarnessInputSegment {
        presentation: HarnessInputPresentation::User,
        text: "legacy words".into(),
        attachments: vec![],
        origin: None,
    };
    insert_projection_row(
        &fx.harness.inner,
        "thread-commit-wake",
        "legacy-projection",
        &[legacy],
    )
    .await
    .unwrap();
    assert_eq!(
        fx.harness.inspect_input(id).await.unwrap(),
        InputInspection::Unavailable(Reason::LegacyProjection)
    );
}

#[tokio::test]
async fn initial_input_inspection_searches_beyond_the_loaded_transcript_page() {
    let fx = Fixture::new().await;
    let entry = QueueEntry::user_message("old accepted sentence".into(), None, vec![]);
    let id = entry.id().unwrap().clone();
    fx.enqueue(vec![entry]).await;
    fx.issue().await;
    confirm_projection(&fx.harness, &fx.daemon, &id).await;
    let original = fx.harness.inspect_input(id.clone()).await.unwrap();
    assert!(matches!(original, InputInspection::Projected { .. }));
    let system = HarnessInputSegment {
        presentation: HarnessInputPresentation::System,
        text: "later system context".into(),
        attachments: vec![],
        origin: None,
    };
    for index in 0..130 {
        insert_projection_row(
            &fx.harness.inner,
            "thread-commit-wake",
            &format!("later-{index}"),
            std::slice::from_ref(&system),
        )
        .await
        .unwrap();
    }
    assert_eq!(fx.harness.inspect_input(id).await.unwrap(), original);
}

#[tokio::test]
async fn initial_input_inspection_preserves_the_shared_id_of_folded_sends() {
    let fx = Fixture::new().await;
    // Folding is the real full-queue admission path, not ordinary adjacent enqueue.
    fx.enqueue(
        (0..MAX_PENDING_QUEUE_LEN - 1)
            .map(|index| {
                QueueEntry::system(
                    Observation::SystemContext {
                        text: format!("context {index}"),
                    },
                    None,
                )
                .unwrap()
            })
            .collect(),
    )
    .await;
    let first = fx
        .harness
        .observe_user_message_durable("first".into(), vec![], SendKey::unique_for_test())
        .await
        .unwrap()
        .entry_id
        .unwrap();
    let second = fx
        .harness
        .observe_user_message_durable("second".into(), vec![], SendKey::unique_for_test())
        .await
        .unwrap()
        .entry_id
        .unwrap();
    assert_eq!(
        first, second,
        "the real enqueue owner folded both accepted sends"
    );
    fx.issue().await;
    confirm_projection(&fx.harness, &fx.daemon, &first).await;
    let observed = fx.harness.inspect_input(first).await.unwrap();
    assert!(matches!(observed, InputInspection::Projected { .. }));
    assert_eq!(fx.harness.inspect_input(second).await.unwrap(), observed);
}

#[tokio::test]
async fn initial_input_inspection_shutdown_is_unknown_even_with_an_empty_queue() {
    let fx = Fixture::new().await;
    fx.harness.shutdown().await.unwrap();
    assert_eq!(
        fx.harness
            .inspect_input(QueueEntryId::from_wire("previously-accepted".into()))
            .await
            .unwrap(),
        InputInspection::Unavailable(Reason::ShuttingDown)
    );
}

#[tokio::test]
async fn initial_input_inspection_legacy_queue_cannot_prove_absence() {
    let fx = Fixture::new().await;
    fx.enqueue(vec![QueueEntry::LegacyUser {
        text: "legacy accepted words".into(),
        envelope_id: None,
        message_ids: vec![],
    }])
    .await;
    assert_eq!(
        fx.harness
            .inspect_input(QueueEntryId::from_wire("old-receipt".into()))
            .await
            .unwrap(),
        InputInspection::Unavailable(Reason::LegacyProjection)
    );
}

#[tokio::test]
async fn initial_input_inspection_rejects_failed_issuance_residue_after_queue_deletion() {
    let fx = Fixture::new().await;
    let entry = QueueEntry::user_message("accepted then removed".into(), None, vec![]);
    let id = entry.id().unwrap().clone();
    fx.enqueue(vec![entry]).await;
    // The first delete finds no rows; the failed turn's cleanup hits the real stored projection.
    fx.reject_projection_retirement().await;
    fx.daemon.fail_turn_start_for_test();
    assert!(maybe_issue_turn(&fx.harness.inner).await.is_err());
    assert_eq!(
        fx.harness.inner.projection_client_id.lock().await.as_ref(),
        Some(&id)
    );
    assert_eq!(
        fx.projected_segments()
            .await
            .last()
            .unwrap()
            .origin
            .as_ref()
            .unwrap()
            .queue_entry
            .as_ref()
            .map(|entry| entry.id.as_str()),
        Some(id.as_str())
    );
    let InputInspection::Queued { rev, .. } = fx.harness.inspect_input(id.clone()).await.unwrap()
    else {
        panic!("failed issuance must rebuffer the actual input before deletion");
    };
    sqlx::query("DROP TRIGGER refuse_projection_cleanup")
        .execute(fx.repo.db().writer_pool())
        .await
        .unwrap();
    fx.harness
        .mutate_pending_entry(
            QueueMutation::Delete {
                entry_id: id.clone(),
                if_entry_rev: rev,
            },
            ActorId::User,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fx.harness.inspect_input(id).await.unwrap(),
        InputInspection::Unavailable(Reason::RuntimeUnavailable)
    );
}

#[tokio::test]
async fn initial_input_inspection_does_not_lose_a_steer_whose_projection_failed() {
    let fx = Fixture::new().await;
    let mut events = fx.harness.inner.backend.subscribe_events();
    fx.enqueue(vec![QueueEntry::user_message(
        "start a running turn".into(),
        None,
        vec![],
    )])
    .await;
    fx.issue().await;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        let started = matches!(&event.kind, PlannerEventKind::TurnStarted { .. });
        on_notification(
            &fx.harness.inner,
            &fx.harness.inner.live_claim.writer(),
            event,
        )
        .await
        .unwrap();
        if started {
            break;
        }
    }
    assert_eq!(
        fx.harness.snapshot().await.phase,
        HarnessPhaseTag::TurnRunning
    );
    let entry = QueueEntry::user_message("accepted by the running turn".into(), None, vec![]);
    let id = entry.id().unwrap().clone();
    fx.enqueue(vec![entry]).await;
    sqlx::query(
        "CREATE TRIGGER refuse_steer_projection BEFORE INSERT ON harness_items \
         BEGIN SELECT RAISE(ABORT, 'forced steer projection failure'); END",
    )
    .execute(fx.repo.pool())
    .await
    .unwrap();
    fx.harness
        .steer_pending_entry(id.clone(), 0, ActorId::User)
        .await
        .unwrap()
        .unwrap();
    assert!(
        fx.harness
            .inner
            .steered_into_running_turn
            .lock()
            .await
            .iter()
            .any(|steered| steered.entry.id() == Some(&id) && steered.row_id.is_none())
    );
    assert_eq!(
        fx.harness.inspect_input(id).await.unwrap(),
        InputInspection::Unavailable(Reason::RuntimeUnavailable)
    );
    assert_eq!(
        fx.harness
            .inspect_input(QueueEntryId::from_wire("unrelated-entry".into()))
            .await
            .unwrap(),
        InputInspection::Unavailable(Reason::RuntimeUnavailable)
    );
}
