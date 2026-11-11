//! Root render erasure preserves linked retry references.
use super::*;

#[tokio::test]
async fn canonical_retry_storage_root_erasure_propagates_to_all_references() {
    let s = seed().await;
    let write_db = (s.repo.db()).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                successor(&mut tx, &s).await;
                let mut erased = s.render.clone();
                let SubmissionRenderPlan::V1 { segments } = &mut erased;
                for segment in segments {
                    match segment {
                        SubmissionRenderSegment::Input {
                            input: SubmissionRenderInput::System { payload, .. },
                            ..
                        } => {
                            *payload = PrivateHistoryPayload::ErasedByPrivateHistoryOperation {
                                operation_id: super::super::tests::operation(),
                            }
                        }
                        SubmissionRenderSegment::Context { payload } => {
                            *payload = PrivateHistoryPayload::ErasedByPrivateHistoryOperation {
                                operation_id: super::super::tests::operation(),
                            }
                        }
                        SubmissionRenderSegment::Input {
                            input:
                                SubmissionRenderInput::Human { .. }
                                | SubmissionRenderInput::LinkGuest { .. },
                            ..
                        } => {}
                    }
                }
                sqlx::query(
                    "UPDATE conversation_input_submissions SET render_plan_json=?1 WHERE id=?2",
                )
                .bind(serde_json::to_string(&erased).unwrap())
                .bind(s.root.as_str())
                .execute(&mut **tx)
                .await
                .unwrap();
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
                for id in [s.root.clone(), InputSubmissionId::parse(SUCCESSOR).unwrap()] {
                    let stored = submission_plan_tx(&mut tx, &id).await.unwrap();
                    assert_eq!(stored.plan, erased);
                    assert_eq!(stored.original_digest.as_str(), digest(&s.render).unwrap());
                }
                assert!(matches!(
                    revision_tx(&mut tx, &s.human).await.unwrap().payload,
                    PrivateHistoryPayload::ErasedByPrivateHistoryOperation { .. }
                ));
                let page = crate::db::sqlite::conversation_read::page_on_connection(
                    &mut tx, &s.card, 0, 100, false,
                )
                .await
                .unwrap();
                let display = serde_json::to_string(&page).unwrap();
                assert!(!display.contains("frozen System") && !display.contains("frozen Human"));
                tx.commit().await.unwrap();
            })
        })
        .await
        .expect("independent write operation")
}
