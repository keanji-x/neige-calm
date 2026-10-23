//! Persist the old request identity before replaying through current HTTP admission.
use super::*;

pub(super) async fn seed(key: &str, message: Option<&str>) -> (Boot, Value, Value) {
    use calm_server::db::sqlite::{TrackCreateBindingClaim, track_create_idempotency_claim_tx};
    use calm_server::model::CardRole;
    use sha2::{Digest, Sha256};

    let mut boot = historical_boot_without_daemon().await;
    // Unkeyed historical allocation provides the same Managed Track/cards. The
    // unavailable daemon leaves no live conversation or keyed operation to repair.
    let (status, original) = boot.create_track(None, None).await;
    assert_eq!(status, StatusCode::CREATED, "body={original}");
    assert_eq!(boot.binding_count().await, 0);
    assert_eq!(boot.operation_count().await, 0);
    let track = original["id"].as_str().unwrap();
    let cards = boot.repo.cards_by_track(track).await.unwrap();
    let card = |role| {
        cards
            .iter()
            .find(|card| boot.state.write().verify_role(&card.id) == Some(role))
            .unwrap()
            .id
            .to_string()
    };
    let claim = TrackCreateBindingClaim {
        track_id: track.into(),
        planner_card_id: card(CardRole::Planner),
        report_card_id: card(CardRole::ReportCard),
        // Frozen pre-cross-area request: investigation, cwd omitted, attach false.
        create_request_sha256: "59e6992a2e9d3cb981bcf451d1cfd839cfa685a075a58750ea7e22c73dca151b"
            .into(),
        first_message_sha256: message.map(|text| hex::encode(Sha256::digest(text.as_bytes()))),
    };
    let area = boot.area_id.clone();
    let key = key.to_string();
    write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            track_create_idempotency_claim_tx(tx, &area, &key, &claim).await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    boot.historical = false;
    boot.app = boot.app_with_running_daemon().await;
    let mut request = json!({
        "planner_provider": "codex",
        "area_id": boot.area_id,
        "title": "",
        "template_id": "investigation",
        "theme": {"fg": [255, 255, 255], "bg": [0, 0, 0]},
    });
    if let Some(message) = message {
        request["first_message"] = json!(message);
    }
    (boot, request, original)
}
