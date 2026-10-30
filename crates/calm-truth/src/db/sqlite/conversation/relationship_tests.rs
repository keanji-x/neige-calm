//! Review regressions through actual SQLite transactions/native receipt owner.
use super::super::{SqlxRepo, begin_immediate_tx};
use super::tests::{body, fixture, plan};
use super::*;
use crate::db::{RepoOutOfDomain, TranscriptReceiptItem};
use calm_types::conversation::{MessageAuthor, PrivateHistoryOperationId};
use calm_types::organization::LEGACY_OWNER_USER_ID;

#[tokio::test]
async fn canonical_turn_initial_relationship_rejects_foreign_initial_same_and_cross_card() {
    let (repo, _, card) = fixture().await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let human = insert_message_tx(
        &mut tx,
        &card,
        &MessageAuthor::AuthenticatedHuman {
            registered_user_id: LEGACY_OWNER_USER_ID.into(),
        },
        body("relationship fixture"),
    )
    .await
    .unwrap();
    let render = plan(human.reference);
    let source_turn = "11111111111111111111111111111111";
    let source_initial = "22222222222222222222222222222222";
    super::tests::submission(&mut tx, &card, source_initial, source_turn, &render).await;
    tx.commit().await.unwrap();
    // A second Card from the real persistent creator, on this same database.
    let (_, other_card) = super::tests::fixture_card(&repo).await;
    for target in [&card, &other_card] {
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        let inserted=sqlx::query("INSERT INTO conversation_turns(id,card_id,initiating_runtime_id,initial_submission_id,execution_kind,
         deployment_owner_user_id,provider,profile,created_at_ms)
         VALUES('44444444444444444444444444444444',?1,'runtime',?2,'legacy_single_user_deployment',?3,'codex',
         'spec',1)")
            .bind(target).bind(source_initial).bind(LEGACY_OWNER_USER_ID).execute(&mut **tx).await;
        let committed = if inserted.is_ok() {
            tx.commit().await.is_ok()
        } else {
            tx.rollback().await.unwrap();
            false
        };
        assert!(
            !committed,
            "a Turn must not commit using another Turn/Card's Initial as its Initial"
        );
    }
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let other_human = insert_message_tx(
        &mut tx,
        &other_card,
        &MessageAuthor::AuthenticatedHuman {
            registered_user_id: LEGACY_OWNER_USER_ID.into(),
        },
        body("own initial on other card"),
    )
    .await
    .unwrap();
    super::tests::submission(
        &mut tx,
        &other_card,
        "55555555555555555555555555555555",
        "66666666666666666666666666666666",
        &plan(other_human.reference),
    )
    .await;
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_turn_initial_relationship_checks_kind_with_deferred_child_first_insertion() {
    let (repo, _, card) = fixture().await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let human = insert_message_tx(
        &mut tx,
        &card,
        &MessageAuthor::AuthenticatedHuman {
            registered_user_id: LEGACY_OWNER_USER_ID.into(),
        },
        body("deferred relationship"),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let render = plan(human.reference);
    for (kind, ordinal, accept) in [("supplemental", 1, false), ("initial", 0, true)] {
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        sqlx::query("PRAGMA defer_foreign_keys=ON")
            .execute(&mut **tx)
            .await
            .unwrap();
        let child = sqlx::query("INSERT INTO conversation_input_submissions(id,card_id,logical_turn_id,ordinal,kind,render_plan_json,
         render_plan_sha256,created_at_ms)
         VALUES('77777777777777777777777777777777',?1,'88888888888888888888888888888888',?2,?3,?4,?5,1)")
            .bind(&card).bind(ordinal).bind(kind).bind(serde_json::to_string(&render).unwrap()).bind(digest(&render).unwrap()).execute(&mut **tx).await;
        // Both are valid children; only Initial may close the Turn's initial relationship.
        child.expect("a valid submission may precede its Turn with deferred foreign keys");
        let inserted=sqlx::query("INSERT INTO conversation_turns(id,card_id,initiating_runtime_id,initial_submission_id,execution_kind,
         deployment_owner_user_id,provider,profile,created_at_ms)
         VALUES('88888888888888888888888888888888',?1,'runtime','77777777777777777777777777777777',
         'legacy_single_user_deployment',?2,'codex','spec',1)")
            .bind(&card).bind(LEGACY_OWNER_USER_ID).execute(&mut **tx).await;
        let committed = if inserted.is_ok() {
            tx.commit().await.is_ok()
        } else {
            tx.rollback().await.unwrap();
            false
        };
        assert_eq!(
            committed, accept,
            "deferred child-first {kind} must obey the own-Initial kind/ordinal contract"
        );
    }
}

async fn native_fixture() -> (SqlxRepo, String, String, i64) {
    let (repo, _, card) = fixture().await;
    let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
        .bind(&card)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    let row = repo
        .fixture_transcript_insert(
            "runtime",
            &card,
            &track,
            "thread",
            Some("turn"),
            Some("reply-stable"),
            Some("agentMessage"),
            "item/started",
            r#"{"item":{"id":"reply-stable","type":"agentMessage","text":"partial"}}"#,
            None,
        )
        .await
        .unwrap();
    (repo, card, track, row)
}

#[tokio::test]
async fn canonical_native_presentation_repairs_real_receipt_restore_without_changing_sequence() {
    let (repo, card, track, started) = native_fixture().await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let sequence = allocate_presentation_tx(
        &mut tx,
        &card,
        PresentationSource::NativeItem { row_id: started },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    repo.transcript_receipt_restore(
        "runtime",
        &card,
        &track,
        "thread",
        "turn",
        &[TranscriptReceiptItem {
            item_uuid: "reply-stable".into(),
            item_type: "agentMessage".into(),
            method: "item/completed".into(),
            params: r#"{"item":{"id":"reply-stable","type":"agentMessage","text":"final"}}"#.into(),
        }],
    )
    .await
    .unwrap();
    let rows = repo
        .transcript_rows_of_thread(&card, "thread")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let restored = rows[0].id;
    assert_ne!(
        restored, started,
        "real receipt owner must rebuild this started row"
    );
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert_eq!(
        allocate_presentation_tx(
            &mut tx,
            &card,
            PresentationSource::NativeItem { row_id: restored }
        )
        .await
        .unwrap(),
        sequence
    );
    let associated: Option<i64> = sqlx::query_scalar(
        "SELECT native_item_row_id FROM conversation_presentations WHERE sequence=?1",
    )
    .bind(sequence)
    .fetch_one(&mut **tx)
    .await
    .unwrap();
    assert_eq!(
        associated,
        Some(restored),
        "stable presentation must repair the new native row binding"
    );
    assert_eq!(
        allocate_presentation_tx(
            &mut tx,
            &card,
            PresentationSource::NativeItem { row_id: restored }
        )
        .await
        .unwrap(),
        sequence
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_native_presentation_refuses_erased_identity_resurrection() {
    let (repo, card, _, row) = native_fixture().await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let sequence = allocate_presentation_tx(
        &mut tx,
        &card,
        PresentationSource::NativeItem { row_id: row },
    )
    .await
    .unwrap();
    let erasure = PrivateHistoryOperationId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
    // Declare the existing erasure owner's identity-only tombstone. This fixture
    // does not reproduce receipt reconstruction or the allocator's update logic.
    sqlx::query("UPDATE conversation_presentations SET erased_by_operation_id=?1,native_item_row_id=NULL WHERE sequence=?2")
        .bind(erasure.as_str()).bind(sequence).execute(&mut **tx).await.unwrap();
    assert!(
        allocate_presentation_tx(
            &mut tx,
            &card,
            PresentationSource::NativeItem { row_id: row }
        )
        .await
        .is_err(),
        "a late native item cannot reattach to an erased identity"
    );
    let associated: Option<i64> = sqlx::query_scalar(
        "SELECT native_item_row_id FROM conversation_presentations WHERE sequence=?1",
    )
    .bind(sequence)
    .fetch_one(&mut **tx)
    .await
    .unwrap();
    assert_eq!(associated, None);
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_native_presentation_excludes_input_projection_and_its_native_upgrade() {
    let (repo, _, card) = fixture().await;
    let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
        .bind(&card)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let human = insert_message_tx(
        &mut tx,
        &card,
        &MessageAuthor::AuthenticatedHuman {
            registered_user_id: LEGACY_OWNER_USER_ID.into(),
        },
        body("canonical only"),
    )
    .await
    .unwrap();
    let sequence = allocate_presentation_tx(
        &mut tx,
        &card,
        PresentationSource::HumanMessage(&human.reference.message_id),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let projection = repo
        .fixture_transcript_insert(
            "runtime",
            &card,
            &track,
            "thread",
            None,
            Some("submission-client"),
            Some("userMessage"),
            "item/completed",
            r#"{"item":{"id":"submission-client","type":"userMessage"},"_projection":true}"#,
            None,
        )
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert!(
        allocate_presentation_tx(
            &mut tx,
            &card,
            PresentationSource::NativeItem { row_id: projection }
        )
        .await
        .is_err(),
        "an input projection already has canonical message/source presentations"
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        repo.transcript_admitted_projection_upgrade(
            &card,
            "submission-client",
            Some("native-turn"),
            "native-input",
            r#"{"item":{"id":"native-input","clientId":"submission-client","type":"userMessage"}}"#
        )
        .await
        .unwrap(),
        Some(projection)
    );
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert!(
        allocate_presentation_tx(
            &mut tx,
            &card,
            PresentationSource::NativeItem { row_id: projection }
        )
        .await
        .is_err(),
        "upgrading input echo native UUID must not allocate a second presentation"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM conversation_presentations WHERE card_id=?1"
        )
        .bind(&card)
        .fetch_one(&mut **tx)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        allocate_presentation_tx(
            &mut tx,
            &card,
            PresentationSource::HumanMessage(&human.reference.message_id)
        )
        .await
        .unwrap(),
        sequence
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_native_presentation_keeps_completed_item_when_started_arrives_late() {
    let (repo, card, track, started) = native_fixture().await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let sequence = allocate_presentation_tx(
        &mut tx,
        &card,
        PresentationSource::NativeItem { row_id: started },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let completed = repo
        .fixture_transcript_insert(
            "runtime",
            &card,
            &track,
            "thread",
            Some("turn"),
            Some("reply-stable"),
            Some("agentMessage"),
            "item/completed",
            "{}",
            None,
        )
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert_eq!(
        allocate_presentation_tx(
            &mut tx,
            &card,
            PresentationSource::NativeItem { row_id: completed }
        )
        .await
        .unwrap(),
        sequence
    );
    tx.commit().await.unwrap();
    let late = repo
        .fixture_transcript_insert(
            "runtime",
            &card,
            &track,
            "thread",
            Some("turn"),
            Some("reply-stable"),
            Some("agentMessage"),
            "item/started",
            "{}",
            None,
        )
        .await
        .unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert_eq!(
        allocate_presentation_tx(
            &mut tx,
            &card,
            PresentationSource::NativeItem { row_id: late }
        )
        .await
        .unwrap(),
        sequence
    );
    assert_eq!(
        sqlx::query_scalar::<_, Option<i64>>(
            "SELECT native_item_row_id FROM conversation_presentations WHERE sequence=?1"
        )
        .bind(sequence)
        .fetch_one(&mut **tx)
        .await
        .unwrap(),
        Some(completed)
    );
    tx.commit().await.unwrap();
}
