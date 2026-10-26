use super::super::{SqlxRepo, begin_immediate_tx};
use super::tests::{body, fixture};
use super::*;
use crate::error::TruthError::Core;
use calm_types::conversation::MessageAuthor;
use calm_types::conversation_display::*;
use calm_types::error::CoreError::Conflict;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use sqlx::{Error::Database, Row};

async fn canonical(repo: &SqlxRepo, card: &str) -> Result<bool> {
    card_has_canonical_storage(&mut *repo.pool().acquire().await?, card).await
}
async fn raw(
    repo: &SqlxRepo,
    card: &str,
    thread: &str,
    id: Option<&str>,
    kind: Option<&str>,
    method: &str,
) -> i64 {
    let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
        .bind(card)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    repo.fixture_transcript_insert(
        "runtime",
        card,
        &track,
        thread,
        Some("turn"),
        id,
        kind,
        method,
        r#"{ "item":{"text":"same words"}}"#,
        None,
    )
    .await
    .unwrap()
}
async fn import(repo: &SqlxRepo, card: &str, id: i64, boundary: i64) -> Result<Option<i64>> {
    let mut tx = begin_immediate_tx(repo.db()).await?;
    let result = import_item_tx(&mut tx, card, id, boundary).await?;
    tx.commit().await?;
    Ok(result)
}
async fn page(repo: &SqlxRepo, card: &str, after: i64, limit: i64, desc: bool) -> ConversationPage {
    super::super::conversation_read::page(repo.pool(), card, after, limit, desc)
        .await
        .unwrap()
}

