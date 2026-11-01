use super::*;
use crate::db::RepoRead;
use crate::db::sqlite::{SqlxRepo, begin_immediate_tx, conversation, session_conversation_tests};
use calm_types::conversation::{HumanPayload, MessageAuthor};
use calm_types::organization::LEGACY_OWNER_USER_ID;

async fn fixture() -> (SqlxRepo, String) {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let (card, _) = session_conversation_tests::card(&repo).await;
    sqlx::query("UPDATE cards SET role='assistant' WHERE id=?1")
        .bind(&card)
        .execute(repo.db().writer_pool())
        .await
        .unwrap();
    (repo, card)
}

async fn insert(
    repo: &SqlxRepo,
    card: &str,
    text: &str,
    present: bool,
) -> conversation::StoredMessageRevision {
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let stored = conversation::insert_message_tx(
        &mut tx,
        card,
        &MessageAuthor::AuthenticatedHuman {
            registered_user_id: LEGACY_OWNER_USER_ID.into(),
        },
        HumanPayload {
            text: text.into(),
            bound_attachments: vec![],
        },
    )
    .await
    .unwrap();
    if present {
        conversation::allocate_presentation_tx(
            &mut tx,
            card,
            conversation::PresentationSource::HumanMessage(&stored.reference.message_id),
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    stored
}

async fn assert_title(repo: &SqlxRepo, card: &str, expected: Option<&str>) {
    let context = repo
        .conversation_title_context(&[card.to_owned()])
        .await
        .unwrap();
    let input = context.sources[0]
        .input
        .as_ref()
        .expect("canonical source, even without a title");
    assert!(matches!(
        input,
        crate::conversation_title::ConversationTitleInput::Canonical(_)
    ));
    assert_eq!(input.title().as_deref(), expected);
    let page = super::super::page(repo.pool(), card, 0, 200, false)
        .await
        .unwrap();
    let displayed = page
        .items
        .iter()
        .find_map(|item| match &item.source {
            ConversationDisplaySource::Human { text, .. } => {
                Some(crate::conversation_title::conversation_title(text))
            }
            _ => None,
        })
        .flatten();
    assert_eq!(
        displayed.as_deref(),
        expected,
        "title and authoritative display must agree"
    );
}

#[tokio::test]
async fn conversation_title_human_current_revision_tombstones_and_empty_first_source() {
    let (repo, card) = fixture().await;
    insert(&repo, &card, "unpresented body is never a title", false).await;
    assert_title(&repo, &card, None).await;
    let first = insert(&repo, &card, "Original", true).await;
    insert(&repo, &card, "Later", true).await;
    assert_title(&repo, &card, Some("Original")).await;
    let body = HumanPayload {
        text: "Revised\nnext line".into(),
        bound_attachments: vec![],
    };
    let payload = serde_json::to_string(&PrivateHistoryPayload::Present { value: &body }).unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    sqlx::query("INSERT INTO conversation_message_revisions(message_id,revision,body_sha256,payload_json,created_at_ms)
        VALUES(?1,2,?2,?3,2)")
        .bind(first.reference.message_id.as_str()).bind(conversation::digest(&body).unwrap())
        .bind(payload).execute(&mut **tx).await.unwrap();
    sqlx::query("UPDATE conversation_messages SET current_revision=2 WHERE id=?1")
        .bind(first.reference.message_id.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_title(&repo, &card, Some("Revised")).await;
    let erased = PrivateHistoryPayload::<HumanPayload>::ErasedByPrivateHistoryOperation {
        operation_id: calm_types::conversation::PrivateHistoryOperationId::parse(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap(),
    };
    sqlx::query("UPDATE conversation_message_revisions SET payload_json=?1 WHERE message_id=?2 AND revision=2")
        .bind(serde_json::to_string(&erased).unwrap()).bind(first.reference.message_id.as_str())
        .execute(repo.db().writer_pool()).await.unwrap();
    assert_title(&repo, &card, Some("Later")).await;
    sqlx::query("UPDATE conversation_messages SET withdrawn_at_ms=1 WHERE card_id=?1")
        .bind(&card)
        .execute(repo.db().writer_pool())
        .await
        .unwrap();
    assert_title(&repo, &card, None).await;
    insert(&repo, &card, "   ", true).await;
    insert(&repo, &card, "Do not borrow this later title", true).await;
    assert_title(&repo, &card, None).await;
}

#[tokio::test]
async fn conversation_title_human_candidates_advance_beyond_removed_batch() {
    let (repo, card) = fixture().await;
    for _ in 0..65 {
        insert(&repo, &card, "removed", true).await;
    }
    sqlx::query("UPDATE conversation_messages SET withdrawn_at_ms=1 WHERE card_id=?1")
        .bind(&card)
        .execute(repo.db().writer_pool())
        .await
        .unwrap();
    insert(&repo, &card, "Beyond removed candidates", true).await;
    assert_title(&repo, &card, Some("Beyond removed candidates")).await;
    let (other, _) = session_conversation_tests::card(&repo).await;
    insert(&repo, &other, "Other card", true).await;
    assert_title(&repo, &card, Some("Beyond removed candidates")).await;
}

#[tokio::test]
async fn conversation_title_human_missing_revision_fails_like_display() {
    let (repo, card) = fixture().await;
    let first = insert(&repo, &card, "Must not disappear silently", true).await;
    insert(&repo, &card, "Do not substitute", true).await;
    // Deferred FKs permit inspecting a missing reference inside a transaction;
    // roll back rather than commit invalid data or disable integrity checks.
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    sqlx::query("DELETE FROM conversation_message_revisions WHERE message_id=?1")
        .bind(first.reference.message_id.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    assert!(first_title_on_connection(&mut tx, &card).await.is_err());
    assert!(
        super::super::page_on_connection(&mut tx, &card, 0, 32, false)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    assert_title(&repo, &card, Some("Must not disappear silently")).await;
}

#[tokio::test]
async fn conversation_title_human_image_only_first_source_blocks_later_title() {
    use calm_types::planner_attachment::{AttachmentId, BoundAttachment};
    let (repo, card) = fixture().await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let stored = conversation::insert_message_tx(
        &mut tx,
        &card,
        &MessageAuthor::AuthenticatedHuman {
            registered_user_id: LEGACY_OWNER_USER_ID.into(),
        },
        HumanPayload {
            text: String::new(),
            bound_attachments: vec![BoundAttachment {
                id: AttachmentId::parse("12345678-1234-4234-8234-123456789abc.png").unwrap(),
                size: 1,
                path: "/tmp/not-read-by-metadata.png".into(),
            }],
        },
    )
    .await
    .unwrap();
    conversation::allocate_presentation_tx(
        &mut tx,
        &card,
        conversation::PresentationSource::HumanMessage(&stored.reference.message_id),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    insert(&repo, &card, "Do not borrow this title", true).await;
    assert_title(&repo, &card, None).await;
}
