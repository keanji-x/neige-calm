//! Display-only gate directory cache. Execution authority stays in FrozenVerify.
use crate::db::sqlite::{append_decision_events_in_tx, card_update_tx};
use crate::error::Result;
use crate::event::{BroadcastEnvelope, Event, EventScope, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, AreaId, CardId, TrackId};
use crate::model::CardPatch;
use serde_json::{Value, json};

pub(super) async fn record_gate_cwd_tx(
    tx: &mut crate::operation::Tx<'_>,
    card_id: &str,
    track_id: &str,
    area_id: &str,
    cwd: &str,
) -> Result<Vec<BroadcastEnvelope>> {
    let payload: Option<String> =
        sqlx::query_scalar("SELECT payload FROM cards WHERE id = ?1 AND track_id = ?2")
            .bind(card_id)
            .bind(track_id)
            .fetch_optional(&mut **tx)
            .await?;
    // Deleted cards have nothing to display; gate evidence still persists.
    let Some(payload) = payload else {
        return Ok(Vec::new());
    };
    let mut payload: Value = serde_json::from_str(&payload)?;
    // Legacy cards may have no payload yet. Preserve all existing object
    // fields; a display cache must not turn unrelated scalar/array metadata
    // into a gate failure or overwrite that data.
    if payload.is_null() {
        payload = json!({});
    }
    let Some(fields) = payload.as_object_mut() else {
        tracing::warn!(
            card_id,
            "gate cwd display unavailable: card payload is not an object"
        );
        return Ok(Vec::new());
    };
    fields.insert("gate_cwd".into(), json!(cwd));
    let card = card_update_tx(
        tx,
        card_id,
        CardPatch {
            title: None,
            kind: None,
            sort: None,
            payload: Some(payload),
            deletable: None,
        },
    )
    .await?;
    let scope = EventScope::Card {
        card: CardId::from(card_id),
        track: TrackId::from(track_id),
        area: AreaId::from(area_id),
    };
    let events = vec![Event::CardUpdated(card)];
    let ids =
        append_decision_events_in_tx(tx, &ActorId::KernelDispatcher, &scope, None, &events).await?;
    Ok(ids
        .into_iter()
        .zip(events)
        .map(|(id, event)| BroadcastEnvelope {
            id,
            event_version: SYNC_EVENT_VERSION,
            actor: ActorId::KernelDispatcher,
            scope: scope.clone(),
            event,
        })
        .collect())
}
