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
