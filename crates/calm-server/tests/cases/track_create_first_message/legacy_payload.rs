//! Released V1 payload compatibility is independent of canonical activation timing.
use super::*;
use calm_types::harness::retained::RetainedPayload;

/// Preserve raw V1 bytes and classify canonical starts through their owning wire codec.
async fn start_payloads(b: &Boot) -> (Vec<String>, Vec<RetainedPayload>) {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT payload_json FROM operations WHERE kind = 'planner-harness-start' ORDER BY id",
    )
    .fetch_all(b.repo.pool())
    .await
    .unwrap();
    let mut legacy = Vec::new();
    let mut canonical = Vec::new();
    for raw in rows {
        let payload: Value = serde_json::from_str(&raw).unwrap();
        match RetainedPayload::decode(&payload).expect("valid persisted start payload") {
            Some(activation) => canonical.push(activation),
            None => legacy.push(raw),
        }
    }
    (legacy, canonical)
}

/// `skip_serializing_if` keeps every existing caller's `payload_hash` stable: a `null` digest would move the
/// bytes, and an operation submitted by an older binary and retried after a deploy would come back 409.
#[tokio::test]
async fn a_message_less_create_writes_byte_identical_payload_json() {
    let mut b = historical_boot().await;
    let (status, body) = b.create_track(None, None).await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let (payloads, activations) = start_payloads(&b).await;
    assert!(activations.is_empty());
    assert_eq!(payloads.len(), 1, "one start: {payloads:?}");
    for raw in &payloads {
        let payload: Value = serde_json::from_str(raw).unwrap();
        assert!(
            payload.get("first_message").is_none(),
            "message-less start: {payload}"
        );
        assert!(
            payload.get("create_request_sha256").is_none(),
            "the message-less payload must not carry the key at all — not even as null: {payload}"
        );
    }

    b.historical = false;
    // Human first input binds the original fingerprint in canonical storage,
    // independently of the old start-operation body and native readiness.
    // Only this test gains a real issuer; the shared legacy fixture is unchanged.
    b.app = b.app.layer(axum::middleware::from_fn_with_state(
        calm_server::auth::AuthState::new(calm_server::auth::AuthConfig {
            username: None,
            password: None,
            dev_autologin: true,
            display_name: "fixture owner".into(),
        }),
        calm_server::auth::require_session,
    ));
    // Hold the real operation driver so HTTP acceptance cannot race activation persistence.
    let start_guard = b
        .state
        .operation_runtime
        .lock_for_track_delete_for_test()
        .await;
    let (status, body) = b
        .create_track(Some("idem-digest"), Some("ship the thing"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "body={body}");
    let (before, activations) = start_payloads(&b).await;
    assert_eq!(before, payloads, "V1 bytes before canonical activation");
    assert!(
        activations.is_empty(),
        "the driver fence must hold activation before persistence"
    );
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(b.repo.db())
        .await
        .unwrap();
    let original = calm_server::db::sqlite::conversation::human_track_creation_get_tx(
        &mut tx,
        &b.area_id,
        "idem-digest",
    )
    .await
    .unwrap()
    .unwrap();
    let calm_server::db::sqlite::conversation::HumanTrackCreationAck::V1 {
        planner_card_id: card,
        ..
    } = &original.acknowledgement;
    tx.commit().await.unwrap();
    let activated = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "canonical-human-wake-finished",
        card,
        activated.clone(),
    );
    drop(start_guard);
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        activated.entered.notified(),
    )
    .await
    .expect("canonical activation must finish");
    activated.release.notify_one();
    let (after, activations) = start_payloads(&b).await;
    assert_eq!(
        after, payloads,
        "accepted Human body must not add or change V1 start bytes"
    );
    assert_eq!(
        activations.len(),
        1,
        "the real wake must persist its own activation"
    );
    assert_eq!(&activations[0].card_id, card);
    assert_eq!(
        activations[0].worker_session_id,
        b.active_runtime_of_card(card).await
    );
    assert_eq!(original.source_user_id, LEGACY_OWNER_USER_ID);
    assert_eq!(original.create_request_sha256.len(), 64);
    let calm_server::db::sqlite::conversation::HumanTrackCreationAck::V1 {
        track_id,
        planner_card_id,
        first_message,
        ..
    } = &original.acknowledgement;
    assert_eq!(track_id, body["id"].as_str().unwrap());
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(b.repo.db())
        .await
        .unwrap();
    let author =
        calm_server::db::sqlite::conversation::message_tx(&mut tx, &first_message.message_id)
            .await
            .unwrap();
    assert_eq!(author.card_id, *planner_card_id);
    assert_eq!(
        author.author,
        calm_types::conversation::MessageAuthor::AuthenticatedHuman {
            registered_user_id: LEGACY_OWNER_USER_ID.into(),
        }
    );
    let revision = calm_server::db::sqlite::conversation::revision_tx(&mut tx, first_message)
        .await
        .unwrap();
    assert!(
        matches!(revision.payload, calm_types::conversation::PrivateHistoryPayload::Present { value } if value.text == "ship the thing")
    );
    tx.commit().await.unwrap();
    let (status, replay) = b
        .create_track(Some("idem-digest"), Some("ship the thing"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(replay["id"], body["id"]);
    let (replayed, _) = start_payloads(&b).await;
    assert_eq!(
        replayed, payloads,
        "Human replay must also leave V1 bytes unchanged"
    );
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(b.repo.db())
        .await
        .unwrap();
    let retained = calm_server::db::sqlite::conversation::human_track_creation_get_tx(
        &mut tx,
        &b.area_id,
        "idem-digest",
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        retained, original,
        "the original fingerprints and ACK survive replay"
    );
    tx.commit().await.unwrap();
    b.shutdown_harnesses().await;
}
