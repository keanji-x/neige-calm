use super::atomic::replay;
use super::*;
use calm_types::conversation::PendingInput;

#[tokio::test]
async fn public_snapshot_setter_cannot_forge_creation_floor() {
    let f = Fixture::new().await;
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    assert!(
        db::session_set_handle_state_tx(
            &mut tx,
            &f.worker,
            Some(serde_json::to_value(CanonicalHarnessSnapshot::dormant(999)).unwrap())
        )
        .await
        .unwrap()
    );
    assert!(
        system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "opening")
            .await
            .is_err()
    );
    assert!(!assistant_key_used_tx(&mut tx, &f.card).await.unwrap());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn public_snapshot_setter_cannot_change_original_acceptance_time() {
    let f = Fixture::new().await;
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "opening")
        .await
        .unwrap();
    let (id, accepted) = f.accept(&mut tx, &f.event("input", "body")).await;
    let carrier = current_input_carrier_tx(&mut tx, &f.card, &f.worker)
        .await
        .unwrap();
    let mut snapshot = carrier.snapshot().clone();
    snapshot["canonical_inputs"]["pending"][0]["queued_at_ms"] = 42.into();
    assert!(
        db::session_set_handle_state_tx(&mut tx, &f.worker, Some(snapshot))
            .await
            .unwrap()
    );
    let original = replay(&mut tx, &f, "input", id, &accepted).await.unwrap();
    assert_ne!(original.queued_at_ms, 42);
    assert_eq!(
        original.queued_at_ms,
        carrier.checkpoint().pending[0].queued_at_ms()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn exact_record_replays_after_only_event_deletion() {
    let f = Fixture::new().await;
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "opening")
        .await
        .unwrap();
    let (id, accepted) = f.accept(&mut tx, &f.event("input", "body")).await;
    let original = replay(&mut tx, &f, "input", id, &accepted).await.unwrap();
    sqlx::query("DELETE FROM events WHERE id=?1")
        .bind(id)
        .execute(&mut **tx)
        .await
        .unwrap();
    assert_eq!(
        replay(&mut tx, &f, "input", id, &accepted).await.unwrap(),
        original
    );
    let other = f.append(&mut tx, &f.event("input", "body")).await;
    assert!(
        system_assistant_accept_tx(&mut tx, &f.card, other)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn replace_cannot_overwrite_original_facts_with_recursive_triggers_off() {
    let f = Fixture::new().await;
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    sqlx::query("PRAGMA recursive_triggers=OFF")
        .execute(&mut **tx)
        .await
        .unwrap();
    system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "opening")
        .await
        .unwrap();
    for (card, worker, digest) in [
        (f.card.as_str(), f.worker.as_str(), text_digest("opening")),
        (&f.card, &f.worker, "0".repeat(64)),
        (&f.card, "other-worker", text_digest("opening")),
        ("other-card", &f.worker, text_digest("opening")),
    ] {
        assert!(sqlx::query("INSERT OR REPLACE INTO conversation_system_assistant_entries VALUES(?1,?2,?3,'today-summary',0,?4)")
            .bind(card).bind(&f.track).bind(worker).bind(digest).execute(&mut **tx).await.is_err());
    }
    let (id, accepted) = f.accept(&mut tx, &f.event("input", "body")).await;
    let original = replay(&mut tx, &f, "input", id, &accepted).await.unwrap();
    let before: String =
        sqlx::query_scalar("SELECT receipt_json FROM conversation_system_assistant_receipts")
            .fetch_one(&mut **tx)
            .await
            .unwrap();
    for assignment in [
        "receipt_json",
        "json_set(receipt_json,'$.payloadDigest','0000000000000000000000000000000000000000000000000000000000000000')",
    ] {
        let sql = format!(
            "INSERT OR REPLACE INTO conversation_system_assistant_receipts SELECT card_id,source,subject,event_id,observation_id,bootstrap,{assignment} FROM conversation_system_assistant_receipts"
        );
        assert!(sqlx::query(&sql).execute(&mut **tx).await.is_err());
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT receipt_json FROM conversation_system_assistant_receipts"
            )
            .fetch_one(&mut **tx)
            .await
            .unwrap(),
            before
        );
    }
    assert_eq!(
        replay(&mut tx, &f, "input", id, &accepted).await.unwrap(),
        original
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn entry_replace_fences_each_unique_identity_even_after_carrier_moves() {
    let f = Fixture::new().await;
    let (_, other_card) = super::super::super::tests::fixture_card(&f.repo).await;
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    sqlx::query("PRAGMA recursive_triggers=OFF")
        .execute(&mut **tx)
        .await
        .unwrap();
    let other_track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
        .bind(&other_card)
        .fetch_one(&mut **tx)
        .await
        .unwrap();
    for worker_conflict in [false, true] {
        sqlx::query("SAVEPOINT original_entry")
            .execute(&mut **tx)
            .await
            .unwrap();
        system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "opening")
            .await
            .unwrap();
        let before: (String, String, String, String, i64, String) =
            sqlx::query_as("SELECT * FROM conversation_system_assistant_entries")
                .fetch_one(&mut **tx)
                .await
                .unwrap();
        // Move the live carrier so the replacement is independently in scope.
        let (card, track, worker) = if worker_conflict {
            sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1")
                .bind(&f.card)
                .execute(&mut **tx)
                .await
                .unwrap();
            sqlx::query("UPDATE cards SET session_id=?1,role='assistant',payload='{\"harness_profile\":\"assistant\",\"assistant_provider\":\"codex\"}' WHERE id=?2").bind(&f.worker).bind(&other_card).execute(&mut **tx).await.unwrap();
            sqlx::query("UPDATE worker_sessions SET card_id=?1,track_id=?2 WHERE id=?3")
                .bind(&other_card)
                .bind(&other_track)
                .bind(&f.worker)
                .execute(&mut **tx)
                .await
                .unwrap();
            (other_card.as_str(), other_track.as_str(), f.worker.as_str())
        } else {
            db::session_supersede_active_tx(&mut tx, &f.worker, 0)
                .await
                .unwrap();
            Fixture::start_worker(&mut tx, &f.card, "replacement-worker").await;
            (f.card.as_str(), f.track.as_str(), "replacement-worker")
        };
        let insert = "INSERT OR REPLACE INTO conversation_system_assistant_entries VALUES(?1,?2,?3,'today-summary',0,?4)";
        assert!(
            sqlx::query(insert)
                .bind(card)
                .bind(track)
                .bind(worker)
                .bind(text_digest("opening"))
                .execute(&mut **tx)
                .await
                .is_err()
        );
        assert_eq!(
            sqlx::query_as::<_, (String, String, String, String, i64, String)>(
                "SELECT * FROM conversation_system_assistant_entries"
            )
            .fetch_one(&mut **tx)
            .await
            .unwrap(),
            before
        );
        // Roll back the original fact, then exercise the same candidate as a legal control.
        sqlx::query("ROLLBACK TO original_entry")
            .execute(&mut **tx)
            .await
            .unwrap();
        if worker_conflict {
            sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1")
                .bind(&f.card)
                .execute(&mut **tx)
                .await
                .unwrap();
            sqlx::query("UPDATE cards SET session_id=?1,role='assistant',payload='{\"harness_profile\":\"assistant\",\"assistant_provider\":\"codex\"}' WHERE id=?2").bind(&f.worker).bind(&other_card).execute(&mut **tx).await.unwrap();
            sqlx::query("UPDATE worker_sessions SET card_id=?1,track_id=?2 WHERE id=?3")
                .bind(&other_card)
                .bind(&other_track)
                .bind(&f.worker)
                .execute(&mut **tx)
                .await
                .unwrap();
        } else {
            db::session_supersede_active_tx(&mut tx, &f.worker, 0)
                .await
                .unwrap();
            Fixture::start_worker(&mut tx, &f.card, "replacement-worker").await;
        }
        sqlx::query(insert)
            .bind(card)
            .bind(track)
            .bind(worker)
            .bind(text_digest("opening"))
            .execute(&mut **tx)
            .await
            .unwrap();
        sqlx::query("ROLLBACK TO original_entry")
            .execute(&mut **tx)
            .await
            .unwrap();
        sqlx::query("RELEASE original_entry")
            .execute(&mut **tx)
            .await
            .unwrap();
    }
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn replay_rejects_cross_event_worker_card_and_database() {
    let f = Fixture::new().await;
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "opening")
        .await
        .unwrap();
    let (id, accepted) = f.accept(&mut tx, &f.event("input", "body")).await;
    let (other_id, other) = f.accept(&mut tx, &f.event("other", "body")).await;
    assert!(replay(&mut tx, &f, "input", id, &other).await.is_err());
    assert!(
        replay(&mut tx, &f, "input", other_id, &accepted)
            .await
            .is_err()
    );
    assert!(
        system_assistant_receipt_replay_tx(
            &mut tx,
            "other-card",
            SOURCE,
            "input",
            id,
            &accepted.input_ref
        )
        .await
        .is_err()
    );
    let g = Fixture::new().await;
    let mut other_tx = db::begin_immediate_tx(g.repo.db()).await.unwrap();
    system_assistant_entry_insert_tx(&mut other_tx, &g.card, SOURCE, "opening")
        .await
        .unwrap();
    let (g_id, g_accepted) = g.accept(&mut other_tx, &g.event("input", "body")).await;
    assert!(replay(&mut tx, &f, "input", id, &g_accepted).await.is_err());
    assert!(
        replay(&mut other_tx, &g, "input", g_id, &accepted)
            .await
            .is_err()
    );
    other_tx.rollback().await.unwrap();
    let original = replay(&mut tx, &f, "input", id, &accepted).await.unwrap();
    sqlx::query("DELETE FROM events WHERE id=?1")
        .bind(id)
        .execute(&mut **tx)
        .await
        .unwrap();
    let before = current_input_carrier_tx(&mut tx, &f.card, &f.worker)
        .await
        .unwrap()
        .snapshot()
        .clone();
    assert_eq!(
        replay(&mut tx, &f, "input", id, &accepted).await.unwrap(),
        original
    );
    assert_eq!(
        current_input_carrier_tx(&mut tx, &f.card, &f.worker)
            .await
            .unwrap()
            .snapshot(),
        &before
    );
    assert!(replay(&mut tx, &f, "input", id, &other).await.is_err());
    // A new original worker cannot accept into the old permanent entry.
    db::session_supersede_active_tx(&mut tx, &f.worker, 0)
        .await
        .unwrap();
    Fixture::start_worker(&mut tx, &f.card, "replacement").await;
    let event = f.append(&mut tx, &f.event("later", "body")).await;
    assert!(
        system_assistant_accept_tx(&mut tx, &f.card, event)
            .await
            .is_err()
    );
    assert_eq!(
        replay(&mut tx, &f, "input", id, &accepted).await.unwrap(),
        original
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn event_creation_floor_survives_pruning_and_requires_exact_empty_cutoff() {
    let f = Fixture::new().await;
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    let (id, _) = f.accept_raw(&mut tx, &f.event("prior", "body")).await;
    sqlx::query("DELETE FROM events")
        .execute(&mut **tx)
        .await
        .unwrap();
    assert_eq!(
        db::event_inputs::event_creation_floor_tx(&mut tx)
            .await
            .unwrap(),
        id
    );
    for floor in [0, id + 999] {
        db::session_set_handle_state_tx(
            &mut tx,
            &f.worker,
            Some(serde_json::to_value(CanonicalHarnessSnapshot::dormant(floor)).unwrap()),
        )
        .await
        .unwrap();
        assert!(
            system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "opening")
                .await
                .is_err()
        );
    }
    // Presentations still make this previously used carrier ineligible.
    db::session_set_handle_state_tx(
        &mut tx,
        &f.worker,
        Some(serde_json::to_value(CanonicalHarnessSnapshot::dormant(id)).unwrap()),
    )
    .await
    .unwrap();
    assert!(
        system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "opening")
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
}

async fn insert_receipt_sql(
    tx: &mut Transaction<'_, Sqlite>,
    r: &SystemAssistantReceipt,
) -> std::result::Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error> {
    sqlx::query("INSERT OR REPLACE INTO conversation_system_assistant_receipts VALUES(?1,?2,?3,?4,?5,?6,?7)")
        .bind(&r.card_id).bind(r.source.namespace()).bind(&r.subject).bind(r.event_id)
        .bind(&r.observation_id).bind(matches!(r.acceptance, SystemAssistantReceiptKind::Bootstrap { .. }))
        .bind(serde_json::to_string(r).unwrap()).execute(&mut **tx).await
}

// Scope facts are deliberately changed through the existing public snapshot API
// and SQL event fixture so each unique conflict is isolated from scope rejection.
async fn receipt_candidate_scope(
    tx: &mut Transaction<'_, Sqlite>,
    f: &Fixture,
    kind: &str,
    first: &SystemAssistantReceipt,
    candidate: &SystemAssistantReceipt,
) {
    let carrier = current_input_carrier_tx(tx, &f.card, &f.worker)
        .await
        .unwrap();
    let mut snapshot = carrier.snapshot().clone();
    match kind {
        "event" => {
            snapshot["canonical_inputs"]["pending"][1]["envelope_id"] = first.event_id.into();
            sqlx::query("UPDATE events SET payload=json_set(payload,'$.subject',?1) WHERE id=?2")
                .bind(&candidate.subject)
                .bind(first.event_id)
                .execute(&mut **tx)
                .await
                .unwrap();
        }
        "observation" => {
            snapshot["canonical_inputs"]["pending"][0]["envelope_id"] = candidate.event_id.into()
        }
        _ => (),
    }
    db::session_set_handle_state_tx(tx, &f.worker, Some(snapshot))
        .await
        .unwrap();
}

#[tokio::test]
async fn receipt_replace_fences_key_event_observation_and_bootstrap_with_legal_controls() {
    for kind in ["key", "event", "observation", "bootstrap"] {
        let f = Fixture::new().await;
        let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
        sqlx::query("PRAGMA recursive_triggers=OFF")
            .execute(&mut **tx)
            .await
            .unwrap();
        system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "body")
            .await
            .unwrap();
        let subject = if kind == "bootstrap" {
            "bootstrap"
        } else {
            "input"
        };
        let id = f.append(&mut tx, &f.event(subject, "body")).await;
        let other_id = f.append(&mut tx, &f.event("other", "body")).await;
        sqlx::query("SAVEPOINT capture")
            .execute(&mut **tx)
            .await
            .unwrap();
        let (accepted, first) = system_assistant_accept_tx(&mut tx, &f.card, id)
            .await
            .unwrap();
        let (other, mut candidate) = system_assistant_accept_tx(&mut tx, &f.card, other_id)
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
        // Restore the real acceptance snapshots/presentations for SQL scope controls.
        // These candidates deliberately test the schema's REPLACE identity fences.
        db::session_set_handle_state_tx(
            &mut tx,
            &f.worker,
            Some(other.snapshot_after_write.clone()),
        )
        .await
        .unwrap();
        for input in [&accepted, &other] {
            let AcceptedInputRef::System { observation_id } = &input.input_ref else {
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
                input.presentation_sequence
            );
        }
        if matches!(kind, "key" | "bootstrap") {
            candidate.subject = first.subject.clone();
            candidate.acceptance = first.acceptance.clone();
            sqlx::query("UPDATE events SET payload=json_set(payload,'$.subject',?1) WHERE id=?2")
                .bind(&candidate.subject)
                .bind(other_id)
                .execute(&mut **tx)
                .await
                .unwrap();
        }
        match kind {
            "event" => candidate.event_id = first.event_id,
            "observation" => {
                candidate.observation_id = first.observation_id.clone();
                candidate.presentation_sequence = first.presentation_sequence;
                candidate.queued_at_ms = first.queued_at_ms;
            }
            _ => (),
        }
        sqlx::query("SAVEPOINT original_receipt")
            .execute(&mut **tx)
            .await
            .unwrap();
        insert_receipt_sql(&mut tx, &first).await.unwrap();
        let before: String =
            sqlx::query_scalar("SELECT receipt_json FROM conversation_system_assistant_receipts")
                .fetch_one(&mut **tx)
                .await
                .unwrap();
        receipt_candidate_scope(&mut tx, &f, kind, &first, &candidate).await;
        assert!(
            insert_receipt_sql(&mut tx, &candidate).await.is_err(),
            "{kind}"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT receipt_json FROM conversation_system_assistant_receipts"
            )
            .fetch_one(&mut **tx)
            .await
            .unwrap(),
            before,
            "{kind}"
        );
        sqlx::query("ROLLBACK TO original_receipt")
            .execute(&mut **tx)
            .await
            .unwrap();
        receipt_candidate_scope(&mut tx, &f, kind, &first, &candidate).await;
        insert_receipt_sql(&mut tx, &candidate).await.unwrap();
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn synthetic_pending_cannot_create_receipt_through_replay() {
    let f = Fixture::new().await;
    let mut tx = db::begin_immediate_tx(f.repo.db()).await.unwrap();
    system_assistant_entry_insert_tx(&mut tx, &f.card, SOURCE, "opening")
        .await
        .unwrap();
    let (_, mut borrowed) = f.accept(&mut tx, &f.event("real", "body")).await;
    let event = f.event("synthetic", "fabricated acceptance");
    let id = db::append_decision_event_in_tx(
        &mut tx,
        &ActorId::Kernel,
        &EventScope::Card {
            card: f.card.clone().into(),
            track: f.track.clone().into(),
            area: f.area.clone().into(),
        },
        None,
        &event,
    )
    .await
    .unwrap();
    let fact = db::event_inputs::event_input_tx(&mut tx, id).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &f.card, &f.worker)
        .await
        .unwrap();
    let observation =
        calm_types::conversation::SystemObservation::try_from(Observation::SystemContext {
            text: "fabricated acceptance".into(),
        })
        .unwrap();
    let observation_id = event_system_observation_id_tx(&mut tx, &carrier, &fact, &observation)
        .await
        .unwrap();
    borrowed.presentation_sequence = allocate_presentation_tx(
        &mut tx,
        &f.card,
        PresentationSource::SystemObservation(&observation_id),
    )
    .await
    .unwrap();
    borrowed.input_ref = AcceptedInputRef::System {
        observation_id: observation_id.clone(),
    };
    let mut snapshot = carrier.snapshot().clone();
    snapshot["push_watermark"] = id.into();
    snapshot["canonical_inputs"]["next_queue_position"] = 1.into();
    snapshot["canonical_inputs"]["pending"] = serde_json::to_value(vec![PendingInput::System {
        observation_id,
        observation,
        queue_position: 0,
        queued_at_ms: 43,
        envelope_id: Some(id),
        addressable: false,
    }])
    .unwrap();
    db::session_set_handle_state_tx(&mut tx, &f.worker, Some(snapshot))
        .await
        .unwrap();
    assert!(
        replay(&mut tx, &f, "synthetic", id, &borrowed)
            .await
            .is_err()
    );
    assert!(
        system_assistant_receipt_tx(&mut tx, &f.card, SOURCE, "synthetic")
            .await
            .unwrap()
            .is_none()
    );
    tx.rollback().await.unwrap();
}
