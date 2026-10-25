//! Kernel-owned references shared by child success and handback.
use crate::error::{CalmError, Result};
use crate::operation::Tx;
use serde_json::{Value, json};

pub(crate) async fn result_ref_tx(
    tx: &mut Tx<'_>,
    child_track_id: &str,
    blocked_on: Option<&str>,
) -> Result<Value> {
    let (card_id, updated_at, payload, body_crdt): (String, i64, String, Option<Vec<u8>>) =
        sqlx::query_as("SELECT id,updated_at,payload,body_crdt FROM cards WHERE track_id=?1 AND kind='track-report'")
            .bind(child_track_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("child track {child_track_id} has no report")))?;
    let report = crate::track_report_read::report_doc_snapshot(
        &card_id,
        updated_at,
        serde_json::from_str(&payload)?,
        body_crdt.as_deref(),
    )?;
    let candidate =
        crate::git_candidate::candidate::latest_candidate_for_track_tx(tx, child_track_id).await?;
    let mut result = json!({
        "source": "child-track",
        "outcome": if blocked_on.is_some() { "handed_back" } else { "met" },
        "child_track_id": child_track_id,
        "report": {"card_id":card_id,"rev":report.doc_rev},
        "candidate": candidate,
    });
    if let Some(blocked_on) = blocked_on {
        result["blocked_on"] = json!(blocked_on);
    }
    Ok(result)
}
