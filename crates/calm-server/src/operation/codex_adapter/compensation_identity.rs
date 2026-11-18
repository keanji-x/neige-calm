use super::interactive::prepared_worker_session_id;
use super::*;

/// The first released preparations predate frozen session ids, but retain their
/// original card and terminal. Never replace that historical owner with today's session.
pub(super) async fn compensation_worker_session_id(
    output: &TxOutput,
    card_id: &str,
    repo: &dyn crate::db::RouteRepo,
) -> Result<String> {
    if output.data.get("runtime_id").is_some() {
        return prepared_worker_session_id(output);
    }
    if output.target_type != "card"
        || output.target_id.as_deref() != Some(card_id)
        || output.data.get("card_id").and_then(Value::as_str) != Some(card_id)
        || output.data.get("workspace_environment_thread").is_some()
    {
        return Err(CalmError::Internal(
            "invalid legacy Codex compensation identity".into(),
        ));
    }
    let terminal_id = output.output_string("terminal_id", "codex")?;
    if terminal_id.is_empty() {
        return Err(CalmError::Internal(
            "empty legacy Codex compensation terminal".into(),
        ));
    }
    let card_id = card_id.to_owned();
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let session =
                crate::db::sqlite::session_unique_for_card_terminal_tx(tx, &card_id, &terminal_id)
                    .await?
                    .ok_or_else(|| {
                        CalmError::NotFound("legacy Codex compensation session".into())
                    })?;
            use calm_types::worker::{SessionMode, WorkerContract, WorkerProviderKind};
            if session.provider != WorkerProviderKind::Codex
                || session.mode != SessionMode::Resumable
                || session.contract != WorkerContract::Executor
            {
                return Err(CalmError::Conflict(
                    "legacy Codex compensation session binding changed".into(),
                ));
            }
            Ok(session.id.to_string())
        })
    })
    .await
}
