use super::atomic::replay;
use super::*;

#[tokio::test]
async fn receipt_rejects_contradictory_original_acceptance() {
    let f = Fixture::new().await;
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "original")
        .await
        .unwrap();
    let id = f.append(&mut tx, &f.event("bootstrap", "original")).await;
    sqlx::query("SAVEPOINT capture")
        .execute(&mut **tx)
        .await
        .unwrap();
    let (accepted, good) = system_assistant_accept_tx(&mut tx, &f.card, id)
        .await
        .unwrap();
    sqlx::query("ROLLBACK TO capture")
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query("RELEASE capture")
        .execute(&mut **tx)
        .await
        .unwrap();
    // Restore real scope facts to test the schema, independently of the private writer.
    db::session_set_handle_state_tx(
        &mut tx,
        &f.worker,
        Some(accepted.snapshot_after_write.clone()),
    )
    .await
    .unwrap();
    let AcceptedInputRef::System { observation_id } = &accepted.input_ref else {
        panic!()
    };
    assert_eq!(
        allocate_presentation_tx(
            &mut tx,
            &f.card,
            PresentationSource::SystemObservation(observation_id)
        )
        .await
        .unwrap(),
        accepted.presentation_sequence
    );
    for field in [
        "floor", "digest", "sequence", "kind", "subject", "track", "source",
    ] {
        let mut bad = good.clone();
        match field {
            "floor" => {
                bad.acceptance = SystemAssistantReceiptKind::Bootstrap {
                    creation_floor: 999,
                }
            }
            "digest" => bad.payload_digest = "0".repeat(64),
            "sequence" => bad.presentation_sequence += 1,
            "kind" => bad.acceptance = SystemAssistantReceiptKind::Input,
            "subject" => bad.subject = "other".into(),
            "track" => bad.track_id = "other".into(),
            "source" => (),
            _ => unreachable!(),
        }
        let mut json = serde_json::to_value(&bad).unwrap();
        if field == "source" {
            json["source"] = "other".into();
        }
        assert!(sqlx::query("INSERT INTO conversation_system_assistant_receipts VALUES(?1,'today-summary','bootstrap',?2,?3,1,?4)")
            .bind(&f.card).bind(id).bind(&good.observation_id).bind(serde_json::to_string(&json).unwrap()).execute(&mut **tx).await.is_err(), "{field}");
    }
    assert!(
        replay(&mut tx, &f, "bootstrap", id, &accepted)
            .await
            .is_err()
    );
    sqlx::query("INSERT INTO conversation_system_assistant_receipts VALUES(?1,'today-summary','bootstrap',?2,?3,1,?4)")
        .bind(&f.card).bind(id).bind(&good.observation_id).bind(serde_json::to_string(&good).unwrap()).execute(&mut **tx).await.unwrap();
    assert_eq!(
        replay(&mut tx, &f, "bootstrap", id, &accepted)
            .await
            .unwrap(),
        good
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn entry_derives_floor_and_refuses_occupied_or_nonempty_original() {
    let f = Fixture::new().await;
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    let mut last = 0;
    for _ in 0..10 {
        last = f.append(&mut tx, &f.event("bootstrap", "original")).await;
    }
    assert_eq!(last, 10);
    db::session_set_handle_state_tx(
        &mut tx,
        &f.worker,
        Some(serde_json::to_value(CanonicalHarnessSnapshot::dormant(last)).unwrap()),
    )
    .await
    .unwrap();
    system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "original")
        .await
        .unwrap();
    assert!(
        system_assistant_accept_tx(&mut tx, &f.card, last)
            .await
            .is_err()
    );
    let (id, accepted) = f.accept(&mut tx, &f.event("bootstrap", "original")).await;
    assert_eq!(
        replay(&mut tx, &f, "bootstrap", id, &accepted)
            .await
            .unwrap()
            .acceptance,
        SystemAssistantReceiptKind::Bootstrap { creation_floor: 10 }
    );
    assert!(matches!(
        system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "original").await,
        Err(CalmError::IdempotencyKeyExhausted(_))
    ));
    tx.rollback().await.unwrap();
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    f.accept_raw(&mut tx, &f.event("ordinary", "body")).await;
    assert!(
        system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "original")
            .await
            .is_err()
    );
    assert!(!assistant_key_used_tx(&mut tx, &f.card).await.unwrap());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn watcher_source_upgrade_preserves_summary_bytes_spent_keys_and_constraints() {
    for deleted in [false, true] {
        let f = Fixture::new().await;
        let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
        // Install the actual released source schema before producing real Summary
        // acceptance. No copied receipt writer or historical JSON fixture.
        sqlx::raw_sql("DROP TRIGGER assistant_creation_system_namespace; DROP TABLE conversation_system_assistant_receipts; DROP TABLE conversation_system_assistant_entries;")
            .execute(&mut **tx).await.unwrap();
        sqlx::raw_sql(include_str!(
            "../../../../../../migrations/0200_system_assistant_entries.sql"
        ))
        .execute(&mut **tx)
        .await
        .unwrap();
        system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "opening")
            .await
            .unwrap();
        f.accept(&mut tx, &f.event("bootstrap", "opening")).await;
        let (_, accepted) = f.accept(&mut tx, &f.event("episode", "body")).await;
        if deleted {
            sqlx::query("DELETE FROM cards WHERE id=?1")
                .bind(&f.card)
                .execute(&mut **tx)
                .await
                .unwrap();
        }
        let entries: Vec<(String, String, String, String, i64, String)> =
            sqlx::query_as("SELECT * FROM conversation_system_assistant_entries")
                .fetch_all(&mut **tx)
                .await
                .unwrap();
        let receipts: Vec<(String, String, String, i64, String, i64, String)> = sqlx::query_as(
            "SELECT * FROM conversation_system_assistant_receipts ORDER BY event_id",
        )
        .fetch_all(&mut **tx)
        .await
        .unwrap();
        let fk_before: Vec<(String, Option<i64>, String, i64)> =
            sqlx::query_as("PRAGMA foreign_key_check")
                .fetch_all(&mut **tx)
                .await
                .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../../../../migrations/0204_worker_watcher_source.sql"
        ))
        .execute(&mut **tx)
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_as::<_, (String, String, String, String, i64, String)>(
                "SELECT * FROM conversation_system_assistant_entries"
            )
            .fetch_all(&mut **tx)
            .await
            .unwrap(),
            entries
        );
        assert_eq!(
            sqlx::query_as::<_, (String, String, String, i64, String, i64, String)>(
                "SELECT * FROM conversation_system_assistant_receipts ORDER BY event_id"
            )
            .fetch_all(&mut **tx)
            .await
            .unwrap(),
            receipts
        );
        assert_eq!(
            sqlx::query_as::<_, (String, Option<i64>, String, i64)>("PRAGMA foreign_key_check")
                .fetch_all(&mut **tx)
                .await
                .unwrap(),
            fk_before
        );
        assert!(assistant_key_used_tx(&mut tx, &f.card).await.unwrap());
        assert!(
            system_assistant_receipt_replay_tx(
                &mut tx,
                &f.card,
                SOURCE,
                "episode",
                receipts[1].3,
                &accepted.input_ref
            )
            .await
            .is_ok()
        );
        for statement in [
            "UPDATE conversation_system_assistant_entries SET creation_floor=creation_floor+1",
            "DELETE FROM conversation_system_assistant_entries",
            "INSERT OR REPLACE INTO conversation_system_assistant_entries SELECT * FROM conversation_system_assistant_entries",
            "UPDATE conversation_system_assistant_receipts SET subject='changed'",
            "DELETE FROM conversation_system_assistant_receipts",
            "INSERT OR REPLACE INTO conversation_system_assistant_receipts SELECT * FROM conversation_system_assistant_receipts",
        ] {
            assert!(
                sqlx::query(statement).execute(&mut **tx).await.is_err(),
                "{statement}"
            );
        }
        tx.commit().await.unwrap();
    }
}
