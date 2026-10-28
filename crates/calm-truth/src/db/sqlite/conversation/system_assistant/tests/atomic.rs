use super::*;

pub(super) async fn replay(
    tx: &mut Transaction<'_, Sqlite>,
    f: &Fixture,
    subject: &str,
    id: i64,
    accepted: &AcceptedPendingInput,
) -> Result<SystemAssistantReceipt> {
    system_assistant_receipt_replay_tx(tx, &f.card, SOURCE, subject, id, &accepted.input_ref).await
}

async fn rollback_repro(cross_transaction: bool) {
    let f = Fixture::new().await;
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "opening")
        .await
        .unwrap();
    let id = f.append(&mut tx, &f.event("input", "body")).await;
    tx.commit().await.unwrap();
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    sqlx::query("SAVEPOINT before_a")
        .execute(&mut **tx)
        .await
        .unwrap();
    let (a, a_receipt) = system_assistant_accept_tx(&mut tx, &f.card, id)
        .await
        .unwrap();
    if cross_transaction {
        tx.rollback().await.unwrap();
        tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    } else {
        sqlx::query("ROLLBACK TO before_a")
            .execute(&mut **tx)
            .await
            .unwrap();
        sqlx::query("RELEASE before_a")
            .execute(&mut **tx)
            .await
            .unwrap();
    }
    // A's read result cannot create anything, before or after a new acceptance.
    assert!(replay(&mut tx, &f, "input", id, &a).await.is_err());
    assert!(
        system_assistant_receipt_tx(&mut tx, &f.card, SOURCE, "input")
            .await
            .unwrap()
            .is_none()
    );
    tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    let (b, b_receipt) = system_assistant_accept_tx(&mut tx, &f.card, id)
        .await
        .unwrap();
    assert_ne!(a_receipt.queued_at_ms, b_receipt.queued_at_ms);
    db::session_set_handle_state_tx(&mut tx, &f.worker, Some(a.snapshot_after_write.clone()))
        .await
        .unwrap();
    // Replay takes identity only; the exact permanent ACK is B, never A's time.
    assert_eq!(
        replay(&mut tx, &f, "input", id, &a).await.unwrap(),
        b_receipt
    );
    assert_eq!(
        replay(&mut tx, &f, "input", id, &b).await.unwrap(),
        b_receipt
    );
    tx.commit().await.unwrap();
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    assert_eq!(
        system_assistant_receipt_tx(&mut tx, &f.card, SOURCE, "input")
            .await
            .unwrap(),
        Some(b_receipt)
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn savepoint_rollback_cannot_freeze_old_time() {
    rollback_repro(false).await;
}
#[tokio::test]
async fn transaction_rollback_cannot_freeze_old_time() {
    rollback_repro(true).await;
}

#[tokio::test]
async fn entry_event_acceptance_and_receipt_commit_together() {
    let f = Fixture::new().await;
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "original")
        .await
        .unwrap();
    let (id, accepted) = f.accept(&mut tx, &f.event("bootstrap", "original")).await;
    let original = replay(&mut tx, &f, "bootstrap", id, &accepted)
        .await
        .unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &f.card, &f.worker)
        .await
        .unwrap();
    assert_eq!(
        original.acceptance,
        SystemAssistantReceiptKind::Bootstrap { creation_floor: 0 }
    );
    assert_eq!(
        original.queued_at_ms,
        carrier.checkpoint().pending[0].queued_at_ms()
    );
    assert_eq!(
        original.presentation_sequence,
        accepted.presentation_sequence
    );
    assert_eq!(original.payload_digest, text_digest("original"));
    assert_eq!(carrier.snapshot(), &accepted.snapshot_after_write);
    let calm_types::conversation::PendingInput::System { observation, .. } =
        &carrier.checkpoint().pending[0]
    else {
        panic!("expected System input")
    };
    assert_eq!(
        observation.observation(),
        &Observation::SystemContext {
            text: "original".into()
        }
    );
    tx.commit().await.unwrap();
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    assert_eq!(
        replay(&mut tx, &f, "bootstrap", id, &accepted)
            .await
            .unwrap(),
        original
    );
    assert_eq!(
        db::event_inputs::event_input_tx(&mut tx, id)
            .await
            .unwrap()
            .envelope
            .id,
        id
    );
    assert_eq!(
        current_input_carrier_tx(&mut tx, &f.card, &f.worker)
            .await
            .unwrap()
            .snapshot(),
        &accepted.snapshot_after_write
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn exact_ack_checks_identity_fingerprint_and_survives_resource_deletion() {
    let f = Fixture::new().await;
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "opening")
        .await
        .unwrap();
    let (id, accepted) = f.accept(&mut tx, &f.event("input", "body")).await;
    let original = replay(&mut tx, &f, "input", id, &accepted).await.unwrap();
    assert_eq!(original.acceptance, SystemAssistantReceiptKind::Input);
    assert_eq!(original.payload_digest, text_digest("body"));
    sqlx::query("UPDATE worker_sessions SET handle_state_json=json_set(handle_state_json,'$.canonical_inputs.pending',json('[]')) WHERE id=?1").bind(&f.worker).execute(&mut **tx).await.unwrap();
    assert_eq!(
        replay(&mut tx, &f, "input", id, &accepted).await.unwrap(),
        original
    );
    for (card, subject, event_id, input_ref) in [
        ("other-card", "input", id, accepted.input_ref.clone()),
        (
            f.card.as_str(),
            "other-subject",
            id,
            accepted.input_ref.clone(),
        ),
        (&f.card, "input", id + 1, accepted.input_ref.clone()),
        (
            &f.card,
            "input",
            id,
            AcceptedInputRef::System {
                observation_id: calm_types::conversation::SystemObservationId::parse(
                    &"a".repeat(32),
                )
                .unwrap(),
            },
        ),
    ] {
        assert!(
            system_assistant_receipt_replay_tx(
                &mut tx, card, SOURCE, subject, event_id, &input_ref
            )
            .await
            .is_err()
        );
    }
    sqlx::query("SAVEPOINT drift")
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query("UPDATE events SET payload=json_set(payload,'$.text','changed') WHERE id=?1")
        .bind(id)
        .execute(&mut **tx)
        .await
        .unwrap();
    assert!(matches!(
        replay(&mut tx, &f, "input", id, &accepted).await,
        Err(CalmError::IdempotencyKeyReused(_))
    ));
    assert!(matches!(
        system_assistant_receipt_tx(&mut tx, &f.card, SOURCE, "input").await,
        Err(CalmError::IdempotencyKeyReused(_))
    ));
    sqlx::query("ROLLBACK TO drift")
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query("RELEASE drift")
        .execute(&mut **tx)
        .await
        .unwrap();
    let other = f.append(&mut tx, &f.event("input", "body")).await;
    assert!(matches!(
        system_assistant_accept_tx(&mut tx, &f.card, other).await,
        Err(CalmError::IdempotencyKeyReused(_))
    ));
    assert!(
        replay(&mut tx, &f, "input", other, &accepted)
            .await
            .is_err()
    );
    for sql in [
        "UPDATE conversation_system_assistant_entries SET creation_floor=999",
        "DELETE FROM conversation_system_assistant_entries",
        "UPDATE conversation_system_assistant_receipts SET bootstrap=0",
        "DELETE FROM conversation_system_assistant_receipts",
    ] {
        assert!(sqlx::query(sql).execute(&mut **tx).await.is_err());
    }
    sqlx::query("DELETE FROM events")
        .execute(&mut **tx)
        .await
        .unwrap();
    assert_eq!(
        replay(&mut tx, &f, "input", id, &accepted).await.unwrap(),
        original
    );
    sqlx::query("DELETE FROM cards WHERE id=?1")
        .bind(&f.card)
        .execute(&mut **tx)
        .await
        .unwrap();
    assert_eq!(
        replay(&mut tx, &f, "input", id, &accepted).await.unwrap(),
        original
    );
    assert!(assistant_key_used_tx(&mut tx, &f.card).await.unwrap());
    assert!(matches!(
        system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "opening").await,
        Err(CalmError::IdempotencyKeyExhausted(_))
    ));
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn acceptance_qualifies_current_event_and_carrier_without_residue() {
    for change in [
        "source", "subject", "target", "track", "worker", "area", "actor", "version", "text",
        "kind", "pruned", "carrier", "role", "profile", "provider", "queue",
    ] {
        let f = Fixture::new().await;
        let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
        system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "original")
            .await
            .unwrap();
        let id = f.append(&mut tx, &f.event("bootstrap", "original")).await;
        let sql = match change {
            "source" => {
                "UPDATE events SET payload=json_set(payload,'$.source','unregistered') WHERE id=?1"
            }
            "subject" => "UPDATE events SET payload=json_set(payload,'$.subject','') WHERE id=?1",
            "target" => {
                "UPDATE events SET payload=json_set(payload,'$.card_id','other') WHERE id=?1"
            }
            "track" => {
                "UPDATE events SET payload=json_set(payload,'$.track_id','other') WHERE id=?1"
            }
            "worker" => {
                "UPDATE events SET payload=json_set(payload,'$.original_worker','other') WHERE id=?1"
            }
            "area" => "UPDATE events SET scope_area='other' WHERE id=?1",
            "actor" => "UPDATE events SET actor='{\"kind\":\"User\"}' WHERE id=?1",
            "version" => "UPDATE events SET payload=json_set(payload,'$.version',2) WHERE id=?1",
            "text" => "UPDATE events SET payload=json_set(payload,'$.text','changed') WHERE id=?1",
            "kind" => {
                "UPDATE events SET kind='track.wake_requested',payload='{\"track_id\":\"x\",\"source\":\"x\",\"key\":\"x\",\"text\":\"x\"}' WHERE id=?1"
            }
            "pruned" => "DELETE FROM events WHERE id=?1",
            "carrier" => "UPDATE cards SET session_id=NULL",
            "role" => "UPDATE cards SET role='worker'",
            "profile" => "UPDATE cards SET payload=json_set(payload,'$.harness_profile','planner')",
            "provider" => "UPDATE worker_sessions SET provider='claude'",
            "queue" => {
                "UPDATE worker_sessions SET handle_state_json=json_set(handle_state_json,'$.canonical_inputs.next_queue_position',9223372036854775807)"
            }
            _ => unreachable!(),
        };
        sqlx::query(sql).bind(id).execute(&mut **tx).await.unwrap();
        let before = db::session_handle_state_by_id_tx(&mut tx, &f.worker)
            .await
            .unwrap();
        let result = system_assistant_accept_tx(&mut tx, &f.card, id).await;
        let rejected = match change {
            "source" => matches!(result, Err(CalmError::Internal(_))),
            "pruned" => matches!(
                result,
                Err(CalmError::Core(calm_types::error::CoreError::NotFound(_)))
            ),
            _ => matches!(
                result,
                Err(CalmError::Core(calm_types::error::CoreError::Conflict(_)))
            ),
        };
        assert!(rejected, "{change}: {result:?}");
        tx.commit().await.unwrap();
        let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
        assert_eq!(
            db::session_handle_state_by_id_tx(&mut tx, &f.worker)
                .await
                .unwrap(),
            before,
            "{change}"
        );
        for table in [
            "conversation_presentations",
            "conversation_system_assistant_receipts",
        ] {
            assert_eq!(
                sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM {table}"))
                    .fetch_one(&mut **tx)
                    .await
                    .unwrap(),
                0,
                "{change}: {table}"
            );
        }
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn receipt_insert_failure_rolls_back_acceptance_when_outer_commits() {
    let f = Fixture::new().await;
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "opening")
        .await
        .unwrap();
    let id = f.append(&mut tx, &f.event("input", "body")).await;
    sqlx::query("CREATE TEMP TRIGGER reject_receipt BEFORE INSERT ON conversation_system_assistant_receipts BEGIN SELECT RAISE(ABORT,'injected failure'); END").execute(&mut **tx).await.unwrap();
    let before = current_input_carrier_tx(&mut tx, &f.card, &f.worker)
        .await
        .unwrap()
        .snapshot()
        .clone();
    assert!(matches!(
        system_assistant_accept_tx(&mut tx, &f.card, id).await,
        Err(CalmError::Db(sqlx::Error::Database(_)))
    ));
    tx.commit().await.unwrap();
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    assert_eq!(
        current_input_carrier_tx(&mut tx, &f.card, &f.worker)
            .await
            .unwrap()
            .snapshot(),
        &before
    );
    for table in [
        "conversation_presentations",
        "conversation_system_assistant_receipts",
    ] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM {table}"))
                .fetch_one(&mut **tx)
                .await
                .unwrap(),
            0
        );
    }
    sqlx::query("DROP TRIGGER reject_receipt")
        .execute(&mut **tx)
        .await
        .unwrap();
    let (accepted, receipt) = system_assistant_accept_tx(&mut tx, &f.card, id)
        .await
        .unwrap();
    assert_eq!(
        replay(&mut tx, &f, "input", id, &accepted).await.unwrap(),
        receipt
    );
    assert_eq!(receipt.presentation_sequence, 1);
    tx.commit().await.unwrap();
}
