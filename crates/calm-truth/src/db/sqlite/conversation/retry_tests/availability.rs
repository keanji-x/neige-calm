//! Keep-only reasons and queue exhaustion use the real resolution writer.
use super::*;

#[tokio::test]
async fn canonical_retry_old_invocation_or_inherited_selection_is_keep_only() {
    for (model, effort) in [(None, Some("high")), (Some("frozen"), None), (None, None)] {
        let (repo, _, card) = super::super::tests::fixture().await;
        let s = super::super::retry_storage_tests::seed_repo_invocation(
            repo,
            card,
            false,
            FrozenInputInvocation {
                model: model.map(str::to_owned),
                effort: effort.map(str::to_owned),
                permission: calm_types::harness::PlannerPermissionMode::Never,
            },
        )
        .await;
        let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
        assert_eq!(
            resolve_retry_tx(
                &mut tx,
                &s.card,
                LEGACY_OWNER_USER_ID,
                "unavailable",
                &request(&s),
                &provenance()
            )
            .await
            .unwrap(),
            Err(RetryUnavailableReason::InvocationNotFullyFrozen)
        );
        let carrier = current_input_fact_carrier_tx(&mut tx, &s.card, &s.carrier)
            .await
            .unwrap();
        assert!(carrier.checkpoint().receipts.is_empty());
        assert!(carrier.checkpoint().pending.is_empty());
        keep_conversation_input_tx(
            &mut tx,
            &s.card,
            LEGACY_OWNER_USER_ID,
            "unavailable",
            &request(&s),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_retry_unavailable_inputs_keep_typed_reasons_and_no_writes() {
    #[derive(Clone, Copy)]
    enum Case {
        Old,
        Attachment,
        Erased,
        Withdrawn,
        Thread,
        Binding,
        Source,
    }
    for case in [
        Case::Old,
        Case::Attachment,
        Case::Erased,
        Case::Withdrawn,
        Case::Thread,
        Case::Binding,
        Case::Source,
    ] {
        let (repo, _, card) = super::super::tests::fixture().await;
        let mut payload = super::super::tests::body("frozen Human");
        if matches!(case, Case::Attachment) {
            payload
                .bound_attachments
                .push(calm_types::planner_attachment::BoundAttachment {
                    id: calm_types::planner_attachment::AttachmentId::parse(
                        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa.png",
                    )
                    .unwrap(),
                    size: 1,
                    path: "/no-frozen-archive/mutable.png".into(),
                });
        }
        let s = super::super::retry_storage_tests::seed_repo_payload(
            repo,
            card,
            false,
            FrozenInputInvocation {
                model: Some("frozen".into()),
                effort: Some("high".into()),
                permission: calm_types::harness::PlannerPermissionMode::Never,
            },
            payload,
        )
        .await;
        let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
        let mut execution = provenance();
        let expected = match case {
            Case::Old => {
                let mut carrier = current_input_carrier_tx(&mut tx, &s.card, &s.carrier)
                    .await
                    .unwrap();
                // This represents the released converter's explicit lack of invocation
                // evidence, never a backfilled model/effort.
                carrier.checkpoint.attempts[0].invocation =
                    InputInvocation::PriorInvocationNotRecorded;
                super::super::carrier::save_input_carrier_tx(&mut tx, &mut carrier)
                    .await
                    .unwrap();
                RetryUnavailableReason::PriorInvocationNotRecorded
            }
            Case::Attachment => RetryUnavailableReason::AttachmentsNotFrozen,
            Case::Erased => {
                sqlx::query(
                    "UPDATE conversation_message_revisions SET payload_json=?1 WHERE message_id=?2",
                )
                .bind(
                    serde_json::to_string(
                        &PrivateHistoryPayload::<HumanPayload>::ErasedByPrivateHistoryOperation {
                            operation_id: super::super::tests::operation(),
                        },
                    )
                    .unwrap(),
                )
                .bind(s.human.message_id.as_str())
                .execute(&mut **tx)
                .await
                .unwrap();
                RetryUnavailableReason::ErasedOrWithdrawn
            }
            Case::Withdrawn => {
                sqlx::query("UPDATE conversation_messages SET withdrawn_at_ms=1 WHERE id=?1")
                    .bind(s.human.message_id.as_str())
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                RetryUnavailableReason::ErasedOrWithdrawn
            }
            Case::Thread => {
                sqlx::query(
                    "UPDATE worker_sessions SET thread_id='different-native-thread' WHERE id=?1",
                )
                .bind(&s.carrier)
                .execute(&mut **tx)
                .await
                .unwrap();
                RetryUnavailableReason::UnsupportedBinding
            }
            Case::Binding => {
                let ConversationExecutionProvenance::LegacySingleUserDeployment { profile, .. } =
                    &mut execution;
                *profile = calm_types::harness::HarnessProfile::PlainChat;
                RetryUnavailableReason::UnsupportedBinding
            }
            Case::Source => {
                sqlx::query("UPDATE areas SET conversation_history_contract='shared_history' WHERE id=(SELECT area_id FROM
        tracks WHERE id=(SELECT track_id FROM cards WHERE id=?1))").bind(&s.card).execute(&mut **tx).await.unwrap();
                RetryUnavailableReason::UnsupportedSource
            }
        };
        assert_eq!(
            resolve_retry_tx(
                &mut tx,
                &s.card,
                LEGACY_OWNER_USER_ID,
                "keep-only",
                &request(&s),
                &execution
            )
            .await
            .unwrap(),
            Err(expected)
        );
        let carrier = current_input_fact_carrier_tx(&mut tx, &s.card, &s.carrier)
            .await
            .unwrap();
        assert!(carrier.checkpoint().receipts.is_empty());
        assert!(carrier.checkpoint().pending.is_empty());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_input_retry_relations")
                .fetch_one(&mut **tx)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM conversation_submission_inputs WHERE effective=1"
            )
            .fetch_one(&mut **tx)
            .await
            .unwrap(),
            2
        );
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_retry_queue_exhaustion_has_zero_resolution_writes() {
    let s = seed().await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &s.card, &s.carrier)
        .await
        .unwrap();
    carrier.checkpoint.next_queue_position = i64::MAX as u64;
    super::super::carrier::save_input_carrier_tx(&mut tx, &mut carrier)
        .await
        .unwrap();
    assert!(
        resolve_retry_tx(
            &mut tx,
            &s.card,
            LEGACY_OWNER_USER_ID,
            "exhausted",
            &request(&s),
            &provenance()
        )
        .await
        .is_err()
    );
    let carrier = current_input_fact_carrier_tx(&mut tx, &s.card, &s.carrier)
        .await
        .unwrap();
    assert!(carrier.checkpoint().receipts.is_empty());
    assert!(carrier.checkpoint().pending.is_empty());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_input_retry_relations")
            .fetch_one(&mut **tx)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM conversation_submission_inputs WHERE effective=1"
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap(),
        2
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn claude_retry_exact_local_binding_available() {
    binding_availability(calm_types::runtime::AgentProvider::Claude, false, true).await;
}

#[tokio::test]
async fn claude_retry_mismatched_local_thread_unsupported() {
    binding_availability(calm_types::runtime::AgentProvider::Claude, true, false).await;
}

#[tokio::test]
async fn claude_retry_acp_binding_unsupported() {
    binding_availability(calm_types::runtime::AgentProvider::OpenCode, false, false).await;
}

async fn binding_availability(
    provider: calm_types::runtime::AgentProvider,
    mismatch: bool,
    available: bool,
) {
    let (repo, _, card) = super::super::tests::fixture().await;
    let thread = uuid::Uuid::new_v4().to_string();
    let s = super::super::retry_storage_tests::seed_repo_binding(
        repo,
        card,
        false,
        FrozenInputInvocation {
            model: Some("frozen".into()),
            effort: Some("high".into()),
            permission: calm_types::harness::PlannerPermissionMode::Never,
        },
        super::super::tests::body("frozen Human"),
        (provider, &thread),
    )
    .await;
    let mut tx = begin_immediate_tx(s.repo.db()).await.unwrap();
    if mismatch {
        sqlx::query("UPDATE worker_sessions SET thread_id=?1 WHERE id=?2")
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&s.carrier)
            .execute(&mut **tx)
            .await
            .unwrap();
    }
    let carrier = current_input_fact_carrier_tx(&mut tx, &s.card, &s.carrier)
        .await
        .unwrap();
    let result = super::super::retry::retry_availability_tx(&mut tx, &carrier, &s.root)
        .await
        .unwrap();
    if available {
        assert!(result.is_ok(), "{result:?}");
    } else {
        assert_eq!(result, Err(RetryUnavailableReason::UnsupportedBinding));
    }
    tx.rollback().await.unwrap();
}
