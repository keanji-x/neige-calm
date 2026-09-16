//! One terminal-outcome projection for live notifications and history recovery.
use crate::db::Repo;
use crate::error::Result;
use serde_json::Value;

pub(crate) async fn record(
    repo: &dyn Repo,
    session_id: &str,
    card_id: &str,
    track_id: &str,
    thread_id: &str,
    turn_id: &str,
    turn: &Value,
) -> Result<i64> {
    let mut outcome = turn.clone();
    if let Some(object) = outcome.as_object_mut() {
        object.remove("items");
        object.remove("itemsView");
    }
    Ok(repo
        .harness_turn_outcome_put(
            session_id,
            card_id,
            track_id,
            thread_id,
            turn_id,
            &serde_json::to_string(&outcome)?,
        )
        .await?)
}
