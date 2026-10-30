//! Idempotently project final provider frames retained in a durable receipt.
use crate::db::Repo;
use crate::error::{CalmError, Result};
use serde_json::Value;

/// A complete native terminal item set, validated before any carrier handoff.
/// This is content evidence only: it cannot change delivery, claims or END.
pub(crate) struct CanonicalTerminalReceipt {
    thread: String,
    turn: String,
    frames: Vec<calm_truth::db::TranscriptReceiptItem>,
}
impl CanonicalTerminalReceipt {
    pub(crate) fn from_native(thread: &str, turn: &str, terminal: &Value) -> Result<Self> {
        if thread.is_empty()
            || turn.is_empty()
            || terminal.get("id").and_then(Value::as_str) != Some(turn)
        {
            return Err(CalmError::Conflict(
                "native terminal receipt names another execution".into(),
            ));
        }
        let items = terminal
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| CalmError::Conflict("native terminal items are unavailable".into()))?;
        let frames = items.iter().map(|item| serde_json::json!({"method":"item/completed","params":{"threadId":thread,"turnId":turn,"item":item}})).collect::<Vec<_>>();
        Ok(Self {
            thread: thread.into(),
            turn: turn.into(),
            frames: decode(thread, turn, &frames)?,
        })
    }
    pub(crate) async fn settle_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        carrier: &crate::db::sqlite::conversation::CurrentInputCarrier,
    ) -> Result<()> {
        apply_canonical_frames_tx(tx, carrier, &self.thread, &self.turn, self.frames.clone()).await
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn restore(
    repo: &dyn Repo,
    worker: &str,
    card: &str,
    track: &str,
    thread: &str,
    turn: &str,
    items: &[Value],
) -> Result<()> {
    let frames = decode(thread, turn, items)?;
    Ok(repo
        .transcript_receipt_restore(worker, card, track, thread, turn, &frames)
        .await?)
}

/// Settle canonical receipt content through the live presentation owner before
/// exposing END. Its erasure fence and native identity rules also govern replay.
pub(crate) async fn settle_canonical(
    repo: &dyn Repo,
    worker: &str,
    card: &str,
    thread: &str,
    turn: &str,
    items: &[Value],
) -> Result<()> {
    use crate::db::sqlite::conversation::current_input_fact_carrier_tx;
    let frames = decode(thread, turn, items)?;
    let (worker, card, thread, turn) = (
        worker.to_owned(),
        card.to_owned(),
        thread.to_owned(),
        turn.to_owned(),
    );
    crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let carrier = current_input_fact_carrier_tx(tx, &card, &worker).await?;
            apply_canonical_frames_tx(tx, &carrier, &thread, &turn, frames).await?;
            Ok(())
        })
    })
    .await
}

async fn apply_canonical_frames_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    carrier: &crate::db::sqlite::conversation::CurrentInputCarrier,
    thread: &str,
    turn: &str,
    frames: Vec<calm_truth::db::TranscriptReceiptItem>,
) -> Result<()> {
    for frame in frames {
        let params = serde_json::from_str(&frame.params)?;
        crate::db::sqlite::conversation::observe_native_item_tx(
            tx,
            carrier,
            thread,
            Some(turn),
            &frame.method,
            &params,
        )
        .await?;
    }
    Ok(())
}

fn decode(
    thread: &str,
    turn: &str,
    items: &[Value],
) -> Result<Vec<calm_truth::db::TranscriptReceiptItem>> {
    let mut frames = Vec::new();
    for frame in items {
        let params = &frame["params"];
        let method = frame["method"]
            .as_str()
            .filter(|method| matches!(*method, "item/started" | "item/completed"))
            .ok_or_else(|| CalmError::Conflict("Receipt item has an invalid phase".into()))?;
        let id = params["item"]["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| CalmError::Conflict("Receipt item has no identity".into()))?;
        let kind = params["item"]["type"]
            .as_str()
            .filter(|kind| !kind.is_empty())
            .ok_or_else(|| CalmError::Conflict("Receipt item has no type".into()))?;
        if params["threadId"].as_str() != Some(thread)
            || params["turnId"].as_str() != Some(turn)
            || frames
                .iter()
                .any(|item: &calm_truth::db::TranscriptReceiptItem| item.item_uuid == id)
        {
            return Err(CalmError::Conflict(
                "Receipt item ownership or identity changed".into(),
            ));
        }
        frames.push(calm_truth::db::TranscriptReceiptItem {
            item_uuid: id.into(),
            item_type: kind.into(),
            method: method.into(),
            params: serde_json::to_string(params)?,
        });
    }
    Ok(frames)
}
