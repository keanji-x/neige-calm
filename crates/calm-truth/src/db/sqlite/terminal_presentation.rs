//! Terminal's single writer for user-controlled inventory placement.
use crate::error::{CalmError, Result};
use crate::model::{Card, now_ms};
use calm_types::card_presentation::{TERMINAL_PRESENTATION_PAYLOAD_KEY, TerminalPresentation};
use sqlx::{Sqlite, Transaction};

pub async fn set_workbench_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    in_workbench: bool,
) -> Result<Card> {
    let mut card = sqlx::query_as::<_, crate::db::rows::CardRow>(
        "SELECT id,track_id,kind,title,sort,payload,deletable,created_at,updated_at FROM cards WHERE id=?1",
    ).bind(id).fetch_optional(&mut **tx).await?.map(Card::from)
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    if card.kind != "terminal" {
        return Err(CalmError::BadRequest(
            "Workbench placement requires a terminal card",
        ));
    }
    let mut presentation = TerminalPresentation::from_payload(&card.payload)
        .map_err(|error| CalmError::Internal(format!("terminal card {id}: {error}")))?;
    presentation.in_workbench = in_workbench;
    let map = card.payload.as_object_mut().ok_or_else(|| {
        CalmError::Internal(format!("terminal card {id} payload is not an object"))
    })?;
    map.insert(
        TERMINAL_PRESENTATION_PAYLOAD_KEY.into(),
        serde_json::to_value(presentation)?,
    );
    card.updated_at = now_ms();
    sqlx::query("UPDATE cards SET payload=?1,updated_at=?2 WHERE id=?3")
        .bind(serde_json::to_string(&card.payload)?)
        .bind(card.updated_at)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(card)
}
