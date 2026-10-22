//! Idempotently project final provider frames retained in a durable receipt.
use crate::db::Repo;
use crate::error::{CalmError, Result};
use serde_json::Value;

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
    use crate::db::sqlite::conversation::{current_input_fact_carrier_tx, observe_native_item_tx};
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
            for frame in frames {
                let params = serde_json::from_str(&frame.params)?;
                observe_native_item_tx(tx, &carrier, &thread, Some(&turn), &frame.method, &params)
                    .await?;
            }
            Ok(())
        })
    })
    .await
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
            .ok_or_else(|| CalmError::Conflict("Receipt item has no identity".into()))?;
        let kind = params["item"]["type"]
            .as_str()
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
