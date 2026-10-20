//! Real schema and transaction primitives; queue/receipt/runtime integration is
//! exercised by their owning production entrances in the closed V2 switch.
use super::super::{
    SqlxRepo, TrackWorkspacePlan, area_create_tx, begin_immediate_tx, card_create_with_id_tx,
    track_create_tx,
};
use super::*;
use crate::model::{CardRole, NewArea, NewCard, NewTrack, RequestTheme};
use calm_types::conversation::*;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use serde_json::json;

pub(super) async fn fixture() -> (SqlxRepo, String, String) {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let (area, card) = fixture_card(&repo).await;
    (repo, area, card)
}

pub(super) async fn fixture_card(repo: &SqlxRepo) -> (String, String) {
    fixture_card_for_provider(repo, calm_types::runtime::AgentProvider::Codex).await
}

pub(super) async fn fixture_card_for_provider(
    repo: &SqlxRepo,
    provider: calm_types::runtime::AgentProvider,
) -> (String, String) {
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let area = area_create_tx(
        &mut tx,
        LEGACY_OWNER_USER_ID,
        NewArea {
            name: "canonical".into(),
            color: "#123".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let track = track_create_tx(
        &mut tx,
        NewTrack {
            area_id: area.id.clone(),
            title: "canonical".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        crate::model::new_id(),
        NewCard {
            track_id: track.id,
            kind: "codex".into(),
            sort: None,
            payload: json!({"planner_provider":provider.wire_name()}),
            title: None,
        },
        CardRole::Planner,
        true,
        repo.card_role_cache(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (area.id.to_string(), card.id.to_string())
}

fn author() -> MessageAuthor {
    MessageAuthor::AuthenticatedHuman {
        registered_user_id: LEGACY_OWNER_USER_ID.into(),
    }
}
pub(super) fn body(text: &str) -> HumanPayload {
    HumanPayload {
        text: text.into(),
        bound_attachments: vec![],
    }
}
pub(super) fn operation() -> PrivateHistoryOperationId {
    PrivateHistoryOperationId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap()
}
pub(super) fn plan(reference: MessageRevisionRef) -> SubmissionRenderPlan {
    SubmissionRenderPlan::V1 {
        segments: vec![SubmissionRenderSegment::Input {
            input_position: 0,
            input: SubmissionRenderInput::Human {
                message: reference,
                author: FrozenHumanAttribution {
                    registered_user_id: LEGACY_OWNER_USER_ID.into(),
                    display_name: "Owner".into(),
                },
            },
        }],
    }
}

pub(super) async fn submission(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card: &str,
    initial: &str,
    turn: &str,
    render: &SubmissionRenderPlan,
) {
    sqlx::query("INSERT INTO conversation_turns(id,card_id,initiating_runtime_id,initial_submission_id,execution_kind,
         deployment_owner_user_id,provider,profile,created_at_ms)
         VALUES(?1,?2,'original-runtime',?3,'legacy_single_user_deployment',?4,'codex','spec',1)")
        .bind(turn).bind(card).bind(initial).bind(LEGACY_OWNER_USER_ID).execute(&mut **tx).await.unwrap();
    sqlx::query("INSERT INTO conversation_input_submissions(id,card_id,logical_turn_id,ordinal,kind,render_plan_json,
         render_plan_sha256,created_at_ms)
         VALUES(?1,?2,?3,0,'initial',?4,?5,1)")
        .bind(initial).bind(card).bind(turn).bind(serde_json::to_string(render).unwrap()).bind(digest(render).unwrap()).execute(&mut **tx).await.unwrap();
}

#[tokio::test]
async fn canonical_revision_preserves_author_digest_and_erasure_without_body_recovery() {
    let (repo, _, card) = fixture().await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let first = insert_message_tx(&mut tx, &card, &author(), body("private body"))
        .await
        .unwrap();
    assert_eq!(
        message_tx(&mut tx, &first.reference.message_id)
            .await
            .unwrap()
            .author,
        author()
    );
    assert_eq!(revision_tx(&mut tx, &first.reference).await.unwrap(), first);
    assert!(
        insert_message_tx(
            &mut tx,
            &card,
            &MessageAuthor::AuthenticatedHuman {
                registered_user_id: "unknown".into()
            },
            body("unknown")
        )
        .await
        .is_err()
    );
    // An explicit persisted tombstone tests the storage/read invariant without an erase API.
    sqlx::query("UPDATE conversation_message_revisions SET payload_json=?1 WHERE message_id=?2")
        .bind(
            serde_json::to_string(
                &PrivateHistoryPayload::<HumanPayload>::ErasedByPrivateHistoryOperation {
                    operation_id: operation(),
                },
            )
            .unwrap(),
        )
        .bind(first.reference.message_id.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    let erased = revision_tx(&mut tx, &first.reference).await.unwrap();
    assert_eq!(erased.body_sha256, first.body_sha256);
    assert!(matches!(
        erased.payload,
        PrivateHistoryPayload::ErasedByPrivateHistoryOperation { .. }
    ));
    assert!(
        sqlx::query(
            "UPDATE conversation_message_revisions SET payload_json=?1 WHERE message_id=?2"
        )
        .bind(serde_json::to_string(&first.payload).unwrap())
        .bind(first.reference.message_id.as_str())
        .execute(&mut **tx)
        .await
        .is_err(),
        "erased revision must not be restored by a stale writer"
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_schema_rejects_missing_author_fields_and_implicit_current_revisions() {
    let (repo, _, card) = fixture().await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert!(sqlx::query("INSERT INTO conversation_messages(id,card_id,current_revision,author_kind,created_at_ms)
         VALUES('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',?1,1,'authenticated_human',1)").bind(&card).execute(&mut **tx).await.is_err());
    tx.rollback().await.unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    sqlx::query(
        "INSERT INTO conversation_messages(id,card_id,current_revision,author_kind,author_user_id,
         created_at_ms)
         VALUES('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',?1,1,'authenticated_human',?2,1)",
    )
    .bind(&card)
    .bind(LEGACY_OWNER_USER_ID)
    .execute(&mut **tx)
    .await
    .unwrap();
    assert!(
        tx.commit().await.is_err(),
        "message cannot commit without its required current revision"
    );
}

#[tokio::test]
async fn canonical_effective_claim_is_unique_by_message_identity_across_revisions() {
    let (repo, _, card) = fixture().await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let first = insert_message_tx(&mut tx, &card, &author(), body("one"))
        .await
        .unwrap();
    let mut second = first.clone();
    second.reference.revision = std::num::NonZeroU32::new(2).unwrap();
    sqlx::query("INSERT INTO conversation_message_revisions(message_id,revision,body_sha256,payload_json,created_at_ms)
         SELECT message_id,2,body_sha256,payload_json,created_at_ms
         FROM conversation_message_revisions WHERE message_id=?1").bind(first.reference.message_id.as_str()).execute(&mut **tx).await.unwrap();
    let initial = "11111111111111111111111111111111";
    let later = "22222222222222222222222222222222";
    let turn = "33333333333333333333333333333333";
    submission(
        &mut tx,
        &card,
        initial,
        turn,
        &plan(second.reference.clone()),
    )
    .await;
    sqlx::query("INSERT INTO conversation_submission_inputs(submission_id,input_position,input_kind,message_id,
         revision,queue_position,queued_at_ms,effective)
         VALUES(?1,0,'human',?2,2,0,10,1)")
        .bind(initial).bind(first.reference.message_id.as_str()).execute(&mut **tx).await.unwrap();
    submission(
        &mut tx,
        &card,
        later,
        "44444444444444444444444444444444",
        &plan(first.reference.clone()),
    )
    .await;
    assert!(sqlx::query("INSERT INTO conversation_submission_inputs(submission_id,input_position,input_kind,message_id,
         revision,queue_position,queued_at_ms,effective)
         VALUES(?1,0,'human',?2,1,0,10,1)")
        .bind(later).bind(first.reference.message_id.as_str()).execute(&mut **tx).await.is_err(),"another revision is not another accepted message identity");
    assert!(
        sqlx::query(
            "UPDATE conversation_submission_inputs SET input_kind='legacy' WHERE submission_id=?1"
        )
        .bind(initial)
        .execute(&mut **tx)
        .await
        .is_err(),
        "claims cannot change source kind"
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_presentation_reuses_identity_before_any_native_thread_and_rolls_back_atomically()
{
    let (repo, area, card) = fixture().await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert_eq!(
        history_contract_tx(&mut tx, &card).await.unwrap(),
        ConversationHistoryContract::Private
    );
    let first = insert_message_tx(&mut tx, &card, &author(), body("same text"))
        .await
        .unwrap();
    let seq = allocate_presentation_tx(
        &mut tx,
        &card,
        PresentationSource::HumanMessage(&first.reference.message_id),
    )
    .await
    .unwrap();
    assert_eq!(
        seq,
        allocate_presentation_tx(
            &mut tx,
            &card,
            PresentationSource::HumanMessage(&first.reference.message_id)
        )
        .await
        .unwrap()
    );
    let second = insert_message_tx(&mut tx, &card, &author(), body("same text"))
        .await
        .unwrap();
    assert!(
        allocate_presentation_tx(
            &mut tx,
            &card,
            PresentationSource::HumanMessage(&second.reference.message_id)
        )
        .await
        .unwrap()
            > seq
    );
    // Model an already-persisted erasure fact; there is no canonical erase command.
    sqlx::query("UPDATE conversation_message_revisions SET payload_json=?1 WHERE message_id=?2")
        .bind(
            serde_json::to_string(
                &PrivateHistoryPayload::<HumanPayload>::ErasedByPrivateHistoryOperation {
                    operation_id: operation(),
                },
            )
            .unwrap(),
        )
        .bind((first.reference.message_id).as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    assert_eq!(
        seq,
        allocate_presentation_tx(
            &mut tx,
            &card,
            PresentationSource::HumanMessage(&first.reference.message_id)
        )
        .await
        .unwrap()
    );
    sqlx::query("UPDATE areas SET conversation_history_contract='shared_history' WHERE id=?1")
        .bind(&area)
        .execute(&mut **tx)
        .await
        .unwrap();
    assert_eq!(
        history_contract_tx(&mut tx, &card).await.unwrap(),
        ConversationHistoryContract::SharedHistory
    );
    assert!(
        sqlx::query("UPDATE areas SET conversation_history_contract='private' WHERE id=?1")
            .bind(&area)
            .execute(&mut **tx)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_messages")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_presentations")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn canonical_system_plan_erasure_preserves_original_digest_and_rejects_body_restore() {
    let (repo, _, card) = fixture().await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let human = insert_message_tx(&mut tx, &card, &author(), body("human private body"))
        .await
        .unwrap();
    let mut render = plan(human.reference.clone());
    let observation_id = SystemObservationId::parse("44444444444444444444444444444444").unwrap();
    let SubmissionRenderPlan::V1 { segments } = &mut render;
    segments.push(SubmissionRenderSegment::Input {
        input_position: 1,
        input: SubmissionRenderInput::System {
            observation_id: observation_id.clone(),
            payload: PrivateHistoryPayload::Present {
                value: SystemObservation::try_from(
                    calm_types::observation::Observation::SystemContext {
                        text: "machine private body".into(),
                    },
                )
                .unwrap(),
            },
        },
    });
    segments.push(SubmissionRenderSegment::Context {
        payload: PrivateHistoryPayload::Present {
            value: SystemRenderContext::new(
                calm_types::model::HarnessInputPresentation::System,
                "private turn context".into(),
            )
            .unwrap(),
        },
    });
    let submission_id = InputSubmissionId::parse("11111111111111111111111111111111").unwrap();
    submission(
        &mut tx,
        &card,
        submission_id.as_str(),
        "33333333333333333333333333333333",
        &render,
    )
    .await;
    sqlx::query("INSERT INTO conversation_submission_inputs(submission_id,input_position,input_kind,message_id,
         revision,queue_position,queued_at_ms,effective)
         VALUES(?1,0,'human',?2,1,0,10,1)")
        .bind(submission_id.as_str()).bind(human.reference.message_id.as_str()).execute(&mut **tx).await.unwrap();
    sqlx::query("INSERT INTO conversation_submission_inputs(submission_id,input_position,input_kind,observation_id,
         queue_position,queued_at_ms,system_addressable,effective)
         VALUES(?1,1,'system',?2,1,10,1,1)")
        .bind(submission_id.as_str()).bind(observation_id.as_str()).execute(&mut **tx).await.unwrap();
    let original = submission_plan_tx(&mut tx, &submission_id).await.unwrap();
    assert_eq!(original.plan, render);
    let mut changed = serde_json::to_value(&render).unwrap();
    changed["segments"][1]["input"]["payload"]["value"]["text"] = json!("quietly replaced body");
    assert!(
        sqlx::query("UPDATE conversation_input_submissions SET render_plan_json=?1 WHERE id=?2")
            .bind(serde_json::to_string(&changed).unwrap())
            .bind(submission_id.as_str())
            .execute(&mut **tx)
            .await
            .is_err()
    );
    // Persist tombstones at this fixture's known payload positions, retaining all identities.
    let tombstone = serde_json::to_string(
        &PrivateHistoryPayload::<HumanPayload>::ErasedByPrivateHistoryOperation {
            operation_id: operation(),
        },
    )
    .unwrap();
    sqlx::query("UPDATE conversation_input_submissions SET render_plan_json=
         json_set(render_plan_json,'$.segments[1].input.payload',json(?1),'$.segments[2].payload',json(?1))
         WHERE id=?2")
        .bind(tombstone).bind(submission_id.as_str()).execute(&mut **tx).await.unwrap();
    // Model an already-persisted erasure fact; there is no canonical erase command.
    sqlx::query("UPDATE conversation_message_revisions SET payload_json=?1 WHERE message_id=?2")
        .bind(
            serde_json::to_string(
                &PrivateHistoryPayload::<HumanPayload>::ErasedByPrivateHistoryOperation {
                    operation_id: operation(),
                },
            )
            .unwrap(),
        )
        .bind((human.reference.message_id).as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    let erased = submission_plan_tx(&mut tx, &submission_id).await.unwrap();
    assert_eq!(erased.original_digest, original.original_digest);
    assert_eq!(
        erased.plan.input_refs().unwrap(),
        original.plan.input_refs().unwrap()
    );
    let serialized = serde_json::to_string(&erased.plan).unwrap();
    assert!(!serialized.contains("machine private body"));
    assert!(!serialized.contains("private turn context"));
    assert!(serialized.contains("erased_by_private_history_operation"));
    let attempt = InputAttempt {
        submission_id: submission_id.clone(),
        attempt_id: InputAttemptId::parse("55555555555555555555555555555555").unwrap(),
    };
    assert!(
        SubmissionDispatchProof::from_stored_plan(attempt, &erased.plan, erased.original_digest)
            .is_err(),
        "erasure cannot mint a replacement dispatch proof"
    );
    assert!(
        sqlx::query("UPDATE conversation_input_submissions SET render_plan_json=?1 WHERE id=?2")
            .bind(serde_json::to_string(&render).unwrap())
            .bind(submission_id.as_str())
            .execute(&mut **tx)
            .await
            .is_err(),
        "old envelopes cannot restore an erased plan"
    );
    tx.commit().await.unwrap();
}