#[tokio::test]
async fn u2_imported_echo_missing_ids_original_facts_and_digest() {
    let (repo, _, card) = fixture().await;
    assert!(!canonical(&repo, &card).await.unwrap());
    for (id, kind, method) in [
        (Some("echo"), Some("userMessage"), "item/completed"),
        (None, Some("agentMessage"), "item/completed"),
        (Some(""), None, "turn/completed"),
        (Some("snake"), Some("user_message"), "item/completed"),
    ] {
        let row = raw(&repo, &card, "thread", id, kind, method).await;
        import(&repo, &card, row, 0).await.unwrap().unwrap();
    }
    let result = page(&repo, &card, 0, 20, false).await;
    assert_eq!(result.items.len(), 4);
    for item in result.items {
        let ConversationDisplaySource::ImportedItem { item, provenance } = item.source else {
            panic!()
        };
        let ImportedProvenance::UnknownLegacy {
            original_row_id,
            worker_session_id,
            turn_id,
            original_digest,
            ..
        } = *provenance;
        assert_eq!(original_row_id, item.id);
        assert_eq!(worker_session_id, item.worker_session_id);
        assert_eq!(turn_id, item.turn_id);
        assert_eq!(
            original_digest,
            format!(
                "sha256-v1:{}",
                digest(&(item.params.clone(), None::<String>)).unwrap()
            )
        );
        assert_eq!(item.params, r#"{ "item":{"text":"same words"}}"#);
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_messages")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM conversation_input_submissions")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn u2_imported_excludes_indexed_native_identity() {
    let (repo, _, card) = fixture().await;
    let first = raw(
        &repo,
        &card,
        "thread",
        Some("native"),
        Some("agentMessage"),
        "item/completed",
    )
    .await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let seq = allocate_presentation_tx(
        &mut tx,
        &card,
        PresentationSource::NativeItem { row_id: first },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(import(&repo, &card, first, seq).await.unwrap(), None);
    let replacement = raw(
        &repo,
        &card,
        "thread",
        Some("native"),
        Some("agentMessage"),
        "item/completed",
    )
    .await;
    sqlx::query("UPDATE harness_items SET worker_session_id='replacement' WHERE id=?1")
        .bind(replacement)
        .execute(repo.pool())
        .await
        .unwrap();
    assert_eq!(import(&repo, &card, replacement, seq).await.unwrap(), None);
    sqlx::query("UPDATE conversation_presentations SET erased_by_operation_id=?1,native_item_row_id=NULL WHERE sequence=?2")
        .bind("a".repeat(32)).bind(seq).execute(repo.pool()).await.unwrap();
    repo.fixture_transcript_delete(first).await.unwrap();
    assert_eq!(import(&repo, &card, replacement, seq).await.unwrap(), None);
    let distinct = raw(
        &repo,
        &card,
        "other-thread",
        Some("native"),
        Some("agentMessage"),
        "item/completed",
    )
    .await;
    let imported = import(&repo, &card, distinct, seq).await.unwrap().unwrap();
    // Defensive read exclusion happens before LIMIT even when native indexing is later.
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    allocate_presentation_tx(
        &mut tx,
        &card,
        PresentationSource::NativeItem { row_id: distinct },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let p = page(&repo, &card, seq, 1, false).await;
    assert_eq!(p.items.len(), 1);
    assert!(p.items[0].cursor.position > imported);
    assert!(matches!(
        p.items[0].source,
        ConversationDisplaySource::Native { .. }
    ));
}

#[tokio::test]
async fn u2_imported_mixed_boundary_order_pagination_and_identity_erasure() {
    let (repo, _, card) = fixture().await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let human = insert_message_tx(
        &mut tx,
        &card,
        &MessageAuthor::AuthenticatedHuman {
            registered_user_id: LEGACY_OWNER_USER_ID.into(),
        },
        body("same words"),
    )
    .await
    .unwrap();
    let boundary = allocate_presentation_tx(
        &mut tx,
        &card,
        PresentationSource::HumanMessage(&human.reference.message_id),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let old = page(&repo, &card, 0, 1, false).await.items[0]
        .cursor
        .clone();
    let a = raw(
        &repo,
        &card,
        "thread",
        Some("a"),
        Some("userMessage"),
        "item/completed",
    )
    .await;
    let b = raw(
        &repo,
        &card,
        "thread",
        Some("b"),
        Some("userMessage"),
        "item/completed",
    )
    .await;
    sqlx::query("UPDATE harness_items SET created_at_ms=1000-id WHERE card_id=?1")
        .bind(&card)
        .execute(repo.pool())
        .await
        .unwrap();
    assert!(matches!(
        import(&repo, &card, a, boundary + 1).await,
        Err(Core(Conflict(_)))
    ));
    let sa = import(&repo, &card, a, boundary).await.unwrap().unwrap();
    let sb = import(&repo, &card, b, boundary).await.unwrap().unwrap();
    assert_eq!(import(&repo, &card, a, boundary).await.unwrap(), Some(sa));
    assert!(sa > boundary && sb > sa);
    assert_eq!(page(&repo, &card, 0, 1, false).await.items[0].cursor, old);
    for desc in [false, true] {
        let mut after = 0;
        let mut positions = Vec::new();
        loop {
            let p = page(&repo, &card, after, 1, desc).await;
            positions.extend(p.items.iter().map(|i| i.cursor.position));
            if let Some(next) = p.next_cursor {
                after = next.position;
            } else {
                break;
            }
        }
        assert_eq!(
            positions,
            if desc {
                vec![sb, sa, boundary]
            } else {
                vec![boundary, sa, sb]
            }
        );
    }
    assert!(page(&repo, &card, 0, 0, false).await.items.is_empty());
    let before =
        serde_json::to_value(&page(&repo, &card, boundary, 1, false).await.items[0]).unwrap();
    sqlx::query(
        "UPDATE conversation_presentations SET erased_by_operation_id=?1 WHERE sequence=?2",
    )
    .bind("b".repeat(32))
    .bind(sa)
    .execute(repo.pool())
    .await
    .unwrap();
    repo.fixture_transcript_delete(a).await.unwrap();
    assert_eq!(import(&repo, &card, a, boundary).await.unwrap(), Some(sa));
    let p = page(&repo, &card, boundary, 20, false).await;
    let removed = serde_json::to_value(&p.items[0]).unwrap();
    assert_eq!(removed["cursor"], before["cursor"]);
    assert_eq!(
        removed["source"]["identity"]["provenance"],
        before["source"]["provenance"]
    );
    assert!(removed["source"].get("item").is_none());
    assert!(matches!(
        p.items[1].source,
        ConversationDisplaySource::ImportedItem { .. }
    ));
    assert!(matches!(
        sqlx::query(
            "UPDATE conversation_presentations SET imported_provenance_json='{}' WHERE sequence=?1"
        )
        .bind(sa)
        .execute(repo.pool())
        .await,
        Err(Database(ref db)) if db.code().as_deref() == Some("1811")
    ));
    sqlx::query("UPDATE harness_items SET params='{}' WHERE id=?1")
        .bind(b)
        .execute(repo.pool())
        .await
        .unwrap();
    assert!(matches!(
        super::super::conversation_read::page(repo.pool(), &card, sa, 1, false).await,
        Err(Core(Conflict(_)))
    ));
    repo.fixture_transcript_delete(b).await.unwrap();
    assert!(matches!(
        super::super::conversation_read::page(repo.pool(), &card, sa, 1, false).await,
        Err(Core(Conflict(_)))
    ));
}

#[tokio::test]
async fn u2_imported_schema_only_preserves_contract_sequence_fk_and_trigger() {
    for empty in [false, true] {
        let db = super::super::SqliteDb::connect("sqlite::memory:")
            .await
            .unwrap();
        let old = sqlx::migrate::Migrator {
            migrations: std::borrow::Cow::Owned(
                crate::MIGRATOR
                    .iter()
                    .filter(|m| m.version < 196)
                    .cloned()
                    .collect(),
            ),
            ..sqlx::migrate::Migrator::DEFAULT
        };
        old.run(db.writer_pool()).await.unwrap();
        let repo = SqlxRepo {
            db,
            card_role_cache: super::super::CardRoleCache::new(),
            track_area_cache: super::super::TrackAreaCache::new(),
            database_id: std::sync::Arc::new("u2".into()),
        };
        let (_, card) = super::tests::fixture_card(&repo).await;
        let (_, historical) = super::tests::fixture_card(&repo).await;
        raw(
            &repo,
            &historical,
            "legacy",
            None,
            Some("userMessage"),
            "item/completed",
        )
        .await;
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        let h = insert_message_tx(
            &mut tx,
            &card,
            &MessageAuthor::AuthenticatedHuman {
                registered_user_id: LEGACY_OWNER_USER_ID.into(),
            },
            body("original"),
        )
        .await
        .unwrap();
        allocate_presentation_tx(
            &mut tx,
            &card,
            PresentationSource::HumanMessage(&h.reference.message_id),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let native = raw(
            &repo,
            &card,
            "old-thread",
            Some("old-item"),
            Some("agentMessage"),
            "item/completed",
        )
        .await;
        let identity = presentation::native_source_identity(
            "old-thread",
            Some("old-item"),
            Some("agentMessage"),
            "item/completed",
            &serde_json::json!({}),
        )
        .unwrap();
        sqlx::query(
            "INSERT INTO conversation_presentations(
            card_id,source_kind,source_identity,native_item_row_id,erased_by_operation_id)
            VALUES(?1,'native_item',?2,?3,?4)",
        )
        .bind(&card)
        .bind(identity)
        .bind(native)
        .bind("e".repeat(32))
        .execute(repo.pool())
        .await
        .unwrap();
        if empty {
            sqlx::query("DELETE FROM conversation_presentations")
                .execute(repo.pool())
                .await
                .unwrap();
        }
        sqlx::query("UPDATE sqlite_sequence SET seq=9000 WHERE name='conversation_presentations'")
            .execute(repo.pool())
            .await
            .unwrap();
        let cards: Vec<(String, String)> =
            sqlx::query_as("SELECT id,payload FROM cards ORDER BY id")
                .fetch_all(repo.pool())
                .await
                .unwrap();
        type OldPresentation = (
            i64,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<String>,
        );
        let old_columns =
            "SELECT sequence,card_id,source_kind,source_identity,message_id,observation_id,
            native_item_row_id,submission_id,erased_by_operation_id
            FROM conversation_presentations ORDER BY sequence";
        let before: Vec<OldPresentation> = sqlx::query_as(old_columns)
            .fetch_all(repo.pool())
            .await
            .unwrap();
        crate::MIGRATOR.run(repo.pool()).await.unwrap();
        assert_eq!(
            sqlx::query_as::<_, (String, String)>("SELECT id,payload FROM cards ORDER BY id")
                .fetch_all(repo.pool())
                .await
                .unwrap(),
            cards
        );
        assert_eq!(
            sqlx::query_as::<_, OldPresentation>(old_columns)
                .fetch_all(repo.pool())
                .await
                .unwrap(),
            before
        );
        assert!(!canonical(&repo, &historical).await.unwrap());
        assert!(canonical(&repo, &card).await.unwrap());
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT seq FROM sqlite_sequence WHERE name='conversation_presentations'"
            )
            .fetch_one(repo.pool())
            .await
            .unwrap(),
            9000
        );
        assert!(
            sqlx::query("PRAGMA foreign_key_check")
                .fetch_all(repo.pool())
                .await
                .unwrap()
                .is_empty()
        );
        let error=sqlx::query("INSERT INTO conversation_assistant_creations VALUES(?1,'track',?2,?3,'carrier','message',1,'{}')")
            .bind(&card).bind("a".repeat(64)).bind(LEGACY_OWNER_USER_ID).execute(repo.pool()).await.unwrap_err();
        assert!(matches!(error, Database(ref db) if db.code().as_deref() == Some("1811")));
        let row = raw(
            &repo,
            &card,
            "thread",
            None,
            Some("userMessage"),
            "item/completed",
        )
        .await;
        assert!(matches!(
            import(&repo, &historical, row, 0).await,
            Err(Core(Conflict(_)))
        ));
        assert!(
            import(
                &repo,
                &card,
                row,
                if empty { 0 } else { before.last().unwrap().0 }
            )
            .await
            .unwrap()
            .unwrap()
                > 9000
        );
        let stored = sqlx::query(
            "SELECT * FROM conversation_presentations WHERE source_kind='imported_item'",
        )
        .fetch_one(repo.pool())
        .await
        .unwrap();
        assert!(
            stored
                .try_get::<Option<String>, _>("submission_id")
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn u2_imported_projection_segments_attachments_original_bytes_and_row_order() {
    let (repo, _, card) = fixture().await;
    let a = raw(
        &repo,
        &card,
        "thread",
        Some("projection"),
        Some("agentMessage"),
        "item/completed",
    )
    .await;
    let b = raw(
        &repo,
        &card,
        "thread",
        None,
        Some("userMessage"),
        "item/completed",
    )
    .await;
    let params = r#"{ "_projection":true,"item":{"type":"agentMessage","text":"legacy"}}"#;
    let segments = r#"[ {"presentation":"user","text":"original","attachments":[{"id":"00000000-0000-4000-8000-000000000001.png","size":1,"url":"/api/cards/image","contentType":"image/png"}]} ]"#;
    sqlx::query("UPDATE harness_items SET params=?1,input_segments=?2 WHERE id=?3")
        .bind(params)
        .bind(segments)
        .bind(a)
        .execute(repo.pool())
        .await
        .unwrap();
    let seq = import(&repo, &card, a, 0).await.unwrap().unwrap();
    import(&repo, &card, b, 0).await.unwrap();
    let p = page(&repo, &card, 0, 20, false).await;
    let ConversationDisplaySource::ImportedItem { item, provenance } = &p.items[0].source else {
        panic!()
    };
    assert_eq!(item.params, params);
    assert_eq!(
        item.input_segments.as_ref().unwrap()[0].attachments[0]
            .id
            .as_str(),
        "00000000-0000-4000-8000-000000000001.png"
    );
    let ImportedProvenance::UnknownLegacy {
        original_digest, ..
    } = provenance.as_ref();
    assert_eq!(
        *original_digest,
        format!("sha256-v1:{}", digest(&(params, Some(segments))).unwrap())
    );
    assert_ne!(
        *original_digest,
        format!("sha256-v1:{}", digest(&(params, None::<String>)).unwrap())
    );
    assert_eq!(p.items[0].cursor.position, seq);
    // Caller cannot reconstruct chronological order by inserting an older row later.
    let (other, _, other_card) = fixture().await;
    let old = raw(&other, &other_card, "t", None, None, "turn/completed").await;
    let new = raw(&other, &other_card, "t", None, None, "turn/completed").await;
    import(&other, &other_card, new, 0).await.unwrap();
    assert!(matches!(
        import(&other, &other_card, old, 0).await,
        Err(Core(Conflict(_)))
    ));
}

#[tokio::test]
async fn u2_imported_erasure_fences_native_replacement_and_observer() {
    for native_first in [false, true] {
        let (repo, _, _) = fixture().await;
        let (card, worker, _) = super::held_projection_tests::held(&repo).await;
        let row = raw(
            &repo,
            &card,
            "thread",
            Some("erased"),
            Some("agentMessage"),
            "item/completed",
        )
        .await;
        let boundary: i64 = sqlx::query_scalar(
            "SELECT MAX(sequence) FROM conversation_presentations WHERE card_id=?1",
        )
        .bind(&card)
        .fetch_one(repo.pool())
        .await
        .unwrap();
        let seq = import(&repo, &card, row, boundary).await.unwrap().unwrap();
        let before =
            serde_json::to_value(&page(&repo, &card, boundary, 20, false).await.items[0]).unwrap();
        let sibling = raw(
            &repo,
            &card,
            "thread",
            Some("erased"),
            Some("agentMessage"),
            "item/completed",
        )
        .await;
        import(&repo, &card, sibling, boundary)
            .await
            .unwrap()
            .unwrap();
        if native_first {
            let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
            allocate_presentation_tx(
                &mut tx,
                &card,
                PresentationSource::NativeItem { row_id: row },
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
        }
        sqlx::query(
            "UPDATE conversation_presentations SET erased_by_operation_id=?1 WHERE sequence=?2",
        )
        .bind("d".repeat(32))
        .bind(seq)
        .execute(repo.pool())
        .await
        .unwrap();
        repo.fixture_transcript_delete(row).await.unwrap();
        let replacement = raw(
            &repo,
            &card,
            "thread",
            Some("erased"),
            Some("agentMessage"),
            "item/completed",
        )
        .await;
        sqlx::query("UPDATE harness_items SET worker_session_id='replacement' WHERE id=?1")
            .bind(replacement)
            .execute(repo.pool())
            .await
            .unwrap();
        assert_eq!(
            import(&repo, &card, replacement, boundary).await.unwrap(),
            None
        );
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        assert!(matches!(
            allocate_presentation_tx(
                &mut tx,
                &card,
                PresentationSource::NativeItem {
                    row_id: replacement
                }
            )
            .await,
            Err(Core(Conflict(_)))
        ));
        let carrier = current_input_carrier_tx(&mut tx, &card, &worker)
            .await
            .unwrap();
        assert!(observe_native_item_tx(&mut tx, &carrier, "thread", Some("turn"), "item/completed",
        &serde_json::json!({"item":{"id":"erased","type":"agentMessage","text":"same words"}})).await.unwrap().is_none());
        tx.commit().await.unwrap();
        let result = page(&repo, &card, boundary, 20, false).await;
        assert_eq!(result.items.len(), 1);
        assert_eq!(
            serde_json::to_value(page(&repo, &card, 0, 1, true).await.items).unwrap(),
            serde_json::to_value(&result.items).unwrap()
        );
        let removed = serde_json::to_value(&result.items[0]).unwrap();
        assert_eq!(
            removed["source"]["identity"]["provenance"],
            before["source"]["provenance"]
        );
        assert_eq!(removed["source"]["kind"], "removed");
        assert!(removed["source"].get("item").is_none());
        for (thread, item) in [("other-thread", "erased"), ("thread", "other-item")] {
            let distinct = raw(
                &repo,
                &card,
                thread,
                Some(item),
                Some("agentMessage"),
                "item/completed",
            )
            .await;
            let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
            allocate_presentation_tx(
                &mut tx,
                &card,
                PresentationSource::NativeItem { row_id: distinct },
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
        }
        assert_eq!(page(&repo, &card, boundary, 20, false).await.items.len(), 3);
        let (_, other_card) = super::tests::fixture_card(&repo).await;
        let other = raw(
            &repo,
            &other_card,
            "thread",
            Some("erased"),
            Some("agentMessage"),
            "item/completed",
        )
        .await;
        assert!(
            import(&repo, &other_card, other, 0)
                .await
                .unwrap()
                .is_some()
        );
    }
}
