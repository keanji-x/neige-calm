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
    let rows = repo.transcript_rows_of_thread(card, thread).await?;
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
        let json = serde_json::to_string(params)?;
        if rows.iter().any(|row| {
            row.turn_id.as_deref() == Some(turn)
                && row.item_uuid.as_deref() == Some(id)
                && row.method == method
                && row.params == json
        }) {
            continue;
        }
        repo.harness_item_insert(
            worker,
            card,
            track,
            thread,
            Some(turn),
            Some(id),
            Some(kind),
            method,
            &json,
            None,
        )
        .await?;
    }
    Ok(())
}
