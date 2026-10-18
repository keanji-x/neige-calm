//! The existing native item consumer. Persistence follows the declared input contract;
//! parsing, live replies, native asks and item events have one implementation.
use super::*;
use calm_types::event::AskQuestion;

pub(super) async fn on_item(
    inner: &Arc<Inner>,
    live: &LiveReplyWriter,
    phase: ItemPhase,
    params: Value,
    questions: Vec<AskQuestion>,
) -> Result<()> {
    let method = phase.method();
    let Some(item) = params.get("item") else {
        tracing::debug!(
            method,
            "planner harness ignoring item notification without item"
        );
        return Ok(());
    };
    let Some(thread_id) = inner.thread_id.read().await.clone() else {
        tracing::warn!(
            runtime_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            method,
            "planner harness item notification arrived before thread id was known"
        );
        return Ok(());
    };

    let item_uuid = item
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let item_type = item
        .get("type")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let turn_id = item_turn_id(&params).map(ToOwned::to_owned);
    if is_rewound(inner, turn_id.as_deref()).await {
        return Ok(());
    }
    let params_json = serde_json::to_string(&params)?;
    let mut presentation_cursor = None;
    let (item_db_id, legacy_segments_json) = if inner.inputs.canonical().is_some() {
        let Some(row) =
            canonical::persist_native_item(inner, &thread_id, turn_id.as_deref(), method, &params)
                .await?
        else {
            if let Some(id) = item_uuid.as_deref() {
                live.item_stored(id);
            }
            return Ok(());
        };
        presentation_cursor = Some(row.cursor);
        (row.raw_row_id, None)
    } else {
        // The one turn an older binary can have left in flight: its drain wrote no projection row,
        // so its echo takes the segments from the legacy slot.
        let legacy_segments_json = if is_user_message_type(item_type.as_deref()) {
            let legacy = inner.legacy_issued_input_segments.lock().await;
            legacy
                .as_ref()
                .filter(|legacy| turn_id.as_deref() == Some(legacy.turn_id.as_str()))
                .map(|legacy| serde_json::to_string(&legacy.segments))
                .transpose()?
        } else {
            None
        };
        // A `userMessage` echo carrying `item.clientId` names the projection row the drain wrote;
        // the echo upgrades it in place and must never become a second copy. Only a completed
        // `userMessage` renders, so a started echo naming a projection is not stored at all.
        let projection_client_id = if is_user_message_type(item_type.as_deref()) {
            item.get("clientId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        } else {
            None
        };
        let item_db_id = match (projection_client_id.as_deref(), phase) {
            (Some(client_id), ItemPhase::Completed) => {
                let upgraded = match item_uuid.as_deref() {
                    Some(codex_item_id) => {
                        inner
                            .repo
                            .transcript_projection_upgrade(
                                inner.card_id.as_str(),
                                client_id,
                                turn_id.as_deref(),
                                codex_item_id,
                                &params_json,
                            )
                            .await?
                    }
                    None => None,
                };
                match upgraded {
                    Some(row_id) => row_id,
                    None => {
                        insert_item_row(
                            inner,
                            &thread_id,
                            turn_id.as_deref(),
                            item_uuid.as_deref(),
                            item_type.as_deref(),
                            method,
                            &params_json,
                            legacy_segments_json.as_deref(),
                        )
                        .await?
                    }
                }
            }
            (Some(client_id), ItemPhase::Started)
                if inner
                    .repo
                    .transcript_projection_id(inner.card_id.as_str(), client_id)
                    .await?
                    .is_some() =>
            {
                tracing::debug!(
                    worker_session_id = %inner.worker_session_id,
                    card_id = %inner.card_id,
                    client_id,
                    "planner harness skipping item/started echo of a projected user message"
                );
                return Ok(());
            }
            _ => {
                insert_item_row(
                    inner,
                    &thread_id,
                    turn_id.as_deref(),
                    item_uuid.as_deref(),
                    item_type.as_deref(),
                    method,
                    &params_json,
                    legacy_segments_json.as_deref(),
                )
                .await?
            }
        };
        (item_db_id, legacy_segments_json)
    };
    live_reply::on_item(live, phase, &params);
    if phase == ItemPhase::Completed && legacy_segments_json.is_some() {
        *inner.legacy_issued_input_segments.lock().await = None;
    }
    // Only a completed item has asked: a started one may still change.
    if phase == ItemPhase::Completed && !questions.is_empty() {
        native_ask::ask_from_item(inner, item_uuid.as_deref(), questions).await;
    }
    if let Some(cursor) = presentation_cursor {
        return emit_presentation_changed(inner, cursor).await;
    }
    emit_item_added(
        inner,
        item_db_id,
        item_uuid,
        item_type,
        turn_id,
        method.into(),
    )
    .await?;
    Ok(())
}
