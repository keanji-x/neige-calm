//! Durable settlement recording; success is separate from display completion.
use super::session::Shared;
use crate::error::CalmError;
use std::time::Duration;
const RECORD_TIMEOUT: Duration = Duration::from_secs(5);

/// Owner completion gets its own bounded write even when driver settlement timed out.
pub(super) async fn record_outcome(
    shared: &Shared,
    thread: &str,
    turn_id: &str,
    turn: &serde_json::Value,
    confirmation: crate::harness::turn_outcome::Confirmation,
) -> crate::error::Result<()> {
    let params = &shared.params;
    let recorded = tokio::time::timeout(
        RECORD_TIMEOUT,
        crate::harness::turn_outcome::record(
            params.repo.as_ref(),
            &params.worker_session_id,
            &params.card_id,
            &params.track_id,
            thread,
            turn_id,
            turn,
            confirmation,
        ),
    )
    .await;
    match recorded {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => {
            tracing::warn!(
                worker_session_id = %params.worker_session_id,
                turn_id,
                %error,
                "claude planner: turn outcome not recorded"
            );
            Err(error)
        }
        Err(_) => {
            tracing::warn!(
                worker_session_id = %params.worker_session_id,
                turn_id,
                "claude planner: turn outcome not recorded by its bound; boot recovery records it"
            );
            Err(CalmError::Conflict(
                "Claude outcome recording did not finish".into(),
            ))
        }
    }
}
